//! One JDBC connection living in the host process.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use arrow_array::{ArrayRef, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use ferrodb_api::{Error, NoticeLevel, Result};
use ferrodb_drivers::{
    BoxFuture, CancellationToken, Connection, Endpoint, ExecSummary, RemoteCancel, Request,
    ResultSink, SinkEvent, sink::collect_request,
};
use ferrodb_lang::sql::{StatementClass, classify};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::{
    bridge::{Incoming, Process},
    flavor::Flavor,
    protocol::ArrowStream,
};

/// Record batches the host may send ahead of the consumer.
const CREDITS: i32 = 4;
/// How long a cancelled statement may take to stop before the connection is
/// given up (the host thread may be stuck in a driver that ignores cancel).
const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// `DatabaseMetaData` calls answered as result sets (`Request::Native`).
pub const META_METHODS: &[&str] = &[
    "catalogs",
    "schemas",
    "tables",
    "columns",
    "primary_keys",
    "imported_keys",
    "exported_keys",
    "index_info",
    "procedures",
    "procedure_columns",
    "functions",
    "function_columns",
    "table_types",
    "type_info",
];

pub struct JdbcConnection {
    pub(crate) proc: Arc<Process>,
    pub(crate) id: String,
    pub(crate) flavor: &'static Flavor,
    pub(crate) version: String,
    /// Writes are refused here as well as by the server (`setReadOnly` is
    /// only a hint for several drivers, H2 among them).
    pub(crate) read_only: bool,
    /// A cancelled statement did not stop: the session is unusable.
    pub(crate) broken: bool,
    /// Request id of the running statement (0 = idle), for remote cancel.
    pub(crate) current: Arc<AtomicU64>,
    /// Keeps an SSH tunnel open.
    pub(crate) _endpoint: Option<Endpoint>,
}

impl Drop for JdbcConnection {
    fn drop(&mut self) {
        self.proc.notify("close", json!({ "conn": self.id }));
        self.proc.connection_closed();
    }
}

struct JdbcCancel {
    proc: Arc<Process>,
    current: Arc<AtomicU64>,
}

impl RemoteCancel for JdbcCancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        let id = self.current.load(Ordering::Acquire);
        if id != 0 {
            self.proc.cancel(id, false);
        }
        Box::pin(async { Ok(()) })
    }
}

/// `INSERT` / `UPDATE` / `DELETE` / `MERGE`: the update count is rows
/// affected. JDBC reports 0 for DDL, which the core shows as nothing.
fn is_dml(sql: &str) -> bool {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map_or("", |(_, r)| r).trim_start();
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map_or("", |(_, r)| r).trim_start();
        } else if let Some(rest) = s.strip_prefix('(') {
            s = rest.trim_start();
        } else {
            break;
        }
    }
    let word: String = s
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        word.as_str(),
        "insert" | "update" | "delete" | "merge" | "upsert" | "replace" | "with"
    )
}

enum Forward {
    Sent,
    Closed,
    Cancelled,
}

async fn forward(
    sink: &ResultSink,
    batch: RecordBatch,
    rows: usize,
    cancel: &CancellationToken,
) -> Forward {
    let mut at = 0;
    let n = batch.num_rows();
    while at < n || (n == 0 && at == 0) {
        let len = rows.min(n - at);
        let part = if at == 0 && len == n {
            batch.clone()
        } else {
            batch.slice(at, len)
        };
        tokio::select! {
            r = sink.send(SinkEvent::Batch(part)) => if r.is_err() { return Forward::Closed },
            _ = cancel.cancelled() => return Forward::Cancelled,
        }
        at += len.max(1);
    }
    Forward::Sent
}

/// A one-column text result (plans, `info`).
async fn text_result(sink: &ResultSink, columns: &[&str], rows: Vec<Vec<String>>) -> Result<()> {
    let schema = Arc::new(Schema::new(
        columns
            .iter()
            .map(|c| Field::new(*c, DataType::Utf8, true))
            .collect::<Vec<_>>(),
    ));
    let arrays: Vec<ArrayRef> = (0..columns.len())
        .map(|i| {
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.get(i).cloned())
                    .collect::<Vec<Option<String>>>(),
            )) as ArrayRef
        })
        .collect();
    let batch =
        RecordBatch::try_new(schema.clone(), arrays).map_err(|e| Error::Other(e.to_string()))?;
    let _ = sink.send(SinkEvent::Begin { schema }).await;
    let _ = sink.send(SinkEvent::Batch(batch)).await;
    let _ = sink.send(SinkEvent::End).await;
    Ok(())
}

