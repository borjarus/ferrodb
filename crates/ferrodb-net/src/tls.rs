//! TLS modes follow libpq semantics, which users know from every SQL tool:
//!
//! | mode          | encrypted        | certificate verified |
//! |---------------|------------------|----------------------|
//! | `disable`     | no               | —                    |
//! | `prefer`      | if server offers | no                   |
//! | `require`     | yes              | no (unless a CA file is given) |
//! | `verify-full` | yes              | chain + host name (system store or CA file) |

use std::{path::Path, sync::Arc};

use rustls::{
    ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject},
};

use crate::NetError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TlsMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyFull,
}

impl TlsMode {
    /// Parses the `ssl.mode` property written by the data source dialog.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "disable" | "false" | "off" => Self::Disable,
            "require" | "required" => Self::Require,
            "verify-full" | "verify_full" | "verify-ca" | "verify_identity" => Self::VerifyFull,
            _ => Self::Prefer,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyFull => "verify-full",
        }
    }
}

/// Installs `ring` as the process-wide rustls provider (idempotent). Every
/// crate in the workspace builds rustls with `ring` only, so this is the
/// provider libraries pick up when they call `ClientConfig::builder()`.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn provider() -> Arc<CryptoProvider> {
    install_crypto_provider();
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()))
}

/// Client configuration for `mode`; `None` for [`TlsMode::Disable`].
pub fn client_config(
    mode: TlsMode,
    ca_file: Option<&Path>,
) -> Result<Option<Arc<ClientConfig>>, NetError> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| NetError::Tls(e.to_string()))?;
    let config = match (mode, ca_file) {
        (TlsMode::Disable, _) => return Ok(None),
        (_, Some(ca)) => {
            let bad = |e: &dyn std::fmt::Display| NetError::Tls(format!("{}: {e}", ca.display()));
            let mut roots = RootCertStore::empty();
            for cert in CertificateDer::pem_file_iter(ca).map_err(|e| bad(&e))? {
                roots.add(cert.map_err(|e| bad(&e))?).map_err(|e| bad(&e))?;
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        (TlsMode::VerifyFull, None) => {
            let verifier = rustls_platform_verifier::Verifier::new(provider)
                .map_err(|e| NetError::Tls(e.to_string()))?;
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(verifier))
                .with_no_client_auth()
        }
        (TlsMode::Prefer | TlsMode::Require, None) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier(provider)))
            .with_no_client_auth(),
    };
    Ok(Some(Arc::new(config)))
}

/// Encrypts without authenticating the server (`prefer` / `require`).
#[derive(Debug)]
struct NoVerifier(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse_and_build() {
        assert_eq!(TlsMode::parse("verify-full"), TlsMode::VerifyFull);
        assert_eq!(TlsMode::parse("REQUIRE"), TlsMode::Require);
        assert_eq!(TlsMode::parse(""), TlsMode::Prefer);
        assert!(client_config(TlsMode::Disable, None).unwrap().is_none());
        assert!(client_config(TlsMode::Require, None).unwrap().is_some());
        assert!(client_config(TlsMode::VerifyFull, None).unwrap().is_some());
        assert!(client_config(TlsMode::VerifyFull, Some(Path::new("missing.pem"))).is_err());
    }
}
