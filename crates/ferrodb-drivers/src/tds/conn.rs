//! One SQL Server session over `tiberius`: login (SQL, Windows, Entra ID),
//! batches streamed token by token, attention-based cancellation, plans.

use std::{sync::Arc, time::Duration};

use ferrodb_api::{Error, NoticeLevel, QueryError, Result, ServerNotice};
use ferrodb_net::TlsMode;
use futures::TryStreamExt as _;
use tiberius::{AuthMethod as TdsAuth, Client, Config, DoneStatus, EncryptionLevel, ReceivedToken};
use tokio::{net::TcpStream, sync::watch};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt as _};

use super::{DIALECT, Flavor, batch, bracket, decode, nliteral};
use crate::{
    BoxFuture, CancellationToken, Connection, Endpoint, ExecSummary, RemoteCancel, Request,
    ResolvedConfig, ResultSink, SinkEvent,
    columns::{BatchBuilder, schema},
};

type Tds = Client<Compat<TcpStream>>;

/// Column of the plan result sets of `SET SHOWPLAN_XML` / `STATISTICS XML`.
pub(crate) const SHOWPLAN_COLUMN: &str = "Microsoft SQL Server 2005 XML Showplan";

/// Maps a tiberius error; `text` / `offset` place a server error's line.
pub(crate) fn map_err(e: tiberius::error::Error, text: &str, offset: usize) -> Error {
    use tiberius::error::Error as E;
    match e {
        E::Server(t) => Error::Query(server_error(
            t.code(),
            t.message(),
            t.procedure(),
            t.line(),
            text,
            offset,
        )),
        E::Io { message, .. } => {
            tracing::debug!("TDS I/O error: {message}");
            Error::ConnectionLost
        }
        other => Error::Other(other.to_string()),
    }
}

/// Byte range of line `line` (1-based) of `text`, shifted by `offset`.
pub(crate) fn line_range(text: &str, line: u32, offset: usize) -> Option<std::ops::Range<usize>> {
    if line == 0 {
        return None;
    }
    let mut start = 0;
    for _ in 1..line {
        start += text[start..].find('\n')? + 1;
    }
    let end = text[start..]
        .find('\n')
        .map(|i| start + i)
        .unwrap_or(text.len());
    let lead = text[start..end].len() - text[start..end].trim_start().len();
    Some(offset + start + lead..offset + end.max(start + lead))
}

fn server_error(
    code: u32,
    message: &str,
    procedure: &str,
    line: u32,
    text: &str,
    offset: usize,
) -> QueryError {
    let mut m = message.to_owned();
    if !procedure.is_empty() {
        m.push_str(&format!("\n(procedure {procedure}, line {line})"));
    }
    QueryError {
        message: m,
        code: Some(code.to_string()),
        // A line inside a procedure is not a line of the script.
        range: if procedure.is_empty() {
            line_range(text, line, offset)
        } else {
            None
        },
    }
}

/// `9.0`-style product names from `SERVERPROPERTY`.
pub(crate) fn product_name(engine_edition: i64) -> &'static str {
    match engine_edition {
        5 => "Azure SQL Database",
        6 | 11 => "Azure Synapse Analytics",
        8 => "Azure SQL Managed Instance",
        9 => "Azure SQL Edge",
        _ => "Microsoft SQL Server",
    }
}

fn tls_config(cfg: &ResolvedConfig, flavor: &Flavor, c: &mut Config) {
    let trust = cfg
        .prop("trust_server_certificate")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(!flavor.azure && cfg.tls != TlsMode::VerifyFull);
    match cfg.tls {
        // Azure always encrypts; asking for no encryption fails there.
        TlsMode::Disable if !flavor.azure => c.encryption(EncryptionLevel::NotSupported),
        TlsMode::Disable | TlsMode::Prefer | TlsMode::Require => {
            c.encryption(EncryptionLevel::On);
            if trust || cfg.tls != TlsMode::VerifyFull && !flavor.azure {
                c.trust_cert();
            }
        }
        TlsMode::VerifyFull => {
            c.encryption(EncryptionLevel::Required);
            if let Some(ca) = &cfg.tls_ca {
                c.trust_cert_ca(ca.to_string_lossy());
            } else if trust {
                c.trust_cert();
            }
        }
    }
}

