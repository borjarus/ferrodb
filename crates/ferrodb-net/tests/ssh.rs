//! SSH tunnel integration tests against a live sshd. Skipped (pass silently)
//! unless `FERRODB_TEST_SSH` is set.
//!
//! Environment:
//! - `FERRODB_TEST_SSH=ssh://user:password@host:port` (required)
//! - `FERRODB_TEST_SSH_TARGET=host:port` of a PostgreSQL server as seen from
//!   the SSH server (default `ferrodb-tck-pg:5432`)
//! - `FERRODB_TEST_SSH_KEY` — unencrypted private key authorized for the user
//!   (optional; key test skipped otherwise)
//! - `FERRODB_TEST_SSH_KEY_ENC` + `FERRODB_TEST_SSH_KEY_PASSPHRASE` — an
//!   encrypted private key (e.g. RSA) and its passphrase (optional)

use std::{path::PathBuf, time::Duration};

use ferrodb_net::{NetError, SshAuth, SshConfig, SshTunnel};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

struct Env {
    host: String,
    port: u16,
    user: String,
    password: String,
    target_host: String,
    target_port: u16,
}

fn env() -> Option<Env> {
    let url = std::env::var("FERRODB_TEST_SSH").ok()?;
    let rest = url
        .strip_prefix("ssh://")
        .expect("FERRODB_TEST_SSH: ssh://...");
    let (creds, hostport) = rest
        .rsplit_once('@')
        .expect("FERRODB_TEST_SSH: missing '@'");
    let (user, password) = creds
        .split_once(':')
        .expect("FERRODB_TEST_SSH: user:password");
    let (host, port) = hostport.rsplit_once(':').unwrap_or((hostport, "22"));
    let target =
        std::env::var("FERRODB_TEST_SSH_TARGET").unwrap_or_else(|_| "ferrodb-tck-pg:5432".into());
    let (target_host, target_port) = target.rsplit_once(':').expect("target host:port");
    Some(Env {
        host: host.into(),
        port: port.parse().expect("ssh port"),
        user: user.into(),
        password: password.into(),
        target_host: target_host.into(),
        target_port: target_port.parse().expect("target port"),
    })
}

impl Env {
    fn config(&self, auth: SshAuth, known_hosts: PathBuf) -> SshConfig {
        SshConfig {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            auth,
            known_hosts: Some(known_hosts),
        }
    }

    async fn open(&self, cfg: &SshConfig) -> Result<SshTunnel, NetError> {
        SshTunnel::open(cfg, &self.target_host, self.target_port).await
    }
}

/// Sends a PostgreSQL SSLRequest and expects the single-byte `N`/`S` answer.
async fn probe_postgres(tunnel: &SshTunnel) -> u8 {
    let mut s = TcpStream::connect(tunnel.local_addr())
        .await
        .expect("connect");
    s.write_all(&[0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f])
        .await
        .expect("write");
    let mut b = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut b))
        .await
        .expect("timeout waiting for PostgreSQL")
        .expect("read");
    assert!(b[0] == b'N' || b[0] == b'S', "unexpected byte {:#x}", b[0]);
    b[0]
}

fn known_hosts(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("known_hosts")
}

