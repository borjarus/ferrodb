//! Microsoft Entra ID (Azure AD) access tokens for Azure SQL, sent in the
//! TDS login (`FEDAUTH` with a security token). Three ways, by property
//! `entra.method` (default: the first that applies):
//!
//! - `token` — an access token pasted as the secret `entra.access_token`;
//! - `client_secret` — a service principal: tenant + `entra.client_id` +
//!   secret `entra.client_secret` (OAuth 2.0 client-credentials flow
//!   against `login.microsoftonline.com`);
//! - `azure_cli` — the signed-in Azure CLI user
//!   (`az account get-access-token`), which also covers interactive and
//!   MFA sign-ins done once with `az login`.

use std::time::Duration;

use ferrodb_api::{AuthMethod, DriverProperty, Error, Result};

use crate::{
    ResolvedConfig, http,
    registry::{property, secret_property},
};

/// The resource Azure SQL tokens are issued for.
pub const RESOURCE: &str = "https://database.windows.net/";

pub fn properties() -> Vec<DriverProperty> {
    let entra = |p: DriverProperty| DriverProperty {
        auth: vec![AuthMethod::EntraId],
        ..p
    };
    vec![
        entra(property(
            "entra.method",
            "Token source: azure_cli, client_secret or token",
            Some("azure_cli"),
            false,
        )),
        entra(property("tenant", "Tenant ID", None, false)),
        entra(property(
            "entra.client_id",
            "Application (client) ID",
            None,
            false,
        )),
        secret_property(
            "entra.client_secret",
            "Client secret",
            &[AuthMethod::EntraId],
        ),
        secret_property("entra.access_token", "Access token", &[AuthMethod::EntraId]),
    ]
}

/// The token source chosen for `cfg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Token,
    ClientSecret,
    AzureCli,
}

pub fn method(cfg: &ResolvedConfig) -> Result<Method> {
    Ok(match cfg.prop("entra.method") {
        Some("token") => Method::Token,
        Some("client_secret") => Method::ClientSecret,
        Some("azure_cli") => Method::AzureCli,
        Some(other) => {
            return Err(Error::Connection(format!(
                "unknown Microsoft Entra ID token source `{other}`"
            )));
        }
        None if cfg.secret("entra.access_token").is_some() => Method::Token,
        None if cfg.secret("entra.client_secret").is_some() => Method::ClientSecret,
        None => Method::AzureCli,
    })
}

/// The `access_token` of a token endpoint response.
pub(crate) fn parse_token_response(body: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| Error::Connection(format!("Microsoft Entra ID: bad response: {e}")))?;
    if let Some(t) = v.get("access_token").and_then(|t| t.as_str()) {
        return Ok(t.to_owned());
    }
    let msg = v
        .get("error_description")
        .or_else(|| v.get("error"))
        .and_then(|e| e.as_str())
        .unwrap_or(body);
    Err(Error::Connection(format!("Microsoft Entra ID: {msg}")))
}

/// An access token for Azure SQL.
pub async fn token(cfg: &ResolvedConfig) -> Result<String> {
    match method(cfg)? {
        Method::Token => cfg
            .secret("entra.access_token")
            .map(str::to_owned)
            .ok_or_else(|| Error::Connection("Microsoft Entra ID: no access token given".into())),
        Method::ClientSecret => {
            let tenant = cfg.prop("tenant").ok_or_else(|| {
                Error::Connection("Microsoft Entra ID: tenant ID is required".into())
            })?;
            let client = cfg.prop("entra.client_id").ok_or_else(|| {
                Error::Connection("Microsoft Entra ID: client ID is required".into())
            })?;
            let secret = cfg.secret("entra.client_secret").ok_or_else(|| {
                Error::Connection("Microsoft Entra ID: client secret is required".into())
            })?;
            let scope = format!("{RESOURCE}.default");
            let body = http::form(&[
                ("client_id", client),
                ("client_secret", secret),
                ("scope", &scope),
                ("grant_type", "client_credentials"),
            ]);
            let url = format!(
                "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
                http::url_encode(tenant)
            );
            let r = http::post(
                &url,
                &[(
                    "Content-Type",
                    "application/x-www-form-urlencoded".to_owned(),
                )],
                &body,
                Duration::from_secs(20),
            )
            .await?;
            parse_token_response(&r.body)
        }
        Method::AzureCli => {
            let mut args = vec![
                "account",
                "get-access-token",
                "--resource",
                RESOURCE,
                "--query",
                "accessToken",
                "--output",
                "tsv",
            ];
            if let Some(t) = cfg.prop("tenant") {
                args.extend(["--tenant", t]);
            }
            // `az` is a batch script on Windows: run it through cmd.
            let mut cmd = if cfg!(windows) {
                let mut c = tokio::process::Command::new("cmd");
                c.arg("/C").arg("az");
                c
            } else {
                tokio::process::Command::new("az")
            };
            let out = tokio::time::timeout(
                Duration::from_secs(60),
                cmd.args(&args).kill_on_drop(true).output(),
            )
            .await
            .map_err(|_| Error::Connection("Azure CLI: timed out".into()))?
            .map_err(|e| Error::Connection(format!("Azure CLI (az) is not available: {e}")))?;
            if !out.status.success() {
                return Err(Error::Connection(format!(
                    "Azure CLI: {} (sign in with `az login`)",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            let token = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if token.is_empty() {
                return Err(Error::Connection("Azure CLI returned no token".into()));
            }
            Ok(token)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_and_responses() {
        let cfg = ResolvedConfig::new("azuresql");
        assert_eq!(method(&cfg).unwrap(), Method::AzureCli);
        let cfg = cfg.with_secret("entra.client_secret", "s");
        assert_eq!(method(&cfg).unwrap(), Method::ClientSecret);
        let cfg = cfg.with("entra.method", "token");
        assert_eq!(method(&cfg).unwrap(), Method::Token);
        assert_eq!(
            parse_token_response(
                r#"{"token_type":"Bearer","expires_in":3599,"access_token":"eyJ0"}"#
            )
            .unwrap(),
            "eyJ0"
        );
        let e = parse_token_response(
            r#"{"error":"invalid_client","error_description":"AADSTS7000215: Invalid client secret provided."}"#,
        )
        .unwrap_err();
        assert!(e.to_string().contains("AADSTS7000215"));
    }

    /// Client credentials against the real endpoint when
    /// `FERRODB_TEST_ENTRA_TENANT`, `_CLIENT_ID` and `_CLIENT_SECRET` are set.
    #[tokio::test]
    async fn client_credentials_when_configured() {
        let (Ok(t), Ok(c), Ok(s)) = (
            std::env::var("FERRODB_TEST_ENTRA_TENANT"),
            std::env::var("FERRODB_TEST_ENTRA_CLIENT_ID"),
            std::env::var("FERRODB_TEST_ENTRA_CLIENT_SECRET"),
        ) else {
            return;
        };
        let cfg = ResolvedConfig::new("azuresql")
            .with("tenant", t)
            .with("entra.client_id", c)
            .with_secret("entra.client_secret", s);
        assert!(!token(&cfg).await.unwrap().is_empty());
    }
}
