//! Protocol code shared by MySQL and MariaDB: connect (TLS modes), text
//! protocol execution with multiple result sets, `KILL QUERY` cancellation.

use std::{sync::Arc, time::Duration};

use ferrodb_api::{Error, NoticeLevel, QueryError, Result, ServerNotice};
use ferrodb_net::TlsMode;
use mysql_async::{Conn, Opts, OptsBuilder, SslOpts, TextProtocol, prelude::Queryable};

use super::{
    Product,
    types::{self, ER_PARSE_ERROR, ER_QUERY_INTERRUPTED},
};
use crate::{
    BoxFuture, CancellationToken, Connection, Endpoint, ExecSummary, RemoteCancel, Request,
    ResolvedConfig, ResultSink, SinkEvent,
    columns::{BatchBuilder, schema},
    quote_ident,
};

/// Upper bound for opening the side connection that sends `KILL QUERY`.
const KILL_TIMEOUT: Duration = Duration::from_secs(5);

pub struct MySqlConnection {
    conn: Conn,
    version: String,
    killer: Arc<Killer>,
    alive: bool,
    /// Keeps an SSH tunnel open for the lifetime of the session.
    _endpoint: Endpoint,
}

/// Cancels the running statement of connection `id` from a second,
/// short-lived connection opened with the same options.
pub struct Killer {
    opts: Opts,
    id: u32,
}

impl Killer {
    async fn kill(&self) -> Result<()> {
        let opts = self.opts.clone();
        let sql = format!("KILL QUERY {}", self.id);
        let work = async move {
            let mut c = Conn::new(opts).await?;
            let r = c.query_drop(sql).await;
            let _ = c.disconnect().await;
            r
        };
        match tokio::time::timeout(KILL_TIMEOUT, work).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(Error::Other(format!("KILL QUERY failed: {e}"))),
            Err(_) => Err(Error::Timeout),
        }
    }
}

impl RemoteCancel for Killer {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        let k = Killer {
            opts: self.opts.clone(),
            id: self.id,
        };
        Box::pin(async move { k.kill().await })
    }
}

fn connect_error(e: mysql_async::Error) -> Error {
    match e {
        mysql_async::Error::Server(s) => Error::Connection(format!("{} ({})", s.message, s.code)),
        other => Error::Connection(other.to_string()),
    }
}

/// Maps an error raised while running `text`.
pub fn query_error(e: mysql_async::Error, text: &str) -> Error {
    use mysql_async::{DriverError, Error as E};
    match e {
        E::Server(s) if s.code == ER_QUERY_INTERRUPTED => Error::Cancelled,
        E::Server(s) => {
            let range = if s.code == ER_PARSE_ERROR {
                types::syntax_error_range(&s.message, text)
            } else {
                None
            };
            Error::Query(QueryError {
                message: s.message,
                code: Some(format!("{} ({})", s.code, s.state)),
                range,
            })
        }
        E::Io(_) | E::Driver(DriverError::ConnectionClosed) => Error::ConnectionLost,
        other => Error::Other(other.to_string()),
    }
}

fn ssl_opts(cfg: &ResolvedConfig, ep: &Endpoint) -> Option<SslOpts> {
    let roots = || {
        cfg.tls_ca
            .clone()
            .map(|p| vec![p.into()])
            .unwrap_or_default()
    };
    match cfg.tls {
        TlsMode::Disable => None,
        // Encrypt without verification (libpq semantics); a CA file makes
        // `require` verify the chain (not the host name).
        TlsMode::Prefer | TlsMode::Require => Some(match &cfg.tls_ca {
            Some(_) if cfg.tls == TlsMode::Require => SslOpts::default()
                .with_root_certs(roots())
                .with_danger_skip_domain_validation(true),
            _ => SslOpts::default()
                .with_danger_accept_invalid_certs(true)
                .with_danger_skip_domain_validation(true),
        }),
        TlsMode::VerifyFull => Some(
            SslOpts::default()
                .with_root_certs(roots())
                .with_danger_tls_hostname_override(Some(ep.server_name.clone())),
        ),
    }
}

