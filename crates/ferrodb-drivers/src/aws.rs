//! AWS IAM authentication for Amazon Redshift: temporary database
//! credentials from `redshift:GetClusterCredentials` (a named database
//! user), `GetClusterCredentialsWithIAM` (the user derived from the IAM
//! identity) or Redshift Serverless `GetCredentials`.
//!
//! The calls are signed with Signature V4 here (HMAC-SHA256 from `ring`,
//! already linked for rustls) and sent with [`crate::http`]: one call does
//! not justify the AWS SDK and its `aws-lc-rs` default. AWS credentials
//! come, in order, from the connection properties (`aws.access_key_id` +
//! secret `aws.secret_access_key`), the environment (`AWS_ACCESS_KEY_ID`…),
//! the shared files (`~/.aws/credentials`, `~/.aws/config`: static keys or
//! `credential_process`), and finally the AWS CLI
//! (`aws configure export-credentials`), which covers SSO and assumed roles.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use ferrodb_api::{AuthMethod, DriverProperty, Error, Result};
use ring::{digest, hmac};

use crate::{
    ResolvedConfig, http,
    registry::{property, secret_property},
};

/// Connection form fields of IAM authentication.
pub fn properties() -> Vec<DriverProperty> {
    let iam = |p: DriverProperty| DriverProperty {
        auth: vec![AuthMethod::AwsIam],
        ..p
    };
    vec![
        iam(property(
            "aws.profile",
            "AWS profile",
            Some("default"),
            false,
        )),
        iam(property("aws.region", "AWS region", None, false)),
        iam(property(
            "aws.cluster",
            "Cluster identifier / serverless workgroup (default: from the host)",
            None,
            false,
        )),
        iam(property(
            "aws.db_groups",
            "Database groups (comma-separated)",
            None,
            false,
        )),
        iam(property(
            "aws.auto_create",
            "Create the database user if missing",
            Some("false"),
            false,
        )),
        iam(property(
            "aws.access_key_id",
            "AWS access key ID",
            None,
            false,
        )),
        secret_property(
            "aws.secret_access_key",
            "AWS secret access key",
            &[AuthMethod::AwsIam],
        ),
        secret_property(
            "aws.session_token",
            "AWS session token",
            &[AuthMethod::AwsIam],
        ),
    ]
}

/// AWS access keys.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key", &self.access_key)
            .finish_non_exhaustive()
    }
}

// ------------------------------------------------------------ signing

fn sha256_hex(data: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, data).as_ref())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hmac_sha256(key: &[u8], data: &str) -> Vec<u8> {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::sign(&k, data.as_bytes()).as_ref().to_vec()
}

/// A request to sign.
pub struct SignRequest<'a> {
    pub method: &'a str,
    pub host: &'a str,
    pub path: &'a str,
    /// Canonical (sorted, encoded) query string.
    pub query: &'a str,
    /// Headers besides `host` and `x-amz-date` (lowercase names).
    pub headers: &'a [(&'a str, String)],
    pub body: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    /// `YYYYMMDDTHHMMSSZ`.
    pub amz_date: &'a str,
}

/// Signature V4: the `Authorization` header value for `r`.
pub fn authorization(r: &SignRequest<'_>, c: &Credentials) -> String {
    let mut headers: Vec<(String, String)> = r
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .chain([
            ("host".to_owned(), r.host.to_owned()),
            ("x-amz-date".to_owned(), r.amz_date.to_owned()),
        ])
        .collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    let signed = signed.join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed}\n{}",
        r.method,
        r.path,
        r.query,
        sha256_hex(r.body.as_bytes())
    );
    let day = &r.amz_date[..8];
    let scope = format!("{day}/{}/{}/aws4_request", r.region, r.service);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        r.amz_date,
        sha256_hex(canonical.as_bytes())
    );
    let k_date = hmac_sha256(format!("AWS4{}", c.secret_key).as_bytes(), day);
    let k_region = hmac_sha256(&k_date, r.region);
    let k_service = hmac_sha256(&k_region, r.service);
    let k_signing = hmac_sha256(&k_service, "aws4_request");
    let signature = hex(&hmac_sha256(&k_signing, &to_sign));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
        c.access_key
    )
}

