//! One ADBC connection. ADBC calls block, so each connection owns a
//! dedicated thread holding the loaded driver, the database and the
//! connection (the same model as SQLite, plan section 5.5). Results are read
//! from the driver's Arrow stream on that thread, re-chunked to the sink's
//! batch size and sent with `blocking_send` (backpressure: the thread waits
//! while the consumer does not read). Cancellation calls
//! `AdbcStatementCancel` through the statement's cancel handle, which ADBC
//! requires to be callable from any thread while the statement runs.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use adbc_core::{
    CancelHandle, Connection as _, Database as _, Driver as _, Optionable, Statement as _,
    error::Status,
    options::{
        AdbcVersion, InfoCode, ObjectDepth, OptionConnection, OptionDatabase, OptionStatement,
        OptionValue,
    },
};
use adbc_driver_manager::{ManagedConnection, ManagedDatabase, ManagedDriver};
use arrow_array::{RecordBatch, RecordBatchReader};
use ferrodb_api::{Error, Result};
use ferrodb_drivers::{
    BoxFuture, CancellationToken, Connection, ExecSummary, RemoteCancel, Request, ResolvedConfig,
    ResultSink, SinkEvent,
};
use tokio::sync::oneshot;

use crate::{
    bigquery::{self, SharedRest},
    errors,
    flavor::{DbOption, Flavor},
    native,
};

/// How long a cancelled statement may take to stop before the connection
/// is given up (the driver may not support cancellation).
const CANCEL_GRACE: Duration = Duration::from_millis(500);

type Job = Box<dyn FnOnce(&mut Worker) + Send>;

/// The driver objects, owned by the connection thread.
struct Worker {
    conn: ManagedConnection,
    // Kept for the life of the connection (ADBC: a database outlives its
    // connections).
    _db: ManagedDatabase,
    statement_options: Vec<(String, String)>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// First keyword of a statement, lower case (comments skipped).
pub(crate) fn first_keyword(sql: &str) -> String {
    let mut s = sql.trim_start();
    loop {
        if let Some(r) = s.strip_prefix("--") {
            s = r.find('\n').map_or("", |i| &r[i + 1..]).trim_start();
        } else if let Some(r) = s.strip_prefix("/*") {
            s = r.find("*/").map_or("", |i| &r[i + 2..]).trim_start();
        } else if let Some(r) = s.strip_prefix('(') {
            s = r.trim_start();
        } else {
            break;
        }
    }
    s.chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Statements run with `ExecuteUpdate` (no result set; rows affected).
/// Everything else runs with `Execute`, so unknown statements still show
/// the rows they return (`SHOW`, `CALL`, `LIST`…).
fn is_update(keyword: &str) -> bool {
    matches!(
        keyword,
        "insert"
            | "update"
            | "delete"
            | "merge"
            | "upsert"
            | "replace"
            | "create"
            | "drop"
            | "alter"
            | "truncate"
            | "grant"
            | "revoke"
            | "use"
            | "set"
            | "unset"
            | "begin"
            | "commit"
            | "rollback"
            | "comment"
            | "undrop"
            | "copy"
            | "put"
            | "remove"
            | "rm"
            | "attach"
            | "detach"
            | "vacuum"
            | "analyze"
    )
}

/// DML whose rows-affected count is meaningful.
fn is_dml(keyword: &str) -> bool {
    matches!(
        keyword,
        "insert" | "update" | "delete" | "merge" | "upsert" | "replace"
    )
}

/// Statements refused on a read-only session when the driver cannot make
/// the session read-only itself.
pub(crate) fn modifies(keyword: &str) -> bool {
    is_update(keyword)
        && !matches!(
            keyword,
            "use" | "set" | "unset" | "begin" | "commit" | "rollback"
        )
}

/// The statement's cancel handle while it runs.
#[derive(Default)]
struct Slot(Mutex<Option<Arc<dyn CancelHandle>>>);

impl Slot {
    fn set(&self, h: Option<Box<dyn CancelHandle>>) {
        if let Ok(mut g) = self.0.lock() {
            *g = h.map(Arc::from);
        }
    }
    /// Calls `AdbcStatementCancel` on its own thread: a driver may block in
    /// it until the statement stops, which must not stall the caller.
    fn cancel(&self) {
        let h = self.0.lock().ok().and_then(|g| g.clone());
        if let Some(h) = h {
            let _ = thread::Builder::new()
                .name("ferrodb-adbc-cancel".into())
                .spawn(move || {
                    let _ = h.try_cancel();
                });
        }
    }
}

/// The statement running now, for cancelling a BigQuery job by its text.
type Running = Arc<Mutex<Option<(String, u64)>>>;

struct Cancel {
    slot: Arc<Slot>,
    rest: Option<SharedRest>,
    running: Running,
}

impl Cancel {
    fn fire(&self) -> BoxFuture<'static, Result<()>> {
        self.slot.cancel();
        let job = self.running.lock().ok().and_then(|g| g.clone());
        let rest = self.rest.clone();
        Box::pin(async move {
            if let (Some(rest), Some((sql, since))) = (rest, job) {
                // Best effort: the ADBC cancel stops reading; this stops
                // the job (and its billing) on the server.
                let _ = rest.cancel_matching(&sql, since).await;
            }
            Ok(())
        })
    }
}

impl RemoteCancel for Cancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        self.fire()
    }
}

