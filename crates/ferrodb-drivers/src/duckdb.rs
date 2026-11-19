//! DuckDB (`duckdb`, compiled from source with the Parquet and JSON
//! readers): a database file or an in-memory database, and **file data
//! sources** (plan, phase 7) — CSV, TSV, Parquet and JSON files given in the
//! `files` property appear as views named after the files, so they can be
//! queried, joined and exported like tables.
//!
//! Like SQLite the API is blocking: each connection owns a thread; results
//! stream to the sink as DuckDB's own Arrow batches (no conversion), cut to
//! the sink's batch size; `duckdb_interrupt` cancels.

use std::{
    path::Path,
    sync::{Arc, mpsc as std_mpsc},
    thread,
};

use duckdb::{AccessMode, Config, InterruptHandle};
use ferrodb_api::{
    AuthMethod, DriverCategory, DriverDescriptor, DriverId, Error, HierarchySpec, ObjectKind,
    QueryError, Result, SqlDialectId,
};
use tokio::sync::oneshot;

use crate::{
    BoxFuture, CancellationToken, Connection, Driver, ExecSummary, FamilyVariant, ListQuery,
    Quirks, RemoteCancel, Request, ResolvedConfig, ResultSink, SinkEvent, quote_literal,
    registry::{descriptor, property, relational_capabilities, relational_hierarchy},
};

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![Arc::new(DuckDbDriver)]
}

pub struct DuckDbDriver;

struct DuckDbVariant;

impl FamilyVariant for DuckDbVariant {
    fn driver_id(&self) -> DriverId {
        "duckdb".into()
    }
    fn dialect(&self) -> SqlDialectId {
        "duckdb".into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        vec![AuthMethod::None]
    }
    fn quirks(&self) -> Quirks {
        Quirks {
            transactional_ddl: true,
            read_only_sessions: false,
        }
    }
    fn hierarchy(&self) -> HierarchySpec {
        relational_hierarchy(ObjectKind::Schema)
    }
    fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        match path {
            [] => vec![ListQuery {
                sql: "select schema_name as name from duckdb_schemas() \
                      where database_name = current_database() and not internal order by 1"
                    .into(),
                kind: ObjectKind::Schema,
                has_children: true,
            }],
            [(ObjectKind::Schema, s)] => vec![ListQuery {
                sql: format!(
                    "select table_name as name from information_schema.tables \
                     where table_schema = {} order by 1",
                    quote_literal(s)
                ),
                kind: ObjectKind::Table,
                has_children: true,
            }],
            _ => Vec::new(),
        }
    }
}

/// The view name for a data file: its stem, lower-case, other characters
/// replaced by `_` (`Sales 2025.csv` → `sales_2025`).
pub fn view_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("data")
        .trim_end_matches(".csv")
        .trim_end_matches(".json");
    let mut out: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 't');
        out.insert(1, '_');
    }
    out
}

/// The DuckDB reader for a data file, by extension.
pub fn reader(path: &Path) -> Result<String> {
    let p = path.to_string_lossy().replace('\\', "/");
    let lit = quote_literal(&p);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    // Compressed CSV / JSON (`x.csv.gz`) read the same way.
    let inner = path
        .file_stem()
        .and_then(|s| Path::new(s).extension())
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let kind = match (ext.as_str(), inner.as_str()) {
        ("gz" | "zst", i) => i.to_owned(),
        (e, _) => e.to_owned(),
    };
    Ok(match kind.as_str() {
        "csv" => format!("read_csv({lit})"),
        "tsv" | "tab" => format!("read_csv({lit}, delim = '\\t')"),
        "parquet" => format!("read_parquet({lit})"),
        "json" | "jsonl" | "ndjson" => format!("read_json({lit})"),
        _ => {
            return Err(Error::Connection(format!(
                "{p}: unsupported data file (CSV, TSV, Parquet or JSON expected)"
            )));
        }
    })
}

