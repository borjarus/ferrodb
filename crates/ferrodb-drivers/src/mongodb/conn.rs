//! Connecting: connection string from the form (or the given URI), SSH
//! tunnel, topology detection; the [`Connection`] implementation.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bson::{Document, doc};
use ferrodb_api::{Error, QueryError, Result};
use ferrodb_lang::mql;
use ferrodb_net::{SshTunnel, TlsMode};
use mongodb::{
    Client, ClientSession,
    error::{ErrorKind, WriteFailure},
    options::{ClientOptions, ServerAddress},
};

use super::{DEFAULT_PORT, exec};
use crate::{
    BoxFuture, CancellationToken, Connection, ExecSummary, RemoteCancel, Request, ResolvedConfig,
    ResultSink,
};

static NEXT_TAG: AtomicU64 = AtomicU64::new(1);

pub struct MongoConnection {
    pub(crate) client: Client,
    /// Current database (`use x`).
    pub(crate) db: String,
    /// Open while a transaction runs.
    pub(crate) session: Option<ClientSession>,
    /// Replica set or sharded cluster.
    supports_tx: bool,
    version: String,
    pub(crate) alive: bool,
    /// `comment` of the running statement (server-side cancel).
    tag: Arc<Mutex<Option<String>>>,
    _tunnel: Option<SshTunnel>,
}