fn conn_err(e: adbc_core::error::Error) -> Error {
    Error::Connection(e.message)
}

impl Worker {
    fn open(
        path: &Path,
        options: Vec<DbOption>,
        statement_options: Vec<(String, String)>,
    ) -> Result<Self> {
        let driver = ManagedDriver::load_dynamic_from_filename(path, None, AdbcVersion::V110)
            .or_else(|_| ManagedDriver::load_dynamic_from_filename(path, None, AdbcVersion::V100))
            .map_err(|e| Error::Connection(format!("{}: {}", path.display(), e.message)))?;
        // Go-based drivers (Snowflake, BigQuery) cannot be unloaded: keep
        // the library loaded for the life of the process.
        std::mem::forget(driver.clone());
        let mut driver = driver;
        let opts = options.into_iter().map(|o| match o {
            DbOption::Uri(u) => (OptionDatabase::Uri, OptionValue::from(u)),
            DbOption::Username(u) => (OptionDatabase::Username, OptionValue::from(u)),
            DbOption::Password(p) => (OptionDatabase::Password, OptionValue::from(p)),
            DbOption::Other(k, v) => (OptionDatabase::Other(k), OptionValue::from(v)),
        });
        let db = driver.new_database_with_opts(opts).map_err(conn_err)?;
        let conn = db.new_connection().map_err(conn_err)?;
        Ok(Self {
            conn,
            _db: db,
            statement_options,
        })
    }

    fn info(&self) -> Vec<(String, String)> {
        let codes: HashSet<InfoCode> = [
            InfoCode::VendorName,
            InfoCode::VendorVersion,
            InfoCode::DriverName,
            InfoCode::DriverVersion,
        ]
        .into_iter()
        .collect();
        self.conn
            .get_info(Some(codes))
            .ok()
            .and_then(|r| native::flatten_info(r).ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| Some((r.first()?.clone()?, r.get(1)?.clone()?)))
            .collect()
    }

    fn version(&self, fallback: &str) -> String {
        let info = self.info();
        let get = |k: &str| info.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        match (get("vendor_name"), get("vendor_version")) {
            (Some(n), Some(v)) if !v.is_empty() && v != "unknown" => format!("{n} {v}"),
            (Some(n), _) => n,
            _ => fallback.to_owned(),
        }
    }
}

/// Sends a reader's batches, sliced to the sink's batch size.
fn stream_reader(
    reader: Box<dyn RecordBatchReader + Send>,
    sink: &ResultSink,
    cancel: &CancellationToken,
    slot: &Slot,
    sql: &str,
) -> Result<ExecSummary> {
    let schema = reader.schema();
    let mut summary = ExecSummary::default();
    if schema.fields().is_empty() {
        // DDL run through `Execute`: no result set.
        for b in reader {
            b.map_err(|e| Error::Other(e.to_string()))?;
        }
        return Ok(summary);
    }
    summary.results = 1;
    if sink.blocking_send(SinkEvent::Begin { schema }).is_err() {
        summary.truncated = true;
        slot.cancel();
        return Ok(summary);
    }
    let rows = sink.batch_rows();
    for batch in reader {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let batch: RecordBatch = batch.map_err(|e| {
            let msg = e.to_string();
            if msg.to_ascii_lowercase().contains("cancel") {
                Error::Cancelled
            } else {
                errors::map(
                    adbc_core::error::Error::with_message_and_status(msg, Status::Internal),
                    sql,
                )
            }
        })?;
        let mut offset = 0;
        while offset < batch.num_rows() {
            let len = rows.min(batch.num_rows() - offset);
            if sink
                .blocking_send(SinkEvent::Batch(batch.slice(offset, len)))
                .is_err()
            {
                summary.truncated = true;
                slot.cancel();
                return Ok(summary);
            }
            offset += len;
        }
    }
    let _ = sink.blocking_send(SinkEvent::End);
    Ok(summary)
}

