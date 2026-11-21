//! HTTP client, query requests streamed into the sink, cluster manager
//! listings, KV operations by key, transactions; the [`Connection`]
//! implementation.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bson::{Bson, Document, RawDocumentBuf, doc};
use ferrodb_api::{Error, NoticeLevel, QueryError, Result};
use ferrodb_net::{SshTunnel, TlsMode};
use serde_json::{Map, Value as Json, json};

use super::{
    QUERY_PORT, TLS_PORT, TLS_QUERY_PORT,
    sqlpp::{self, TxControl},
    stream::{self, Event, Reader},
};
use crate::{
    BoxFuture, CancellationToken, Connection, ExecSummary, RemoteCancel, Request, ResolvedConfig,
    ResultSink, SinkEvent,
};

static NEXT_CONTEXT: AtomicU64 = AtomicU64::new(1);

/// The statement running now (server-side cancel).
#[derive(Clone, Debug, Default)]
struct Running {
    context: String,
    request: Option<String>,
}

#[derive(Clone)]
struct Http {
    client: reqwest::Client,
    /// `http(s)://host:port` of the query service.
    query: String,
    /// `http(s)://host:port` of the cluster manager.
    mgmt: String,
    auth: Option<(String, String)>,
}

impl Http {
    fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        let r = self.client.request(method, url);
        match &self.auth {
            Some((u, p)) => r.basic_auth(u, Some(p)),
            None => r,
        }
    }

    fn post_query(&self, body: &Json) -> reqwest::RequestBuilder {
        self.request(
            reqwest::Method::POST,
            format!("{}/query/service", self.query),
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
    }

    /// GET on the cluster manager, JSON answer.
    async fn mgmt_get(&self, path: &str) -> Result<Json> {
        let r = self
            .request(reqwest::Method::GET, format!("{}{path}", self.mgmt))
            .send()
            .await
            .map_err(net)?;
        let status = r.status();
        let body = r.bytes().await.map_err(net)?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Connection(
                "authentication failed (cluster manager)".into(),
            ));
        }
        if !status.is_success() {
            return Err(Error::Query(QueryError {
                message: format!(
                    "{path}: HTTP {status}: {}",
                    String::from_utf8_lossy(&body).trim()
                ),
                code: Some(status.as_u16().to_string()),
                range: None,
            }));
        }
        serde_json::from_slice(&body)
            .map_err(|e| Error::Other(format!("cluster manager answer for {path}: {e}")))
    }

    /// Stops a running statement: by request id when known, else by the
    /// client context id.
    async fn cancel(&self, running: &Running) -> Result<()> {
        if let Some(id) = &running.request {
            let r = self
                .request(
                    reqwest::Method::DELETE,
                    format!("{}/admin/active_requests/{id}", self.query),
                )
                .send()
                .await
                .map_err(net)?;
            if r.status().is_success() {
                return Ok(());
            }
        }
        let body = json!({
            "statement": "DELETE FROM system:active_requests WHERE clientContextID = $1",
            "args": [running.context],
        });
        self.post_query(&body).send().await.map_err(net)?;
        Ok(())
    }
}

fn net(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout
    } else if e.is_connect() || e.is_request() || e.is_body() {
        Error::Connection(format!("Couchbase: {e}"))
    } else {
        Error::Other(format!("Couchbase: {e}"))
    }
}

