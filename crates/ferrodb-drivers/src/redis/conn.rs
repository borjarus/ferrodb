//! Connecting (standalone, cluster, Sentinel; TLS, ACL, database index,
//! SSH tunnel) and the [`Connection`] implementation.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ferrodb_api::{Error, NoticeLevel, QueryError, Result};
use ferrodb_net::TlsMode;
use redis::{
    AsyncConnectionConfig, Client, Cmd, ConnectionAddr, IntoConnectionInfo, Pipeline,
    ProtocolVersion, RedisConnectionInfo, RedisError, Value,
    aio::{ConnectionLike, MultiplexedConnection},
    cluster_async::ClusterConnection,
    cluster_routing::{RoutingInfo, SingleNodeRoutingInfo},
};

use super::{DEFAULT_PORT, DEFAULT_SENTINEL_PORT, native, reply};
use crate::{
    BoxFuture, CancellationToken, Connection, Endpoint, ExecSummary, RemoteCancel, Request,
    ResolvedConfig, ResultSink, SinkEvent,
    kv::{self, CommandClass},
};

/// How the source is deployed (`mode` property).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Topology {
    Standalone,
    Cluster,
    /// Master discovered through Sentinel (`sentinel.master`).
    Sentinel {
        master: String,
    },
}

/// The client: one multiplexed connection, or the cluster client routing
/// by slot.
#[derive(Clone)]
pub(crate) enum Link {
    Single(MultiplexedConnection),
    Cluster(ClusterConnection),
}

impl Link {
    pub(crate) async fn req(&mut self, cmd: &Cmd) -> redis::RedisResult<Value> {
        match self {
            Self::Single(c) => c.req_packed_command(cmd).await,
            Self::Cluster(c) => c.req_packed_command(cmd).await,
        }
    }

    /// A pipeline; with `node` (cluster) it runs on that master. Errors of
    /// single commands come back as [`Value::ServerError`].
    pub(crate) async fn pipe(
        &mut self,
        p: &Pipeline,
        node: Option<&(String, u16)>,
    ) -> redis::RedisResult<Vec<Value>> {
        let n = p.len();
        match (self, node) {
            (Self::Single(c), _) => c.req_packed_commands(p, 0, n).await,
            (Self::Cluster(c), Some((host, port))) => {
                c.route_pipeline(
                    p.clone(),
                    0,
                    n,
                    SingleNodeRoutingInfo::ByAddress {
                        host: host.clone(),
                        port: *port,
                    },
                )
                .await
            }
            (Self::Cluster(c), None) => c.req_packed_commands(p, 0, n).await,
        }
    }

    /// A command on one cluster node (or the single server).
    pub(crate) async fn on_node(
        &mut self,
        cmd: Cmd,
        node: Option<&(String, u16)>,
    ) -> redis::RedisResult<Value> {
        match (self, node) {
            (Self::Cluster(c), Some((host, port))) => {
                c.route_command(
                    cmd,
                    RoutingInfo::SingleNode(SingleNodeRoutingInfo::ByAddress {
                        host: host.clone(),
                        port: *port,
                    }),
                )
                .await
            }
            (l, _) => l.req(&cmd).await,
        }
    }
}

pub struct RedisConnection {
    pub(crate) link: Link,
    pub(crate) topology: Topology,
    pub(crate) db: i64,
    pub(crate) read_only: bool,
    /// `CLIENT ID` of the connection (standalone / Sentinel): cancelling a
    /// blocking command unblocks it from a second connection.
    client_id: Option<i64>,
    /// Opens the side connection used for `CLIENT UNBLOCK`.
    side: Option<Client>,
    version: String,
    pub(crate) alive: Arc<AtomicBool>,
    /// A `MULTI` is open: commands are queued, not run.
    in_multi: bool,
    /// Cluster: commands queued after `MULTI`, sent on `EXEC` as one atomic
    /// pipeline to the node of their slot (the cluster client routes every
    /// command on its own, so `MULTI` cannot stay open on one node).
    queued: Option<Vec<Vec<String>>>,
    /// Cluster masters (`host`, `port`), for scans across the cluster.
    pub(crate) masters: Vec<(String, u16)>,
    _endpoint: Option<Endpoint>,
    pub(crate) cfg_timeout: Duration,
}