/// Percent-encodes a URI component.
fn enc(s: &str) -> String {
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

fn has_param(uri: &str, key: &str) -> bool {
    uri.split_once('?').is_some_and(|(_, q)| {
        q.split('&').any(|kv| {
            kv.split('=')
                .next()
                .is_some_and(|k| k.eq_ignore_ascii_case(key))
        })
    })
}

/// The connection string for a configuration.
pub fn connection_string(cfg: &ResolvedConfig) -> Result<String> {
    let user = cfg.user();
    let password = cfg.password();
    let creds = match (user, password) {
        (Some(u), Some(p)) => format!("{}:{}@", enc(u), enc(p)),
        (Some(u), None) => format!("{}@", enc(u)),
        _ => String::new(),
    };
    let mut uri = match cfg.prop("uri") {
        Some(u) => {
            let u = u.trim().to_owned();
            if !(u.starts_with("mongodb://") || u.starts_with("mongodb+srv://")) {
                return Err(Error::Connection(
                    "the connection string must start with mongodb:// or mongodb+srv://".into(),
                ));
            }
            let (scheme, rest) = u.split_once("://").unwrap_or(("mongodb", &u));
            let authority = rest.split(['/', '?']).next().unwrap_or_default();
            if !creds.is_empty() && !authority.contains('@') {
                format!("{scheme}://{creds}{rest}")
            } else {
                u
            }
        }
        None => {
            let host = cfg.host();
            let port = cfg.port(DEFAULT_PORT)?;
            // `a:1,b:2` (replica set seed list) and `host:port` stay as typed.
            let hostport = if host.contains(',')
                || host
                    .rsplit_once(':')
                    .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
            {
                host.to_owned()
            } else {
                format!("{host}:{port}")
            };
            format!("mongodb://{creds}{hostport}/")
        }
    };
    if !uri.contains('?') {
        if !uri[uri.find("://").map_or(0, |i| i + 3)..].contains('/') {
            uri.push('/');
        }
        uri.push('?');
    }
    let mut params: Vec<(String, String)> = Vec::new();
    let mut add = |k: &str, v: &str| params.push((k.to_owned(), v.to_owned()));
    if let Some(v) = cfg.prop("auth_source") {
        add("authSource", v);
    }
    let cert = cfg.prop("ssl.cert");
    match cfg.prop("auth_mechanism") {
        Some(m) => add("authMechanism", m),
        None if cert.is_some() && user.is_none() => add("authMechanism", "MONGODB-X509"),
        None => {}
    }
    if let Some(v) = cfg.prop("replica_set") {
        add("replicaSet", v);
    }
    if let Some(v) = cfg.prop("read_preference") {
        add("readPreference", v);
    }
    match cfg.tls {
        TlsMode::Disable => add("tls", "false"),
        TlsMode::Prefer => {
            if cert.is_some() || cfg.tls_ca.is_some() {
                add("tls", "true");
            }
        }
        TlsMode::Require => {
            add("tls", "true");
            add("tlsAllowInvalidCertificates", "true");
        }
        TlsMode::VerifyFull => add("tls", "true"),
    }
    if let Some(ca) = &cfg.tls_ca {
        add("tlsCAFile", &ca.to_string_lossy());
    }
    if let Some(c) = cert {
        add("tlsCertificateKeyFile", c);
    }
    add("appName", &cfg.application_name);
    add(
        "serverSelectionTimeoutMS",
        &cfg.connect_timeout.as_millis().to_string(),
    );
    add(
        "connectTimeoutMS",
        &cfg.connect_timeout.as_millis().to_string(),
    );
    for (k, v) in params {
        if has_param(&uri, &k) {
            continue;
        }
        if !uri.ends_with('?') && !uri.ends_with('&') {
            uri.push('&');
        }
        uri.push_str(&format!("{k}={}", enc(&v)));
    }
    Ok(uri.trim_end_matches(['?', '&']).to_owned())
}

/// Driver error → contract error. Network failures lose the connection.
pub(crate) fn map_err(e: mongodb::error::Error) -> Error {
    let q = |message: String, code: Option<String>| {
        Error::Query(QueryError {
            message,
            code,
            range: None,
        })
    };
    match *e.kind {
        ErrorKind::Command(c) => q(c.message, Some(format!("{} {}", c.code, c.code_name))),
        ErrorKind::Write(WriteFailure::WriteError(w)) => q(
            w.message,
            Some(match w.code_name {
                Some(n) => format!("{} {n}", w.code),
                None => w.code.to_string(),
            }),
        ),
        ErrorKind::Write(WriteFailure::WriteConcernError(w)) => {
            q(w.message, Some(format!("{} {}", w.code, w.code_name)))
        }
        ErrorKind::Io(_) | ErrorKind::ConnectionPoolCleared { .. } => Error::ConnectionLost,
        ErrorKind::ServerSelection { message, .. } => Error::Connection(message),
        ErrorKind::Authentication { message, .. } => {
            Error::Connection(format!("authentication failed: {message}"))
        }
        ErrorKind::DnsResolve { message, .. } => Error::Connection(message),
        ErrorKind::InvalidArgument { message, .. } => q(message, None),
        other => q(format!("{other}"), None),
    }
}

impl MongoConnection {
    pub async fn connect(cfg: &ResolvedConfig) -> Result<Self> {
        let uri = connection_string(cfg)?;
        let mut opts = ClientOptions::parse(&uri)
            .await
            .map_err(|e| Error::Connection(map_err(e).to_string()))?;
        let mut tunnel = None;
        if let Some(ssh) = &cfg.ssh {
            if uri.starts_with("mongodb+srv://") || opts.hosts.len() > 1 {
                return Err(Error::Connection(
                    "SSH tunnels reach one MongoDB host: give a single host (direct connection)"
                        .into(),
                ));
            }
            let (host, port) = match opts.hosts.first() {
                Some(ServerAddress::Tcp { host, port }) => {
                    (host.clone(), port.unwrap_or(DEFAULT_PORT))
                }
                _ => (cfg.host().to_owned(), cfg.port(DEFAULT_PORT)?),
            };
            let t = SshTunnel::open(ssh, &host, port)
                .await
                .map_err(|e| Error::Connection(e.to_string()))?;
            opts.hosts = vec![
                ServerAddress::parse(t.local_addr().to_string())
                    .map_err(|e| Error::Connection(e.to_string()))?,
            ];
            opts.direct_connection = Some(true);
            tunnel = Some(t);
        }
        let db = cfg
            .database()
            .map(str::to_owned)
            .or_else(|| opts.default_database.clone())
            .unwrap_or_else(|| "test".into());
        let client =
            Client::with_options(opts).map_err(|e| Error::Connection(map_err(e).to_string()))?;
        let admin = client.database("admin");
        let hello = tokio::time::timeout(
            cfg.connect_timeout + Duration::from_secs(1),
            admin.run_command(doc! { "hello": 1 }),
        )
        .await
        .map_err(|_| Error::Connection("timed out connecting to MongoDB".into()))?
        .map_err(|e| match map_err(e) {
            Error::Query(q) => Error::Connection(q.message),
            other => other,
        })?;
        let supports_tx =
            hello.contains_key("setName") || hello.get_str("msg").is_ok_and(|m| m == "isdbgrid");
        let version = admin
            .run_command(doc! { "buildInfo": 1 })
            .await
            .ok()
            .and_then(|b| b.get_str("version").ok().map(str::to_owned))
            .unwrap_or_default();
        Ok(Self {
            client,
            db,
            session: None,
            supports_tx,
            version,
            alive: true,
            tag: Arc::default(),
            _tunnel: tunnel,
        })
    }

    /// Runs a command, in the open transaction if there is one.
    pub(crate) async fn command(&mut self, db: &str, cmd: Document) -> Result<Document> {
        let d = self.client.database(db);
        let r = match self.session.as_mut() {
            Some(s) => d.run_command(cmd).session(s).await,
            None => d.run_command(cmd).await,
        };
        self.check(r)
    }

    pub(crate) fn check<T>(&mut self, r: mongodb::error::Result<T>) -> Result<T> {
        r.map_err(|e| {
            let e = map_err(e);
            if e == Error::ConnectionLost {
                self.alive = false;
            }
            e
        })
    }
}

struct MongoCancel {
    client: Client,
    tag: Arc<Mutex<Option<String>>>,
}

/// Kills the server operations tagged with `tag` (`currentOp` + `killOp`).
async fn kill_tagged(client: &Client, tag: &str) -> Result<()> {
    let admin = client.database("admin");
    let ops = admin
        .run_command(doc! { "currentOp": 1, "command.comment": tag })
        .await
        .map_err(map_err)?;
    for op in ops
        .get_array("inprog")
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if let Some(id) = op.as_document().and_then(|d| d.get("opid")) {
            let _ = admin
                .run_command(doc! { "killOp": 1, "op": id.clone() })
                .await;
        }
    }
    Ok(())
}