/// Byte offset of a 1-based line / column in `text`.
fn offset(text: &str, line: usize, column: usize) -> usize {
    let start: usize = text
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum();
    let col = text[start.min(text.len())..]
        .char_indices()
        .nth(column.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    (start + col).min(text.len())
}

/// The first server error as a contract error; positions are mapped back
/// through the driver's rewrite to the text the user typed.
fn query_error(errors: &[Json], sent: &str, rewrite: Option<&sqlpp::Rewrite>) -> Error {
    let first = errors.first().cloned().unwrap_or(Json::Null);
    let code = first.get("code").map(|c| match c {
        Json::Number(n) => n.to_string(),
        other => other.as_str().unwrap_or_default().to_owned(),
    });
    let mut message = first
        .get("msg")
        .and_then(Json::as_str)
        .unwrap_or("query failed")
        .to_owned();
    if let Some(reason) = first.get("reason")
        && !reason.is_null()
    {
        message.push_str(&format!(" ({reason})"));
    }
    if errors.len() > 1 {
        let more: Vec<String> = errors[1..]
            .iter()
            .filter_map(|e| e.get("msg").and_then(Json::as_str).map(str::to_owned))
            .collect();
        if !more.is_empty() {
            message.push_str(&format!("; {}", more.join("; ")));
        }
    }
    let range = match (
        first.get("line").and_then(Json::as_u64),
        first.get("column").and_then(Json::as_u64),
    ) {
        (Some(l), Some(c)) if l > 0 => {
            let mut at = offset(sent, l as usize, c as usize);
            if let Some(r) = rewrite
                && at > r.at
            {
                at = at.saturating_sub(r.grew).max(r.at);
            }
            Some(at..at + 1)
        }
        // "syntax error - at end of input" comes without a position.
        _ if message.contains("at end of input") && !sent.is_empty() => {
            let mut at = sent.trim_end().len().saturating_sub(1);
            if let Some(r) = rewrite
                && at > r.at
            {
                at = at.saturating_sub(r.grew).max(r.at);
            }
            Some(at..at + 1)
        }
        _ => None,
    };
    if code.as_deref() == Some("10000") {
        return Error::Connection(format!("authentication failed: {message}"));
    }
    Error::Query(QueryError {
        message,
        code,
        range,
    })
}

/// Where the rows of a request go.
enum Target<'a> {
    Sink {
        sink: &'a ResultSink,
        database: Option<String>,
        collection: Option<String>,
        /// Begin a result even when `results` is empty.
        rows: bool,
        began: bool,
        buf: Vec<RawDocumentBuf>,
    },
    Collect(Vec<Document>),
    Discard,
}

impl Target<'_> {
    async fn begin(&mut self) -> bool {
        if let Target::Sink {
            sink,
            database,
            collection,
            began,
            ..
        } = self
            && !*began
        {
            *began = true;
            return sink
                .send(SinkEvent::BeginDocuments {
                    database: database.clone(),
                    collection: collection.clone(),
                })
                .await
                .is_ok();
        }
        true
    }

    /// `false` once the consumer went away.
    async fn push(&mut self, d: Document) -> Result<bool> {
        match self {
            Target::Sink { sink, buf, .. } => {
                let raw =
                    RawDocumentBuf::try_from(&d).map_err(|e| Error::Other(format!("BSON: {e}")))?;
                buf.push(raw);
                if buf.len() >= sink.batch_rows() {
                    let batch = std::mem::take(buf);
                    return Ok(sink.send(SinkEvent::Documents(batch)).await.is_ok());
                }
                Ok(true)
            }
            Target::Collect(v) => {
                v.push(d);
                Ok(true)
            }
            Target::Discard => Ok(true),
        }
    }

    async fn finish(&mut self, truncated: bool) -> bool {
        if let Target::Sink {
            sink, buf, began, ..
        } = self
            && *began
        {
            let mut ok = !truncated;
            if ok && !buf.is_empty() {
                ok = sink
                    .send(SinkEvent::Documents(std::mem::take(buf)))
                    .await
                    .is_ok();
            }
            let _ = sink.send(SinkEvent::End).await;
            return ok;
        }
        !truncated
    }
}

/// What a request returned besides its rows.
#[derive(Debug, Default)]
struct Outcome {
    errors: Vec<Json>,
    warnings: Vec<Json>,
    metrics: Option<Json>,
    profile: Option<Json>,
    status: Option<String>,
    rows: usize,
    truncated: bool,
}

impl Outcome {
    fn mutations(&self) -> Option<u64> {
        self.metrics
            .as_ref()
            .and_then(|m| m.get("mutationCount"))
            .and_then(Json::as_u64)
    }
}

pub struct CouchbaseConnection {
    http: Http,
    bucket: Option<String>,
    scope: Option<String>,
    scan_consistency: Option<String>,
    txid: Option<String>,
    version: String,
    alive: bool,
    running: Arc<Mutex<Option<Running>>>,
    _tunnels: Vec<SshTunnel>,
}