/// Redis error → contract error. I/O failures lose the connection.
pub(crate) fn map_err(e: RedisError) -> Error {
    if e.is_io_error() || e.is_connection_dropped() || e.is_unrecoverable_error() {
        return Error::ConnectionLost;
    }
    if e.is_timeout() {
        return Error::Timeout;
    }
    let code = e.code().map(str::to_owned);
    let message = match (e.code(), e.detail()) {
        (Some(_), Some(d)) => d.to_owned(),
        _ => e.to_string(),
    };
    Error::Query(QueryError {
        message,
        code,
        range: None,
    })
}

/// `host:port` items of a comma-separated list.
pub fn parse_hosts(list: &str, default_port: u16) -> Result<Vec<(String, u16)>> {
    list.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|h| {
            // `[::1]:6379`, `host:6379`, `host`.
            let (host, port) = match h.rsplit_once(':') {
                Some((a, p)) if !a.ends_with(':') && p.parse::<u16>().is_ok() => (
                    a.trim_matches(['[', ']']),
                    p.parse().unwrap_or(default_port),
                ),
                _ => (h.trim_matches(['[', ']']), default_port),
            };
            if host.is_empty() {
                return Err(Error::Connection(format!("invalid host: {h}")));
            }
            Ok((host.to_owned(), port))
        })
        .collect()
}

pub fn topology(cfg: &ResolvedConfig) -> Result<Topology> {
    Ok(
        match cfg.prop("mode").map(str::to_ascii_lowercase).as_deref() {
            None | Some("standalone") => Topology::Standalone,
            Some("cluster") => Topology::Cluster,
            Some("sentinel") => Topology::Sentinel {
                master: cfg
                    .prop("sentinel.master")
                    .ok_or_else(|| {
                        Error::Connection(
                            "Sentinel: the master name (sentinel.master) is required".into(),
                        )
                    })?
                    .to_owned(),
            },
            Some(other) => {
                return Err(Error::Connection(format!(
                    "mode: expected standalone, cluster or sentinel, got {other}"
                )));
            }
        },
    )
}

fn protocol(cfg: &ResolvedConfig) -> ProtocolVersion {
    match cfg.prop("protocol").map(str::to_ascii_lowercase).as_deref() {
        Some("resp2" | "2") => ProtocolVersion::RESP2,
        _ => ProtocolVersion::RESP3,
    }
}

/// TLS on (`ssl.mode` require / verify-full, or prefer with a CA or a
/// client certificate: Redis has no TLS negotiation) and whether
/// certificates are verified.
fn tls(cfg: &ResolvedConfig) -> Option<bool> {
    match cfg.tls {
        TlsMode::Disable => None,
        TlsMode::Prefer if cfg.tls_ca.is_none() && cfg.prop("ssl.cert").is_none() => None,
        TlsMode::Require => Some(false),
        TlsMode::Prefer | TlsMode::VerifyFull => Some(true),
    }
}

fn certificates(cfg: &ResolvedConfig) -> Result<redis::TlsCertificates> {
    let read =
        |p: &str| std::fs::read(p).map_err(|e| Error::Connection(format!("TLS file {p}: {e}")));
    let root_cert = match &cfg.tls_ca {
        Some(p) => Some(read(&p.to_string_lossy())?),
        None => None,
    };
    let client_tls = match cfg.prop("ssl.cert") {
        Some(cert) => {
            let client_cert = read(cert)?;
            let client_key = match cfg.prop("ssl.key") {
                Some(k) => read(k)?,
                None => client_cert.clone(),
            };
            Some(redis::ClientTlsConfig {
                client_cert,
                client_key,
            })
        }
        None => None,
    };
    Ok(redis::TlsCertificates {
        client_tls,
        root_cert,
    })
}

fn settings(cfg: &ResolvedConfig, db: i64) -> RedisConnectionInfo {
    let mut s = RedisConnectionInfo::default()
        .set_db(db)
        .set_protocol(protocol(cfg))
        .set_lib_name("ferrodb", env!("CARGO_PKG_VERSION"));
    if let Some(u) = cfg.user() {
        s = s.set_username(u);
    }
    if let Some(p) = cfg.password() {
        s = s.set_password(p);
    }
    s
}

