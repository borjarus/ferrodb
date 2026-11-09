//! A data source configuration with secrets resolved, ready for `connect`.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use ferrodb_api::{DataSourceConfig, DriverId, Error, Result, SecretValues};
use ferrodb_net::{SshAuth, SshConfig, SshTunnel, TlsMode};

#[derive(Clone, Debug)]
pub struct ResolvedConfig {
    pub driver: DriverId,
    /// Non-secret properties from the connection form (`host`, `port`,
    /// `database`, `user`, `file`, `option.*`…).
    pub properties: BTreeMap<String, String>,
    /// Secrets from the keyring (`password`, `ssh.password`…).
    pub secrets: SecretValues,
    pub read_only: bool,
    pub tls: TlsMode,
    /// CA bundle (PEM) used to verify the server instead of the system store.
    pub tls_ca: Option<PathBuf>,
    pub ssh: Option<SshConfig>,
    pub connect_timeout: Duration,
    /// Reported to servers that show client names (`pg_stat_activity`…).
    pub application_name: String,
}

impl ResolvedConfig {
    pub fn new(driver: impl Into<DriverId>) -> Self {
        Self {
            driver: driver.into(),
            properties: BTreeMap::new(),
            secrets: SecretValues::new(),
            read_only: false,
            tls: TlsMode::Prefer,
            tls_ca: None,
            ssh: None,
            connect_timeout: Duration::from_secs(15),
            application_name: "FerroDb".into(),
        }
    }

    /// Parses `scheme://user:password@host:port/database?key=value` (tests and
    /// the TCK; percent-escapes are not decoded). Query keys become
    /// properties, e.g. `?ssl.mode=disable`.
    pub fn from_url(driver: impl Into<DriverId>, url: &str) -> Result<Self> {
        let bad = || Error::Connection(format!("invalid URL: {url}"));
        let (_, rest) = url.split_once("://").ok_or_else(bad)?;
        let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (auth, rest) = match rest.rsplit_once('@') {
            Some((a, r)) => (Some(a), r),
            None => (None, rest),
        };
        let (hostport, db) = rest.split_once('/').unwrap_or((rest, ""));
        let mut r = Self::new(driver);
        match hostport.rsplit_once(':') {
            Some((h, p)) => {
                r = r.with("host", h).with("port", p);
            }
            None if !hostport.is_empty() => r = r.with("host", hostport),
            None => {}
        }
        if !db.is_empty() {
            r = r.with("database", db);
        }
        if let Some(a) = auth {
            let (u, p) = a.split_once(':').unwrap_or((a, ""));
            r = r.with("user", u);
            if !p.is_empty() {
                r = r.with_secret("password", p);
            }
        }
        for kv in query.split('&').filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            r = r.with(k, v);
        }
        r.tls = r.prop("ssl.mode").map(TlsMode::parse).unwrap_or_default();
        Ok(r)
    }

    /// Builder helper for tests and the TCK.
    pub fn with(mut self, key: &str, value: impl Into<String>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    pub fn with_secret(mut self, key: &str, value: impl Into<String>) -> Self {
        self.secrets.0.retain(|(k, _)| k != key);
        self.secrets.0.push((key.into(), value.into()));
        self
    }

    /// Interprets the properties written by the data source dialog.
    pub fn from_config(cfg: &DataSourceConfig, secrets: SecretValues) -> Result<Self> {
        let mut r = Self::new(cfg.driver.clone());
        r.properties = cfg
            .properties
            .iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, v)| (k.clone(), v.trim().to_owned()))
            .collect();
        r.secrets = secrets;
        r.read_only = cfg.read_only;
        r.tls = r.prop("ssl.mode").map(TlsMode::parse).unwrap_or_default();
        r.tls_ca = r.prop("ssl.ca").map(PathBuf::from);
        if let Some(t) = r.prop("option.connect_timeout") {
            let secs = t
                .parse()
                .map_err(|_| Error::Connection(format!("connect_timeout: not a number: {t}")))?;
            r.connect_timeout = Duration::from_secs(secs);
        }
        if let Some(host) = r.prop("ssh.host") {
            let (host, port) = match host.rsplit_once(':') {
                Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_owned(), p.parse().unwrap_or(22)),
                _ => (host.to_owned(), 22),
            };
            let user = r
                .prop("ssh.user")
                .ok_or_else(|| Error::Connection("SSH: user is required".into()))?
                .to_owned();
            let auth = match r.prop("ssh.key") {
                Some(path) => SshAuth::Key {
                    path: expand_home(path),
                    passphrase: r.secret("ssh.passphrase").map(str::to_owned),
                },
                None => SshAuth::Password(
                    r.secret("ssh.password")
                        .ok_or_else(|| {
                            Error::Connection("SSH: a key file or a password is required".into())
                        })?
                        .to_owned(),
                ),
            };
            r.ssh = Some(SshConfig {
                host,
                port,
                user,
                auth,
                known_hosts: r.prop("ssh.known_hosts").map(expand_home),
            });
        }
        Ok(r)
    }

    /// Property value, `None` when missing or blank.
    pub fn prop(&self, key: &str) -> Option<&str> {
        self.properties
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    pub fn secret(&self, key: &str) -> Option<&str> {
        self.secrets.get(key).filter(|v| !v.is_empty())
    }

    pub fn host(&self) -> &str {
        self.prop("host").unwrap_or("localhost")
    }

    pub fn port(&self, default: u16) -> Result<u16> {
        match self.prop("port") {
            None => Ok(default),
            Some(p) => p
                .parse()
                .map_err(|_| Error::Connection(format!("port: not a number: {p}"))),
        }
    }

    pub fn user(&self) -> Option<&str> {
        self.prop("user")
    }

    pub fn password(&self) -> Option<&str> {
        self.secret("password")
    }

    pub fn database(&self) -> Option<&str> {
        self.prop("database")
    }

    /// Driver options typed as `key=value;…` in the Options tab.
    pub fn options(&self) -> impl Iterator<Item = (&str, &str)> {
        self.properties
            .iter()
            .filter_map(|(k, v)| k.strip_prefix("option.").map(|k| (k, v.as_str())))
    }

    /// Where to connect: the database host, or the local end of an SSH
    /// tunnel. Keep the endpoint alive as long as the connection.
    pub async fn endpoint(&self, default_port: u16) -> Result<Endpoint> {
        let host = self.host().to_owned();
        let port = self.port(default_port)?;
        match &self.ssh {
            None => Ok(Endpoint {
                host: host.clone(),
                port,
                server_name: host,
                tunnel: None,
            }),
            Some(ssh) => {
                let tunnel =
                    tokio::time::timeout(self.connect_timeout, SshTunnel::open(ssh, &host, port))
                        .await
                        .map_err(|_| Error::Connection("SSH: timed out".into()))?
                        .map_err(|e| Error::Connection(e.to_string()))?;
                let local = tunnel.local_addr();
                Ok(Endpoint {
                    host: local.ip().to_string(),
                    port: local.port(),
                    server_name: host,
                    tunnel: Some(Arc::new(tunnel)),
                })
            }
        }
    }
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) => std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|h| PathBuf::from(h).join(rest))
            .unwrap_or_else(|| PathBuf::from(p)),
        None => PathBuf::from(p),
    }
}

