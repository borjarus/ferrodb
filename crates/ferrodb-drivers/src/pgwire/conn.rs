//! One PostgreSQL session over `tokio-postgres`: connect (TLS, SSH tunnel
//! endpoint), streaming execution, notices, server-side cancellation.

use std::{error::Error as StdError, sync::Arc, time::Duration};

use ferrodb_api::{Error, NoticeLevel, QueryError, Result, ServerNotice};
use ferrodb_net::TlsMode;
use futures::StreamExt as _;
use tokio::sync::mpsc;
use tokio_postgres::{
    AsyncMessage, CancelToken, Client, Socket,
    config::SslMode,
    error::{DbError, ErrorPosition, Severity, SqlState},
    tls::MakeTlsConnect,
    types::ToSql,
};
use tokio_postgres_rustls::MakeRustlsConnect;

use super::{
    Flavor,
    decode::{self, Raw, error_range},
};
use crate::{
    BoxFuture, CancellationToken, Connection, Endpoint, ExecSummary, RemoteCancel, Request,
    ResolvedConfig, ResultSink, SinkEvent,
    columns::{BatchBuilder, schema},
    quote_ident,
};

/// TLS connector that always presents the real server name (not the local
/// end of an SSH tunnel) for SNI and certificate verification.
#[derive(Clone)]
pub(crate) struct PgTls {
    inner: MakeRustlsConnect,
    server_name: String,
}

impl MakeTlsConnect<Socket> for PgTls {
    type Stream = <MakeRustlsConnect as MakeTlsConnect<Socket>>::Stream;
    type TlsConnect = <MakeRustlsConnect as MakeTlsConnect<Socket>>::TlsConnect;
    type Error = <MakeRustlsConnect as MakeTlsConnect<Socket>>::Error;

    fn make_tls_connect(
        &mut self,
        _host: &str,
    ) -> std::result::Result<Self::TlsConnect, Self::Error> {
        let name = self.server_name.clone();
        <MakeRustlsConnect as MakeTlsConnect<Socket>>::make_tls_connect(&mut self.inner, &name)
    }
}

fn tls(cfg: &ResolvedConfig, server_name: &str) -> Result<(SslMode, PgTls)> {
    let (mode, rustls) = match cfg.tls {
        // The connector is never used with `SslMode::Disable`; build one
        // anyway so connection and cancel requests share one type.
        TlsMode::Disable => (
            SslMode::Disable,
            ferrodb_net::client_config(TlsMode::Prefer, None),
        ),
        TlsMode::Prefer => (
            SslMode::Prefer,
            ferrodb_net::client_config(TlsMode::Prefer, cfg.tls_ca.as_deref()),
        ),
        m @ (TlsMode::Require | TlsMode::VerifyFull) => (
            SslMode::Require,
            ferrodb_net::client_config(m, cfg.tls_ca.as_deref()),
        ),
    };
    let config = rustls
        .map_err(|e| Error::Connection(e.to_string()))?
        .ok_or_else(|| Error::Connection("TLS configuration unavailable".into()))?;
    Ok((
        mode,
        PgTls {
            inner: MakeRustlsConnect::new((*config).clone()),
            server_name: server_name.to_owned(),
        },
    ))
}

/// Error text with its source chain (`error connecting to server: …`).
fn chain(e: &dyn StdError) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(c) = src {
        let t = c.to_string();
        if !s.contains(&t) {
            s.push_str(": ");
            s.push_str(&t);
        }
        src = c.source();
    }
    s
}

fn db_message(db: &DbError) -> String {
    let mut m = db.message().to_owned();
    if let Some(d) = db.detail() {
        m.push_str("\nDETAIL: ");
        m.push_str(d);
    }
    if let Some(h) = db.hint() {
        m.push_str("\nHINT: ");
        m.push_str(h);
    }
    m
}

/// Maps a statement error; `text` is the submitted statement (for error
/// positions).
pub(crate) fn map_err(e: tokio_postgres::Error, text: &str) -> Error {
    if let Some(db) = e.as_db_error() {
        if *db.code() == SqlState::QUERY_CANCELED {
            return Error::Cancelled;
        }
        let range = match db.position() {
            Some(ErrorPosition::Original(p)) => error_range(text, *p),
            _ => None,
        };
        return Error::Query(QueryError {
            message: db_message(db),
            code: Some(db.code().code().to_owned()),
            range,
        });
    }
    if e.is_closed() {
        return Error::ConnectionLost;
    }
    Error::Other(chain(&e))
}

fn notice(db: &DbError) -> ServerNotice {
    let warning = matches!(db.parsed_severity(), Some(Severity::Warning))
        || db.severity().eq_ignore_ascii_case("WARNING");
    ServerNotice {
        level: if warning {
            NoticeLevel::Warning
        } else {
            NoticeLevel::Info
        },
        message: db_message(db),
    }
}