impl CouchbaseConnection {
    pub async fn connect(cfg: &ResolvedConfig) -> Result<Self> {
        ferrodb_net::install_crypto_provider();
        let tls = matches!(cfg.tls, TlsMode::Require | TlsMode::VerifyFull)
            || cfg.prop("ssl.cert").is_some();
        let host = cfg.host().to_owned();
        let query_port = cfg.port(if tls { TLS_QUERY_PORT } else { QUERY_PORT })?;
        let mgmt_port = match cfg.prop("mgmt_port") {
            Some(p) => p
                .parse()
                .map_err(|_| Error::Connection(format!("mgmt_port: not a number: {p}")))?,
            None if tls => TLS_PORT,
            None => super::DEFAULT_PORT,
        };
        // Through SSH: one tunnel per service.
        let mut tunnels = Vec::new();
        let (qhost, qport, mhost, mport) = match &cfg.ssh {
            Some(ssh) => {
                let q = SshTunnel::open(ssh, &host, query_port)
                    .await
                    .map_err(|e| Error::Connection(e.to_string()))?;
                let m = SshTunnel::open(ssh, &host, mgmt_port)
                    .await
                    .map_err(|e| Error::Connection(e.to_string()))?;
                let (qa, ma) = (q.local_addr(), m.local_addr());
                tunnels.push(q);
                tunnels.push(m);
                (
                    qa.ip().to_string(),
                    qa.port(),
                    ma.ip().to_string(),
                    ma.port(),
                )
            }
            None => (host.clone(), query_port, host.clone(), mgmt_port),
        };
        let mut builder = reqwest::Client::builder()
            .connect_timeout(cfg.connect_timeout)
            .user_agent(cfg.application_name.clone());
        if tls {
            let mode = if cfg.tls == TlsMode::Prefer {
                TlsMode::Require
            } else {
                cfg.tls
            };
            let config = match cfg.prop("ssl.cert") {
                Some(cert) => ferrodb_net::client_config_with_cert(
                    mode,
                    cfg.tls_ca.as_deref(),
                    std::path::Path::new(cert),
                ),
                None => ferrodb_net::client_config(mode, cfg.tls_ca.as_deref())
                    .map(|c| c.expect("TLS is on")),
            }
            .map_err(|e| Error::Connection(e.to_string()))?;
            builder = builder.tls_backend_preconfigured((*config).clone());
        }
        let client = builder
            .build()
            .map_err(|e| Error::Connection(format!("HTTP client: {e}")))?;
        let scheme = if tls { "https" } else { "http" };
        let auth = match (cfg.user(), cfg.password()) {
            (Some(u), p) => Some((u.to_owned(), p.unwrap_or_default().to_owned())),
            _ => None,
        };
        let http = Http {
            client,
            query: format!("{scheme}://{}", bracket(&qhost, qport)),
            mgmt: format!("{scheme}://{}", bracket(&mhost, mport)),
            auth,
        };
        let pools = tokio::time::timeout(
            cfg.connect_timeout + Duration::from_secs(2),
            http.mgmt_get("/pools"),
        )
        .await
        .map_err(|_| Error::Connection("timed out connecting to Couchbase".into()))?
        .map_err(|e| match e {
            Error::Query(q) => Error::Connection(q.message),
            other => other,
        })?;
        let version = pools
            .get("implementationVersion")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut c = Self {
            http,
            bucket: cfg.database().map(str::to_owned),
            scope: cfg.prop("scope").map(str::to_owned),
            scan_consistency: cfg.prop("scan_consistency").map(str::to_owned),
            txid: None,
            version,
            alive: true,
            running: Arc::default(),
            _tunnels: tunnels,
        };
        // The query service answers too (credentials are checked here).
        c.ping_query().await?;
        Ok(c)
    }

    async fn ping_query(&mut self) -> Result<()> {
        let mut t = Target::Discard;
        let body = json!({ "statement": "SELECT RAW 1" });
        let out = self
            .send(
                &body,
                "SELECT RAW 1",
                None,
                &CancellationToken::new(),
                &mut t,
            )
            .await?;
        if !out.errors.is_empty() {
            return Err(query_error(&out.errors, "SELECT RAW 1", None));
        }
        Ok(())
    }

    fn query_context(&self) -> Option<String> {
        let b = self.bucket.as_ref()?;
        Some(match &self.scope {
            Some(s) => format!("default:`{b}`.`{s}`"),
            None => format!("default:`{b}`"),
        })
    }

    /// The request body for `statement`.
    fn body(&self, statement: &str, context: &str, extra: Option<&Map<String, Json>>) -> Json {
        let mut m = Map::new();
        m.insert("statement".into(), Json::String(statement.to_owned()));
        m.insert("client_context_id".into(), Json::String(context.to_owned()));
        if let Some(q) = self.query_context() {
            m.insert("query_context".into(), Json::String(q));
        }
        if let Some(s) = &self.scan_consistency {
            m.insert("scan_consistency".into(), Json::String(s.clone()));
        }
        if let Some(t) = &self.txid {
            m.insert("txid".into(), Json::String(t.clone()));
        }
        if let Some(e) = extra {
            for (k, v) in e {
                m.insert(k.clone(), v.clone());
            }
        }
        Json::Object(m)
    }