/// Data files of the `files` property: one per line or separated by `;`.
pub fn data_files(prop: Option<&str>) -> Vec<String> {
    prop.unwrap_or_default()
        .split(['\n', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

impl Driver for DuckDbDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let mut caps = relational_capabilities("duckdb", vec![AuthMethod::None]);
        caps.editable_results = true;
        let mut d = descriptor(
            "duckdb",
            "duckdb",
            "DuckDB",
            caps,
            vec![
                property("file", "Database file (empty = in memory)", None, false),
                property(
                    "files",
                    "Data files (CSV, Parquet, JSON; one per line)",
                    None,
                    false,
                ),
            ],
            None,
            "duckdb:{file}",
        );
        d.category = DriverCategory::AnalyticalCloud;
        d
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(DuckDbVariant)
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let file = cfg
                .prop("file")
                .filter(|f| *f != ":memory:")
                .map(str::to_owned);
            let files = data_files(cfg.prop("files"));
            if file.is_none() && files.is_empty() {
                return Err(Error::Connection(
                    "DuckDB: give a database file or at least one data file".into(),
                ));
            }
            let mut views = Vec::new();
            for f in &files {
                let path = Path::new(f);
                if !path.exists() {
                    return Err(Error::Connection(format!("{f}: file not found")));
                }
                let name = view_name(path);
                if views.iter().any(|(n, _)| *n == name) {
                    return Err(Error::Connection(format!(
                        "two data files would both be named `{name}`"
                    )));
                }
                views.push((name, reader(path)?));
            }
            // A database file opened read-only; data files only read anyway.
            let read_only = cfg.read_only && file.is_some();
            let (tx, rx) = oneshot::channel();
            let (jobs_tx, jobs_rx) = std_mpsc::channel::<Job>();
            thread::Builder::new()
                .name("ferrodb-duckdb".into())
                .spawn(move || {
                    let opened = (|| -> duckdb::Result<duckdb::Connection> {
                        let mut config = Config::default();
                        if read_only {
                            config = config.access_mode(AccessMode::ReadOnly)?;
                        }
                        let c = match &file {
                            Some(f) => duckdb::Connection::open_with_flags(f, config)?,
                            None => duckdb::Connection::open_in_memory_with_flags(config)?,
                        };
                        for (name, reader) in &views {
                            c.execute_batch(&format!(
                                "CREATE OR REPLACE TEMP VIEW \"{name}\" AS SELECT * FROM {reader}"
                            ))?;
                        }
                        Ok(c)
                    })();
                    let mut conn = match opened {
                        Ok(c) => {
                            let version = c
                                .query_row("select version()", [], |r| r.get::<_, String>(0))
                                .unwrap_or_else(|_| "?".into());
                            let _ = tx.send(Ok((c.interrupt_handle(), version)));
                            c
                        }
                        Err(e) => {
                            let _ = tx.send(Err(Error::Connection(format!("DuckDB: {e}"))));
                            return;
                        }
                    };
                    while let Ok(job) = jobs_rx.recv() {
                        job(&mut conn);
                    }
                })
                .map_err(|e| Error::Other(e.to_string()))?;
            let (interrupt, version) = rx
                .await
                .map_err(|_| Error::Connection("DuckDB thread exited".into()))??;
            Ok(Box::new(DuckDbConnection {
                jobs: jobs_tx,
                interrupt: Arc::new(DuckCancel(interrupt)),
                in_tx: false,
                version,
            }) as Box<dyn Connection>)
        })
    }
}

type Job = Box<dyn FnOnce(&mut duckdb::Connection) + Send>;

struct DuckDbConnection {
    jobs: std_mpsc::Sender<Job>,
    interrupt: Arc<DuckCancel>,
    in_tx: bool,
    version: String,
}

struct DuckCancel(Arc<InterruptHandle>);

impl RemoteCancel for DuckCancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        self.0.interrupt();
        Box::pin(async { Ok(()) })
    }
}

fn map_err(e: duckdb::Error) -> Error {
    let msg = e.to_string();
    if msg.contains("INTERRUPT") || msg.to_ascii_lowercase().contains("interrupted") {
        return Error::Cancelled;
    }
    Error::Query(QueryError {
        message: msg,
        code: None,
        range: None,
    })
}