pub(crate) async fn connect(
    flavor: &'static Flavor,
    cfg: &ResolvedConfig,
) -> Result<Box<dyn Connection>> {
    let endpoint = cfg.endpoint(flavor.default_port).await?;
    let (ssl_mode, tls) = tls(cfg, &endpoint.server_name)?;
    let mut pc = tokio_postgres::Config::new();
    pc.host(&endpoint.host)
        .port(endpoint.port)
        .user(cfg.user().unwrap_or(flavor.default_user))
        .connect_timeout(cfg.connect_timeout)
        .application_name(&cfg.application_name)
        .ssl_mode(ssl_mode)
        .keepalives(true);
    if let Some(p) = cfg.password() {
        pc.password(p);
    }
    if let Some(db) = cfg.database() {
        pc.dbname(db);
    }
    let mut options = Vec::new();
    for (k, v) in cfg.options() {
        match k {
            "options" => options.push(v.to_owned()),
            "search_path" => options.push(format!("-c search_path={}", v.replace(' ', "\\ "))),
            _ => {}
        }
    }
    if !options.is_empty() {
        pc.options(options.join(" "));
    }
    let target = format!("{}:{}", endpoint.server_name, endpoint.port);
    let fail = |e: tokio_postgres::Error| match e.as_db_error() {
        Some(db) => Error::Connection(format!(
            "{target}: {} (SQLSTATE {})",
            db.message(),
            db.code().code()
        )),
        None => Error::Connection(format!("{target}: {}", chain(&e))),
    };
    let (client, mut conn) = tokio::time::timeout(cfg.connect_timeout, pc.connect(tls.clone()))
        .await
        .map_err(|_| Error::Connection(format!("{target}: timed out")))?
        .map_err(fail)?;
    let version = conn
        .parameter("server_version")
        .and_then(|v| v.split_whitespace().next())
        .map(|v| format!("{} {v}", flavor.product))
        .unwrap_or_else(|| flavor.product.to_owned());
    let (notices_tx, notices) = mpsc::unbounded_channel();
    let tunnel = endpoint.clone();
    tokio::spawn(async move {
        let _tunnel = tunnel;
        while let Some(m) = futures::future::poll_fn(|cx| conn.poll_message(cx)).await {
            match m {
                Ok(AsyncMessage::Notice(db)) => {
                    let _ = notices_tx.send(notice(&db));
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!("postgres connection ended: {e}");
                    break;
                }
            }
        }
    });
    let mut c = PgConnection {
        cancel: Arc::new(PgCancel {
            token: client.cancel_token(),
            tls,
            _endpoint: endpoint.clone(),
        }),
        client,
        notices,
        version,
        _endpoint: endpoint,
    };
    if cfg.read_only {
        c.set_read_only(true).await?;
    }
    Ok(Box::new(c))
}

pub(crate) struct PgConnection {
    client: Client,
    notices: mpsc::UnboundedReceiver<ServerNotice>,
    cancel: Arc<PgCancel>,
    version: String,
    /// Keeps an SSH tunnel open as long as the session.
    _endpoint: Endpoint,
}

pub(crate) struct PgCancel {
    token: CancelToken,
    tls: PgTls,
    _endpoint: Endpoint,
}

impl PgCancel {
    async fn send(&self) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.token.cancel_query(self.tls.clone()),
        )
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|e| Error::Other(format!("cancel request failed: {}", chain(&e))))
    }
}

impl RemoteCancel for PgCancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        let me = PgCancel {
            token: self.token.clone(),
            tls: self.tls.clone(),
            _endpoint: self._endpoint.clone(),
        };
        Box::pin(async move { me.send().await })
    }
}

/// Statements whose row count is reported (by first keyword). `WITH`
/// without result columns is a data-modifying CTE.
fn is_dml(text: &str) -> bool {
    let word: String = skip_comments(text)
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        word.as_str(),
        "insert" | "update" | "delete" | "merge" | "copy" | "with"
    )
}

/// Skips leading whitespace and comments.
fn skip_comments(mut t: &str) -> &str {
    loop {
        t = t.trim_start();
        if let Some(r) = t.strip_prefix("--") {
            t = r.split_once('\n').map(|(_, r)| r).unwrap_or("");
        } else if let Some(r) = t.strip_prefix("/*") {
            t = r.split_once("*/").map(|(_, r)| r).unwrap_or("");
        } else {
            return t;
        }
    }
}

fn forward_notices(rx: &mut mpsc::UnboundedReceiver<ServerNotice>) -> Vec<ServerNotice> {
    let mut v = Vec::new();
    while let Ok(n) = rx.try_recv() {
        v.push(n);
    }
    v
}

async fn send_notices(sink: &ResultSink, rx: &mut mpsc::UnboundedReceiver<ServerNotice>) {
    for n in forward_notices(rx) {
        let _ = sink.send(SinkEvent::Notice(n)).await;
    }
}