async fn auth(cfg: &ResolvedConfig, flavor: &Flavor) -> Result<TdsAuth> {
    match cfg.prop("auth") {
        Some("EntraId") => Ok(TdsAuth::aad_token(super::entra::token(cfg).await?)),
        Some("Kerberos") => windows_auth(cfg),
        _ => {
            let user = cfg.user().unwrap_or(flavor.default_user);
            if user.is_empty() {
                return Err(Error::Connection("a user name is required".into()));
            }
            Ok(TdsAuth::sql_server(
                user,
                cfg.password().unwrap_or_default(),
            ))
        }
    }
}

/// Windows authentication through SSPI: the current Windows user
/// (Kerberos when the server has an SPN, NTLM otherwise), or NTLM with an
/// explicit `DOMAIN\user` and password.
#[cfg(windows)]
fn windows_auth(cfg: &ResolvedConfig) -> Result<TdsAuth> {
    let account = cfg.prop("principal").or(cfg.user());
    Ok(match (account, cfg.password()) {
        (Some(u), Some(p)) => TdsAuth::windows(u, p),
        _ => TdsAuth::Integrated,
    })
}

/// Kerberos on Linux / macOS needs GSSAPI (a C library, `libgssapi`),
/// which this build does not link.
#[cfg(not(windows))]
fn windows_auth(_cfg: &ResolvedConfig) -> Result<TdsAuth> {
    Err(Error::Unsupported(
        "Windows / Kerberos authentication is available in the Windows build only".into(),
    ))
}

async fn open(config: Config, endpoint: &Endpoint, timeout: Duration) -> Result<Tds> {
    let target = format!("{}:{}", endpoint.server_name, endpoint.port);
    let mut config = config;
    // Azure SQL may redirect the login to the node holding the database.
    for _ in 0..3 {
        let addr = (config.get_addr()).clone();
        let tcp = tokio::time::timeout(timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| Error::Connection(format!("{target}: timed out")))?
            .map_err(|e| Error::Connection(format!("{target}: {e}")))?;
        let _ = tcp.set_nodelay(true);
        let r = tokio::time::timeout(timeout, Client::connect(config.clone(), tcp.compat_write()))
            .await
            .map_err(|_| Error::Connection(format!("{target}: login timed out")))?;
        match r {
            Ok(c) => return Ok(c),
            Err(tiberius::error::Error::Routing { host, port }) if !endpoint.tunnelled() => {
                config.host(&host);
                config.port(port);
            }
            Err(tiberius::error::Error::Server(e)) => {
                return Err(Error::Connection(format!(
                    "{target}: {} (error {})",
                    e.message(),
                    e.code()
                )));
            }
            Err(e) => return Err(Error::Connection(format!("{target}: {e}"))),
        }
    }
    Err(Error::Connection(format!("{target}: too many redirects")))
}

pub(crate) async fn connect(
    flavor: &'static Flavor,
    cfg: &ResolvedConfig,
) -> Result<Box<dyn Connection>> {
    let endpoint = cfg.endpoint(flavor.default_port).await?;
    let mut config = Config::new();
    config.host(&endpoint.host);
    config.port(endpoint.port);
    if let Some(db) = cfg.database().or(flavor.default_database) {
        config.database(db);
    }
    config.application_name(&cfg.application_name);
    config.readonly(cfg.read_only);
    tls_config(cfg, flavor, &mut config);
    config.authentication(auth(cfg, flavor).await?);
    let mut client = open(config, &endpoint, cfg.connect_timeout).await?;
    // The options SSMS and ODBC set: needed for indexed views, computed
    // column indexes and filtered indexes.
    let setup = "SET ANSI_NULLS ON; SET ANSI_PADDING ON; SET ANSI_WARNINGS ON; \
                 SET ARITHABORT ON; SET CONCAT_NULL_YIELDS_NULL ON; SET QUOTED_IDENTIFIER ON; \
                 SET NUMERIC_ROUNDABORT OFF; SET TEXTSIZE 2147483647; \
                 select cast(serverproperty('ProductVersion') as nvarchar(128)), \
                 cast(serverproperty('EngineEdition') as int), @@spid";
    let row = client
        .simple_query(setup)
        .await
        .map_err(|e| map_err(e, setup, 0))?
        .into_row()
        .await
        .map_err(|e| map_err(e, setup, 0))?;
    let (version, spid) = match row {
        Some(r) => {
            let v: Option<&str> = r.get(0);
            let edition: Option<i32> = r.get(1);
            let spid: Option<i16> = r.get(2);
            (
                format!(
                    "{} {}",
                    product_name(edition.unwrap_or(0) as i64),
                    v.unwrap_or_default()
                ),
                spid.unwrap_or(0),
            )
        }
        None => ("Microsoft SQL Server".into(), 0),
    };
    let (tx, rx) = watch::channel(0u64);
    Ok(Box::new(TdsConnection {
        client,
        version,
        spid,
        schema: None,
        read_only: cfg.read_only,
        last_batch: None,
        cancel: Arc::new(TdsCancel(tx)),
        cancel_rx: rx,
        alive: true,
        _endpoint: endpoint,
    }))
}