/// Address to connect to. Holds the SSH tunnel (if any) open.
#[derive(Clone)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    /// Host name for TLS verification (the real server, not the tunnel).
    pub server_name: String,
    tunnel: Option<Arc<SshTunnel>>,
}

impl Endpoint {
    pub fn tunnelled(&self) -> bool {
        self.tunnel.is_some()
    }
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("server_name", &self.server_name)
            .field("tunnelled", &self.tunnelled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_properties_are_interpreted() {
        let cfg = DataSourceConfig {
            id: "x".into(),
            name: "x".into(),
            driver: "postgresql".into(),
            folder: vec![],
            environment: Default::default(),
            read_only: true,
            tx_mode: Default::default(),
            properties: vec![
                ("host".into(), "db.example".into()),
                ("port".into(), " 6432 ".into()),
                ("user".into(), "".into()),
                ("ssl.mode".into(), "verify-full".into()),
                ("ssh.host".into(), "bastion:2222".into()),
                ("ssh.user".into(), "ops".into()),
                ("ssh.key".into(), "/keys/id".into()),
                ("option.connect_timeout".into(), "3".into()),
            ],
            secret_ref: None,
        };
        let r =
            ResolvedConfig::from_config(&cfg, SecretValues::new().with("password", "pw")).unwrap();
        assert_eq!(r.host(), "db.example");
        assert_eq!(r.port(5432).unwrap(), 6432);
        assert_eq!(r.user(), None);
        assert_eq!(r.password(), Some("pw"));
        assert_eq!(r.tls, TlsMode::VerifyFull);
        assert!(r.read_only);
        assert_eq!(r.connect_timeout, Duration::from_secs(3));
        let ssh = r.ssh.unwrap();
        assert_eq!(
            (ssh.host.as_str(), ssh.port, ssh.user.as_str()),
            ("bastion", 2222, "ops")
        );
        assert!(matches!(ssh.auth, SshAuth::Key { .. }));
        assert!(!format!("{:?}", r.secrets).contains("pw"));
    }
}