async fn run(
    client: &Client,
    notices: &mut mpsc::UnboundedReceiver<ServerNotice>,
    text: &str,
    sink: &ResultSink,
    cancel: &CancellationToken,
) -> Result<ExecSummary> {
    let stmt = client.prepare(text).await.map_err(|e| map_err(e, text))?;
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let rows = client
        .query_raw(&stmt, std::iter::empty::<&(dyn ToSql + Sync)>())
        .await
        .map_err(|e| map_err(e, text))?;
    let mut rows = std::pin::pin!(rows);
    let cols = stmt.columns();
    if cols.is_empty() {
        while let Some(r) = rows.next().await {
            r.map_err(|e| map_err(e, text))?;
        }
        send_notices(sink, notices).await;
        return Ok(ExecSummary {
            affected: if is_dml(text) {
                rows.rows_affected()
            } else {
                None
            },
            results: 0,
            truncated: false,
        });
    }
    let (specs, plans): (Vec<_>, Vec<_>) = cols
        .iter()
        .map(|c| decode::plan(c.name(), c.type_(), c.type_modifier()))
        .unzip();
    let s = schema(&specs);
    let mut summary = ExecSummary {
        results: 1,
        ..Default::default()
    };
    send_notices(sink, notices).await;
    if sink
        .send(SinkEvent::Begin { schema: s.clone() })
        .await
        .is_err()
    {
        summary.truncated = true;
        return Ok(summary);
    }
    let mut b = BatchBuilder::new(s, sink.batch_rows());
    let mut scratch = String::new();
    while let Some(row) = rows.next().await {
        let row = row.map_err(|e| map_err(e, text))?;
        for (i, plan) in plans.iter().enumerate() {
            let raw: Option<Raw<'_>> = row.try_get(i).map_err(|e| map_err(e, text))?;
            match raw {
                None => b.push(i, crate::columns::Cell::Null),
                Some(Raw(v)) => b.push(i, decode::cell(plan, v, &mut scratch)),
            }
        }
        b.end_row();
        if b.should_flush()
            && let Some(batch) = b.flush()
        {
            for n in forward_notices(notices) {
                let _ = sink.send(SinkEvent::Notice(n)).await;
            }
            if sink.send(SinkEvent::Batch(batch)).await.is_err() {
                summary.truncated = true;
                return Ok(summary);
            }
        }
    }
    if let Some(batch) = b.flush()
        && sink.send(SinkEvent::Batch(batch)).await.is_err()
    {
        summary.truncated = true;
        return Ok(summary);
    }
    send_notices(sink, notices).await;
    if b.lossy() > 0 {
        sink.notice(
            NoticeLevel::Warning,
            format!(
                "{} value(s) could not be represented in their column type (e.g. infinity, NaN) and are shown as NULL",
                b.lossy()
            ),
        )
        .await;
    }
    let _ = sink.send(SinkEvent::End).await;
    Ok(summary)
}

impl PgConnection {
    async fn batch(&self, sql: &str) -> Result<()> {
        self.client
            .batch_execute(sql)
            .await
            .map_err(|e| map_err(e, sql))
    }
}

impl Connection for PgConnection {
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
            // Notices left over from session commands belong to no statement.
            forward_notices(&mut self.notices);
            let canceller = self.cancel.clone();
            let client = &self.client;
            let mut job = Box::pin(run(client, &mut self.notices, &text, &sink, &cancel));
            let r = tokio::select! {
                r = &mut job => return r,
                _ = cancel.cancelled() => {
                    if let Err(e) = canceller.send().await {
                        tracing::warn!("{e}");
                    }
                    // Let the statement stop so the session is idle again.
                    tokio::time::timeout(Duration::from_secs(2), &mut job).await
                }
            };
            drop(job);
            if r.is_err() {
                // Still busy (the consumer stopped reading): the dropped
                // stream is discarded by the connection; wait for the server
                // to finish the statement with a trivial round trip.
                let _ = tokio::time::timeout(Duration::from_secs(2), self.client.simple_query(""))
                    .await;
            }
            if self.client.is_closed() {
                return Err(Error::ConnectionLost);
            }
            Err(Error::Cancelled)
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(self.cancel.clone())
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.batch(if read_only {
                "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY"
            } else {
                "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE"
            })
            .await
        })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.batch(&format!("SET search_path TO {}", quote_ident(schema, '"')))
                .await
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            let sql = "select current_schema()";
            let row = self
                .client
                .query_one(sql, &[])
                .await
                .map_err(|e| map_err(e, sql))?;
            row.try_get::<_, Option<String>>(0)
                .map_err(|e| map_err(e, sql))
        })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.batch("BEGIN").await })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.batch("COMMIT").await })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.batch("ROLLBACK").await })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.batch("select 1").await })
    }

    fn is_alive(&self) -> bool {
        !self.client.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dml_detection() {
        assert!(is_dml("insert into t values (1)"));
        assert!(is_dml("  -- c\n /* x */ UPDATE t set a = 1"));
        assert!(is_dml("with x as (delete from t returning 1) select 1"));
        assert!(!is_dml("create table t (a int)"));
        assert!(!is_dml("select 1"));
    }
}