/// A client for one server (`server_name` = TLS host name).
fn client(
    cfg: &ResolvedConfig,
    host: &str,
    port: u16,
    server_name: &str,
    settings: RedisConnectionInfo,
) -> Result<Client> {
    let conn_err = |e: RedisError| Error::Connection(e.to_string());
    match tls(cfg) {
        None => {
            let info = ConnectionAddr::Tcp(host.to_owned(), port)
                .into_connection_info()
                .map_err(conn_err)?
                .set_redis_settings(settings);
            Client::open(info).map_err(conn_err)
        }
        Some(verify) => {
            // Through an SSH tunnel the certificate names the real server,
            // not 127.0.0.1: verify it only when connecting directly.
            let insecure = !verify || host != server_name;
            let info = ConnectionAddr::TcpTls {
                host: host.to_owned(),
                port,
                insecure,
                tls_params: None,
            }
            .into_connection_info()
            .map_err(conn_err)?
            .set_redis_settings(settings);
            Client::build_with_tls(info, certificates(cfg)?).map_err(conn_err)
        }
    }
}

fn async_config(cfg: &ResolvedConfig) -> AsyncConnectionConfig {
    AsyncConnectionConfig::new()
        .set_connection_timeout(Some(cfg.connect_timeout))
        // Blocking commands (`BLPOP q 0`) wait as long as they need; the
        // core applies the statement timeout.
        .set_response_timeout(None)
}

async fn open_single(client: &Client, cfg: &ResolvedConfig) -> Result<MultiplexedConnection> {
    let ac = async_config(cfg);
    let fut = client.get_multiplexed_async_connection_with_config(&ac);
    match tokio::time::timeout(cfg.connect_timeout + Duration::from_secs(1), fut).await {
        Err(_) => Err(Error::Connection("timed out connecting to Redis".into())),
        Ok(Err(e)) => Err(connect_error(e)),
        Ok(Ok(c)) => Ok(c),
    }
}

fn connect_error(e: RedisError) -> Error {
    match map_err(e.clone()) {
        Error::Query(q) => Error::Connection(q.to_string()),
        Error::ConnectionLost => Error::Connection(e.to_string()),
        other => other,
    }
}

/// Asks the Sentinels for the address of `master`.
async fn discover_master(
    cfg: &ResolvedConfig,
    sentinels: &[(String, u16)],
    master: &str,
) -> Result<(String, u16)> {
    let mut last = String::from("no Sentinel given");
    for (host, port) in sentinels {
        let mut s = RedisConnectionInfo::default().set_protocol(ProtocolVersion::RESP2);
        if let Some(u) = cfg.prop("sentinel.user") {
            s = s.set_username(u);
        }
        if let Some(p) = cfg.secret("sentinel.password") {
            s = s.set_password(p);
        }
        let r = async {
            let c = client(cfg, host, *port, host, s)?;
            let mut conn = open_single(&c, cfg).await?;
            let v = conn
                .req_packed_command(
                    redis::cmd("SENTINEL")
                        .arg("GET-MASTER-ADDR-BY-NAME")
                        .arg(master),
                )
                .await
                .map_err(map_err)?;
            match reply::items(&v) {
                Some([h, p]) => Ok((
                    reply::text(h).unwrap_or_default(),
                    reply::int(p).unwrap_or(DEFAULT_PORT as i64) as u16,
                )),
                _ => Err(Error::Connection(format!(
                    "Sentinel {host}:{port} does not know master {master}"
                ))),
            }
        }
        .await;
        match r {
            Ok(addr) => return Ok(addr),
            Err(e) => last = format!("{host}:{port}: {e}"),
        }
    }
    Err(Error::Connection(format!(
        "Sentinel: master {master} not found ({last})"
    )))
}

/// Parses `CLUSTER NODES` into the master addresses.
pub fn cluster_masters(nodes: &str) -> Vec<(String, u16)> {
    let mut out = Vec::new();
    for line in nodes.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 || !f[2].contains("master") || f[2].contains("fail") {
            continue;
        }
        // `ip:port@cport[,hostname]`
        let addr = f[1].split(['@', ',']).next().unwrap_or_default();
        if let Some((h, p)) = addr.rsplit_once(':')
            && let Ok(p) = p.parse()
            && !h.is_empty()
        {
            out.push((h.to_owned(), p));
        }
    }
    out.sort();
    out
}