    /// Posts a query and reads the response incrementally into `target`.
    async fn send(
        &mut self,
        body: &Json,
        sent: &str,
        rewrite: Option<&sqlpp::Rewrite>,
        cancel: &CancellationToken,
        target: &mut Target<'_>,
    ) -> Result<Outcome> {
        let _ = (sent, rewrite);
        let context = body
            .get("client_context_id")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Ok(mut r) = self.running.lock() {
            *r = Some(Running {
                context: context.clone(),
                request: None,
            });
        }
        let r = self.read(body, cancel, target).await;
        let running = self.running.lock().ok().and_then(|mut r| r.take());
        match r {
            Err(Error::Cancelled) => {
                if let Some(run) = running {
                    let _ =
                        tokio::time::timeout(Duration::from_secs(2), self.http.cancel(&run)).await;
                }
                Err(Error::Cancelled)
            }
            Ok(out) if out.truncated => {
                // The consumer stopped reading: the closed response ends
                // the request; make sure the server stops too.
                if let Some(run) = running {
                    let _ =
                        tokio::time::timeout(Duration::from_secs(2), self.http.cancel(&run)).await;
                }
                Ok(out)
            }
            Err(e @ Error::Connection(_)) => {
                self.alive = false;
                Err(e)
            }
            other => other,
        }
    }

