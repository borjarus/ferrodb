//! SSH tunnels: a local TCP listener on 127.0.0.1 whose connections are
//! forwarded through an SSH `direct-tcpip` channel to the database host.
//!
//! One SSH session is opened per tunnel; every accepted local connection gets
//! its own `direct-tcpip` channel on that session, so drivers can open side
//! connections (cancel requests, `KILL QUERY`, …) through the same tunnel.
//!
//! Host keys are verified against `known_hosts` (trust on first use: unknown
//! hosts are accepted with a warning and recorded; a changed key is refused).

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use russh::{
    Disconnect,
    client::{self, AuthResult, Handle},
    keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, known_hosts},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::{AbortHandle, JoinSet},
};

use crate::NetError;

/// Interval between SSH keepalive requests.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// Unanswered keepalives after which the session is considered dead.
const KEEPALIVE_MAX: usize = 3;
/// Timeout for the TCP connect + handshake + authentication.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, PartialEq, Eq)]
pub enum SshAuth {
    /// Private key file (OpenSSH / PEM) with optional passphrase.
    Key {
        path: PathBuf,
        passphrase: Option<String>,
    },
    Password(String),
}

impl std::fmt::Debug for SshAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Key { path, .. } => f
                .debug_struct("Key")
                .field("path", path)
                .finish_non_exhaustive(),
            Self::Password(_) => f.write_str("Password(***)"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// `known_hosts` file used to verify the server key; `None` = the user's
    /// `~/.ssh/known_hosts`.
    pub known_hosts: Option<PathBuf>,
}

/// An open tunnel. Dropping it closes the listener and the SSH session.
pub struct SshTunnel {
    local: SocketAddr,
    /// Signals the background task to disconnect gracefully.
    shutdown: Option<oneshot::Sender<()>>,
    /// Fallback: hard-stops the background task if it is still running.
    task: AbortHandle,
}

impl std::fmt::Debug for SshTunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshTunnel")
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

impl SshTunnel {
    /// Connects and authenticates to the SSH server, then listens on an
    /// ephemeral local port forwarding to `target_host:target_port`.
    pub async fn open(
        cfg: &SshConfig,
        target_host: &str,
        target_port: u16,
    ) -> Result<Self, NetError> {
        let handle = tokio::time::timeout(CONNECT_TIMEOUT, connect(cfg))
            .await
            .map_err(|_| {
                NetError::Ssh(format!(
                    "timed out connecting to {}:{} after {}s",
                    cfg.host,
                    cfg.port,
                    CONNECT_TIMEOUT.as_secs()
                ))
            })??;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let local = listener.local_addr()?;
        tracing::debug!(
            %local,
            target = %format_args!("{target_host}:{target_port}"),
            "SSH tunnel listening"
        );

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(run_tunnel(
            handle,
            listener,
            target_host.to_owned(),
            target_port,
            shutdown_rx,
        ));

        Ok(Self {
            local,
            shutdown: Some(shutdown_tx),
            task: task.abort_handle(),
        })
    }

    /// Address drivers connect to instead of the database host.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        tracing::debug!(local = %self.local, "closing SSH tunnel");
        let graceful = self.shutdown.take().is_some_and(|tx| tx.send(()).is_ok());
        if !graceful {
            // The task is gone already (or cannot receive); make sure.
            self.task.abort();
        }
    }
}