/// `redis_version` (or `valkey_version`) of an `INFO server` reply.
pub fn info_field(info: &str, key: &str) -> Option<String> {
    info.lines().find_map(|l| {
        l.trim()
            .strip_prefix(key)
            .and_then(|r| r.strip_prefix(':'))
            .map(str::to_owned)
    })
}

impl RedisConnection {
    pub async fn connect(cfg: &ResolvedConfig) -> Result<Self> {
        let topology = topology(cfg)?;
        let db = match cfg.database() {
            None => 0,
            Some(d) => kv::database_index(d)
                .ok_or_else(|| Error::Connection(format!("database: not an index: {d}")))?,
        };
        let mut endpoint = None;
        let (link, side) = match &topology {
            Topology::Standalone => {
                let ep = cfg.endpoint(DEFAULT_PORT).await?;
                let c = client(cfg, &ep.host, ep.port, &ep.server_name, settings(cfg, db))?;
                let conn = open_single(&c, cfg).await?;
                endpoint = Some(ep);
                (Link::Single(conn), Some(c))
            }
            Topology::Sentinel { master } => {
                if cfg.ssh.is_some() {
                    return Err(Error::Connection(
                        "SSH tunnels reach one Redis server: use the standalone mode".into(),
                    ));
                }
                let sentinels = parse_hosts(cfg.host(), cfg.port(DEFAULT_SENTINEL_PORT)?)?;
                let (host, port) = discover_master(cfg, &sentinels, master).await?;
                let c = client(cfg, &host, port, &host, settings(cfg, db))?;
                let mut conn = open_single(&c, cfg).await?;
                let role = conn
                    .req_packed_command(&redis::cmd("ROLE"))
                    .await
                    .map_err(connect_error)?;
                let role = reply::items(&role)
                    .and_then(|r| r.first())
                    .and_then(reply::text)
                    .unwrap_or_default();
                if role != "master" {
                    return Err(Error::Connection(format!(
                        "Sentinel gave {host}:{port} for {master}, but it is a {role}"
                    )));
                }
                (Link::Single(conn), Some(c))
            }
            Topology::Cluster => {
                if cfg.ssh.is_some() {
                    return Err(Error::Connection(
                        "SSH tunnels reach one Redis server: a cluster cannot be tunnelled".into(),
                    ));
                }
                if db != 0 {
                    return Err(Error::Connection(
                        "Redis Cluster has only database 0".into(),
                    ));
                }
                let seeds = parse_hosts(cfg.host(), cfg.port(DEFAULT_PORT)?)?;
                let nodes: Vec<redis::ConnectionInfo> = seeds
                    .iter()
                    .map(|(h, p)| {
                        let addr = match tls(cfg) {
                            None => ConnectionAddr::Tcp(h.clone(), *p),
                            Some(verify) => ConnectionAddr::TcpTls {
                                host: h.clone(),
                                port: *p,
                                insecure: !verify,
                                tls_params: None,
                            },
                        };
                        addr.into_connection_info()
                            .map(|i| i.set_redis_settings(settings(cfg, 0)))
                    })
                    .collect::<redis::RedisResult<_>>()
                    .map_err(|e| Error::Connection(e.to_string()))?;
                let mut b = redis::cluster::ClusterClientBuilder::new(nodes)
                    .use_protocol(protocol(cfg))
                    .connection_timeout(cfg.connect_timeout)
                    .response_timeout(Duration::from_secs(24 * 3600));
                if let Some(u) = cfg.user() {
                    b = b.username(u);
                }
                if let Some(p) = cfg.password() {
                    b = b.password(p);
                }
                if let Some(verify) = tls(cfg) {
                    b = b
                        .tls(if verify {
                            redis::TlsMode::Secure
                        } else {
                            redis::TlsMode::Insecure
                        })
                        .certs(certificates(cfg)?);
                }
                let client = b.build().map_err(|e| Error::Connection(e.to_string()))?;
                let conn = tokio::time::timeout(
                    cfg.connect_timeout + Duration::from_secs(1),
                    client.get_async_connection(),
                )
                .await
                .map_err(|_| Error::Connection("timed out connecting to the Redis Cluster".into()))?
                .map_err(connect_error)?;
                (Link::Cluster(conn), None)
            }
        };
        let mut c = Self {
            link,
            topology,
            db,
            read_only: cfg.read_only,
            client_id: None,
            side,
            version: String::new(),
            alive: Arc::new(AtomicBool::new(true)),
            in_multi: false,
            queued: None,
            masters: Vec::new(),
            _endpoint: endpoint,
            cfg_timeout: cfg.connect_timeout,
        };
        let info = c
            .link
            .req(redis::cmd("INFO").arg("server"))
            .await
            .map_err(connect_error)?;
        // A cluster answers per node (a map); any node will do.
        let info_text = match &info {
            Value::Map(m) => m.first().and_then(|(_, v)| reply::text(v)),
            other => reply::text(other),
        }
        .unwrap_or_default();
        c.version = info_field(&info_text, "valkey_version")
            .map(|v| format!("Valkey {v}"))
            .or_else(|| info_field(&info_text, "redis_version").map(|v| format!("Redis {v}")))
            .unwrap_or_else(|| "Redis".into());
        if matches!(c.link, Link::Single(_)) {
            c.client_id = c
                .link
                .req(redis::cmd("CLIENT").arg("ID"))
                .await
                .ok()
                .and_then(|v| reply::int(&v));
        } else {
            c.refresh_masters().await?;
        }
        Ok(c)
    }