    async fn read(
        &mut self,
        body: &Json,
        cancel: &CancellationToken,
        target: &mut Target<'_>,
    ) -> Result<Outcome> {
        let req = self.http.post_query(body).send();
        let mut resp = tokio::select! {
            r = req => r.map_err(net)?,
            _ = cancel.cancelled() => return Err(Error::Cancelled),
        };
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Connection("authentication failed".into()));
        }
        let mut reader = Reader::new();
        let mut out = Outcome::default();
        let mut eof = false;
        let rows_stmt = matches!(target, Target::Sink { rows: true, .. });
        'read: loop {
            while let Some(ev) = reader.next(eof).map_err(Error::Other)? {
                match ev {
                    Event::ResultsStart => {}
                    Event::Result(bytes) => {
                        if !target.begin().await {
                            out.truncated = true;
                            break 'read;
                        }
                        let d = stream::to_document(&bytes).map_err(Error::Other)?;
                        out.rows += 1;
                        if !target.push(d).await? {
                            out.truncated = true;
                            break 'read;
                        }
                    }
                    Event::ResultsEnd => {
                        if rows_stmt && !target.begin().await {
                            out.truncated = true;
                            break 'read;
                        }
                    }
                    Event::Field(k, v) => match k.as_str() {
                        "requestID" => {
                            if let (Ok(mut r), Some(id)) = (self.running.lock(), v.as_str())
                                && let Some(run) = r.as_mut()
                            {
                                run.request = Some(id.to_owned());
                            }
                        }
                        "errors" => out.errors = v.as_array().cloned().unwrap_or_default(),
                        "warnings" => out.warnings = v.as_array().cloned().unwrap_or_default(),
                        "metrics" => out.metrics = Some(v),
                        "profile" => out.profile = Some(v),
                        "status" => out.status = v.as_str().map(str::to_owned),
                        _ => {}
                    },
                }
            }
            if eof || reader.is_done() {
                break;
            }
            let chunk = tokio::select! {
                c = resp.chunk() => c.map_err(net)?,
                _ = cancel.cancelled() => return Err(Error::Cancelled),
            };
            match chunk {
                Some(b) => reader.feed(&b),
                None => eof = true,
            }
        }
        if !target.finish(out.truncated).await {
            out.truncated = true;
        }
        Ok(out)
    }

    /// Runs a statement and collects its rows (metadata, internal calls).
    async fn collect(
        &mut self,
        statement: &str,
        extra: Option<Map<String, Json>>,
        cancel: &CancellationToken,
    ) -> Result<(Vec<Document>, Outcome)> {
        let ctx = next_context();
        let body = self.body(statement, &ctx, extra.as_ref());
        let mut t = Target::Collect(Vec::new());
        let out = self.send(&body, statement, None, cancel, &mut t).await?;
        if !out.errors.is_empty() {
            return Err(query_error(&out.errors, statement, None));
        }
        let Target::Collect(rows) = t else {
            unreachable!()
        };
        Ok((rows, out))
    }

    async fn notices(&self, sink: &ResultSink, out: &Outcome) {
        for w in &out.warnings {
            let msg = w.get("msg").and_then(Json::as_str).unwrap_or_default();
            let code = w.get("code").map(|c| c.to_string()).unwrap_or_default();
            sink.notice(
                NoticeLevel::Warning,
                format!("{code} {msg}").trim().to_owned(),
            )
            .await;
        }
        if let Some(m) = &out.metrics {
            let get = |k: &str| {
                m.get(k)
                    .map(|v| v.as_str().map_or(v.to_string(), str::to_owned))
            };
            let mut parts = Vec::new();
            if let Some(e) = get("elapsedTime") {
                parts.push(format!("elapsed {e}"));
            }
            if let Some(e) = get("executionTime") {
                parts.push(format!("execution {e}"));
            }
            if let Some(n) = get("resultCount") {
                parts.push(format!("{n} results"));
            }
            if let Some(n) = get("resultSize") {
                parts.push(format!("{n} bytes"));
            }
            if let Some(n) = get("mutationCount") {
                parts.push(format!("{n} mutations"));
            }
            if let Some(n) = get("sortCount") {
                parts.push(format!("{n} sorted"));
            }
            if !parts.is_empty() {
                sink.notice(NoticeLevel::Info, parts.join(", ")).await;
            }
        }
    }

    /// One console statement.
    async fn statement(
        &mut self,
        text: &str,
        args: Option<Json>,
        sink: &ResultSink,
        cancel: &CancellationToken,
    ) -> Result<ExecSummary> {
        let trimmed = text.trim().trim_end_matches(';').trim();
        if let Some(tc) = sqlpp::tx_control(trimmed) {
            match tc {
                TxControl::Begin => self.begin_tx().await?,
                TxControl::Commit => self.end_tx(true).await?,
                TxControl::Rollback => self.end_tx(false).await?,
            }
            let msg = match tc {
                TxControl::Begin => format!(
                    "transaction started (txid {})",
                    self.txid.as_deref().unwrap_or_default()
                ),
                TxControl::Commit => "transaction committed".into(),
                TxControl::Rollback => "transaction rolled back".into(),
            };
            sink.notice(NoticeLevel::Info, msg).await;
            return Ok(ExecSummary::default());
        }
        let rewrite = sqlpp::rewrite_select_star(trimmed);
        let sent = rewrite.as_ref().map_or(trimmed, |r| r.text.as_str());
        let collection = rewrite.as_ref().and_then(|r| {
            r.keyspace
                .resolve(self.bucket.as_deref(), self.scope.as_deref())
                .map(|p| sqlpp::path(&p))
        });
        let database = collection
            .as_deref()
            .map(|c| sqlpp::split_path(c)[0].clone())
            .or_else(|| self.bucket.clone());
        let ctx = next_context();
        let extra = args.map(|a| {
            let mut m = Map::new();
            match a {
                Json::Array(_) => {
                    m.insert("args".into(), a);
                }
                Json::Object(o) => {
                    for (k, v) in o {
                        let k = if k.starts_with('$') {
                            k
                        } else {
                            format!("${k}")
                        };
                        m.insert(k, v);
                    }
                }
                other => {
                    m.insert("args".into(), Json::Array(vec![other]));
                }
            }
            m
        });
        let body = self.body(sent, &ctx, extra.as_ref());
        let mut target = Target::Sink {
            sink,
            database,
            collection,
            rows: sqlpp::returns_rows(trimmed),
            began: false,
            buf: Vec::new(),
        };
        let out = self
            .send(&body, sent, rewrite.as_ref(), cancel, &mut target)
            .await?;
        let began = matches!(target, Target::Sink { began: true, .. });
        self.notices(sink, &out).await;
        if !out.errors.is_empty() {
            let e = query_error(&out.errors, sent, rewrite.as_ref());
            // A failed statement inside a transaction rolls it back
            // server-side; keep the txid so COMMIT reports it.
            return Err(match e {
                Error::Query(mut q) => {
                    // Positions refer to the trimmed statement.
                    let lead = text.len() - text.trim_start().len();
                    q.range = q.range.map(|r| r.start + lead..r.end + lead);
                    Error::Query(q)
                }
                other => other,
            });
        }
        Ok(ExecSummary {
            // DML / DDL: the mutation count (absent = none); queries: rows.
            affected: (!began && !sqlpp::returns_rows(trimmed))
                .then(|| out.mutations().unwrap_or(0)),
            results: usize::from(began),
            truncated: out.truncated,
        })
    }

    async fn begin_tx(&mut self) -> Result<()> {
        if self.txid.is_some() {
            return Ok(());
        }
        let mut extra = Map::new();
        extra.insert("txtimeout".into(), Json::String("120s".into()));
        let r = self
            .collect("BEGIN WORK", Some(extra), &CancellationToken::new())
            .await;
        let rows = match r {
            Ok((rows, _)) => rows,
            Err(Error::Query(q))
                if q.message.contains("syntax error") || q.message.contains("not supported") =>
            {
                return Err(Error::Unsupported(format!(
                    "SQL++ transactions need Couchbase Server 7.0+: {}",
                    q.message
                )));
            }
            Err(e) => return Err(e),
        };
        let txid = rows
            .first()
            .and_then(|d| d.get_str("txid").ok())
            .ok_or_else(|| Error::Other("BEGIN WORK returned no txid".into()))?;
        self.txid = Some(txid.to_owned());
        Ok(())
    }

    async fn end_tx(&mut self, commit: bool) -> Result<()> {
        if self.txid.is_none() {
            return Ok(());
        }
        let r = self
            .collect(
                if commit { "COMMIT" } else { "ROLLBACK" },
                None,
                &CancellationToken::new(),
            )
            .await;
        // Committed, rolled back or expired: the transaction is over.
        self.txid = None;
        r.map(|_| ())
    }

    /// A result of a few documents.
    async fn documents(&self, sink: &ResultSink, docs: Vec<Document>) -> Result<ExecSummary> {
        let mut t = Target::Sink {
            sink,
            database: self.bucket.clone(),
            collection: None,
            rows: true,
            began: false,
            buf: Vec::new(),
        };
        let mut truncated = !t.begin().await;
        for d in docs {
            if truncated || !t.push(d).await? {
                truncated = true;
                break;
            }
        }
        let ok = t.finish(truncated).await;
        Ok(ExecSummary {
            affected: None,
            results: 1,
            truncated: !ok,
        })
    }

    async fn native(
        &mut self,
        method: &str,
        args: &[(String, String)],
        sink: &ResultSink,
        cancel: &CancellationToken,
    ) -> Result<ExecSummary> {
        let arg = |k: &str| args.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        let need = |k: &str| {
            arg(k).ok_or_else(|| {
                Error::Query(QueryError {
                    message: format!("{method}: argument `{k}` is required"),
                    code: None,
                    range: None,
                })
            })
        };
        let json_arg = |k: &str| -> Result<Option<Json>> {
            arg(k)
                .map(|v| {
                    serde_json::from_str(v).map_err(|e| {
                        Error::Query(QueryError {
                            message: format!("{method}: `{k}` is not JSON: {e}"),
                            code: Some("FERRODB-JSON".into()),
                            range: None,
                        })
                    })
                })
                .transpose()
        };
        match method {
            "buckets" => {
                let v = self
                    .http
                    .mgmt_get("/pools/default/buckets?skipMap=true")
                    .await?;
                let docs = v
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .map(bucket_doc)
                    .collect();
                self.documents(sink, docs).await
            }
            "scopes" => {
                let b = need("bucket")?;
                let v = self
                    .http
                    .mgmt_get(&format!("/pools/default/buckets/{}/scopes", encode(b)))
                    .await?;
                let docs = v
                    .get("scopes")
                    .and_then(Json::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .map(|s| match stream::to_bson(s.clone()) {
                        Bson::Document(d) => d,
                        _ => Document::new(),
                    })
                    .collect();
                self.documents(sink, docs).await
            }
            "indexes" => {
                let (stmt, extra) = match arg("bucket") {
                    Some(b) => {
                        let mut m = Map::new();
                        m.insert("args".into(), json!([b]));
                        (
                            "SELECT RAW i FROM system:indexes AS i \
                             WHERE IFMISSING(i.bucket_id, i.keyspace_id) = $1 \
                             ORDER BY i.name",
                            Some(m),
                        )
                    }
                    None => (
                        "SELECT RAW i FROM system:indexes AS i ORDER BY i.name",
                        None,
                    ),
                };
                // system: keyspaces ignore the query context.
                let saved = (self.bucket.take(), self.scope.take());
                let r = self.collect(stmt, extra, cancel).await;
                (self.bucket, self.scope) = saved;
                let (rows, _) = r?;
                self.documents(sink, rows).await
            }
            "query" => {
                let stmt = need("statement")?.to_owned();
                self.statement(&stmt, json_arg("args")?, sink, cancel).await
            }
            "profile" => {
                // Runs the statement with timings; rows are discarded.
                let stmt = need("statement")?.to_owned();
                let mut extra = Map::new();
                extra.insert("profile".into(), Json::String("timings".into()));
                let ctx = next_context();
                let body = self.body(&stmt, &ctx, Some(&extra));
                let mut t = Target::Discard;
                let out = self.send(&body, &stmt, None, cancel, &mut t).await?;
                if !out.errors.is_empty() {
                    return Err(query_error(&out.errors, &stmt, None));
                }
                let mut d = doc! { "rows": out.rows as i64 };
                if let Some(p) = out.profile.clone() {
                    d.insert("profile", stream::to_bson(p));
                }
                if let Some(m) = out.metrics.clone() {
                    d.insert("metrics", stream::to_bson(m));
                }
                self.documents(sink, vec![d]).await
            }
            "kv_get" | "kv_insert" | "kv_upsert" | "kv_replace" | "kv_remove" => {
                let ks = need("keyspace")?.to_owned();
                let key = need("key")?.to_owned();
                let value = json_arg("value")?;
                let (stmt, args) = kv_statement(method, &ks, &key, value)?;
                self.statement(&stmt, Some(args), sink, cancel).await
            }
            other => Err(Error::Unsupported(format!("native call `{other}`"))),
        }
    }
}

