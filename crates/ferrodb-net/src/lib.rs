//! # ferrodb-net
//!
//! Network plumbing shared by drivers: TLS client configuration (rustls with
//! the platform verifier) and SSH tunnels (pure Rust, `russh`).

pub mod ssh;
pub mod tls;

pub use ssh::{SshAuth, SshConfig, SshTunnel};
pub use tls::{TlsMode, client_config, install_crypto_provider};

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("TLS: {0}")]
    Tls(String),
    #[error("SSH: {0}")]
    Ssh(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}
