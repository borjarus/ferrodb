//! SQLite (`rusqlite`, bundled). Blocking API → each connection owns a
//! dedicated thread (plan, section 5.5); results go to the sink with
//! `blocking_send`, cancellation uses `sqlite3_interrupt`.

use std::{
    sync::{Arc, mpsc as std_mpsc},
    thread,
};

use ferrodb_api::{
    AuthMethod, DriverDescriptor, DriverId, Error, HierarchySpec, NoticeLevel, ObjectKind,
    QueryError, Result, ServerNotice, SqlDialectId,
};
use rusqlite::{OpenFlags, types::ValueRef};
use tokio::sync::oneshot;

use crate::{
    BoxFuture, CancellationToken, Connection, Driver, ExecSummary, FamilyVariant, ListQuery,
    Quirks, RemoteCancel, Request, ResolvedConfig, ResultSink, SinkEvent,
    columns::{BatchBuilder, Cell, ColumnSpec, ColumnType, schema},
    quote_ident, quote_literal,
    registry::{descriptor, property, relational_capabilities, relational_hierarchy},
};

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![Arc::new(SqliteDriver)]
}

pub struct SqliteDriver;

struct SqliteVariant;

impl FamilyVariant for SqliteVariant {
    fn driver_id(&self) -> DriverId {
        "sqlite".into()
    }
    fn dialect(&self) -> SqlDialectId {
        "sqlite".into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        vec![AuthMethod::None]
    }
    fn quirks(&self) -> Quirks {
        Quirks {
            transactional_ddl: true,
            read_only_sessions: true,
        }
    }
    fn hierarchy(&self) -> HierarchySpec {
        relational_hierarchy(ObjectKind::Schema)
    }
    fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        match path {
            [] => vec![ListQuery {
                sql: "select name from pragma_database_list order by seq".into(),
                kind: ObjectKind::Schema,
                has_children: true,
            }],
            [(ObjectKind::Schema, s)] => {
                let master = format!("{}.sqlite_master", quote_ident(s, '"'));
                [("table", ObjectKind::Table), ("view", ObjectKind::View)]
                    .into_iter()
                    .map(|(ty, kind)| ListQuery {
                        sql: format!(
                            "select name from {master} where type = '{ty}' \
                             and name not like 'sqlite\\_%' escape '\\' order by name"
                        ),
                        kind,
                        has_children: true,
                    })
                    .collect()
            }
            [
                (ObjectKind::Schema, s),
                (ObjectKind::Table | ObjectKind::View, t),
            ] => {
                vec![ListQuery {
                    sql: format!(
                        "select name, type from pragma_table_info({}, {}) order by cid",
                        quote_literal(t),
                        quote_literal(s)
                    ),
                    kind: ObjectKind::Column,
                    has_children: false,
                }]
            }
            _ => Vec::new(),
        }
    }
}

impl Driver for SqliteDriver {
    fn descriptor(&self) -> DriverDescriptor {
        descriptor(
            "sqlite",
            "sqlite",
            "SQLite",
            relational_capabilities("sqlite", vec![AuthMethod::None]),
            vec![property("file", "File", None, true)],
            None,
            "sqlite:{file}",
        )
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(SqliteVariant)
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let path = cfg
                .prop("file")
                .ok_or_else(|| Error::Connection("SQLite: no database file given".into()))?
                .to_owned();
            let read_only = cfg.read_only;
            let (tx, rx) = oneshot::channel();
            let (jobs_tx, jobs_rx) = std_mpsc::channel::<Job>();
            thread::Builder::new()
                .name("ferrodb-sqlite".into())
                .spawn(move || {
                    let flags = if read_only {
                        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI
                    } else {
                        OpenFlags::SQLITE_OPEN_READ_WRITE
                            | OpenFlags::SQLITE_OPEN_CREATE
                            | OpenFlags::SQLITE_OPEN_URI
                    };
                    let opened =
                        rusqlite::Connection::open_with_flags(&path, flags).and_then(|c| {
                            c.busy_timeout(std::time::Duration::from_secs(5))?;
                            if read_only {
                                c.pragma_update(None, "query_only", 1)?;
                            }
                            Ok(c)
                        });
                    let mut conn = match opened {
                        Ok(c) => {
                            let _ = tx.send(Ok(c.get_interrupt_handle()));
                            c
                        }
                        Err(e) => {
                            let _ = tx.send(Err(Error::Connection(format!("{path}: {e}"))));
                            return;
                        }
                    };
                    while let Ok(job) = jobs_rx.recv() {
                        job(&mut conn);
                    }
                })
                .map_err(|e| Error::Other(e.to_string()))?;
            let interrupt = rx
                .await
                .map_err(|_| Error::Connection("SQLite thread exited".into()))??;
            Ok(Box::new(SqliteConnection {
                jobs: jobs_tx,
                interrupt: Arc::new(SqliteCancel(Arc::new(interrupt))),
            }) as Box<dyn Connection>)
        })
    }
}