fn execute_blocking(
    w: &mut Worker,
    sql: &str,
    sink: &ResultSink,
    cancel: &CancellationToken,
    slot: &Slot,
) -> Result<ExecSummary> {
    let mut stmt = w.conn.new_statement().map_err(|e| errors::map(e, ""))?;
    for (k, v) in &w.statement_options {
        let _ = stmt.set_option(OptionStatement::Other(k.clone()), v.as_str().into());
    }
    stmt.set_sql_query(sql).map_err(|e| errors::map(e, sql))?;
    slot.set(Some(stmt.get_cancel_handle()));
    let keyword = first_keyword(sql);
    let r = if is_update(&keyword) {
        stmt.execute_update()
            .map(|n| ExecSummary {
                affected: if is_dml(&keyword) {
                    n.and_then(|n| u64::try_from(n).ok())
                } else {
                    None
                },
                ..Default::default()
            })
            .map_err(|e| errors::map(e, sql))
    } else {
        match stmt.execute() {
            Ok(reader) => stream_reader(reader, sink, cancel, slot, sql),
            Err(e) => Err(errors::map(e, sql)),
        }
    };
    slot.set(None);
    if cancel.is_cancelled() && r.is_err() {
        return Err(Error::Cancelled);
    }
    r
}

/// Column names and text rows of a native call.
type TextRows = (Vec<&'static str>, Vec<Vec<Option<String>>>);

/// `GetObjects` / `GetInfo` / `GetTableSchema` / `GetTableTypes` as flat
/// text rows.
fn native_blocking(w: &mut Worker, method: &str, args: &[(String, String)]) -> Result<TextRows> {
    let arg = |k: &str| {
        args.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    };
    let arrow = |e: arrow_schema::ArrowError| Error::Other(e.to_string());
    let adbc = |e| errors::map(e, "");
    match method {
        "get_objects" => {
            let depth = match arg("depth").unwrap_or("all") {
                "catalogs" => ObjectDepth::Catalogs,
                "schemas" => ObjectDepth::Schemas,
                "tables" => ObjectDepth::Tables,
                "columns" => ObjectDepth::Columns,
                _ => ObjectDepth::All,
            };
            let table_type = arg("table_type").map(|t| t.split(',').map(str::trim).collect());
            let reader = w
                .conn
                .get_objects(
                    depth,
                    arg("catalog"),
                    arg("schema"),
                    arg("table"),
                    table_type,
                    arg("column"),
                )
                .map_err(adbc)?;
            Ok((
                native::OBJECT_COLUMNS.to_vec(),
                native::flatten_objects(reader).map_err(arrow)?,
            ))
        }
        "get_table_schema" => {
            let table = arg("table")
                .ok_or_else(|| Error::Other("get_table_schema: `table` is required".into()))?;
            let schema = w
                .conn
                .get_table_schema(arg("catalog"), arg("schema"), table)
                .map_err(adbc)?;
            Ok((
                vec!["column_name", "arrow_type", "is_nullable"],
                schema
                    .fields()
                    .iter()
                    .map(|f| {
                        vec![
                            Some(f.name().clone()),
                            Some(f.data_type().to_string()),
                            Some(if f.is_nullable() { "YES" } else { "NO" }.to_owned()),
                        ]
                    })
                    .collect(),
            ))
        }
        "get_info" => Ok((
            vec!["info_name", "info_value"],
            native::flatten_info(w.conn.get_info(None).map_err(adbc)?).map_err(arrow)?,
        )),
        "get_table_types" => {
            let reader = w.conn.get_table_types().map_err(adbc)?;
            let mut rows = Vec::new();
            for b in reader {
                let b = b.map_err(arrow)?;
                for i in 0..b.num_rows() {
                    rows.push(vec![native::cell_text(b.column(0).as_ref(), i)]);
                }
            }
            Ok((vec!["table_type"], rows))
        }
        other => Err(Error::Unsupported(format!("native call `{other}`"))),
    }
}

/// Sends a small text result.
async fn send_rows(
    sink: &ResultSink,
    names: &[&str],
    rows: &[Vec<Option<String>>],
) -> Result<ExecSummary> {
    let batch = native::text_batch(names, rows);
    let mut s = ExecSummary {
        results: 1,
        ..Default::default()
    };
    if sink
        .send(SinkEvent::Begin {
            schema: batch.schema(),
        })
        .await
        .is_err()
    {
        s.truncated = true;
        return Ok(s);
    }
    let mut offset = 0;
    while offset < batch.num_rows() {
        let len = sink.batch_rows().min(batch.num_rows() - offset);
        if sink
            .send(SinkEvent::Batch(batch.slice(offset, len)))
            .await
            .is_err()
        {
            s.truncated = true;
            return Ok(s);
        }
        offset += len;
    }
    let _ = sink.send(SinkEvent::End).await;
    Ok(s)
}

pub struct AdbcConnection {
    jobs: std_mpsc::Sender<Job>,
    flavor: Flavor,
    version: String,
    slot: Arc<Slot>,
    running: Running,
    rest: Option<SharedRest>,
    read_only: bool,
    /// The driver enforces read-only sessions itself.
    driver_read_only: bool,
    schema: Option<String>,
    in_tx: bool,
    alive: Arc<AtomicBool>,
}

impl AdbcConnection {
    pub(crate) async fn open(
        flavor: Flavor,
        library: PathBuf,
        cfg: &ResolvedConfig,
    ) -> Result<Self> {
        let options = flavor.database_options(cfg)?;
        let statement_options = flavor.statement_options(cfg);
        let rest = match flavor {
            // Missing REST credentials only disable cost estimates and
            // plans; the ADBC driver may still connect (it has its own
            // credential chain).
            Flavor::BigQuery => bigquery::Rest::from_config(cfg).ok().map(Arc::new),
            _ => None,
        };
        let (tx, rx) = oneshot::channel();
        let (jobs, jobs_rx) = std_mpsc::channel::<Job>();
        let fallback = flavor.name().to_owned();
        thread::Builder::new()
            .name(format!("ferrodb-adbc-{}", flavor.id()))
            .spawn(move || {
                let mut worker = match Worker::open(&library, options, statement_options) {
                    Ok(w) => w,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                let _ = tx.send(Ok(worker.version(&fallback)));
                while let Ok(job) = jobs_rx.recv() {
                    job(&mut worker);
                }
            })
            .map_err(|e| Error::Other(e.to_string()))?;
        let version = rx
            .await
            .map_err(|_| Error::Connection("ADBC driver thread exited".into()))??;
        let mut c = Self {
            jobs,
            schema: match &flavor {
                Flavor::BigQuery => cfg.prop("dataset").map(str::to_owned),
                Flavor::Snowflake => cfg.prop("schema").map(str::to_owned),
                Flavor::Generic(_) => None,
            },
            flavor,
            version,
            slot: Arc::new(Slot::default()),
            running: Arc::new(Mutex::new(None)),
            rest,
            read_only: false,
            driver_read_only: false,
            in_tx: false,
            alive: Arc::new(AtomicBool::new(true)),
        };
        if cfg.read_only {
            c.apply_read_only(true).await?;
        }
        Ok(c)
    }

    /// Runs `f` on the connection thread.
    fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Worker) -> Result<T> + Send + 'static,
    ) -> BoxFuture<'static, Result<T>> {
        let (tx, rx) = oneshot::channel();
        let sent = self.jobs.send(Box::new(move |w| {
            let _ = tx.send(f(w));
        }));
        let alive = self.alive.clone();
        Box::pin(async move {
            if !alive.load(Ordering::Relaxed) {
                return Err(Error::ConnectionLost);
            }
            sent.map_err(|_| Error::ConnectionLost)?;
            rx.await.map_err(|_| Error::ConnectionLost)?
        })
    }

    async fn apply_read_only(&mut self, read_only: bool) -> Result<()> {
        self.read_only = read_only;
        let r = self
            .run(move |w| {
                w.conn
                    .set_option(OptionConnection::ReadOnly, OptionValue::from(read_only))
                    .map_err(|e| errors::map(e, ""))
            })
            .await;
        self.driver_read_only = read_only && r.is_ok();
        Ok(())
    }

    fn cancel_handle(&self) -> Cancel {
        Cancel {
            slot: self.slot.clone(),
            rest: self.rest.clone(),
            running: self.running.clone(),
        }
    }

    async fn statement(
        &mut self,
        sql: String,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> Result<ExecSummary> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err(Error::ConnectionLost);
        }
        let keyword = first_keyword(&sql);
        if self.read_only && !self.driver_read_only && modifies(&keyword) {
            return Err(Error::ReadOnly);
        }
        if let Ok(mut g) = self.running.lock() {
            *g = Some((sql.clone(), now_ms()));
        }
        let slot = self.slot.clone();
        let token = cancel.clone();
        let text = sql.clone();
        // The thread writes into its own small channel, forwarded here: a
        // cancelled statement that the driver cannot stop keeps that
        // channel, not the caller's sink (whose reader would wait forever).
        let (inner, mut rx) = ResultSink::channel(2, sink.batch_rows());
        let job = self.run(move |w| execute_blocking(w, &text, &inner, &token, &slot));
        let forward = async move {
            while let Some(ev) = rx.recv().await {
                if sink.send(ev).await.is_err() {
                    // Consumer gone: dropping `rx` shows the thread a
                    // closed sink, and it stops.
                    break;
                }
            }
        };
        let job = async move {
            let (r, ()) = futures::join!(job, forward);
            r
        };
        tokio::pin!(job);
        let r = tokio::select! {
            r = &mut job => r,
            _ = cancel.cancelled() => {
                let _ = self.cancel_handle().fire().await;
                // Wait for the thread so the connection is idle again; a
                // driver without cancellation leaves it busy: give it up.
                match tokio::time::timeout(CANCEL_GRACE, &mut job).await {
                    Ok(_) => {}
                    Err(_) => self.alive.store(false, Ordering::Relaxed),
                }
                Err(Error::Cancelled)
            }
        };
        if let Ok(mut g) = self.running.lock() {
            *g = None;
        }
        if r.is_ok() && keyword == "use" {
            // `USE SCHEMA x` / `USE WAREHOUSE x`: forget the remembered
            // schema, the server knows it.
            self.schema = None;
        }
        r
    }

    async fn native(
        &mut self,
        method: String,
        args: Vec<(String, String)>,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> Result<ExecSummary> {
        let arg = |k: &str| {
            args.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };
        let dq = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        // Snowflake session objects.
        if self.flavor == Flavor::Snowflake {
            let sql = match method.as_str() {
                "warehouses" => Some("show warehouses".to_owned()),
                "roles" => Some("show roles".to_owned()),
                "databases" => Some("show databases".to_owned()),
                "current_context" => Some(
                    "select current_account() as account, current_region() as region, \
                     current_warehouse() as warehouse, current_role() as role, \
                     current_database() as database, current_schema() as schema"
                        .to_owned(),
                ),
                "use_warehouse" | "use_role" | "use_database" => {
                    let name = arg("name")
                        .ok_or_else(|| Error::Other(format!("{method}: `name` is required")))?;
                    let what = method.trim_start_matches("use_");
                    Some(format!("use {what} {}", dq(&name)))
                }
                _ => None,
            };
            if let Some(sql) = sql {
                return self.statement(sql, sink, cancel).await;
            }
        }
        // BigQuery REST.
        if self.flavor == Flavor::BigQuery
            && matches!(
                method.as_str(),
                "dry_run" | "explain" | "jobs" | "cancel_job"
            )
        {
            let rest = self.rest.clone().ok_or_else(|| {
                Error::Connection(
                    "BigQuery REST: no credentials (service account key, application default \
                     credentials or gcloud)"
                        .into(),
                )
            })?;
            return match method.as_str() {
                "dry_run" => {
                    let sql = arg("sql").unwrap_or_default();
                    let job = rest.dry_run(&sql).await?;
                    let bytes = bigquery::bytes_processed(&job);
                    send_rows(
                        &sink,
                        &["bytes", "statement_type", "job"],
                        &[vec![
                            bytes.map(|b| b.to_string()),
                            job.pointer("/statistics/query/statementType")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned),
                            Some(job.to_string()),
                        ]],
                    )
                    .await
                }
                "explain" => {
                    let sql = arg("sql").unwrap_or_default();
                    let job = if arg("analyze").as_deref() == Some("true") {
                        rest.run_job(&sql, &cancel).await?
                    } else {
                        rest.dry_run(&sql).await?
                    };
                    send_rows(&sink, &["plan"], &[vec![Some(job.to_string())]]).await
                }
                "jobs" => {
                    let limit = arg("limit").and_then(|l| l.parse().ok()).unwrap_or(50);
                    let jobs = rest.jobs(arg("state").as_deref(), limit, None).await?;
                    let rows: Vec<_> = jobs.iter().map(bigquery::job_row).collect();
                    send_rows(&sink, &bigquery::JOB_COLUMNS, &rows).await
                }
                _ => {
                    let id = arg("job_id")
                        .ok_or_else(|| Error::Other("cancel_job: `job_id` is required".into()))?;
                    rest.cancel_job(&id, arg("location").as_deref()).await?;
                    Ok(ExecSummary::default())
                }
            };
        }
        if method == "dry_run" || method == "explain" {
            return Err(Error::Unsupported(format!(
                "`{method}` on {}",
                self.flavor.name()
            )));
        }
        let (names, rows) = self
            .run(move |w| native_blocking(w, &method, &args))
            .await?;
        send_rows(&sink, &names, &rows).await
    }

    /// First column of the first row of a query.
    async fn scalar(&mut self, sql: &str) -> Result<Option<String>> {
        let out = ferrodb_drivers::sink::collect(self, sql, CancellationToken::new()).await?;
        Ok(out
            .results
            .first()
            .and_then(|r| r.batches.iter().find(|b| b.num_rows() > 0))
            .and_then(|b| native::cell_text(b.column(0).as_ref(), 0)))
    }
}