/// The SQL++ statement of a KV operation and its positional arguments.
pub fn kv_statement(
    method: &str,
    keyspace: &str,
    key: &str,
    value: Option<Json>,
) -> Result<(String, Json)> {
    let need_value = || {
        value.clone().ok_or_else(|| {
            Error::Query(QueryError {
                message: format!("{method}: argument `value` is required"),
                code: None,
                range: None,
            })
        })
    };
    Ok(match method {
        "kv_get" => (
            format!("SELECT META(d).id AS _id, d.* FROM {keyspace} AS d USE KEYS $1"),
            json!([key]),
        ),
        "kv_insert" => (
            format!("INSERT INTO {keyspace} (KEY, VALUE) VALUES ($1, $2)"),
            json!([key, need_value()?]),
        ),
        "kv_upsert" => (
            format!("UPSERT INTO {keyspace} (KEY, VALUE) VALUES ($1, $2)"),
            json!([key, need_value()?]),
        ),
        // Replace = write only when the document exists.
        "kv_replace" => (
            format!(
                "UPSERT INTO {keyspace} (KEY k, VALUE v) \
                 SELECT META(d).id AS k, $2 AS v FROM {keyspace} AS d USE KEYS $1"
            ),
            json!([key, need_value()?]),
        ),
        _ => (format!("DELETE FROM {keyspace} USE KEYS $1"), json!([key])),
    })
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn bracket(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn next_context() -> String {
    format!("ferrodb-{}", NEXT_CONTEXT.fetch_add(1, Ordering::Relaxed))
}

/// The fields of a bucket listing the explorer shows.
fn bucket_doc(b: &Json) -> Document {
    let mut d = Document::new();
    let s = |k: &str| b.get(k).and_then(Json::as_str).map(str::to_owned);
    let n = |v: Option<&Json>| v.and_then(Json::as_i64);
    if let Some(v) = s("name") {
        d.insert("name", v);
    }
    if let Some(v) = s("bucketType") {
        d.insert("bucketType", v);
    }
    if let Some(v) = s("storageBackend") {
        d.insert("storageBackend", v);
    }
    if let Some(v) = n(b.get("replicaNumber")) {
        d.insert("replicaNumber", v);
    }
    if let Some(v) = n(b.get("quota").and_then(|q| q.get("ram"))) {
        d.insert("ramQuota", v);
    }
    if let Some(v) = n(b.get("basicStats").and_then(|q| q.get("itemCount"))) {
        d.insert("itemCount", v);
    }
    if let Some(v) = n(b.get("maxTTL")) {
        d.insert("maxTTL", v);
    }
    if let Some(v) = s("durabilityMinLevel") {
        d.insert("durabilityMinLevel", v);
    }
    if let Some(v) = s("evictionPolicy") {
        d.insert("evictionPolicy", v);
    }
    d
}

struct CouchbaseCancel {
    http: Http,
    running: Arc<Mutex<Option<Running>>>,
}

impl RemoteCancel for CouchbaseCancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        let http = self.http.clone();
        let running = self.running.lock().ok().and_then(|r| r.clone());
        Box::pin(async move {
            match running {
                Some(r) => http.cancel(&r).await,
                None => Ok(()),
            }
        })
    }
}