impl RemoteCancel for MongoCancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        let client = self.client.clone();
        let tag = self.tag.lock().map(|t| t.clone()).unwrap_or(None);
        Box::pin(async move {
            match tag {
                Some(t) => kill_tagged(&client, &t).await,
                None => Ok(()),
            }
        })
    }
}

impl Connection for MongoConnection {
    fn server_version(&self) -> String {
        format!("MongoDB {}", self.version).trim().to_owned()
    }

    fn execute<'a>(
        &'a mut self,
        req: Request,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ExecSummary>> {
        Box::pin(async move {
            let cmd = match req {
                Request::Mql(c) => c,
                Request::Text { text } => mql::parse(&text).map_err(|e| {
                    Error::Query(QueryError {
                        message: e.message,
                        code: Some("FERRODB-MQL".into()),
                        range: Some(e.range),
                    })
                })?,
            };
            let tag = format!("ferrodb-{}", NEXT_TAG.fetch_add(1, Ordering::Relaxed));
            if let Ok(mut t) = self.tag.lock() {
                *t = Some(tag.clone());
            }
            let client = self.client.clone();
            let r = {
                let fut = exec::run(self, cmd, &sink, &tag);
                tokio::pin!(fut);
                tokio::select! {
                    r = &mut fut => r,
                    _ = cancel.cancelled() => Err(Error::Cancelled),
                }
            };
            if let Ok(mut t) = self.tag.lock() {
                *t = None;
            }
            if r == Err(Error::Cancelled) {
                // The cursor was dropped; stop what still runs on the server.
                let _ =
                    tokio::time::timeout(Duration::from_secs(2), kill_tagged(&client, &tag)).await;
            }
            r
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        Some(Arc::new(MongoCancel {
            client: self.client.clone(),
            tag: self.tag.clone(),
        }))
    }

    fn set_read_only(&mut self, _read_only: bool) -> BoxFuture<'_, Result<()>> {
        // MongoDB has no read-only sessions: the core refuses writes.
        Box::pin(async { Ok(()) })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.db = schema.to_owned();
            Ok(())
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move { Ok(Some(self.db.clone())) })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if !self.supports_tx {
                return Err(Error::Unsupported(
                    "transactions need a replica set or a sharded cluster".into(),
                ));
            }
            if self.session.is_some() {
                return Ok(());
            }
            let r = self.client.start_session().await;
            let mut s = self.check(r)?;
            let r = s.start_transaction().await;
            self.check(r)?;
            self.session = Some(s);
            Ok(())
        })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            match self.session.take() {
                Some(mut s) => {
                    let r = s.commit_transaction().await;
                    self.check(r)
                }
                None => Ok(()),
            }
        })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            match self.session.take() {
                Some(mut s) => {
                    let r = s.abort_transaction().await;
                    self.check(r)
                }
                None => Ok(()),
            }
        })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.command("admin", doc! { "ping": 1 }).await.map(|_| ()) })
    }

    fn is_alive(&self) -> bool {
        self.alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_strings() {
        let cfg = ResolvedConfig::new("mongodb")
            .with("host", "db.example")
            .with("port", "27018")
            .with("user", "app user")
            .with("auth_source", "admin")
            .with("replica_set", "rs0")
            .with_secret("password", "p@ss:/w");
        let uri = connection_string(&cfg).unwrap();
        assert!(
            uri.starts_with("mongodb://app%20user:p%40ss%3A%2Fw@db.example:27018/?authSource=admin&replicaSet=rs0&appName=FerroDb"),
            "{uri}"
        );
        let cfg = ResolvedConfig::new("mongodb")
            .with(
                "uri",
                "mongodb+srv://cluster0.example.net/shop?retryWrites=true",
            )
            .with("user", "u")
            .with_secret("password", "p");
        let uri = connection_string(&cfg).unwrap();
        assert!(
            uri.starts_with(
                "mongodb+srv://u:p@cluster0.example.net/shop?retryWrites=true&appName=FerroDb"
            ),
            "{uri}"
        );
        let cfg = ResolvedConfig::new("mongodb").with("ssl.cert", "/c/client.pem");
        let uri = connection_string(&cfg).unwrap();
        assert!(uri.contains("authMechanism=MONGODB-X509"), "{uri}");
        assert!(uri.contains("tls=true"), "{uri}");
        assert!(
            uri.contains("tlsCertificateKeyFile=%2Fc%2Fclient.pem"),
            "{uri}"
        );
        assert!(
            connection_string(&ResolvedConfig::new("mongodb").with("uri", "http://x")).is_err()
        );
    }
}