impl Connection for AdbcConnection {
    fn server_version(&self) -> String {
        self.version.clone()
    }

    fn execute<'a>(
        &'a mut self,
        req: Request,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ExecSummary>> {
        Box::pin(async move {
            match req {
                Request::Native { method, args } => self.native(method, args, sink, cancel).await,
                other => {
                    let text = other.into_text()?;
                    self.statement(text, sink, cancel).await
                }
            }
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(Arc::new(self.cancel_handle()))
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.apply_read_only(read_only).await })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let s = schema.to_owned();
            let by_option = self
                .run(move |w| {
                    w.conn
                        .set_option(OptionConnection::CurrentSchema, OptionValue::from(s))
                        .map_err(|e| errors::map(e, ""))
                })
                .await;
            if by_option.is_err() {
                match self.flavor.use_schema_sql(schema) {
                    Some(sql) => {
                        ferrodb_drivers::sink::collect(self, sql, CancellationToken::new()).await?;
                    }
                    None if self.flavor.dialect() == "sqlite" && schema != "main" => {
                        return Err(Error::Unsupported(
                            "SQLite cannot change the default schema; qualify names instead".into(),
                        ));
                    }
                    None => {}
                }
            }
            self.schema = Some(schema.to_owned());
            Ok(())
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            if let Some(s) = &self.schema {
                return Ok(Some(s.clone()));
            }
            let by_option = self
                .run(|w| {
                    w.conn
                        .get_option_string(OptionConnection::CurrentSchema)
                        .map_err(|e| errors::map(e, ""))
                })
                .await
                .ok()
                .filter(|s| !s.is_empty());
            if by_option.is_some() {
                return Ok(by_option);
            }
            match self.flavor.current_schema_sql() {
                Some(sql) => self.scalar(sql).await,
                None => Ok(None),
            }
        })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if self.flavor.transactions() != ferrodb_api::TxSupport::MultiStatement {
                return Err(Error::Unsupported(format!(
                    "{} has no multi-statement transactions here",
                    self.flavor.name()
                )));
            }
            self.run(|w| {
                w.conn
                    .set_option(OptionConnection::AutoCommit, OptionValue::from(false))
                    .map_err(|e| errors::map(e, ""))
            })
            .await?;
            self.in_tx = true;
            Ok(())
        })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if !self.in_tx {
                return Ok(());
            }
            self.in_tx = false;
            self.run(|w| {
                let r = w.conn.commit().map_err(|e| errors::map(e, ""));
                let _ = w
                    .conn
                    .set_option(OptionConnection::AutoCommit, OptionValue::from(true));
                r
            })
            .await
        })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if !self.in_tx {
                return Ok(());
            }
            self.in_tx = false;
            self.run(|w| {
                let r = w.conn.rollback().map_err(|e| errors::map(e, ""));
                let _ = w
                    .conn
                    .set_option(OptionConnection::AutoCommit, OptionValue::from(true));
                r
            })
            .await
        })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            ferrodb_drivers::sink::collect(self, "select 1", CancellationToken::new())
                .await
                .map(|_| ())
        })
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_kinds() {
        assert_eq!(first_keyword("/* x */ -- y\n  (SELECT 1)"), "select");
        assert!(is_update("insert") && is_dml("insert"));
        assert!(is_update("use") && !modifies("use"));
        assert!(modifies("create") && !modifies("select"));
        assert!(!is_update("show") && !is_update("call"));
    }
}