    /// Re-reads the cluster masters (`CLUSTER NODES`).
    pub(crate) async fn refresh_masters(&mut self) -> Result<()> {
        if !matches!(self.link, Link::Cluster(_)) {
            return Ok(());
        }
        let v = self
            .link
            .req(redis::cmd("CLUSTER").arg("NODES"))
            .await
            .map_err(|e| self.lost(e))?;
        self.masters = cluster_masters(&reply::text(&v).unwrap_or_default());
        Ok(())
    }

    /// Nodes a whole-keyspace operation visits: every master, or `None` for
    /// a single server.
    pub(crate) fn nodes(&self) -> Vec<Option<(String, u16)>> {
        match self.link {
            Link::Cluster(_) => self.masters.iter().cloned().map(Some).collect(),
            Link::Single(_) => vec![None],
        }
    }

    /// Maps an error, marking the connection dead when the transport broke.
    pub(crate) fn lost(&self, e: RedisError) -> Error {
        let e = map_err(e);
        if e == Error::ConnectionLost {
            self.alive.store(false, Ordering::SeqCst);
        }
        e
    }

    /// Runs one command.
    pub(crate) async fn run(&mut self, cmd: &Cmd) -> Result<Value> {
        let r = self.link.req(cmd).await;
        r.map_err(|e| self.lost(e))
    }

    fn check_read_only(&self, args: &[String]) -> Result<CommandClass> {
        let class = kv::classify(args);
        if self.read_only && !class.allowed_read_only() {
            return Err(Error::ReadOnly);
        }
        Ok(class)
    }