fn amz_date_now() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// Signs and sends a POST to `https://{host}/`.
async fn call(
    host: &str,
    region: &str,
    service: &str,
    content_type: &str,
    extra: &[(&str, String)],
    body: &str,
    c: &Credentials,
) -> Result<http::Response> {
    let date = amz_date_now();
    let mut headers: Vec<(&str, String)> = vec![("content-type", content_type.to_owned())];
    headers.extend(extra.iter().cloned());
    if let Some(t) = &c.token {
        headers.push(("x-amz-security-token", t.clone()));
    }
    let auth = authorization(
        &SignRequest {
            method: "POST",
            host,
            path: "/",
            query: "",
            headers: &headers,
            body,
            region,
            service,
            amz_date: &date,
        },
        c,
    );
    headers.push(("x-amz-date", date));
    headers.push(("authorization", auth));
    http::post(
        &format!("https://{host}/"),
        &headers,
        body,
        Duration::from_secs(20),
    )
    .await
}

// -------------------------------------------------------- credentials

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Sections of an INI file: `[name]` → key → value.
pub fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_owned();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.entry(section.clone())
                .or_default()
                .insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    out
}

fn read_ini(env: &str, default: &str) -> HashMap<String, HashMap<String, String>> {
    let path = std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".aws").join(default)));
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse_ini(&t))
        .unwrap_or_default()
}

fn keys_of(s: &HashMap<String, String>) -> Option<Credentials> {
    Some(Credentials {
        access_key: s.get("aws_access_key_id")?.clone(),
        secret_key: s.get("aws_secret_access_key")?.clone(),
        token: s.get("aws_session_token").cloned(),
    })
}

/// `credential_process` / `aws configure export-credentials` output.
fn process_credentials(json: &str) -> Option<Credentials> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(Credentials {
        access_key: v.get("AccessKeyId")?.as_str()?.to_owned(),
        secret_key: v.get("SecretAccessKey")?.as_str()?.to_owned(),
        token: v
            .get("SessionToken")
            .and_then(|t| t.as_str())
            .map(str::to_owned),
    })
}