/// Remote cancellation: bumps a counter the running batch watches; the
/// batch then sends a TDS attention on its own connection.
pub(crate) struct TdsCancel(watch::Sender<u64>);

impl RemoteCancel for TdsCancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        self.0.send_modify(|n| *n += 1);
        Box::pin(async { Ok(()) })
    }
}

pub(crate) struct TdsConnection {
    client: Tds,
    version: String,
    #[allow(dead_code)] // shown in diagnostics; `KILL` target
    spid: i16,
    /// Schema chosen with `set_schema` (no server-side equivalent).
    schema: Option<String>,
    read_only: bool,
    /// The last batch run, repeated by a lone `GO n`.
    last_batch: Option<String>,
    cancel: Arc<TdsCancel>,
    cancel_rx: watch::Receiver<u64>,
    alive: bool,
    /// Keeps an SSH tunnel open as long as the session.
    _endpoint: Endpoint,
}

/// What one batch produced.
#[derive(Default)]
struct Outcome {
    affected: Option<u64>,
    results: usize,
    truncated: bool,
    error: Option<QueryError>,
}

/// Which result sets reach the sink.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    /// Only `STATISTICS XML` plans (the statement's own results are
    /// discarded).
    Plans,
}

/// Reads every token of `text` (one batch) into `sink`.
async fn process(
    client: &mut Tds,
    text: &str,
    offset: usize,
    sink: &ResultSink,
    filter: Filter,
) -> std::result::Result<Outcome, Error> {
    let mut out = Outcome::default();
    let mut stream = client
        .batch_tokens(text)
        .await
        .map_err(|e| map_err(e, text, offset))?;
    // The open result set: its builder and decoding plans (None = skipped).
    let mut current: Option<(BatchBuilder, Vec<decode::Plan>)> = None;
    let mut in_result = false;
    let mut scratch = String::new();
    macro_rules! close {
        () => {
            if let Some((mut b, _)) = current.take() {
                if let Some(batch) = b.flush()
                    && sink.send(SinkEvent::Batch(batch)).await.is_err()
                {
                    out.truncated = true;
                    return Ok(out);
                }
                if b.lossy() > 0 {
                    sink.notice(
                        NoticeLevel::Warning,
                        format!(
                            "{} value(s) could not be represented and are shown as NULL",
                            b.lossy()
                        ),
                    )
                    .await;
                }
                let _ = sink.send(SinkEvent::End).await;
            }
        };
    }
    loop {
        let tok = match stream.try_next().await {
            Ok(Some(t)) => t,
            Ok(None) => break,
            // The error token was already seen; anything else is fatal.
            Err(tiberius::error::Error::Server(_)) if out.error.is_some() => break,
            Err(e) => return Err(map_err(e, text, offset)),
        };
        match tok {
            ReceivedToken::NewResultset(meta) => {
                close!();
                in_result = true;
                let plan_set = meta
                    .columns
                    .first()
                    .is_some_and(|c| c.col_name == SHOWPLAN_COLUMN);
                if filter == Filter::Plans && !plan_set {
                    continue;
                }
                let (specs, plans): (Vec<_>, Vec<_>) =
                    meta.columns.iter().map(decode::column).unzip();
                let s = schema(&specs);
                out.results += 1;
                if sink
                    .send(SinkEvent::Begin { schema: s.clone() })
                    .await
                    .is_err()
                {
                    out.truncated = true;
                    return Ok(out);
                }
                current = Some((BatchBuilder::new(s, sink.batch_rows()), plans));
            }
            ReceivedToken::Row(row) => {
                if let Some((b, plans)) = current.as_mut() {
                    for (i, v) in row.iter().enumerate() {
                        let plan = plans.get(i).copied().unwrap_or(decode::Plan::Plain);
                        let c = decode::cell(plan, v, &mut scratch);
                        b.push(i, c);
                    }
                    b.end_row();
                    if b.should_flush()
                        && let Some(batch) = b.flush()
                        && sink.send(SinkEvent::Batch(batch)).await.is_err()
                    {
                        out.truncated = true;
                        return Ok(out);
                    }
                }
            }
            ReceivedToken::Done(d) | ReceivedToken::DoneInProc(d) | ReceivedToken::DoneProc(d) => {
                let counted = d.status().contains(DoneStatus::Count);
                if in_result {
                    // The count of a SELECT is its rows, not rows affected.
                    in_result = false;
                    close!();
                } else if counted && !d.status().contains(DoneStatus::Error) {
                    *out.affected.get_or_insert(0) += d.rows();
                }
            }
            ReceivedToken::Info(i) => {
                let level = if i.number() == 0 || i.class() == 0 {
                    NoticeLevel::Info
                } else {
                    NoticeLevel::Warning
                };
                let _ = sink
                    .send(SinkEvent::Notice(ServerNotice {
                        level,
                        message: i.message().to_owned(),
                    }))
                    .await;
            }
            ReceivedToken::Error(e) => {
                close!();
                in_result = false;
                let q = server_error(e.code(), e.message(), e.procedure(), e.line(), text, offset);
                match &mut out.error {
                    None => out.error = Some(q),
                    // Several errors (e.g. 2627 then 3621): keep them all.
                    Some(first) => {
                        first.message.push('\n');
                        first.message.push_str(&q.message);
                    }
                }
            }
            _ => {}
        }
    }
    close!();
    Ok(out)
}