    /// One console command line.
    async fn command(&mut self, line: &str, sink: &ResultSink) -> Result<ExecSummary> {
        let args = kv::split_args(line).map_err(|m| {
            Error::Query(QueryError {
                message: m,
                code: Some("FERRODB-REDIS".into()),
                range: None,
            })
        })?;
        let Some(name) = args.first().map(|a| a.to_ascii_uppercase()) else {
            return Err(Error::Query(QueryError {
                message: "empty command".into(),
                code: Some("FERRODB-REDIS".into()),
                range: None,
            }));
        };
        let class = self.check_read_only(&args)?;
        if self.read_only && name == "KEYS" {
            let n = self
                .run(&redis::cmd("DBSIZE"))
                .await
                .ok()
                .and_then(|v| reply::int(&v))
                .unwrap_or(0);
            if n > KEYS_LIMIT {
                return Err(Error::Query(QueryError {
                    message: format!(
                        "KEYS on {n} keys blocks the server; use SCAN (the key browser) instead"
                    ),
                    code: Some("FERRODB-REDIS".into()),
                    range: None,
                }));
            }
        }
        if name == "SELECT" && matches!(self.link, Link::Cluster(_)) {
            return Err(Error::Query(QueryError {
                message: "Redis Cluster has only database 0".into(),
                code: Some("ERR".into()),
                range: None,
            }));
        }
        if matches!(
            name.as_str(),
            "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" | "MONITOR"
        ) {
            return Err(Error::Unsupported(format!(
                "{name} in the console (it never returns)"
            )));
        }
        if matches!(self.link, Link::Cluster(_))
            && let Some(t) = self.cluster_multi(&name, &args).await?
        {
            let truncated = send_table(&t, sink).await;
            return Ok(ExecSummary {
                affected: None,
                results: 1,
                truncated,
            });
        }
        let mut cmd = redis::cmd(&args[0]);
        for a in &args[1..] {
            cmd.arg(a.as_str());
        }
        let v = self.run(&cmd).await?;
        if let Value::ServerError(e) = &v {
            return Err(Error::Query(QueryError {
                message: e.details().unwrap_or_default().to_owned(),
                code: Some(e.code().to_owned()),
                range: None,
            }));
        }
        match name.as_str() {
            "SELECT" => {
                if let Some(db) = args.get(1).and_then(|d| d.parse().ok()) {
                    self.db = db;
                }
            }
            "MULTI" => self.in_multi = true,
            "EXEC" | "DISCARD" => self.in_multi = false,
            _ => {}
        }
        let table = reply::render(&args, &v);
        if let Some(c) = &table.cursor {
            sink.notice(
                NoticeLevel::Info,
                format!("Next cursor: {c}{}", if c == "0" { " (done)" } else { "" }),
            )
            .await;
        }
        let affected = match (class, &v) {
            (CommandClass::Write | CommandClass::Dangerous, Value::Int(n)) if *n >= 0 => {
                Some(*n as u64)
            }
            _ => None,
        };
        let truncated = send_table(&table, sink).await;
        Ok(ExecSummary {
            affected,
            results: 1,
            truncated,
        })
    }

    /// `MULTI` / `EXEC` / `DISCARD` and queued commands on a cluster.
    async fn cluster_multi(&mut self, name: &str, args: &[String]) -> Result<Option<reply::Table>> {
        let status = |s: &str| reply::render(args, &Value::SimpleString(s.to_owned()));
        let err = |m: &str| {
            Error::Query(QueryError {
                message: m.to_owned(),
                code: Some("ERR".into()),
                range: None,
            })
        };
        match (name, self.queued.is_some()) {
            ("MULTI", true) => Err(err("MULTI calls can not be nested")),
            ("MULTI", false) => {
                self.queued = Some(Vec::new());
                self.in_multi = true;
                Ok(Some(status("OK")))
            }
            ("EXEC" | "DISCARD", false) => Err(err(&format!("{name} without MULTI"))),
            ("DISCARD", true) => {
                self.queued = None;
                self.in_multi = false;
                Ok(Some(status("OK")))
            }
            ("EXEC", true) => {
                let cmds = self.queued.take().unwrap_or_default();
                self.in_multi = false;
                if cmds.is_empty() {
                    return Ok(Some(reply::render(args, &Value::Array(Vec::new()))));
                }
                let mut p = redis::pipe();
                p.atomic();
                for c in &cmds {
                    let mut cmd = redis::cmd(&c[0]);
                    for a in &c[1..] {
                        cmd.arg(a.as_str());
                    }
                    p.add_command(cmd);
                }
                let n = cmds.len();
                let r = match &mut self.link {
                    Link::Cluster(c) => c.req_packed_commands(&p, n + 1, 1).await,
                    Link::Single(c) => c.req_packed_commands(&p, n + 1, 1).await,
                };
                let v = r.map_err(|e| self.lost(e))?;
                let v = v.into_iter().next().unwrap_or(Value::Nil);
                Ok(Some(reply::render(args, &v)))
            }
            (_, true) => {
                if kv::is_blocking(args) {
                    return Err(err("blocking commands cannot run in MULTI"));
                }
                if let Some(q) = self.queued.as_mut() {
                    q.push(args.to_vec());
                }
                Ok(Some(status("QUEUED")))
            }
            _ => Ok(None),
        }
    }

