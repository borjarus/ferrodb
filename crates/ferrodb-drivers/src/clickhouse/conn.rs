//! One ClickHouse HTTP session: statements as POST requests with the
//! session's `session_id`, results decoded from the Arrow IPC stream as the
//! body arrives.

use std::{
    net::{SocketAddr, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use arrow_array::{RecordBatch, StringArray};
use arrow_buffer::Buffer;
use arrow_ipc::reader::StreamDecoder;
use arrow_schema::{DataType, Field, Schema};
use ferrodb_api::{Error, Result};
use ferrodb_net::TlsMode;
use futures::StreamExt;

use super::{DEFAULT_PORT, DEFAULT_TLS_PORT, ch_ident, error, types};
use crate::{
    BoxFuture, CancellationToken, Connection, Endpoint, ExecSummary, RemoteCancel, Request,
    ResolvedConfig, ResultSink, SinkEvent,
};

/// Bytes of the raw body kept to find an exception written after the
/// rows that were already sent.
const TAIL: usize = 16 * 1024;
/// How long a statement waits for the session to be released by a
/// statement that was just killed.
const SESSION_RETRY: Duration = Duration::from_millis(100);
const SESSION_RETRIES: u32 = 50;

/// Settings sent with every statement.
const FIXED_SETTINGS: &[(&str, &str)] = &[
    // The IPC reader is built without codecs; the bytes are local anyway
    // compared with the cost of a codec on both ends.
    ("output_format_arrow_compression_method", "none"),
    ("output_format_arrow_string_as_string", "1"),
    // A client that goes away (cancel, closed result) stops its SELECT.
    ("cancel_http_readonly_queries_on_client_close", "1"),
];

/// Options of the connection form that are not ClickHouse settings.
const NOT_SETTINGS: &[&str] = &["connect_timeout", "query_timeout"];

fn next_id(prefix: &str) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "ferrodb-{prefix}-{}-{nanos:x}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn net(e: reqwest::Error) -> Error {
    // reqwest's message is generic; the cause says what happened.
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    if e.is_timeout() {
        Error::Timeout
    } else if e.is_connect() {
        Error::Connection(msg)
    } else {
        Error::Other(format!("ClickHouse HTTP: {msg}"))
    }
}

/// The HTTP endpoint and credentials, shared with the cancel handle.
pub(crate) struct Http {
    client: reqwest::Client,
    base: String,
    user: String,
    password: String,
    settings: Vec<(String, String)>,
}

impl Http {
    fn request(&self, params: &[(&str, String)]) -> reqwest::RequestBuilder {
        let mut r = self
            .client
            .post(&self.base)
            .header("X-ClickHouse-User", &self.user)
            .query(&self.settings)
            .query(params);
        if !self.password.is_empty() {
            r = r.header("X-ClickHouse-Key", &self.password);
        }
        r
    }

    /// One statement read fully as text (session-less unless `params`
    /// say otherwise).
    async fn text(&self, sql: &str, params: &[(&str, String)]) -> Result<String> {
        let resp = self
            .request(params)
            .body(sql.to_owned())
            .send()
            .await
            .map_err(net)?;
        let ok = resp.status().is_success();
        let body = resp.text().await.map_err(net)?;
        if ok {
            Ok(body)
        } else {
            Err(error::to_error(&body, sql))
        }
    }

    async fn kill(&self, query_id: &str) -> Result<()> {
        self.text(
            &format!(
                "KILL QUERY WHERE query_id = {} ASYNC FORMAT Null",
                super::ch_literal(query_id)
            ),
            &[],
        )
        .await
        .map(|_| ())
    }
}

/// Cancels the running statement from another task.
struct Cancel {
    http: Arc<Http>,
    running: Arc<Mutex<Option<String>>>,
}

impl RemoteCancel for Cancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        let id = self.running.lock().ok().and_then(|g| g.clone());
        let http = self.http.clone();
        Box::pin(async move {
            match id {
                Some(id) => http.kill(&id).await,
                None => Ok(()),
            }
        })
    }
}