impl MySqlConnection {
    pub async fn connect(cfg: &ResolvedConfig) -> Result<Self> {
        ferrodb_net::install_crypto_provider();
        let ep = cfg.endpoint(super::DEFAULT_PORT).await?;
        let base = OptsBuilder::default()
            .ip_or_hostname(ep.host.clone())
            .tcp_port(ep.port)
            .user(cfg.user())
            .pass(cfg.password())
            .db_name(cfg.database())
            .prefer_socket(false)
            .stmt_cache_size(0)
            .connect_attribute("program_name", cfg.application_name.clone());
        let ssl = ssl_opts(cfg, &ep);
        let open = |opts: Opts| async move {
            tokio::time::timeout(cfg.connect_timeout, Conn::new(opts))
                .await
                .map_err(|_| Error::Connection("timed out".into()))?
                .map_err(connect_error)
        };
        let (conn, opts) = match ssl {
            None => {
                let opts = Opts::from(base);
                (open(opts.clone()).await?, opts)
            }
            Some(ssl) => {
                let opts = Opts::from(base.clone().ssl_opts(ssl));
                let tls_only = cfg.tls != TlsMode::Prefer;
                match tokio::time::timeout(cfg.connect_timeout, Conn::new(opts.clone())).await {
                    Err(_) => return Err(Error::Connection("timed out".into())),
                    Ok(Ok(c)) => (c, opts),
                    Ok(Err(mysql_async::Error::Driver(
                        mysql_async::DriverError::NoClientSslFlagFromServer,
                    ))) if !tls_only => {
                        // `prefer`: the server does not offer TLS.
                        let opts = Opts::from(base);
                        (open(opts.clone()).await?, opts)
                    }
                    Ok(Err(mysql_async::Error::Driver(
                        mysql_async::DriverError::NoClientSslFlagFromServer,
                    ))) => {
                        return Err(Error::Connection("the server does not support TLS".into()));
                    }
                    Ok(Err(e)) => return Err(connect_error(e)),
                }
            }
        };
        let mut conn = conn;
        let raw: Option<String> = conn
            .query_first("select version()")
            .await
            .map_err(connect_error)?;
        let raw = raw.unwrap_or_default();
        let product = Product::from_version(&raw);
        if cfg.read_only {
            conn.query_drop(product.read_only_sql(true))
                .await
                .map_err(connect_error)?;
        }
        let id = conn.id();
        Ok(Self {
            conn,
            version: Product::version_string(&raw),
            killer: Arc::new(Killer { opts, id }),
            alive: true,
            _endpoint: ep,
        })
    }

    fn product(&self) -> Product {
        Product::from_version(&self.version)
    }

    async fn simple(&mut self, sql: &str) -> Result<()> {
        let r = self
            .conn
            .query_drop(sql)
            .await
            .map_err(|e| query_error(e, sql));
        self.note(&r);
        r
    }

    fn note<T>(&mut self, r: &Result<T>) {
        if matches!(r, Err(Error::ConnectionLost)) {
            self.alive = false;
        }
    }
}

/// Sends `ev` unless the consumer is gone or the statement was cancelled.
async fn send(sink: &ResultSink, cancel: &CancellationToken, ev: SinkEvent) -> bool {
    if cancel.is_cancelled() {
        return false;
    }
    tokio::select! {
        r = sink.send(ev) => r.is_ok(),
        _ = cancel.cancelled() => false,
    }
}

type TextResult<'a> = mysql_async::QueryResult<'a, 'static, TextProtocol>;

/// Stops a statement whose results are no longer wanted and reads what is
/// left so the connection can run the next statement.
async fn drain(
    mut result: TextResult<'_>,
    killer: &Killer,
    text: &str,
    already_killed: bool,
) -> Result<()> {
    if !already_killed {
        let _ = killer.kill().await;
    }
    loop {
        match result.next().await {
            Ok(Some(_)) => {}
            Ok(None) if result.is_empty() => break,
            // Next statement of a multi-statement text: it is running now.
            Ok(None) => {
                let _ = killer.kill().await;
            }
            Err(mysql_async::Error::Server(_)) => break,
            Err(e) => return Err(query_error(e, text)),
        }
    }
    match result.drop_result().await {
        Ok(()) | Err(mysql_async::Error::Server(_)) => Ok(()),
        Err(e) => Err(query_error(e, text)),
    }
}

