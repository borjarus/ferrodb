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
    Flavor, Product,
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
    // Redshift IAM: temporary credentials replace the configured ones.
    let iam = if flavor.kind == Product::Redshift && cfg.prop("auth") == Some("AwsIam") {
        let db = cfg.database().or(flavor.default_database).unwrap_or("dev");
        Some(crate::aws::redshift_credentials(cfg, &endpoint.server_name, db).await?)
    } else {
        None
    };
    pc.host(&endpoint.host)
        .port(endpoint.port)
        .user(
            iam.as_ref()
                .map(|(u, _)| u.as_str())
                .or(cfg.user())
                .unwrap_or(flavor.default_user),
        )
        .connect_timeout(cfg.connect_timeout)
        .application_name(&cfg.application_name)
        .ssl_mode(ssl_mode)
        .keepalives(true);
    if let Some((_, p)) = &iam {
        pc.password(p);
    } else if let Some(p) = cfg.password() {
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
    let pg_version = conn
        .parameter("server_version")
        .and_then(|v| v.split_whitespace().next())
        .map(str::to_owned);
    // CockroachDB announces itself (`CockroachDB CCL v24.3.1 (…)`) even
    // when reached through the PostgreSQL driver.
    let crdb = conn.parameter("crdb_version").map(str::to_owned);
    let mut version = match (&crdb, &pg_version) {
        (Some(c), _) => crdb_version(c),
        (None, Some(v)) => format!("{} {v}", flavor.product),
        (None, None) => flavor.product.to_owned(),
    };
    let product = match (&crdb, flavor.kind) {
        (Some(_), _) => Product::CockroachDb,
        (None, Product::CockroachDb) => Product::PostgreSql,
        (None, k) => k,
    };
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
        version: version.clone(),
        product,
        dialect: flavor.dialect,
        read_only: false,
        in_tx: false,
        retries: cfg
            .options()
            .find(|(k, _)| *k == "retries")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(3),
        _endpoint: endpoint,
    };
    // Greenplum and Redshift report the PostgreSQL version they derive
    // from; `version()` names the product and its own version.
    if matches!(product, Product::Greenplum | Product::Redshift)
        && let Ok(row) = c.client.query_one("select version()", &[]).await
        && let Ok(text) = row.try_get::<_, String>(0)
    {
        version = product_version(&text, pg_version.as_deref()).unwrap_or(version);
        c.version = version;
    }
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
    /// The product actually connected to (CockroachDB is recognized
    /// whatever the flavor).
    product: Product,
    /// SQL dialect id, for classifying statements.
    dialect: &'static str,
    /// Read-only enforced here (Redshift has no read-only sessions).
    read_only: bool,
    /// An explicit transaction is open: a CockroachDB retry error then
    /// belongs to the whole transaction, not to one statement.
    in_tx: bool,
    /// Automatic retries of a statement failing with SQLSTATE 40001.
    retries: u32,
    /// Keeps an SSH tunnel open as long as the session.
    _endpoint: Endpoint,
}

/// `CockroachDB CCL v24.3.1 (x86_64-pc-linux-gnu, …)` → `CockroachDB 24.3.1`.
pub(crate) fn crdb_version(param: &str) -> String {
    let v = param
        .split_whitespace()
        .find(|w| w.starts_with('v') && w[1..].starts_with(|c: char| c.is_ascii_digit()))
        .map(|w| &w[1..]);
    match v {
        Some(v) => format!("CockroachDB {v}"),
        None => "CockroachDB".into(),
    }
}