pub struct ClickHouseConnection {
    http: Arc<Http>,
    session_id: String,
    database: Option<String>,
    read_only: bool,
    version: String,
    /// Server time zone (`DateTime` columns without their own).
    time_zone: String,
    running: Arc<Mutex<Option<String>>>,
    alive: bool,
    _endpoint: Endpoint,
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

/// Statements that return rows (streamed as they are produced).
fn returns_rows(keyword: &str) -> bool {
    matches!(
        keyword,
        "select" | "with" | "show" | "describe" | "desc" | "explain" | "exists" | "check"
    )
}

/// TSV escapes (`\t`, `\n`, `\\`, `\'`).
fn tsv_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

/// Database named by `USE db` / ``USE `my db` ``.
fn use_target(sql: &str) -> Option<String> {
    let s = statement(sql).trim_start();
    let rest = s.get(3..)?.trim();
    let name = match rest.chars().next()? {
        q @ ('`' | '"') => rest.strip_prefix(q)?.strip_suffix(q)?.replace("\\\\", "\\"),
        _ => rest.split_whitespace().next()?.to_owned(),
    };
    (!name.is_empty()).then_some(name)
}

/// The statement without trailing semicolons.
fn statement(sql: &str) -> &str {
    sql.trim().trim_end_matches(';').trim_end()
}

fn summary_rows(header: Option<&str>, key: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(header?).ok()?;
    match v.get(key)? {
        serde_json::Value::String(s) => s.parse().ok(),
        serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

impl ClickHouseConnection {
    pub(crate) async fn open(cfg: &ResolvedConfig) -> Result<Self> {
        let explicit_port = cfg.port(0)?;
        let https = matches!(cfg.tls, TlsMode::Require | TlsMode::VerifyFull)
            || matches!(explicit_port, DEFAULT_TLS_PORT | 443);
        let default_port = if https {
            DEFAULT_TLS_PORT
        } else {
            DEFAULT_PORT
        };
        let endpoint = cfg.endpoint(default_port).await?;
        let port = cfg.port(default_port)?;
        // reqwest builds its TLS layer even for plain HTTP: with `ring`.
        ferrodb_net::install_crypto_provider();
        let mut builder = reqwest::Client::builder()
            .connect_timeout(cfg.connect_timeout)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_nodelay(true)
            .no_proxy();
        let host = if endpoint.tunnelled() {
            // Keep the real host name in the URL (TLS name, Host header);
            // DNS resolves it to the local end of the tunnel.
            let addr: SocketAddr = (endpoint.host.as_str(), endpoint.port)
                .to_socket_addrs()
                .map_err(|e| Error::Connection(e.to_string()))?
                .next()
                .ok_or_else(|| Error::Connection("SSH tunnel has no address".into()))?;
            builder = builder.resolve(&endpoint.server_name, addr);
            endpoint.server_name.clone()
        } else {
            endpoint.host.clone()
        };
        if https {
            let mode = if cfg.tls == TlsMode::VerifyFull {
                TlsMode::VerifyFull
            } else {
                TlsMode::Require
            };
            let tls = ferrodb_net::client_config(mode, cfg.tls_ca.as_deref())
                .map_err(|e| Error::Connection(e.to_string()))?
                .ok_or_else(|| Error::Connection("TLS configuration".into()))?;
            builder = builder.tls_backend_preconfigured((*tls).clone());
        }
        let client = builder
            .build()
            .map_err(|e| Error::Connection(e.to_string()))?;
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        let mut settings: Vec<(String, String)> = FIXED_SETTINGS
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        settings.extend(
            cfg.options()
                .filter(|(k, _)| !NOT_SETTINGS.contains(k))
                .map(|(k, v)| (k.to_owned(), v.to_owned())),
        );
        let http = Arc::new(Http {
            client,
            base: format!("{}://{host}:{port}/", if https { "https" } else { "http" }),
            user: cfg.user().unwrap_or("default").to_owned(),
            password: cfg.password().unwrap_or_default().to_owned(),
            settings,
        });
        let mut c = Self {
            http,
            session_id: next_id("s"),
            database: cfg.database().map(str::to_owned),
            read_only: cfg.read_only,
            version: String::new(),
            time_zone: "UTC".into(),
            running: Arc::new(Mutex::new(None)),
            alive: true,
            _endpoint: endpoint,
        };
        let info = tokio::time::timeout(
            cfg.connect_timeout,
            c.session_text("SELECT version(), currentDatabase(), timezone() FORMAT TSV"),
        )
        .await
        .map_err(|_| Error::Connection("ClickHouse: timed out".into()))?
        .map_err(|e| match e {
            Error::Query(q) => Error::Connection(q.message),
            other => other,
        })?;
        let mut parts = info.trim_end().split('\t').map(tsv_unescape);
        c.version = format!("ClickHouse {}", parts.next().unwrap_or_default());
        c.database = parts.next().filter(|d| !d.is_empty()).or(c.database);
        if let Some(tz) = parts.next().filter(|t| !t.is_empty()) {
            c.time_zone = tz;
        }
        Ok(c)
    }

    fn params(&self, query_id: &str) -> Vec<(&'static str, String)> {
        let mut p = vec![
            ("session_id", self.session_id.clone()),
            ("session_timeout", "3600".to_owned()),
            ("query_id", query_id.to_owned()),
        ];
        if let Some(db) = &self.database {
            p.push(("database", db.clone()));
        }
        if self.read_only {
            p.push(("readonly", "2".to_owned()));
        }
        p
    }

    /// A small statement in the session, read as text.
    async fn session_text(&self, sql: &str) -> Result<String> {
        let params = self.params(&next_id("q"));
        let mut tries = 0;
        loop {
            match self.http.text(sql, &params).await {
                Err(Error::Query(q))
                    if q.code.as_deref() == Some(error::SESSION_IS_LOCKED)
                        && tries < SESSION_RETRIES =>
                {
                    tries += 1;
                    tokio::time::sleep(SESSION_RETRY).await;
                }
                other => return other,
            }
        }
    }

    /// Sends a statement, retrying while the session is still held by a
    /// statement that was killed.
    async fn send(
        &self,
        params: &[(&str, String)],
        body: reqwest::Body,
        body_copy: impl Fn() -> reqwest::Body,
        sql: &str,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response> {
        let mut body = Some(body);
        let mut tries = 0;
        loop {
            let b = body.take().unwrap_or_else(&body_copy);
            let send = self.http.request(params).body(b).send();
            let resp = tokio::select! {
                r = send => r.map_err(net)?,
                _ = cancel.cancelled() => return Err(Error::Cancelled),
            };
            if resp.status().is_success() {
                return Ok(resp);
            }
            let text = resp.text().await.map_err(net)?;
            let err = error::to_error(&text, sql);
            match &err {
                Error::Query(q)
                    if q.code.as_deref() == Some(error::SESSION_IS_LOCKED)
                        && tries < SESSION_RETRIES =>
                {
                    tries += 1;
                    tokio::time::sleep(SESSION_RETRY).await;
                }
                _ => return Err(err),
            }
        }
    }

    /// Column types of a query (`DESCRIBE (query)`), session-less so it can
    /// run while the query streams; `None` when the server refuses.
    async fn describe(&self, sql: &str) -> Option<Vec<String>> {
        let mut params = Vec::new();
        if let Some(db) = &self.database {
            params.push(("database", db.clone()));
        }
        let text = self
            .http
            .text(
                &format!("DESCRIBE TABLE ({}) FORMAT TSV", statement(sql)),
                &params,
            )
            .await
            .ok()?;
        Some(
            text.lines()
                .map(|l| tsv_unescape(l.split('\t').nth(1).unwrap_or_default()))
                .collect(),
        )
    }

    async fn plan(&self, raw: &Schema, sql: &str, tz: &str) -> types::Plan {
        let described = if types::needs_types(raw) && returns_rows(&first_keyword(sql)) {
            self.describe(sql)
                .await
                .filter(|t| t.len() == raw.fields().len())
        } else {
            None
        };
        types::plan(raw, described.as_deref(), tz)
    }

    fn kill_later(&self, query_id: String) {
        let http = self.http.clone();
        tokio::spawn(async move {
            let _ = http.kill(&query_id).await;
        });
    }

    async fn run(
        &mut self,
        sql: String,
        sink: &ResultSink,
        cancel: &CancellationToken,
    ) -> Result<ExecSummary> {
        let qid = next_id("q");
        if let Ok(mut g) = self.running.lock() {
            *g = Some(qid.clone());
        }
        let r = self.stream(&sql, &qid, sink, cancel).await;
        if let Ok(mut g) = self.running.lock() {
            *g = None;
        }
        match &r {
            Err(Error::Cancelled) => self.kill_later(qid),
            Ok(s) if s.truncated => self.kill_later(qid),
            Err(Error::Connection(_)) => self.alive = false,
            _ => {}
        }
        if r.is_ok()
            && first_keyword(&sql) == "use"
            && let Some(db) = use_target(&sql)
        {
            // Every request names its database: follow the switch.
            self.database = Some(db);
        }
        r
    }

    async fn stream(
        &mut self,
        sql: &str,
        qid: &str,
        sink: &ResultSink,
        cancel: &CancellationToken,
    ) -> Result<ExecSummary> {
        let keyword = first_keyword(sql);
        let mut params = self.params(qid);
        params.push(("default_format", "ArrowStream".to_owned()));
        if !returns_rows(&keyword) {
            // Complete summary (written rows) instead of streaming.
            params.push(("wait_end_of_query", "1".to_owned()));
        }
        let text = statement(sql).to_owned();
        let copy = text.clone();
        let resp = self
            .send(
                &params,
                text.clone().into(),
                move || copy.clone().into(),
                sql,
                cancel,
            )
            .await?;
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let tz = header("X-ClickHouse-Timezone").unwrap_or_else(|| self.time_zone.clone());
        let arrow = header("X-ClickHouse-Format").is_none_or(|f| f == "ArrowStream");
        let mut summary = ExecSummary {
            affected: (keyword == "insert")
                .then(|| summary_rows(header("X-ClickHouse-Summary").as_deref(), "written_rows"))
                .flatten(),
            ..Default::default()
        };
        let mut body = resp.bytes_stream();
        let mut decoder = StreamDecoder::new();
        let mut plan: Option<types::Plan> = None;
        let mut tail: Vec<u8> = Vec::new();
        let mut text_body: Vec<u8> = Vec::new();
        let mut decode_error: Option<String> = None;
        let batch_rows = sink.batch_rows();
        loop {
            let chunk = tokio::select! {
                c = body.next() => c,
                _ = cancel.cancelled() => return Err(Error::Cancelled),
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(net)?;
            tail.extend_from_slice(&chunk);
            if tail.len() > 2 * TAIL {
                tail.drain(..tail.len() - TAIL);
            }
            if !arrow {
                text_body.extend_from_slice(&chunk);
                continue;
            }
            if decode_error.is_some() {
                continue;
            }
            let mut buf = Buffer::from(chunk);
            while !buf.is_empty() {
                let decoded = match decoder.decode(&mut buf) {
                    Ok(b) => b,
                    Err(e) => {
                        decode_error = Some(e.to_string());
                        break;
                    }
                };
                if plan.is_none()
                    && let Some(raw) = decoder.schema()
                {
                    let p = self.plan(&raw, sql, &tz).await;
                    summary.results += 1;
                    if sink
                        .send(SinkEvent::Begin {
                            schema: p.schema.clone(),
                        })
                        .await
                        .is_err()
                    {
                        summary.truncated = true;
                        return Ok(summary);
                    }
                    plan = Some(p);
                }
                let (Some(batch), Some(p)) = (decoded, &plan) else {
                    continue;
                };
                let batch = p.apply(&batch).map_err(Error::Other)?;
                let mut offset = 0;
                while offset < batch.num_rows() {
                    let len = batch_rows.min(batch.num_rows() - offset);
                    if sink
                        .send(SinkEvent::Batch(batch.slice(offset, len)))
                        .await
                        .is_err()
                    {
                        summary.truncated = true;
                        return Ok(summary);
                    }
                    offset += len;
                }
            }
        }
        // An exception after the first rows is appended to the body.
        let tail_text = String::from_utf8_lossy(&tail);
        let exception = tail_text
            .rfind("__exception__")
            .and_then(|_| tail_text.find("Code: "))
            .map(|i| tail_text[i..].to_owned());
        if arrow {
            let finished = decoder.finish().map_err(|e| e.to_string());
            if let Some(e) = decode_error.or(finished.err()) {
                return Err(
                    match exception
                        .or_else(|| tail_text.find("Code: ").map(|i| tail_text[i..].to_owned()))
                    {
                        Some(x) => error::to_error(&x, sql),
                        None => Error::Other(format!("ClickHouse result stream: {e}")),
                    },
                );
            }
            if let Some(x) = exception {
                return Err(error::to_error(&x, sql));
            }
            if plan.is_some() {
                let _ = sink.send(SinkEvent::End).await;
            }
        } else if !text_body.is_empty() {
            // `FORMAT JSON`, `FORMAT CSV`… written by the user: the text,
            // one row per line.
            let text = String::from_utf8_lossy(&text_body);
            let schema = Arc::new(Schema::new(vec![Field::new(
                "result",
                DataType::Utf8,
                false,
            )]));
            summary.results += 1;
            let lines: Vec<&str> = text.lines().collect();
            let _ = sink
                .send(SinkEvent::Begin {
                    schema: schema.clone(),
                })
                .await;
            for chunk in lines.chunks(batch_rows) {
                let batch = RecordBatch::try_new(
                    schema.clone(),
                    vec![Arc::new(StringArray::from(chunk.to_vec()))],
                )
                .map_err(|e| Error::Other(e.to_string()))?;
                if sink.send(SinkEvent::Batch(batch)).await.is_err() {
                    summary.truncated = true;
                    return Ok(summary);
                }
            }
            let _ = sink.send(SinkEvent::End).await;
        }
        Ok(summary)
    }
}

impl Connection for ClickHouseConnection {
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
            let text = req.into_text()?;
            self.run(text, &sink, &cancel).await
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(Arc::new(Cancel {
            http: self.http.clone(),
            running: self.running.clone(),
        }))
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        self.read_only = read_only;
        Box::pin(async { Ok(()) })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.session_text(&format!("USE {}", ch_ident(schema)))
                .await?;
            self.database = Some(schema.to_owned());
            Ok(())
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            if let Some(db) = &self.database {
                return Ok(Some(db.clone()));
            }
            let db = self
                .session_text("SELECT currentDatabase() FORMAT TSV")
                .await?;
            Ok(Some(tsv_unescape(db.trim_end())))
        })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            Err(Error::Unsupported(
                "ClickHouse has no transactions; every statement is applied at once".into(),
            ))
        })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        // Nothing is ever pending.
        Box::pin(async { Ok(()) })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let url = format!("{}ping", self.http.base);
            let r = self.http.client.get(url).send().await;
            match r {
                Ok(resp) if resp.status().is_success() => Ok(()),
                Ok(resp) => Err(Error::Connection(format!("ping: HTTP {}", resp.status()))),
                Err(e) => {
                    self.alive = false;
                    Err(net(e))
                }
            }
        })
    }

    fn is_alive(&self) -> bool {
        self.alive
    }

    fn bulk_load<'a>(
        &'a mut self,
        table: &'a str,
        columns: &'a [String],
        data: Vec<u8>,
    ) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move {
            let sql = format!(
                "INSERT INTO {table} ({}) FORMAT TabSeparated",
                columns.join(", ")
            );
            let qid = next_id("q");
            let mut params = self.params(&qid);
            params.push(("query", sql.clone()));
            params.push(("wait_end_of_query", "1".to_owned()));
            let data = bytes::Bytes::from(data);
            let copy = data.clone();
            let resp = self
                .send(
                    &params,
                    data.into(),
                    move || copy.clone().into(),
                    &sql,
                    &CancellationToken::new(),
                )
                .await?;
            let rows = summary_rows(
                resp.headers()
                    .get("X-ClickHouse-Summary")
                    .and_then(|v| v.to_str().ok()),
                "written_rows",
            );
            let _ = resp.bytes().await;
            Ok(rows.unwrap_or(0))
        })
    }

    fn bulk_load_method(&self) -> Option<&'static str> {
        Some("INSERT FORMAT TabSeparated")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_and_summaries() {
        assert_eq!(first_keyword("  -- c\n/* x */ (SELECT 1)"), "select");
        assert_eq!(first_keyword("insert into t values (1)"), "insert");
        assert!(returns_rows("with"));
        assert!(!returns_rows("insert"));
        assert_eq!(
            summary_rows(
                Some(r#"{"read_rows":"3","written_rows":"3","elapsed_ns":"64125244"}"#),
                "written_rows"
            ),
            Some(3)
        );
        assert_eq!(tsv_unescape("Enum8(\\'a\\' = 1)"), "Enum8('a' = 1)");
        assert_eq!(statement("select 1;; "), "select 1");
        assert_eq!(use_target("USE `my db`;").as_deref(), Some("my db"));
        assert_eq!(use_target("use system").as_deref(), Some("system"));
    }
}