/// Accept loop: one `direct-tcpip` channel per local connection. Stops (and
/// disconnects the session) when the tunnel is dropped or the session dies.
async fn run_tunnel(
    handle: Handle<ClientHandler>,
    listener: TcpListener,
    target_host: String,
    target_port: u16,
    mut shutdown: oneshot::Receiver<()>,
) {
    let handle = Arc::new(handle);
    // Dropping the JoinSet (when this task ends or is aborted) aborts every
    // forwarded connection, so no tasks outlive the tunnel.
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            // Reap finished connection tasks so the set does not grow forever.
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    if handle.is_closed() {
                        tracing::warn!("SSH session closed; refusing forwarded connection");
                        break;
                    }
                    conns.spawn(forward(
                        Arc::clone(&handle),
                        stream,
                        peer,
                        target_host.clone(),
                        target_port,
                    ));
                }
                Err(e) => {
                    // Transient accept errors (e.g. fd exhaustion) must not
                    // kill the tunnel.
                    tracing::warn!(error = %e, "SSH tunnel accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
    conns.abort_all();
    drop(listener);
    if let Err(e) = handle
        .disconnect(Disconnect::ByApplication, "tunnel closed", "en")
        .await
    {
        tracing::debug!(error = %e, "SSH disconnect failed (session already closed?)");
    }
    tracing::debug!("SSH tunnel stopped");
}

/// Forwards one accepted local connection through its own channel.
async fn forward(
    handle: Arc<Handle<ClientHandler>>,
    mut stream: TcpStream,
    peer: SocketAddr,
    target_host: String,
    target_port: u16,
) {
    let _ = stream.set_nodelay(true);
    let channel = match handle
        .channel_open_direct_tcpip(
            target_host.as_str(),
            u32::from(target_port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        )
        .await
    {
        Ok(channel) => channel,
        Err(e) => {
            tracing::warn!(
                %peer,
                target = %format_args!("{target_host}:{target_port}"),
                error = %e,
                "SSH direct-tcpip channel open failed"
            );
            return;
        }
    };
    tracing::debug!(%peer, target = %format_args!("{target_host}:{target_port}"), "forwarded connection opened");
    let mut remote = channel.into_stream();
    match tokio::io::copy_bidirectional(&mut stream, &mut remote).await {
        Ok((up, down)) => {
            tracing::debug!(%peer, bytes_up = up, bytes_down = down, "forwarded connection closed");
        }
        Err(e) => {
            tracing::debug!(%peer, error = %e, "forwarded connection ended with error");
        }
    }
}

/// TCP connect, host-key verification and authentication.
async fn connect(cfg: &SshConfig) -> Result<Handle<ClientHandler>, NetError> {
    let config = Arc::new(client::Config {
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        keepalive_max: KEEPALIVE_MAX,
        nodelay: true,
        ..Default::default()
    });
    let handler = ClientHandler {
        host: cfg.host.clone(),
        port: cfg.port,
        known_hosts: cfg.known_hosts.clone().or_else(default_known_hosts),
    };

    tracing::debug!(host = %cfg.host, port = cfg.port, user = %cfg.user, "SSH connecting");
    let mut handle = client::connect(config, (cfg.host.as_str(), cfg.port), handler)
        .await
        .map_err(|e| match e {
            HandlerError::HostKey(msg) => NetError::Ssh(msg),
            HandlerError::Russh(e) => {
                NetError::Ssh(format!("cannot connect to {}:{}: {e}", cfg.host, cfg.port))
            }
        })?;

    let (method, result) = match &cfg.auth {
        SshAuth::Password(password) => {
            tracing::debug!(user = %cfg.user, "SSH authenticating with password");
            let r = handle
                .authenticate_password(cfg.user.as_str(), password.as_str())
                .await;
            ("password", r)
        }
        SshAuth::Key { path, passphrase } => {
            tracing::debug!(user = %cfg.user, key = %path.display(), "SSH authenticating with key");
            let key = load_key(path, passphrase.as_deref())?;
            let hash = if key.algorithm().is_rsa() {
                // Prefer rsa-sha2-512/256 when the server advertises them.
                handle
                    .best_supported_rsa_hash()
                    .await
                    .map_err(|e| NetError::Ssh(format!("SSH negotiation failed: {e}")))?
                    .flatten()
                    .or(Some(HashAlg::Sha512))
            } else {
                None
            };
            let r = handle
                .authenticate_publickey(
                    cfg.user.as_str(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await;
            ("public key", r)
        }
    };
    match result {
        Ok(AuthResult::Success) => {
            tracing::debug!(user = %cfg.user, method, "SSH authenticated");
            Ok(handle)
        }
        Ok(AuthResult::Failure { .. }) => Err(NetError::Ssh(format!(
            "authentication failed for user '{}' on {}:{} ({method})",
            cfg.user, cfg.host, cfg.port
        ))),
        Err(e) => Err(NetError::Ssh(format!(
            "authentication error for user '{}' on {}:{} ({method}): {e}",
            cfg.user, cfg.host, cfg.port
        ))),
    }
}

fn load_key(path: &Path, passphrase: Option<&str>) -> Result<keys::PrivateKey, NetError> {
    keys::load_secret_key(path, passphrase).map_err(|e| {
        let what = match e {
            keys::Error::KeyIsEncrypted => {
                "the key is encrypted; a passphrase is required".to_owned()
            }
            keys::Error::IO(e) => format!("cannot read file: {e}"),
            // Wrong passphrase surfaces as a decoding/decryption error.
            other if passphrase.is_some() => {
                format!("cannot decode key (wrong passphrase?): {other}")
            }
            other => format!("cannot decode key: {other}"),
        };
        NetError::Ssh(format!("private key {}: {what}", path.display()))
    })
}

/// `~/.ssh/known_hosts` (`%USERPROFILE%` on Windows).
fn default_known_hosts() -> Option<PathBuf> {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    } else {
        std::env::var_os("HOME")
    }?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".ssh").join("known_hosts"))
}

#[derive(Debug)]
enum HandlerError {
    /// Host-key verification refused the server (message is user-facing).
    HostKey(String),
    Russh(russh::Error),
}

impl From<russh::Error> for HandlerError {
    fn from(e: russh::Error) -> Self {
        Self::Russh(e)
    }
}

struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: Option<PathBuf>,
}

impl client::Handler for ClientHandler {
    type Error = HandlerError;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = server_key.public_key();
        let fingerprint = key.fingerprint(HashAlg::Sha256);
        let Some(path) = &self.known_hosts else {
            tracing::warn!(
                host = %self.host,
                port = self.port,
                %fingerprint,
                "no known_hosts file location (no home directory); accepting SSH host key unverified"
            );
            return Ok(true);
        };
        match known_hosts::check_known_hosts_path(&self.host, self.port, &key, path) {
            Ok(true) => {
                tracing::debug!(host = %self.host, %fingerprint, "SSH host key verified");
                Ok(true)
            }
            Ok(false) => {
                tracing::warn!(
                    host = %self.host,
                    port = self.port,
                    %fingerprint,
                    known_hosts = %path.display(),
                    "unknown SSH host key; trusting on first use"
                );
                if let Err(e) =
                    known_hosts::learn_known_hosts_path(&self.host, self.port, &key, path)
                {
                    tracing::warn!(
                        known_hosts = %path.display(),
                        error = %e,
                        "could not record SSH host key"
                    );
                }
                Ok(true)
            }
            Err(keys::Error::KeyChanged { line }) => Err(HandlerError::HostKey(format!(
                "host key changed for {}:{}: the server presented {fingerprint}, which does not \
                 match {}:{line}. This may be a man-in-the-middle attack; if the server key was \
                 legitimately replaced, remove that line and reconnect",
                self.host,
                self.port,
                path.display()
            ))),
            Err(e) => Err(HandlerError::HostKey(format!(
                "cannot verify host key of {}:{} against {}: {e}",
                self.host,
                self.port,
                path.display()
            ))),
        }
    }
}