type Job = Box<dyn FnOnce(&mut rusqlite::Connection) + Send>;

struct SqliteConnection {
    jobs: std_mpsc::Sender<Job>,
    interrupt: Arc<SqliteCancel>,
}

struct SqliteCancel(Arc<rusqlite::InterruptHandle>);

impl RemoteCancel for SqliteCancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        self.0.interrupt();
        Box::pin(async { Ok(()) })
    }
}

fn map_err(e: rusqlite::Error) -> Error {
    match e {
        rusqlite::Error::SqliteFailure(f, msg)
            if f.code == rusqlite::ErrorCode::OperationInterrupted =>
        {
            let _ = msg;
            Error::Cancelled
        }
        rusqlite::Error::SqliteFailure(f, msg) => Error::Query(QueryError {
            message: msg.unwrap_or_else(|| f.to_string()),
            code: Some(format!("SQLITE_{}", f.extended_code)),
            range: None,
        }),
        rusqlite::Error::SqlInputError {
            error, msg, offset, ..
        } => Error::Query(QueryError {
            message: msg,
            code: Some(format!("SQLITE_{}", error.extended_code)),
            range: usize::try_from(offset).ok().map(|o| o..o + 1),
        }),
        other => Error::Other(other.to_string()),
    }
}

impl SqliteConnection {
    /// Runs `f` on the connection thread.
    fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut rusqlite::Connection) -> Result<T> + Send + 'static,
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

/// Arrow type for a declared column type, following SQLite affinity rules
/// (https://sqlite.org/datatype3.html#determination_of_column_affinity).
fn declared_type(decl: &str) -> ColumnType {
    let d = decl.to_ascii_uppercase();
    if d.contains("INT") {
        ColumnType::Int64
    } else if d.contains("CHAR") || d.contains("CLOB") || d.contains("TEXT") {
        ColumnType::Text
    } else if d.contains("BLOB") {
        ColumnType::Binary
    } else if d.contains("REAL") || d.contains("FLOA") || d.contains("DOUB") {
        ColumnType::Float64
    } else if d.contains("BOOL") {
        ColumnType::Bool
    } else if d.contains("DATE") || d.contains("TIME") {
        // Stored as text, integer or real by convention: keep it verbatim.
        ColumnType::Text
    } else {
        // NUMERIC affinity (DECIMAL, NUMERIC…): integer or real values.
        ColumnType::Float64
    }
}

fn value_type(v: ValueRef<'_>) -> ColumnType {
    match v {
        ValueRef::Integer(_) => ColumnType::Int64,
        ValueRef::Real(_) => ColumnType::Float64,
        ValueRef::Blob(_) => ColumnType::Binary,
        ValueRef::Null | ValueRef::Text(_) => ColumnType::Text,
    }
}