    async fn dispatch(&mut self, req: Request, sink: &ResultSink) -> Result<ExecSummary> {
        match req {
            Request::Text { text } => self.command(text.trim(), sink).await,
            Request::Native { method, args } => native::run(self, &method, &args, sink).await,
            Request::Mql(_) => Err(Error::Unsupported(
                "MongoDB commands on a Redis connection".into(),
            )),
        }
    }

    fn unblock_handle(&self) -> Option<RedisCancel> {
        Some(RedisCancel {
            client: self.side.clone()?,
            id: self.client_id?,
            timeout: self.cfg_timeout,
        })
    }
}

/// `KEYS` is refused on read-only sources above this many keys.
pub const KEYS_LIMIT: i64 = 10_000;

/// Streams a rendered reply (one result set, `batch_rows` per batch).
/// Returns whether the consumer went away.
pub(crate) async fn send_table(t: &reply::Table, sink: &ResultSink) -> bool {
    let schema = t.schema();
    if sink
        .send(SinkEvent::Begin {
            schema: schema.clone(),
        })
        .await
        .is_err()
    {
        return true;
    }
    let mut at = 0;
    while at < t.rows.len() {
        let end = (at + sink.batch_rows()).min(t.rows.len());
        if sink
            .send(SinkEvent::Batch(t.batch(&schema, at, end)))
            .await
            .is_err()
        {
            return true;
        }
        at = end;
    }
    sink.send(SinkEvent::End).await.is_err()
}

/// Unblocks a blocked client (`CLIENT UNBLOCK id ERROR`) from a second
/// connection. Returns whether the client was blocked.
pub(crate) struct RedisCancel {
    client: Client,
    id: i64,
    timeout: Duration,
}

impl RedisCancel {
    async fn unblock(&self) -> Result<bool> {
        let fut = async {
            let mut c = self
                .client
                .get_multiplexed_async_connection_with_config(
                    &AsyncConnectionConfig::new()
                        .set_connection_timeout(Some(self.timeout))
                        .set_response_timeout(Some(Duration::from_secs(2))),
                )
                .await
                .map_err(map_err)?;
            let v = c
                .req_packed_command(
                    redis::cmd("CLIENT")
                        .arg("UNBLOCK")
                        .arg(self.id)
                        .arg("ERROR"),
                )
                .await
                .map_err(map_err)?;
            Ok(reply::int(&v) == Some(1))
        };
        tokio::time::timeout(Duration::from_secs(3), fut)
            .await
            .map_err(|_| Error::Timeout)?
    }
}

impl RemoteCancel for RedisCancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        let me = RedisCancel {
            client: self.client.clone(),
            id: self.id,
            timeout: self.timeout,
        };
        Box::pin(async move { me.unblock().await.map(|_| ()) })
    }
}

impl Connection for RedisConnection {
    fn server_version(&self) -> String {
        match &self.topology {
            Topology::Standalone => self.version.clone(),
            Topology::Cluster => {
                format!("{} (cluster, {} masters)", self.version, self.masters.len())
            }
            Topology::Sentinel { master } => format!("{} (Sentinel master {master})", self.version),
        }
    }