async fn run(
    conn: &mut Conn,
    text: &str,
    sink: &ResultSink,
    cancel: &CancellationToken,
    killer: &Killer,
) -> Result<ExecSummary> {
    let mut result: TextResult<'_> = conn
        .query_iter(text)
        .await
        .map_err(|e| query_error(e, text))?;
    let dml = types::is_dml(text);
    let mut summary = ExecSummary::default();
    let mut affected = 0u64;
    let mut warnings = 0u32;
    let mut stopped = false;
    'sets: loop {
        let Some(cols) = result.columns() else {
            // Surfaces an error of a later statement, if any.
            result.next().await.map_err(|e| query_error(e, text))?;
            break;
        };
        if cols.is_empty() {
            // OK packet: DML, DDL, SET, the status of a CALL…
            affected += result.affected_rows();
            warnings += u32::from(result.warnings());
            result.next().await.map_err(|e| query_error(e, text))?;
            continue;
        }
        summary.results += 1;
        let (specs, convs): (Vec<_>, Vec<_>) = cols.iter().map(types::column_spec).unzip();
        let s = schema(&specs);
        if !send(sink, cancel, SinkEvent::Begin { schema: s.clone() }).await {
            stopped = true;
            break;
        }
        let mut b = BatchBuilder::new(s, sink.batch_rows());
        loop {
            if cancel.is_cancelled() {
                stopped = true;
                break 'sets;
            }
            let Some(row) = result.next().await.map_err(|e| query_error(e, text))? else {
                break;
            };
            for (i, conv) in convs.iter().enumerate() {
                b.push(i, types::cell(row.as_ref(i), *conv));
            }
            b.end_row();
            if b.should_flush()
                && let Some(batch) = b.flush()
                && !send(sink, cancel, SinkEvent::Batch(batch)).await
            {
                stopped = true;
                break 'sets;
            }
        }
        if let Some(batch) = b.flush()
            && !send(sink, cancel, SinkEvent::Batch(batch)).await
        {
            stopped = true;
            break;
        }
        if b.lossy() > 0 {
            let _ = send(
                sink,
                cancel,
                SinkEvent::Notice(ServerNotice {
                    level: NoticeLevel::Warning,
                    message: format!(
                        "{} value(s) could not be represented (e.g. zero dates) and are shown as NULL",
                        b.lossy()
                    ),
                }),
            )
            .await;
        }
        warnings += u32::from(result.warnings());
        if !send(sink, cancel, SinkEvent::End).await {
            stopped = true;
            break;
        }
    }
    if stopped {
        drain(result, killer, text, cancel.is_cancelled()).await?;
        summary.truncated = true;
        return Ok(summary);
    }
    drop(result);
    summary.affected = dml.then_some(affected);
    if warnings > 0 {
        forward_warnings(conn, sink, cancel, warnings).await;
    }
    Ok(summary)
}

/// `SHOW WARNINGS` (last statement) as notices; the count alone if that
/// fails or comes back empty.
async fn forward_warnings(
    conn: &mut Conn,
    sink: &ResultSink,
    cancel: &CancellationToken,
    count: u32,
) {
    let rows: Vec<(String, u32, String)> = conn
        .query("SHOW WARNINGS LIMIT 50")
        .await
        .unwrap_or_default();
    if rows.is_empty() {
        let n = ServerNotice {
            level: NoticeLevel::Warning,
            message: format!("{count} warning(s)"),
        };
        let _ = send(sink, cancel, SinkEvent::Notice(n)).await;
        return;
    }
    for (level, code, message) in rows {
        let level = if level.eq_ignore_ascii_case("note") {
            NoticeLevel::Info
        } else {
            NoticeLevel::Warning
        };
        let n = ServerNotice {
            level,
            message: format!("{message} ({code})"),
        };
        if !send(sink, cancel, SinkEvent::Notice(n)).await {
            break;
        }
    }
}

impl Connection for MySqlConnection {
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
            let Request::Text { text } = req;
            let killer = self.killer.clone();
            let r = {
                let fut = run(&mut self.conn, &text, &sink, &cancel, &killer);
                tokio::pin!(fut);
                tokio::select! {
                    r = &mut fut => r,
                    _ = cancel.cancelled() => {
                        let _ = killer.kill().await;
                        // Let the statement stop so the connection is idle
                        // (and usable) when we return.
                        match fut.await {
                            Err(Error::ConnectionLost) => Err(Error::ConnectionLost),
                            _ => Err(Error::Cancelled),
                        }
                    }
                }
            };
            self.note(&r);
            r
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(self.killer.clone())
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let sql = self.product().read_only_sql(read_only);
            self.simple(sql).await
        })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let sql = format!("USE {}", quote_ident(schema, '`'));
            self.simple(&sql).await
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            let sql = "select database()";
            let r = self
                .conn
                .query_first::<Option<String>, _>(sql)
                .await
                .map(Option::flatten)
                .map_err(|e| query_error(e, sql));
            self.note(&r);
            r
        })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.simple("START TRANSACTION"))
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.simple("COMMIT"))
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.simple("ROLLBACK"))
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let r = self.conn.ping().await.map_err(|e| query_error(e, ""));
            self.note(&r);
            r
        })
    }

    fn is_alive(&self) -> bool {
        self.alive
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