fn cell(v: ValueRef<'_>) -> Cell<'_> {
    match v {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(i) => Cell::Int(i),
        ValueRef::Real(f) => Cell::Float(f),
        ValueRef::Text(t) => match std::str::from_utf8(t) {
            Ok(s) => Cell::Text(s),
            Err(_) => Cell::Bytes(t),
        },
        ValueRef::Blob(b) => Cell::Bytes(b),
    }
}

fn execute_blocking(
    conn: &mut rusqlite::Connection,
    text: &str,
    sink: &ResultSink,
    cancel: &CancellationToken,
) -> Result<ExecSummary> {
    let mut stmt = conn.prepare(text).map_err(map_err)?;
    let ncols = stmt.column_count();
    if ncols == 0 {
        stmt.raw_execute().map_err(map_err)?;
        drop(stmt);
        let affected = conn.changes();
        let first = text
            .trim_start()
            .get(..6)
            .unwrap_or("")
            .to_ascii_lowercase();
        let dml = ["insert", "update", "delete", "replac"].contains(&first.as_str());
        return Ok(ExecSummary {
            affected: dml.then_some(affected),
            results: 0,
            truncated: false,
        });
    }
    let decls: Vec<(String, Option<String>)> = stmt
        .columns()
        .map(|c| (c.name().to_owned(), c.decl_type().map(str::to_owned)))
        .collect();
    let mut rows = stmt.raw_query();
    let mut builder: Option<BatchBuilder> = None;
    let mut summary = ExecSummary {
        results: 1,
        ..Default::default()
    };
    let closed = |summary: &mut ExecSummary| {
        summary.truncated = true;
    };
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let row = rows.next().map_err(map_err)?;
        let Some(row) = row else { break };
        let b = match &mut builder {
            Some(b) => b,
            None => {
                // Columns without a declared type (expressions) take the
                // type of their first value.
                let specs: Vec<ColumnSpec> = decls
                    .iter()
                    .enumerate()
                    .map(|(i, (name, decl))| match decl {
                        Some(d) => ColumnSpec::new(name, declared_type(d), d.to_ascii_lowercase()),
                        None => {
                            let ty = row.get_ref(i).map(value_type).unwrap_or(ColumnType::Text);
                            ColumnSpec::new(name, ty, "")
                        }
                    })
                    .collect();
                let s = schema(&specs);
                if sink
                    .blocking_send(SinkEvent::Begin { schema: s.clone() })
                    .is_err()
                {
                    closed(&mut summary);
                    return Ok(summary);
                }
                builder.insert(BatchBuilder::new(s, sink.batch_rows()))
            }
        };
        for i in 0..ncols {
            let v = row.get_ref(i).map_err(map_err)?;
            b.push(i, cell(v));
        }
        b.end_row();
        if b.should_flush()
            && let Some(batch) = b.flush()
            && sink.blocking_send(SinkEvent::Batch(batch)).is_err()
        {
            closed(&mut summary);
            return Ok(summary);
        }
    }
    let b = match builder {
        Some(b) => b,
        None => {
            // No rows: the shape comes from the declarations alone.
            let specs: Vec<ColumnSpec> = decls
                .iter()
                .map(|(name, decl)| match decl {
                    Some(d) => ColumnSpec::new(name, declared_type(d), d.to_ascii_lowercase()),
                    None => ColumnSpec::new(name, ColumnType::Text, ""),
                })
                .collect();
            let s = schema(&specs);
            let _ = sink.blocking_send(SinkEvent::Begin { schema: s.clone() });
            BatchBuilder::new(s, 1)
        }
    };
    let mut b = b;
    if let Some(batch) = b.flush() {
        let _ = sink.blocking_send(SinkEvent::Batch(batch));
    }
    if b.lossy() > 0 {
        let _ = sink.blocking_send(SinkEvent::Notice(ServerNotice {
            level: NoticeLevel::Warning,
            message: format!(
                "{} value(s) did not match their column type and are shown as NULL",
                b.lossy()
            ),
        }));
    }
    let _ = sink.blocking_send(SinkEvent::End);
    Ok(summary)
}