impl JdbcConnection {
    fn live(&self) -> Result<Arc<Process>> {
        if self.broken {
            return Err(Error::ConnectionLost);
        }
        if !self.proc.is_alive() {
            return Err(self.proc.lost());
        }
        Ok(self.proc.clone())
    }

    async fn call(&self, method: &str, mut params: Value) -> Result<Value> {
        let proc = self.live()?;
        params["conn"] = json!(self.id);
        proc.call(method, params).await
    }

    /// Runs a streaming request (`execute`, `meta`) into `sink`.
    async fn stream(
        &mut self,
        method: &str,
        mut params: Value,
        sql: Option<&str>,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> Result<ExecSummary> {
        let proc = self.live()?;
        let batch_rows = sink.batch_rows();
        params["conn"] = json!(self.id);
        params["batch_rows"] = json!(batch_rows);
        params["credits"] = json!(CREDITS);
        let mut p = proc.request(method, params)?;
        self.current.store(p.id, Ordering::Release);
        let mut decoder: Option<ArrowStream> = None;
        let mut began = false;
        let mut sink_closed = false;
        let mut cancel_sent = false;
        let mut deadline: Option<Instant> = None;
        let far = Instant::now() + Duration::from_secs(86_400 * 365);
        let result = loop {
            let msg = tokio::select! {
                biased;
                m = p.rx.recv() => m,
                _ = cancel.cancelled(), if !cancel_sent => {
                    proc.cancel(p.id, false);
                    cancel_sent = true;
                    deadline = Some(Instant::now() + CANCEL_GRACE);
                    continue;
                }
                _ = tokio::time::sleep_until(deadline.unwrap_or(far)), if deadline.is_some() => {
                    // The driver ignored the cancel: give the session up.
                    self.broken = true;
                    proc.notify("close", json!({ "conn": self.id }));
                    break Err(Error::Cancelled);
                }
            };
            match msg {
                None => break Err(proc.lost()),
                Some(Incoming::Event(ev)) => match ev["event"].as_str() {
                    Some("begin") => {
                        decoder = Some(ArrowStream::new());
                        began = false;
                    }
                    Some("end") => {
                        if began && !sink_closed && !cancel_sent {
                            let _ = sink.send(SinkEvent::End).await;
                        }
                        decoder = None;
                    }
                    Some("notice") if !sink_closed => {
                        sink.notice(
                            NoticeLevel::Warning,
                            ev["message"].as_str().unwrap_or_default(),
                        )
                        .await;
                    }
                    _ => {}
                },
                Some(Incoming::Data(bytes)) => {
                    if sink_closed || cancel_sent {
                        continue;
                    }
                    let Some(dec) = decoder.as_mut() else {
                        continue;
                    };
                    // The first frame of a result set is its schema.
                    if !began {
                        began = true;
                        match crate::protocol::stream_schema(&bytes) {
                            Ok(schema) => {
                                if sink.send(SinkEvent::Begin { schema }).await.is_err() {
                                    sink_closed = true;
                                    proc.cancel(p.id, true);
                                    continue;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "bad Arrow schema from the JDBC host");
                                proc.cancel(p.id, false);
                                continue;
                            }
                        }
                    }
                    let mut buf = match dec.feed(bytes) {
                        Ok(buf) => buf,
                        Err(e) => {
                            tracing::warn!(error = %e, "bad Arrow stream from the JDBC host");
                            proc.cancel(p.id, false);
                            continue;
                        }
                    };
                    while !buf.is_empty() {
                        let before = buf.len();
                        match dec.next_batch(&mut buf) {
                            Ok(Some(batch)) => {
                                match forward(&sink, batch, batch_rows, &cancel).await {
                                    Forward::Sent => proc.credit(p.id, 1),
                                    Forward::Closed => {
                                        sink_closed = true;
                                        proc.cancel(p.id, true);
                                        break;
                                    }
                                    Forward::Cancelled => break,
                                }
                            }
                            Ok(None) => {
                                if buf.len() == before {
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "bad Arrow stream from the JDBC host");
                                proc.cancel(p.id, false);
                                break;
                            }
                        }
                    }
                }
                Some(Incoming::Done(Ok(v))) => {
                    if cancel_sent {
                        break Err(Error::Cancelled);
                    }
                    let affected = v["affected"].as_u64().filter(|_| sql.is_some_and(is_dml));
                    break Ok(ExecSummary {
                        affected,
                        results: v["results"].as_u64().unwrap_or(0) as usize,
                        truncated: sink_closed || v["truncated"].as_bool().unwrap_or(false),
                    });
                }
                Some(Incoming::Done(Err(e))) => {
                    if cancel_sent || e.kind == "cancelled" {
                        break Err(Error::Cancelled);
                    }
                    break Err(e.into_error(sql));
                }
            }
        };
        self.current.store(0, Ordering::Release);
        result
    }

    async fn run_text(
        &mut self,
        text: String,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> Result<ExecSummary> {
        if self.read_only && !classify(&text, self.flavor.dialect).allowed_read_only() {
            return Err(Error::ReadOnly);
        }
        let params = json!({ "sql": text });
        self.stream("execute", params, Some(&text), sink, cancel)
            .await
    }

    /// Runs `sql` and returns the text of every row (first column) and the
    /// server messages.
    async fn lines(&mut self, sql: &str) -> Result<(Vec<String>, Vec<String>)> {
        let out = collect_request(
            self,
            Request::Text { text: sql.into() },
            CancellationToken::new(),
        )
        .await?;
        let mut lines = Vec::new();
        for r in &out.results {
            for b in &r.batches {
                if b.num_columns() == 0 {
                    continue;
                }
                let col = arrow_cast_text(b.column(0));
                lines.extend(col.into_iter().flatten());
            }
        }
        Ok((lines, out.notices.into_iter().map(|n| n.message).collect()))
    }

    async fn quiet(&mut self, sql: &str) {
        if let Err(e) = self.lines(sql).await {
            tracing::debug!(%sql, error = %e, "ignored");
        }
    }

    async fn native(
        &mut self,
        method: &str,
        args: Vec<(String, String)>,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> Result<ExecSummary> {
        let arg = |k: &str| {
            args.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        let done = |n| {
            Ok(ExecSummary {
                affected: None,
                results: n,
                truncated: false,
            })
        };
        if META_METHODS.contains(&method) {
            let mut a = serde_json::Map::new();
            for (k, v) in &args {
                let value = match k.as_str() {
                    "each_table" | "unique" => json!(v == "true" || v == "1"),
                    _ => json!(v),
                };
                a.insert(k.clone(), value);
            }
            let params = json!({ "method": method, "args": a });
            return self.stream("meta", params, None, sink, cancel).await;
        }
        match method {
            "info" => {
                let v = self.call("info", json!({})).await?;
                let rows: Vec<Vec<String>> = v
                    .as_object()
                    .map(|o| {
                        o.iter()
                            .map(|(k, v)| {
                                let s = match v {
                                    Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                vec![k.clone(), s]
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                text_result(&sink, &["key", "value"], rows).await?;
                done(1)
            }
            "explain_plan" => {
                let sql =
                    arg("sql").ok_or_else(|| Error::Other("explain_plan: no `sql`".into()))?;
                self.oracle_plan(&sql, sink, cancel).await
            }
            "explain_text" => {
                let sql =
                    arg("sql").ok_or_else(|| Error::Other("explain_text: no `sql`".into()))?;
                let analyze = arg("analyze").as_deref() == Some("true");
                self.text_plan(&sql, analyze, sink, cancel).await
            }
            other => Err(Error::Unsupported(format!(
                "native call `{other}` on a JDBC connection"
            ))),
        }
    }

    /// Oracle: `EXPLAIN PLAN SET STATEMENT_ID = … FOR …`, then the rows of
    /// `PLAN_TABLE` (parsed by `ferrodb_data::plan_jdbc::oracle_plan_table`).
    async fn oracle_plan(
        &mut self,
        sql: &str,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> Result<ExecSummary> {
        let sid = format!("FERRODB_{}", self.current_id_hint());
        self.lines(&format!(
            "EXPLAIN PLAN SET STATEMENT_ID = '{sid}' FOR {sql}"
        ))
        .await?;
        let select = format!(
            "SELECT id, parent_id, depth, operation, options, object_owner, object_name, \
             object_alias, object_type, optimizer, cost, cardinality, bytes, cpu_cost, io_cost, \
             time, access_predicates, filter_predicates, partition_start, partition_stop \
             FROM plan_table WHERE statement_id = '{sid}' ORDER BY id"
        );
        let r = self.run_text(select, sink, cancel).await;
        self.quiet(&format!(
            "DELETE FROM plan_table WHERE statement_id = '{sid}'"
        ))
        .await;
        r
    }

    fn current_id_hint(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64 % 1_000_000_000)
            .unwrap_or(0)
    }

    /// Text plans: the database's `EXPLAIN` where it has one, otherwise the
    /// statistics of a measured run (Derby, Exasol: reads only).
    async fn text_plan(
        &mut self,
        sql: &str,
        analyze: bool,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> Result<ExecSummary> {
        let read = classify(sql, self.flavor.dialect) == StatementClass::Read;
        let measured_only = || {
            if read {
                Ok(())
            } else {
                Err(Error::Unsupported(
                    "this database only reports plans of a measured run: refused for a \
                     statement that is not a query"
                        .into(),
                ))
            }
        };
        let lines: Vec<String> = match self.flavor.id {
            "h2" => {
                let stmt = if analyze {
                    "EXPLAIN ANALYZE"
                } else {
                    "EXPLAIN"
                };
                self.lines(&format!("{stmt} {sql}")).await?.0
            }
            "hsqldb" => self.lines(&format!("EXPLAIN PLAN FOR {sql}")).await?.0,
            "vertica" | "hive" => self.lines(&format!("EXPLAIN {sql}")).await?.0,
            "derby" => {
                measured_only()?;
                self.lines("CALL SYSCS_UTIL.SYSCS_SET_RUNTIMESTATISTICS(1)")
                    .await?;
                let run = self.lines(sql).await;
                let stats = self
                    .lines("VALUES SYSCS_UTIL.SYSCS_GET_RUNTIMESTATISTICS()")
                    .await;
                self.quiet("CALL SYSCS_UTIL.SYSCS_SET_RUNTIMESTATISTICS(0)")
                    .await;
                run?;
                stats?.0
            }
            "exasol" => {
                measured_only()?;
                self.lines("ALTER SESSION SET PROFILE = 'ON'").await?;
                let run = self.lines(sql).await;
                self.quiet("ALTER SESSION SET PROFILE = 'OFF'").await;
                run?;
                self.lines("FLUSH STATISTICS").await?;
                self.lines(
                    "SELECT part_id || ' ' || part_name || coalesce(' ' || part_info, '') || \
                     coalesce(' ' || object_schema || '.' || object_name, '') || \
                     ' (rows: ' || coalesce(to_char(out_rows), '?') || ', ' || \
                     coalesce(to_char(duration), '?') || ' s)' \
                     FROM exa_user_profile_last_day WHERE session_id = CURRENT_SESSION \
                     AND stmt_id = (SELECT max(stmt_id) FROM exa_user_profile_last_day \
                     WHERE session_id = CURRENT_SESSION AND command_name = 'SELECT') ORDER BY part_id",
                )
                .await?
                .0
            }
            "db2" => {
                // The explain tables are created once per user.
                self.quiet(
                    "CALL SYSPROC.SYSINSTALLOBJECTS('EXPLAIN', 'C', CAST(NULL AS VARCHAR(128)), CURRENT USER)",
                )
                .await;
                self.lines(&format!("EXPLAIN PLAN FOR {sql}")).await?;
                self.lines(
                    "SELECT repeat(' ', 2 * coalesce(s.lvl, 0)) || o.operator_type || ' #' || \
                     rtrim(char(o.operator_id)) || ' (cost ' || rtrim(char(decimal(o.total_cost, 20, 2))) || ')' \
                     FROM explain_operator o LEFT JOIN (SELECT target_id, count(*) AS lvl FROM explain_stream \
                     WHERE explain_time = (SELECT max(explain_time) FROM explain_instance) GROUP BY target_id) s \
                     ON s.target_id = o.operator_id \
                     WHERE o.explain_time = (SELECT max(explain_time) FROM explain_instance) \
                     ORDER BY o.operator_id",
                )
                .await?
                .0
            }
            "sybase" => {
                self.lines("set showplan on").await?;
                self.quiet("set noexec on").await;
                let run = self.lines(sql).await;
                self.quiet("set noexec off").await;
                self.quiet("set showplan off").await;
                run?.1
            }
            other => {
                return Err(Error::Unsupported(format!(
                    "execution plans of {other} through JDBC"
                )));
            }
        };
        let _ = cancel;
        let rows: Vec<Vec<String>> = lines
            .iter()
            .flat_map(|l| l.lines().map(|x| vec![x.to_owned()]).collect::<Vec<_>>())
            .collect();
        text_result(&sink, &["plan"], rows).await?;
        Ok(ExecSummary {
            affected: None,
            results: 1,
            truncated: false,
        })
    }
}

/// Text values of a column (plans and messages are text; other types are
/// skipped).
fn arrow_cast_text(a: &ArrayRef) -> Vec<Option<String>> {
    use arrow_array::Array;
    match a.as_any().downcast_ref::<StringArray>() {
        Some(s) => (0..s.len())
            .map(|i| (!s.is_null(i)).then(|| s.value(i).to_owned()))
            .collect(),
        None => Vec::new(),
    }
}

impl Connection for JdbcConnection {
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
                Request::Text { text } => self.run_text(text, sink, cancel).await,
                Request::Native { method, args } => self.native(&method, args, sink, cancel).await,
                Request::Mql(_) => Err(Error::Unsupported(
                    "MongoDB commands on a JDBC connection".into(),
                )),
            }
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(Arc::new(JdbcCancel {
            proc: self.proc.clone(),
            current: self.current.clone(),
        }))
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // A hint for some drivers, enforced by others; enforced here too.
            if let Err(e) = self
                .call("set_read_only", json!({ "value": read_only }))
                .await
            {
                tracing::debug!(error = %e, "setReadOnly not supported");
                self.live()?;
            }
            self.read_only = read_only;
            Ok(())
        })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let params = match self.flavor.set_schema_sql(schema) {
                Some(sql) => json!({ "schema": schema, "sql": sql }),
                None => json!({ "schema": schema, "catalog": self.flavor.catalog_container }),
            };
            self.call("set_schema", params).await.map(|_| ())
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            let mut params = json!({ "catalog": self.flavor.catalog_container });
            if let Some(sql) = self.flavor.current_schema_sql() {
                params["sql"] = json!(sql);
            }
            let v = self.call("current_schema", params).await?;
            Ok(v["schema"]
                .as_str()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()))
        })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if self.flavor.tx == ferrodb_api::TxSupport::None {
                return Err(Error::Unsupported(format!(
                    "transactions ({} has none)",
                    self.flavor.name
                )));
            }
            self.call("set_autocommit", json!({ "value": false }))
                .await
                .map(|_| ())
        })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if self.flavor.tx == ferrodb_api::TxSupport::None {
                return Ok(());
            }
            self.call("commit", json!({})).await?;
            self.call("set_autocommit", json!({ "value": true }))
                .await
                .map(|_| ())
        })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if self.flavor.tx == ferrodb_api::TxSupport::None {
                return Ok(());
            }
            self.call("rollback", json!({})).await?;
            self.call("set_autocommit", json!({ "value": true }))
                .await
                .map(|_| ())
        })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let params = match self.flavor.ping_sql() {
                Some(sql) => json!({ "sql": sql }),
                None => json!({}),
            };
            self.call("ping", params).await.map(|_| ())
        })
    }

    fn is_alive(&self) -> bool {
        !self.broken && self.proc.is_alive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dml_detection() {
        assert!(is_dml("insert into t values (1)"));
        assert!(is_dml("  -- c\n /* x */ UPDATE t set a = 1"));
        assert!(!is_dml("create table t (a int)"));
        assert!(!is_dml("select 1"));
    }
}