impl DuckDbConnection {
    fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut duckdb::Connection) -> Result<T> + Send + 'static,
    ) -> BoxFuture<'static, Result<T>> {
        let (tx, rx) = oneshot::channel();
        let sent = self.jobs.send(Box::new(move |c| {
            let _ = tx.send(f(c));
        }));
        Box::pin(async move {
            sent.map_err(|_| Error::ConnectionLost)?;
            rx.await.map_err(|_| Error::ConnectionLost)?
        })
    }
}

fn first_word(text: &str) -> String {
    let mut s = text.trim_start();
    // Skip leading comments.
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = rest
                .split_once('\n')
                .map(|(_, r)| r)
                .unwrap_or("")
                .trim_start();
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest
                .split_once("*/")
                .map(|(_, r)| r)
                .unwrap_or("")
                .trim_start();
        } else {
            break;
        }
    }
    if s.starts_with('(') {
        return "(".into();
    }
    s.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn execute_blocking(
    conn: &mut duckdb::Connection,
    text: &str,
    sink: &ResultSink,
    cancel: &CancellationToken,
) -> Result<ExecSummary> {
    let word = first_word(text);
    let mut stmt = conn.prepare(text).map_err(map_err)?;
    let returns_rows = matches!(
        word.as_str(),
        "select"
            | "with"
            | "from"
            | "values"
            | "table"
            | "show"
            | "describe"
            | "summarize"
            | "explain"
            | "pragma"
            | "call"
            | "pivot"
            | "unpivot"
            | "("
    );
    if !returns_rows {
        let n = stmt.execute([]).map_err(map_err)?;
        let dml = matches!(
            word.as_str(),
            "insert" | "update" | "delete" | "merge" | "copy"
        );
        return Ok(ExecSummary {
            affected: dml.then_some(n as u64),
            results: 0,
            truncated: false,
        });
    }
    let stream = stmt.stream_arrow([]).map_err(map_err)?;
    let schema = stream.get_schema();
    let mut summary = ExecSummary {
        results: 1,
        ..Default::default()
    };
    if sink.blocking_send(SinkEvent::Begin { schema }).is_err() {
        summary.truncated = true;
        return Ok(summary);
    }
    let rows = sink.batch_rows();
    for batch in stream {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let batch = batch.map_err(map_err)?;
        let mut offset = 0;
        while offset < batch.num_rows() {
            let len = rows.min(batch.num_rows() - offset);
            if sink
                .blocking_send(SinkEvent::Batch(batch.slice(offset, len)))
                .is_err()
            {
                summary.truncated = true;
                return Ok(summary);
            }
            offset += len;
        }
    }
    let _ = sink.blocking_send(SinkEvent::End);
    Ok(summary)
}

impl Connection for DuckDbConnection {
    fn server_version(&self) -> String {
        format!("DuckDB {}", self.version)
    }