    fn execute<'a>(
        &'a mut self,
        req: Request,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ExecSummary>> {
        Box::pin(async move {
            let blocking = match &req {
                Request::Text { text } => kv::split_args(text)
                    .map(|a| kv::is_blocking(&a))
                    .unwrap_or(false),
                _ => false,
            };
            let unblock = self.unblock_handle();
            let alive = self.alive.clone();
            let r = {
                let fut = self.dispatch(req, &sink);
                tokio::pin!(fut);
                tokio::select! {
                    r = &mut fut => r,
                    _ = cancel.cancelled() => Err(Error::Cancelled),
                }
            };
            if r == Err(Error::Cancelled) {
                // The reply of the abandoned command is discarded by the
                // multiplexer when it arrives. A blocked command is
                // unblocked; anything else may still occupy the server
                // connection, so it is dropped and reopened by the core.
                let unblocked = match unblock {
                    Some(u) => u.unblock().await.unwrap_or(false),
                    None => false,
                };
                if !unblocked || !blocking {
                    alive.store(false, Ordering::SeqCst);
                }
            }
            r
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        self.unblock_handle()
            .map(|c| Arc::new(c) as Arc<dyn RemoteCancel>)
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        // Redis has no read-only sessions for masters: commands are
        // classified client-side (`kv::classify`).
        Box::pin(async move {
            self.read_only = read_only;
            Ok(())
        })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let db = kv::database_index(schema)
                .ok_or_else(|| Error::Other(format!("not a database: {schema}")))?;
            if db == self.db {
                return Ok(());
            }
            if matches!(self.link, Link::Cluster(_)) {
                return Err(Error::Unsupported(
                    "databases other than 0 in Redis Cluster".into(),
                ));
            }
            self.run(redis::cmd("SELECT").arg(db)).await?;
            self.db = db;
            Ok(())
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move { Ok(Some(format!("db{}", self.db))) })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        // `MULTI … EXEC` is typed in the console (TxSupport::SingleStatement).
        Box::pin(async {
            Err(Error::Unsupported(
                "interactive transactions (use MULTI … EXEC)".into(),
            ))
        })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if self.queued.is_some() {
                self.cluster_multi("EXEC", &["EXEC".to_owned()]).await?;
                return Ok(());
            }
            if self.in_multi {
                self.run(&redis::cmd("EXEC")).await?;
                self.in_multi = false;
            }
            Ok(())
        })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if self.queued.take().is_some() {
                self.in_multi = false;
                return Ok(());
            }
            if self.in_multi {
                let _ = self.run(&redis::cmd("DISCARD")).await;
                self.in_multi = false;
            }
            Ok(())
        })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let v = self.run(&redis::cmd("PING")).await?;
            match reply::text(&v).as_deref() {
                Some("PONG") => Ok(()),
                // A cluster answers per node.
                _ if matches!(self.link, Link::Cluster(_)) => Ok(()),
                other => Err(Error::Other(format!("unexpected PING reply {other:?}"))),
            }
        })
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(
            parse_hosts("a:7000, b ,[::1]:7002", 6379).unwrap(),
            vec![("a".into(), 7000), ("b".into(), 6379), ("::1".into(), 7002)]
        );
        assert!(parse_hosts(":7000", 1).is_err());
    }

    #[test]
    fn cluster_nodes_reply() {
        let nodes = "\
07c37dfeb235213a872192d90877d0cd55635b91 127.0.0.1:30004@31004,hostname4 slave e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca 0 1426238317239 4 connected
67ed2db8d677e59ec4a4cefb06858cf2a1a89fa1 127.0.0.1:30002@31002,hostname2 master - 0 1426238316232 2 connected 5461-10922
292f8b365bb7edb5e285caf0b7e6ddc7265d2f4f 127.0.0.1:30003@31003 master - 0 1426238318243 3 connected 10923-16383
e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca 127.0.0.1:30001@31001 myself,master - 0 0 1 connected 0-5460
6ec23923021cf3ffec47632106199cb7f496ce01 127.0.0.1:30005@31005 master,fail - 0 0 5 connected";
        assert_eq!(
            cluster_masters(nodes),
            vec![
                ("127.0.0.1".into(), 30001),
                ("127.0.0.1".into(), 30002),
                ("127.0.0.1".into(), 30003)
            ]
        );
    }

    #[test]
    fn info_and_config() {
        let info = "# Server\r\nredis_version:8.0.2\r\nredis_mode:standalone\r\n";
        assert_eq!(info_field(info, "redis_version").as_deref(), Some("8.0.2"));
        assert_eq!(info_field(info, "valkey_version"), None);
        let cfg = ResolvedConfig::new("redis").with("mode", "sentinel");
        assert!(topology(&cfg).is_err());
        let cfg = cfg.with("sentinel.master", "mymaster");
        assert_eq!(
            topology(&cfg).unwrap(),
            Topology::Sentinel {
                master: "mymaster".into()
            }
        );
        let mut cfg = ResolvedConfig::new("redis");
        cfg.tls = TlsMode::Prefer;
        assert_eq!(tls(&cfg), None);
        cfg.tls = TlsMode::Require;
        assert_eq!(tls(&cfg), Some(false));
    }
}