async fn run_process(program: &str, args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| Error::Connection(format!("{program}: {e}")))?;
    if !out.status.success() {
        return Err(Error::Connection(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A shell command line split on whitespace (quotes group words).
fn split_command(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    for ch in cmd.chars() {
        match (quote, ch) {
            (None, '"' | '\'') => quote = Some(ch),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (_, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Resolves AWS credentials for `cfg` (see the module documentation).
pub async fn credentials(cfg: &ResolvedConfig) -> Result<Credentials> {
    if let (Some(k), Some(s)) = (
        cfg.prop("aws.access_key_id"),
        cfg.secret("aws.secret_access_key"),
    ) {
        return Ok(Credentials {
            access_key: k.to_owned(),
            secret_key: s.to_owned(),
            token: cfg.secret("aws.session_token").map(str::to_owned),
        });
    }
    let explicit = cfg.prop("aws.profile").filter(|p| *p != "default");
    if explicit.is_none()
        && let (Ok(k), Ok(s)) = (
            std::env::var("AWS_ACCESS_KEY_ID"),
            std::env::var("AWS_SECRET_ACCESS_KEY"),
        )
    {
        return Ok(Credentials {
            access_key: k,
            secret_key: s,
            token: std::env::var("AWS_SESSION_TOKEN").ok(),
        });
    }
    let profile = explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("AWS_PROFILE").ok())
        .unwrap_or_else(|| "default".into());
    let files = read_ini("AWS_SHARED_CREDENTIALS_FILE", "credentials");
    if let Some(c) = files.get(&profile).and_then(keys_of) {
        return Ok(c);
    }
    let config = read_ini("AWS_CONFIG_FILE", "config");
    let section = config
        .get(&format!("profile {profile}"))
        .or_else(|| config.get(&profile));
    if let Some(s) = section {
        if let Some(c) = keys_of(s) {
            return Ok(c);
        }
        if let Some(cmd) = s.get("credential_process") {
            let words = split_command(cmd);
            if let Some((prog, args)) = words.split_first() {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let out = run_process(prog, &args).await?;
                return process_credentials(&out).ok_or_else(|| {
                    Error::Connection("credential_process returned no credentials".into())
                });
            }
        }
    }
    // SSO, assumed roles, IMDS…: the AWS CLI v2 knows them all.
    let out = run_process(
        "aws",
        &[
            "configure",
            "export-credentials",
            "--profile",
            &profile,
            "--format",
            "process",
        ],
    )
    .await
    .map_err(|e| {
        Error::Connection(format!(
            "no AWS credentials for profile `{profile}` (properties, environment, ~/.aws files); \
             the AWS CLI fallback failed too: {e}"
        ))
    })?;
    process_credentials(&out)
        .ok_or_else(|| Error::Connection("the AWS CLI returned no credentials".into()))
}

/// Region of a profile (`~/.aws/config`).
fn profile_region(cfg: &ResolvedConfig) -> Option<String> {
    let profile = cfg
        .prop("aws.profile")
        .map(str::to_owned)
        .or_else(|| std::env::var("AWS_PROFILE").ok())
        .unwrap_or_else(|| "default".into());
    let config = read_ini("AWS_CONFIG_FILE", "config");
    config
        .get(&format!("profile {profile}"))
        .or_else(|| config.get(&profile))
        .and_then(|s| s.get("region").cloned())
}

// ----------------------------------------------------------- Redshift

/// What a Redshift endpoint host tells: provisioned cluster or serverless
/// workgroup, its name and the region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedshiftHost {
    pub name: String,
    pub region: Option<String>,
    pub serverless: bool,
}

/// `examplecluster.abc123.us-west-2.redshift.amazonaws.com`,
/// `wg.123456789012.eu-central-1.redshift-serverless.amazonaws.com`.
pub fn parse_host(host: &str) -> RedshiftHost {
    let parts: Vec<&str> = host.split('.').collect();
    let serverless = host.contains(".redshift-serverless.");
    let marker = parts
        .iter()
        .position(|p| *p == "redshift" || *p == "redshift-serverless");
    RedshiftHost {
        name: parts.first().copied().unwrap_or_default().to_owned(),
        region: marker.filter(|&i| i >= 3).map(|i| parts[i - 1].to_owned()),
        serverless,
    }
}

/// Temporary (user, password) for the database.
pub async fn redshift_credentials(
    cfg: &ResolvedConfig,
    host: &str,
    database: &str,
) -> Result<(String, String)> {
    let parsed = parse_host(host);
    let serverless = parsed.serverless;
    let name = cfg
        .prop("aws.cluster")
        .map(str::to_owned)
        .unwrap_or(parsed.name);
    let region = cfg
        .prop("aws.region")
        .map(str::to_owned)
        .or(parsed.region)
        .or_else(|| std::env::var("AWS_REGION").ok())
        .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
        .or_else(|| profile_region(cfg))
        .ok_or_else(|| {
            Error::Connection("AWS region unknown: set the AWS region property".into())
        })?;
    let creds = credentials(cfg).await?;
    let duration = "900";
    if serverless {
        let body = serde_json::json!({
            "workgroupName": name,
            "dbName": database,
            "durationSeconds": 900,
        })
        .to_string();
        let host = format!("redshift-serverless.{region}.amazonaws.com");
        let r = call(
            &host,
            &region,
            "redshift-serverless",
            "application/x-amz-json-1.1",
            &[("x-amz-target", "RedshiftServerless.GetCredentials".into())],
            &body,
            &creds,
        )
        .await?;
        let v: serde_json::Value = serde_json::from_str(&r.body).unwrap_or_default();
        if !r.ok() {
            return Err(Error::Connection(format!(
                "Redshift Serverless GetCredentials: {}",
                v.get("message")
                    .or_else(|| v.get("Message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or(&r.body)
            )));
        }
        let get = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_owned);
        return get("dbUser").zip(get("dbPassword")).ok_or_else(|| {
            Error::Connection("Redshift Serverless returned no credentials".into())
        });
    }
    let user = cfg.user();
    let groups: Vec<String> = cfg
        .prop("aws.db_groups")
        .map(|g| {
            g.split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let group_keys: Vec<String> = (1..=groups.len())
        .map(|i| format!("DbGroups.member.{i}"))
        .collect();
    let mut params: Vec<(&str, &str)> = vec![
        ("ClusterIdentifier", &name),
        ("DbName", database),
        ("DurationSeconds", duration),
        ("Version", "2012-12-01"),
    ];
    let auto_create = cfg
        .prop("aws.auto_create")
        .map(|v| {
            if v.eq_ignore_ascii_case("true") {
                "true"
            } else {
                "false"
            }
        })
        .unwrap_or("false");
    let action = match user {
        Some(u) => {
            params.push(("DbUser", u));
            params.push(("AutoCreate", auto_create));
            for (k, g) in group_keys.iter().zip(&groups) {
                params.push((k, g));
            }
            "GetClusterCredentials"
        }
        None => "GetClusterCredentialsWithIAM",
    };
    params.push(("Action", action));
    params.sort();
    let body = http::form(&params);
    let host = format!("redshift.{region}.amazonaws.com");
    let r = call(
        &host,
        &region,
        "redshift",
        "application/x-www-form-urlencoded; charset=utf-8",
        &[],
        &body,
        &creds,
    )
    .await?;
    if !r.ok() {
        return Err(Error::Connection(format!(
            "Redshift {action}: {}",
            http::xml_text(&r.body, "Message").unwrap_or(r.body)
        )));
    }
    http::xml_text(&r.body, "DbUser")
        .zip(http::xml_text(&r.body, "DbPassword"))
        .ok_or_else(|| Error::Connection(format!("Redshift {action} returned no credentials")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example of the AWS Signature V4 documentation (IAM ListUsers).
    #[test]
    fn signature_v4_matches_the_documented_example() {
        let c = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            token: None,
        };
        let auth = authorization(
            &SignRequest {
                method: "GET",
                host: "iam.amazonaws.com",
                path: "/",
                query: "Action=ListUsers&Version=2010-05-08",
                headers: &[(
                    "content-type",
                    "application/x-www-form-urlencoded; charset=utf-8".into(),
                )],
                body: "",
                region: "us-east-1",
                service: "iam",
                amz_date: "20150830T123600Z",
            },
            &c,
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
        assert!(!format!("{c:?}").contains("EXAMPLEKEY"));
    }

    #[test]
    fn hosts_profiles_and_processes() {
        assert_eq!(
            parse_host("examplecluster.abc123xyz789.us-west-2.redshift.amazonaws.com"),
            RedshiftHost {
                name: "examplecluster".into(),
                region: Some("us-west-2".into()),
                serverless: false
            }
        );
        let s = parse_host("wg.123456789012.eu-central-1.redshift-serverless.amazonaws.com");
        assert!(s.serverless);
        assert_eq!(s.region.as_deref(), Some("eu-central-1"));
        assert_eq!(parse_host("localhost").region, None);
        let ini = parse_ini(
            "[default]\naws_access_key_id = A\naws_secret_access_key=S\n# c\n[profile dev]\nregion = eu-west-1\ncredential_process = \"C:\\Program Files\\x.exe\" --p dev\n",
        );
        assert_eq!(keys_of(&ini["default"]).unwrap().access_key, "A");
        assert_eq!(ini["profile dev"]["region"], "eu-west-1");
        assert_eq!(
            split_command(&ini["profile dev"]["credential_process"]),
            vec!["C:\\Program Files\\x.exe", "--p", "dev"]
        );
        let c = process_credentials(
            r#"{"Version":1,"AccessKeyId":"K","SecretAccessKey":"S","SessionToken":"T"}"#,
        )
        .unwrap();
        assert_eq!(c.token.as_deref(), Some("T"));
    }
}