/// The product's own version from `version()`:
/// `PostgreSQL 12.12 (Greenplum Database 7.1.0 build …)` → `Greenplum 7.1.0`,
/// `PostgreSQL 8.0.2 on i686-pc-linux-gnu, …, Redshift 1.0.77467` →
/// `Redshift 1.0.77467`.
pub(crate) fn product_version(text: &str, pg: Option<&str>) -> Option<String> {
    let after = |marker: &str| {
        let i = text.find(marker)? + marker.len();
        let v: String = text[i..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        (!v.is_empty()).then_some(v)
    };
    let pg = pg.map(|p| format!(" (PostgreSQL {p})")).unwrap_or_default();
    if let Some(v) = after("Greenplum Database") {
        return Some(format!("Greenplum {v}{pg}"));
    }
    after("Redshift").map(|v| format!("Redshift {v}"))
}

/// Transaction control by first keyword: `Some(true)` opens, `Some(false)`
/// ends a transaction.
fn tx_effect(text: &str) -> Option<bool> {
    let words: Vec<String> = skip_comments(text)
        .split_whitespace()
        .take(2)
        .map(|w| {
            w.trim_end_matches(';')
                .chars()
                .filter(char::is_ascii_alphabetic)
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .collect();
    match words.first().map(String::as_str) {
        Some("begin") => Some(true),
        Some("start") if words.get(1).map(String::as_str) == Some("transaction") => Some(true),
        Some("commit" | "rollback" | "end" | "abort") => {
            // `ROLLBACK TO SAVEPOINT` keeps the transaction open.
            (words.get(1).map(String::as_str) != Some("to")).then_some(false)
        }
        _ => None,
    }
}

/// CockroachDB asks the client to retry (serialization conflict).
fn is_retry_error(e: &Error) -> bool {
    matches!(e, Error::Query(q) if q.code.as_deref() == Some("40001"))
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
            if self.read_only
                && self.product == Product::Redshift
                && !ferrodb_lang::sql::analyze::classify(&text, self.dialect).allowed_read_only()
            {
                return Err(Error::ReadOnly);
            }
            let tx = tx_effect(&text);
            let mut attempt = 0;
            let r = loop {
                let r = self.attempt(&text, &sink, &cancel).await;
                match &r {
                    Err(e)
                        if self.product == Product::CockroachDb
                            && is_retry_error(e)
                            && !self.in_tx
                            && tx.is_none()
                            && attempt < self.retries
                            && !returns_rows(&text) =>
                    {
                        attempt += 1;
                        sink.notice(
                            NoticeLevel::Info,
                            format!("serialization conflict (40001): retry {attempt}"),
                        )
                        .await;
                        tokio::time::sleep(Duration::from_millis(25 << attempt)).await;
                    }
                    _ => break r,
                }
            };
            match (&r, tx) {
                (Ok(_), Some(open)) => self.in_tx = open,
                // A failed COMMIT ends the transaction too.
                (Err(_), Some(false)) => self.in_tx = false,
                _ => {}
            }
            r.map_err(|e| self.explain_error(e))
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(self.cancel.clone())
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.read_only = read_only;
            if self.product == Product::Redshift {
                // No read-only sessions: statements are checked before
                // they are sent (and the core refuses them too).
                return Ok(());
            }
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
        Box::pin(async move {
            self.batch("BEGIN").await?;
            self.in_tx = true;
            Ok(())
        })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.in_tx = false;
            self.batch("COMMIT")
                .await
                .map_err(|e| self.explain_error(e))
        })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.in_tx = false;
            self.batch("ROLLBACK").await
        })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.batch("select 1").await })
    }

    fn is_alive(&self) -> bool {
        !self.client.is_closed()
    }

    fn bulk_load<'a>(
        &'a mut self,
        table: &'a str,
        columns: &'a [String],
        data: Vec<u8>,
    ) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move {
            use futures::SinkExt as _;
            if self.product == Product::Redshift {
                // Redshift copies from S3 only, not from the client.
                return Err(Error::Unsupported("COPY FROM STDIN on Redshift".into()));
            }
            let sql = format!("COPY {table} ({}) FROM STDIN", columns.join(", "));
            let sink = self
                .client
                .copy_in::<_, bytes::Bytes>(&sql)
                .await
                .map_err(|e| map_err(e, &sql))?;
            futures::pin_mut!(sink);
            sink.send(bytes::Bytes::from(data))
                .await
                .map_err(|e| map_err(e, &sql))?;
            sink.as_mut().finish().await.map_err(|e| map_err(e, &sql))
        })
    }

    fn bulk_load_method(&self) -> Option<&'static str> {
        (self.product != Product::Redshift).then_some("COPY")
    }
}

/// Statements that stream rows: a retry after rows were shown would show
/// them twice, so only other statements are retried automatically.
fn returns_rows(text: &str) -> bool {
    let word: String = skip_comments(text)
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        word.as_str(),
        "select" | "with" | "show" | "values" | "table" | "explain"
    ) || text.to_ascii_lowercase().contains("returning")
}

impl PgConnection {
    /// Adds what to do about a CockroachDB transaction retry error.
    fn explain_error(&self, e: Error) -> Error {
        match e {
            Error::Query(mut q)
                if self.product == Product::CockroachDb && q.code.as_deref() == Some("40001") =>
            {
                q.message.push_str(if self.in_tx {
                    "\nCockroachDB aborted the transaction to resolve a conflict with \
                     another one: ROLLBACK and run the whole transaction again."
                } else {
                    "\nCockroachDB could not serialize the statement with concurrent \
                     transactions; it was retried and still conflicts: run it again."
                });
                Error::Query(q)
            }
            other => other,
        }
    }

    /// One execution of `text`, cancellable.
    async fn attempt(
        &mut self,
        text: &str,
        sink: &ResultSink,
        cancel: &CancellationToken,
    ) -> Result<ExecSummary> {
        {
            // Notices left over from session commands belong to no statement.
            forward_notices(&mut self.notices);
            let canceller = self.cancel.clone();
            let client = &self.client;
            let mut job = Box::pin(run(client, &mut self.notices, text, sink, cancel));
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
        }
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
        assert_eq!(tx_effect("BEGIN"), Some(true));
        assert_eq!(tx_effect("start transaction read only"), Some(true));
        assert_eq!(tx_effect("commit;"), Some(false));
        assert_eq!(tx_effect("rollback to savepoint a"), None);
        assert_eq!(tx_effect("select 1"), None);
        assert!(returns_rows("show tables"));
        assert!(returns_rows("insert into t values (1) returning id"));
        assert!(!returns_rows("update t set a = 1"));
    }
}