    fn execute<'a>(
        &'a mut self,
        req: Request,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ExecSummary>> {
        let text = match req.into_text() {
            Ok(t) => t,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let interrupt = self.interrupt.clone();
        let token = cancel.clone();
        let job = self.run(move |c| execute_blocking(c, &text, &sink, &token));
        Box::pin(async move {
            tokio::pin!(job);
            tokio::select! {
                r = &mut job => r,
                _ = cancel.cancelled() => {
                    interrupt.0.interrupt();
                    let _ = job.await;
                    Err(Error::Cancelled)
                }
            }
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(self.interrupt.clone())
    }

    fn set_read_only(&mut self, _read_only: bool) -> BoxFuture<'_, Result<()>> {
        // Fixed when the database is opened; the core also refuses writes.
        Box::pin(async { Ok(()) })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        let sql = format!("SET schema = {}", quote_literal(schema));
        self.run(move |c| c.execute_batch(&sql).map_err(map_err))
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        self.run(|c| {
            c.query_row("select current_schema()", [], |r| r.get::<_, String>(0))
                .map(Some)
                .map_err(map_err)
        })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        self.in_tx = true;
        self.run(|c| c.execute_batch("BEGIN TRANSACTION").map_err(map_err))
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        self.in_tx = false;
        self.run(|c| c.execute_batch("COMMIT").map_err(map_err))
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        let open = std::mem::take(&mut self.in_tx);
        self.run(move |c| {
            if !open {
                return Ok(());
            }
            c.execute_batch("ROLLBACK").map_err(map_err)
        })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        self.run(|c| c.execute_batch("select 1").map_err(map_err))
    }
}

#[cfg(test)]
mod tests {
    use arrow_array::{Array, Int64Array, StringArray};

    use super::*;
    use crate::sink::collect;

    #[test]
    fn view_names_and_readers() {
        assert_eq!(view_name(Path::new("C:/x/Sales 2025.csv")), "sales_2025");
        assert_eq!(view_name(Path::new("/d/2025.parquet")), "t_2025");
        assert_eq!(view_name(Path::new("/d/a.csv.gz")), "a");
        assert!(
            reader(Path::new("a.parquet"))
                .unwrap()
                .starts_with("read_parquet(")
        );
        assert!(
            reader(Path::new("a.csv.gz"))
                .unwrap()
                .starts_with("read_csv(")
        );
        assert!(reader(Path::new("a.tsv")).unwrap().contains("delim"));
        assert!(reader(Path::new("a.doc")).is_err());
        assert_eq!(
            data_files(Some("a.csv; b.json\nc.parquet\n")),
            vec!["a.csv", "b.json", "c.parquet"]
        );
    }

    #[tokio::test]
    async fn csv_and_json_files_as_views() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("Orders.csv");
        std::fs::write(&csv, "id,customer,total\n1,ala,10.5\n2,ola,20\n3,ala,7\n").unwrap();
        let json = dir.path().join("customers.json");
        std::fs::write(
            &json,
            r#"[{"name":"ala","city":"Kraków"},{"name":"ola","city":"Gdańsk"}]"#,
        )
        .unwrap();
        let files = format!("{}\n{}", csv.display(), json.display());
        let mut c = DuckDbDriver
            .connect(&ResolvedConfig::new("duckdb").with("files", files))
            .await
            .unwrap();
        let out = collect(
            c.as_mut(),
            "select c.city, count(*) as n from orders o join customers c on c.name = o.customer \
             group by c.city order by n desc",
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let b = &out.results[0].batches[0];
        let city = b.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        let n = b.column(1).as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!((city.value(0), n.value(0)), ("Kraków", 2));
        // DML on a table reports the affected rows; a big result is cut
        // into sink-sized batches.
        collect(
            c.as_mut(),
            "create table t as select * from range(5000) r(i)",
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let out = collect(
            c.as_mut(),
            "delete from t where i < 10",
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.summary.affected, Some(10));
        let out = collect(c.as_mut(), "select * from t", CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.results[0].num_rows(), 4990);
        assert!(
            out.results[0]
                .batches
                .iter()
                .all(|b| b.num_rows() <= ResultSink::BATCH_ROWS)
        );
        let err = collect(
            c.as_mut(),
            "select * from missing",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Query(_)), "{err:?}");
        assert!(
            c.server_version().starts_with("DuckDB v"),
            "{}",
            c.server_version()
        );
        assert_eq!(c.current_schema().await.unwrap().as_deref(), Some("main"));
        // Missing files are reported at connect.
        let e = DuckDbDriver
            .connect(&ResolvedConfig::new("duckdb").with("files", "nope.csv"))
            .await;
        assert!(e.is_err());
    }

    #[tokio::test]
    async fn cancel_interrupts() {
        let mut c = DuckDbDriver
            .connect(
                &ResolvedConfig::new("duckdb")
                    .with("file", ":memory:")
                    .with("files", ""),
            )
            .await;
        // An in-memory database needs a data file or a file name.
        assert!(c.is_err());
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("x.duckdb");
        c = DuckDbDriver
            .connect(&ResolvedConfig::new("duckdb").with("file", db.to_string_lossy()))
            .await;
        let mut c = c.unwrap();
        let cancel = CancellationToken::new();
        let t = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            t.cancel();
        });
        let started = std::time::Instant::now();
        let r = collect(
            c.as_mut(),
            "select count(*) from range(100000000000) a",
            cancel,
        )
        .await;
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        c.ping().await.unwrap();
    }
}