impl Connection for SqliteConnection {
    fn server_version(&self) -> String {
        format!("SQLite {}", rusqlite::version())
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
                    // Wait for the thread to notice so the connection is
                    // idle again when we return.
                    let _ = job.await;
                    Err(Error::Cancelled)
                }
            }
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(self.interrupt.clone())
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        self.run(move |c| {
            c.pragma_update(None, "query_only", read_only as i32)
                .map_err(map_err)
        })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        // SQLite resolves unqualified names in `main` (then attached
        // databases); there is no default-schema switch.
        Box::pin(async move {
            if schema == "main" {
                Ok(())
            } else {
                Err(Error::Unsupported(
                    "SQLite cannot change the default schema; qualify names instead".into(),
                ))
            }
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async { Ok(Some("main".into())) })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        self.run(|c| c.execute_batch("BEGIN").map_err(map_err))
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        self.run(|c| c.execute_batch("COMMIT").map_err(map_err))
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        self.run(|c| {
            if c.is_autocommit() {
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
    use arrow_array::{Array, Float64Array, Int64Array, StringArray};

    use super::*;
    use crate::sink::collect;

    async fn open(file: &str) -> Box<dyn Connection> {
        SqliteDriver
            .connect(&ResolvedConfig::new("sqlite").with("file", file))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn types_dml_and_errors() {
        let mut c = open(":memory:").await;
        let cancel = CancellationToken::new();
        collect(
            c.as_mut(),
            "create table t (i integer, r real, s text, b blob, d decimal(10,2))",
            cancel.clone(),
        )
        .await
        .unwrap();
        let out = collect(
            c.as_mut(),
            "insert into t values (1, 1.5, 'a', x'00ff', 2.5), (2, null, 'b', null, 3)",
            cancel.clone(),
        )
        .await
        .unwrap();
        assert_eq!(out.summary.affected, Some(2));
        let out = collect(
            c.as_mut(),
            "select i, r, s, b, d, i * 2 as e from t order by i",
            cancel.clone(),
        )
        .await
        .unwrap();
        let r = &out.results[0];
        assert_eq!(r.num_rows(), 2);
        let b = &r.batches[0];
        assert_eq!(
            b.column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .value(1),
            2
        );
        assert!(
            b.column(1)
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .is_null(1)
        );
        assert_eq!(
            b.column(2)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(0),
            "a"
        );
        assert_eq!(
            b.column(5)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .value(1),
            4
        );
        let err = collect(c.as_mut(), "select * from missing", cancel.clone())
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Query(ref q) if q.message.contains("missing")),
            "{err:?}"
        );
        // Empty result keeps its shape.
        let out = collect(c.as_mut(), "select i, s from t where 0", cancel)
            .await
            .unwrap();
        assert_eq!(out.results[0].schema.fields().len(), 2);
        assert_eq!(out.results[0].num_rows(), 0);
    }

    #[tokio::test]
    async fn cancel_interrupts_a_long_query() {
        let mut c = open(":memory:").await;
        let cancel = CancellationToken::new();
        let t = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            t.cancel();
        });
        let started = std::time::Instant::now();
        let r = collect(
            c.as_mut(),
            "with recursive r(i) as (select 1 union all select i + 1 from r) select count(*) from r",
            cancel,
        )
        .await;
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        // The connection is usable afterwards.
        c.ping().await.unwrap();
    }

    #[tokio::test]
    async fn read_only_rejects_writes() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("db.sqlite");
        let f = f.to_str().unwrap();
        let mut w = open(f).await;
        collect(
            w.as_mut(),
            "create table t (a int)",
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let mut r = SqliteDriver
            .connect(&ResolvedConfig {
                read_only: true,
                ..ResolvedConfig::new("sqlite").with("file", f)
            })
            .await
            .unwrap();
        let e = collect(
            r.as_mut(),
            "insert into t values (1)",
            CancellationToken::new(),
        )
        .await;
        assert!(e.is_err());
    }
}