#[tokio::test]
async fn password_tunnel_forwards_multiple_connections() {
    let Some(env) = env() else { return };
    let dir = tempfile::tempdir().unwrap();
    let cfg = env.config(SshAuth::Password(env.password.clone()), known_hosts(&dir));
    let tunnel = env.open(&cfg).await.expect("open tunnel");
    assert!(tunnel.local_addr().ip().is_loopback());

    // Two concurrent connections, each its own direct-tcpip channel.
    let (a, b) = tokio::join!(probe_postgres(&tunnel), probe_postgres(&tunnel));
    assert_eq!(a, b);
    // The tunnel survives connections being closed; open another one.
    probe_postgres(&tunnel).await;

    // A connection the target refuses must not kill the tunnel.
    let bad = SshTunnel::open(&cfg, "127.0.0.1", 1).await.expect("open");
    let mut s = TcpStream::connect(bad.local_addr()).await.unwrap();
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut buf))
        .await
        .expect("refused channel should close the local socket")
        .unwrap_or(0);
    assert_eq!(n, 0);
    drop(s);
    probe_postgres(&tunnel).await;
    drop(bad);

    // Dropping the tunnel closes the listener.
    let addr = tunnel.local_addr();
    drop(tunnel);
    let mut closed = false;
    for _ in 0..50 {
        if TcpStream::connect(addr).await.is_err() {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(closed, "listener still accepting after drop");
}

#[tokio::test]
async fn wrong_password_is_auth_error() {
    let Some(env) = env() else { return };
    let dir = tempfile::tempdir().unwrap();
    let cfg = env.config(
        SshAuth::Password("definitely-not-the-password".into()),
        known_hosts(&dir),
    );
    let err = env.open(&cfg).await.expect_err("must fail").to_string();
    assert!(err.contains("authentication failed"), "{err}");
    assert!(
        !err.contains("definitely-not-the-password"),
        "secret leaked: {err}"
    );
}

#[tokio::test]
async fn host_key_tofu_then_verify_then_changed() {
    let Some(env) = env() else { return };
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(&dir);
    let cfg = env.config(SshAuth::Password(env.password.clone()), kh.clone());

    // Unknown host: accepted and recorded.
    drop(env.open(&cfg).await.expect("tofu"));
    let recorded = std::fs::read_to_string(&kh).expect("known_hosts written");
    let line = recorded.lines().find(|l| !l.trim().is_empty()).unwrap();
    let host_pattern = if env.port == 22 {
        env.host.clone()
    } else {
        format!("[{}]:{}", env.host, env.port)
    };
    assert!(line.starts_with(&format!("{host_pattern} ")), "{line}");

    // Known and matching: accepted, file unchanged.
    drop(env.open(&cfg).await.expect("verified"));
    assert_eq!(std::fs::read_to_string(&kh).unwrap(), recorded);

    // Known but different (same algorithm): refused.
    let algo = line.split(' ').nth(1).unwrap();
    assert_eq!(algo, "ssh-ed25519", "test assumes an ed25519 host key");
    std::fs::write(
        &kh,
        format!(
            "{host_pattern} ssh-ed25519 \
             AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ\n"
        ),
    )
    .unwrap();
    let err = env.open(&cfg).await.expect_err("must refuse").to_string();
    assert!(err.contains("host key changed"), "{err}");
}

#[tokio::test]
async fn key_auth() {
    let Some(env) = env() else { return };
    let Some(key) = std::env::var_os("FERRODB_TEST_SSH_KEY") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let cfg = env.config(
        SshAuth::Key {
            path: key.into(),
            passphrase: None,
        },
        known_hosts(&dir),
    );
    let tunnel = env.open(&cfg).await.expect("key auth");
    probe_postgres(&tunnel).await;
}

#[tokio::test]
async fn encrypted_key_auth() {
    let Some(env) = env() else { return };
    let (Some(key), Ok(pass)) = (
        std::env::var_os("FERRODB_TEST_SSH_KEY_ENC"),
        std::env::var("FERRODB_TEST_SSH_KEY_PASSPHRASE"),
    ) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let key = PathBuf::from(key);

    let ok = env.config(
        SshAuth::Key {
            path: key.clone(),
            passphrase: Some(pass.clone()),
        },
        known_hosts(&dir),
    );
    let tunnel = env.open(&ok).await.expect("encrypted key auth");
    probe_postgres(&tunnel).await;

    let missing = env.config(
        SshAuth::Key {
            path: key.clone(),
            passphrase: None,
        },
        known_hosts(&dir),
    );
    let err = env
        .open(&missing)
        .await
        .expect_err("needs passphrase")
        .to_string();
    assert!(err.contains("passphrase"), "{err}");

    let wrong = env.config(
        SshAuth::Key {
            path: key,
            passphrase: Some("wrong-passphrase".into()),
        },
        known_hosts(&dir),
    );
    let err = env
        .open(&wrong)
        .await
        .expect_err("wrong passphrase")
        .to_string();
    assert!(!err.contains("wrong-passphrase"), "secret leaked: {err}");
    assert!(err.contains("private key"), "{err}");
}