/// A lone `GO` / `GO n` chunk (the console splitter sends them alone).
fn lone_go(text: &str) -> Option<u32> {
    let t = text.trim().trim_end_matches(';').trim();
    let rest = t.get(..2).filter(|g| g.eq_ignore_ascii_case("go"))?;
    let _ = rest;
    let tail = t[2..].trim();
    if tail.is_empty() {
        Some(1)
    } else if t[2..].starts_with(char::is_whitespace) {
        tail.parse().ok()
    } else {
        None
    }
}

impl TdsConnection {
    /// Runs one batch, cancellable by `cancel` or the remote handle.
    async fn run_batch(
        &mut self,
        text: &str,
        offset: usize,
        sink: &ResultSink,
        cancel: &CancellationToken,
        filter: Filter,
    ) -> Result<Outcome> {
        let mut remote = self.cancel_rx.clone();
        remote.borrow_and_update();
        let r = {
            let job = process(&mut self.client, text, offset, sink, filter);
            tokio::select! {
                r = job => Some(r),
                _ = cancel.cancelled() => None,
                _ = remote.changed() => None,
            }
        };
        match r {
            Some(Ok(o)) => {
                if o.truncated {
                    // The consumer went away: stop the server, drain.
                    self.attention().await?;
                }
                Ok(o)
            }
            Some(Err(e)) => {
                if matches!(e, Error::ConnectionLost) {
                    self.alive = false;
                }
                Err(e)
            }
            None => {
                self.attention().await?;
                Err(Error::Cancelled)
            }
        }
    }