impl Connection for CouchbaseConnection {
    fn server_version(&self) -> String {
        format!("Couchbase Server {}", self.version)
            .trim()
            .to_owned()
    }

    fn execute<'a>(
        &'a mut self,
        req: Request,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ExecSummary>> {
        Box::pin(async move {
            match req {
                Request::Text { text } => self.statement(&text, None, &sink, &cancel).await,
                Request::Native { method, args } => {
                    self.native(&method, &args, &sink, &cancel).await
                }
                Request::Mql(_) => Err(Error::Unsupported(
                    "MongoDB commands on a Couchbase connection".into(),
                )),
            }
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(Arc::new(CouchbaseCancel {
            http: self.http.clone(),
            running: self.running.clone(),
        }))
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        // The query service has a per-request `readonly` flag, but the
        // core already refuses writes on read-only sources.
        let _ = read_only;
        Box::pin(async { Ok(()) })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let s = schema.trim();
            if s.is_empty() {
                self.bucket = None;
            } else if self.bucket.as_deref() != Some(s) {
                self.bucket = Some(s.to_owned());
                self.scope = None;
            }
            Ok(())
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move { Ok(self.bucket.clone()) })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.begin_tx().await })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.end_tx(true).await })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.end_tx(false).await })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let r = self
                .http
                .request(
                    reqwest::Method::GET,
                    format!("{}/admin/ping", self.http.query),
                )
                .send()
                .await
                .map_err(net);
            match r {
                Ok(r) if r.status().is_success() => Ok(()),
                Ok(r) => Err(Error::Connection(format!(
                    "query service: HTTP {}",
                    r.status()
                ))),
                Err(e) => {
                    self.alive = false;
                    Err(e)
                }
            }
        })
    }

    fn is_alive(&self) -> bool {
        self.alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_errors_keep_code_and_position() {
        // Recorded from Couchbase Server 8.0.
        let errors: Vec<Json> = serde_json::from_str(
            r#"[{"code":3000,"column":30,"line":1,"msg":"syntax error - line 1, column 30, near '...FROM docs WHERE x = ', at: LIMIT (reserved word)"}]"#,
        )
        .unwrap();
        let sent = "SELECT * FROM docs WHERE x = LIMIT 2";
        let Error::Query(q) = query_error(&errors, sent, None) else {
            panic!()
        };
        assert_eq!(q.code.as_deref(), Some("3000"));
        assert_eq!(q.range, Some(29..30));
        assert_eq!(&sent[29..34], "LIMIT");
        // Through the SELECT * rewrite, back to the typed text.
        let typed = "SELECT * FROM docs WHERE x = LIMIT 2";
        let rw = sqlpp::rewrite_select_star(typed).unwrap();
        let col = rw.text.find("LIMIT").unwrap() + 1;
        let errors = vec![json!({"code": 3000, "line": 1, "column": col, "msg": "x"})];
        let Error::Query(q) = query_error(&errors, &rw.text, Some(&rw)) else {
            panic!()
        };
        assert_eq!(q.range, Some(29..30));
        let auth = vec![json!({"code": 10000, "msg": "Authentication failure"})];
        assert!(matches!(query_error(&auth, "", None), Error::Connection(_)));
        assert_eq!(offset("a\nbcd", 2, 3), 4);
        let end = vec![json!({"code": 3000, "msg": "syntax error - at end of input"})];
        let Error::Query(q) = query_error(&end, "SELECT * FROM t WHERE", None) else {
            panic!()
        };
        assert_eq!(q.range, Some(20..21));
    }

    #[test]
    fn kv_statements() {
        let (s, a) = kv_statement("kv_get", "tck.app.docs", "k1", None).unwrap();
        assert_eq!(
            s,
            "SELECT META(d).id AS _id, d.* FROM tck.app.docs AS d USE KEYS $1"
        );
        assert_eq!(a, json!(["k1"]));
        let (s, a) = kv_statement("kv_upsert", "b", "k", Some(json!({"a": 1}))).unwrap();
        assert!(s.starts_with("UPSERT INTO b (KEY, VALUE)"));
        assert_eq!(a, json!(["k", {"a": 1}]));
        assert!(kv_statement("kv_insert", "b", "k", None).is_err());
        assert_eq!(
            kv_statement("kv_remove", "b", "k", None).unwrap().0,
            "DELETE FROM b USE KEYS $1"
        );
        let b = bucket_doc(
            &json!({"name": "tck", "bucketType": "membase", "replicaNumber": 0,
            "quota": {"ram": 268435456}, "basicStats": {"itemCount": 12}}),
        );
        assert_eq!(b.get_i64("itemCount").unwrap(), 12);
        assert_eq!(encode("travel sample"), "travel%20sample");
        assert_eq!(bracket("::1", 8091), "[::1]:8091");
    }
}