    async fn attention(&mut self) -> Result<()> {
        match tokio::time::timeout(Duration::from_secs(10), self.client.attention()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                self.alive = false;
                Err(map_err(e, "", 0))
            }
            Err(_) => {
                self.alive = false;
                Err(Error::ConnectionLost)
            }
        }
    }

    /// Runs `sql` (internal statements) and returns the first column of the
    /// first row.
    async fn scalar(&mut self, sql: &str) -> Result<Option<String>> {
        let rows = self
            .client
            .simple_query(sql)
            .await
            .map_err(|e| map_err(e, sql, 0))?
            .into_first_result()
            .await
            .map_err(|e| map_err(e, sql, 0))?;
        Ok(rows
            .first()
            .and_then(|r| r.try_get::<&str, _>(0).ok().flatten())
            .map(str::to_owned))
    }

    async fn simple(&mut self, sql: &str) -> Result<()> {
        // Internal statements produce a handful of events at most.
        let (sink, _rx) = ResultSink::channel(16, ResultSink::BATCH_ROWS);
        let o = self
            .run_batch(sql, 0, &sink, &CancellationToken::new(), Filter::All)
            .await?;
        match o.error {
            Some(q) => Err(Error::Query(q)),
            None => Ok(()),
        }
    }

    /// Runs `text` split on `GO`, accumulating the outcome.
    async fn run_script(
        &mut self,
        text: &str,
        sink: &ResultSink,
        cancel: &CancellationToken,
        filter: Filter,
    ) -> Result<ExecSummary> {
        let mut summary = ExecSummary::default();
        let batches: Vec<(String, usize, u32)> = match lone_go(text) {
            // `GO n` alone repeats the previous batch n - 1 more times.
            Some(n) => match &self.last_batch {
                Some(b) if n > 1 => vec![(b.clone(), 0, n - 1)],
                _ => return Ok(summary),
            },
            None => batch::split(text)
                .into_iter()
                .map(|b| (b.text.to_owned(), b.offset, b.repeat))
                .collect(),
        };
        let script = text;
        for (b, offset, repeat) in batches {
            for _ in 0..repeat {
                let o = self.run_batch(&b, offset, sink, cancel, filter).await?;
                summary.results += o.results;
                if let Some(a) = o.affected {
                    *summary.affected.get_or_insert(0) += a;
                }
                if o.truncated {
                    summary.truncated = true;
                    return Ok(summary);
                }
                if let Some(mut q) = o.error {
                    // Ranges were placed with the batch's offset in the script.
                    q.range = q
                        .range
                        .filter(|r| r.end <= script.len() && r.start <= r.end);
                    self.last_batch = Some(b);
                    return Err(Error::Query(q));
                }
            }
            self.last_batch = Some(b);
        }
        Ok(summary)
    }

    /// The XML plan of `statement`: estimated (`SHOWPLAN_XML`, nothing
    /// runs) or actual (`STATISTICS XML`, the statement runs and its own
    /// results are discarded).
    async fn explain(
        &mut self,
        statement: &str,
        analyze: bool,
        sink: &ResultSink,
        cancel: &CancellationToken,
    ) -> Result<ExecSummary> {
        let (on, off) = if analyze {
            ("SET STATISTICS XML ON", "SET STATISTICS XML OFF")
        } else {
            ("SET SHOWPLAN_XML ON", "SET SHOWPLAN_XML OFF")
        };
        self.simple(on).await?;
        let filter = if analyze { Filter::Plans } else { Filter::All };
        let r = self.run_script(statement, sink, cancel, filter).await;
        let off_r = self.simple(off).await;
        let s = r?;
        off_r?;
        Ok(s)
    }
}

impl Connection for TdsConnection {
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
            if let Request::Native { method, .. } = &req {
                return match method.as_str() {
                    "explain" => {
                        let stmt = req.arg("statement").unwrap_or_default().to_owned();
                        let analyze = req.arg("analyze") == Some("true");
                        self.explain(&stmt, analyze, &sink, &cancel).await
                    }
                    other => Err(Error::Unsupported(format!("native call `{other}`"))),
                };
            }
            let text = req.into_text()?;
            if self.read_only
                && !ferrodb_lang::sql::analyze::classify(&text, DIALECT).allowed_read_only()
            {
                return Err(Error::ReadOnly);
            }
            self.run_script(&text, &sink, &cancel, Filter::All).await
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(self.cancel.clone())
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.read_only = read_only;
            Ok(())
        })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let sql = format!(
                "select case when schema_id({0}) is not null then N's' \
                 when db_id({0}) is not null then N'd' end",
                nliteral(schema)
            );
            match self.scalar(&sql).await?.as_deref() {
                Some("s") => {
                    self.schema = Some(schema.to_owned());
                    Ok(())
                }
                Some(_) => {
                    self.simple(&format!("USE {}", bracket(schema))).await?;
                    self.schema = None;
                    Ok(())
                }
                None => Err(Error::NotFound(format!("schema or database `{schema}`"))),
            }
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            if let Some(s) = &self.schema {
                return Ok(Some(s.clone()));
            }
            self.scalar("select schema_name()").await
        })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.simple("BEGIN TRANSACTION").await })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.simple("IF @@TRANCOUNT > 0 COMMIT TRANSACTION").await })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.simple("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.simple("select 1").await })
    }

    fn is_alive(&self) -> bool {
        self.alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(lone_go("GO"), Some(1));
        assert_eq!(lone_go(" go 3 "), Some(3));
        assert_eq!(lone_go("GOTO x"), None);
        assert_eq!(lone_go("select 1"), None);
        let text = "select 1\n  select * from nope\nselect 3";
        let r = line_range(text, 2, 10).unwrap();
        assert_eq!(&text[r.start - 10..r.end - 10], "select * from nope");
        assert_eq!(line_range(text, 9, 0), None);
        assert_eq!(product_name(5), "Azure SQL Database");
        assert_eq!(product_name(3), "Microsoft SQL Server");
    }
}
