//! BigQuery REST API (v2) next to the ADBC driver, for what ADBC does not
//! expose: cost estimates (`dryRun` jobs report the bytes a query would
//! process), query plans (job statistics: `queryPlan` stages), the list of
//! jobs and their cancellation (`jobs.cancel`), with the job location.
//!
//! Credentials, in order: the service account key JSON of the data source
//! (`credentials` secret or `credentials_file`), then Application Default
//! Credentials (`GOOGLE_APPLICATION_CREDENTIALS`, the gcloud ADC file:
//! service account or authorized user), then `gcloud auth
//! print-access-token`, then the GCE metadata server. A service account
//! token is obtained by signing a JWT with RS256 (`ring`) and exchanging it
//! at the token endpoint (RFC 7523).

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::Engine as _;
use ferrodb_api::{Error, QueryError, Result};
use ferrodb_drivers::{CancellationToken, ResolvedConfig};
use rustls::pki_types::{PrivatePkcs8KeyDer, pem::PemObject};
use serde_json::{Value, json};

pub const API: &str = "https://bigquery.googleapis.com/bigquery/v2";
const SCOPE: &str = "https://www.googleapis.com/auth/bigquery";
const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const METADATA_TOKEN: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";

/// How access tokens are obtained.
#[derive(Clone)]
pub enum Credentials {
    ServiceAccount {
        email: String,
        /// PKCS#8 DER of the RSA key.
        key: Vec<u8>,
        token_uri: String,
    },
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
    },
    /// `gcloud auth print-access-token`.
    Gcloud,
    /// GCE / GKE metadata server.
    Metadata,
    /// A token given as is (tests).
    Token(String),
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ServiceAccount { .. } => "ServiceAccount",
            Self::AuthorizedUser { .. } => "AuthorizedUser",
            Self::Gcloud => "Gcloud",
            Self::Metadata => "Metadata",
            Self::Token(_) => "Token",
        })
    }
}

fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Credentials {
    /// Parses a credentials JSON file's content (service account or
    /// authorized user).
    pub fn from_json(text: &str) -> Result<Self> {
        let bad = |m: &str| Error::Connection(format!("Google credentials: {m}"));
        let v: Value = serde_json::from_str(text).map_err(|e| bad(&e.to_string()))?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        match s("type").as_deref() {
            Some("service_account") => {
                let pem = s("private_key").ok_or_else(|| bad("no private_key"))?;
                let key = PrivatePkcs8KeyDer::from_pem_slice(pem.as_bytes())
                    .map_err(|e| bad(&format!("private_key: {e}")))?;
                Ok(Self::ServiceAccount {
                    email: s("client_email").ok_or_else(|| bad("no client_email"))?,
                    key: key.secret_pkcs8_der().to_vec(),
                    token_uri: s("token_uri").unwrap_or_else(|| GOOGLE_TOKEN_URI.into()),
                })
            }
            Some("authorized_user") => Ok(Self::AuthorizedUser {
                client_id: s("client_id").ok_or_else(|| bad("no client_id"))?,
                client_secret: s("client_secret").ok_or_else(|| bad("no client_secret"))?,
                refresh_token: s("refresh_token").ok_or_else(|| bad("no refresh_token"))?,
            }),
            Some(other) => Err(bad(&format!(
                "credentials of type `{other}` are not supported; use a service account key \
                 or `gcloud auth application-default login`"
            ))),
            None => Err(bad("no `type`")),
        }
    }

    /// The gcloud ADC file of this user.
    fn adc_file() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS") {
            return Some(PathBuf::from(p));
        }
        let base = if cfg!(windows) {
            std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("gcloud"))
        } else {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/gcloud"))
        }?;
        let f = base.join("application_default_credentials.json");
        f.is_file().then_some(f)
    }

    /// Credentials for a data source (see the module documentation).
    pub fn from_config(cfg: &ResolvedConfig) -> Result<Self> {
        if let Some(json) = cfg.secret("credentials") {
            return Self::from_json(json);
        }
        if let Some(path) = cfg.prop("credentials_file") {
            let text = std::fs::read_to_string(path)
                .map_err(|e| Error::Connection(format!("{path}: {e}")))?;
            return Self::from_json(&text);
        }
        if let Some(f) = Self::adc_file()
            && let Ok(text) = std::fs::read_to_string(&f)
        {
            return Self::from_json(&text);
        }
        if gcloud_available() {
            return Ok(Self::Gcloud);
        }
        Ok(Self::Metadata)
    }

    /// The signed JWT assertion of a service account (RS256).
    pub fn assertion(email: &str, key: &[u8], token_uri: &str, iat: u64) -> Result<String> {
        let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = b64url(
            json!({
                "iss": email,
                "scope": SCOPE,
                "aud": token_uri,
                "iat": iat,
                "exp": iat + 3600,
            })
            .to_string()
            .as_bytes(),
        );
        let signing_input = format!("{header}.{claims}");
        let pair = ring::signature::RsaKeyPair::from_pkcs8(key)
            .map_err(|e| Error::Connection(format!("service account key: {e}")))?;
        let mut sig = vec![0u8; pair.public().modulus_len()];
        pair.sign(
            &ring::signature::RSA_PKCS1_SHA256,
            &ring::rand::SystemRandom::new(),
            signing_input.as_bytes(),
            &mut sig,
        )
        .map_err(|_| Error::Connection("service account key: signing failed".into()))?;
        Ok(format!("{signing_input}.{}", b64url(&sig)))
    }
}

fn gcloud_command() -> std::process::Command {
    if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "gcloud"]);
        c
    } else {
        std::process::Command::new("gcloud")
    }
}

fn gcloud_available() -> bool {
    gcloud_command()
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `[line:col]` (1-based) in a GoogleSQL error → byte range in `sql`.
pub fn error_position(message: &str, sql: &str) -> Option<std::ops::Range<usize>> {
    let start = message.rfind("at [")? + 4;
    let end = message[start..].find(']')? + start;
    let (line, col) = message[start..end].split_once(':')?;
    let (line, col): (usize, usize) = (line.trim().parse().ok()?, col.trim().parse().ok()?);
    crate::errors::offset_of(sql, line.checked_sub(1)?, col.checked_sub(1)?).map(|o| o..o + 1)
}

/// A REST error body → [`Error`].
pub fn api_error(status: u16, body: &str, sql: &str) -> Error {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let e = v.get("error");
    let message = e
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(body)
        .to_owned();
    let reason = e
        .and_then(|e| e.get("errors"))
        .and_then(|x| x.get(0))
        .and_then(|x| x.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    match status {
        401 | 403 if reason.as_deref() != Some("accessDenied") || sql.is_empty() => {
            Error::Connection(format!("BigQuery: {message}"))
        }
        _ => Error::Query(QueryError {
            range: error_position(&message, sql),
            message,
            code: reason.or_else(|| Some(status.to_string())),
        }),
    }
}

/// Bytes processed by a (dry-run) job resource.
pub fn bytes_processed(job: &Value) -> Option<u64> {
    let s = job.get("statistics")?;
    let v = s
        .get("totalBytesProcessed")
        .or_else(|| s.get("query").and_then(|q| q.get("totalBytesProcessed")))?;
    match v {
        Value::String(x) => x.parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

/// One row of [`Rest::jobs`].
pub const JOB_COLUMNS: [&str; 9] = [
    "job_id",
    "location",
    "state",
    "statement_type",
    "user_email",
    "creation_time",
    "total_bytes_processed",
    "error",
    "query",
];

/// A job resource → [`JOB_COLUMNS`] values.
pub fn job_row(j: &Value) -> Vec<Option<String>> {
    let s = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
    let r = j.get("jobReference");
    let stats = j.get("statistics");
    let created = stats
        .and_then(|x| x.get("creationTime"))
        .and_then(Value::as_str)
        .and_then(|ms| ms.parse::<i64>().ok())
        .and_then(chrono_iso);
    vec![
        s(r.and_then(|r| r.get("jobId"))),
        s(r.and_then(|r| r.get("location"))),
        s(j.get("state")
            .or_else(|| j.get("status").and_then(|x| x.get("state")))),
        s(stats
            .and_then(|x| x.get("query"))
            .and_then(|q| q.get("statementType"))),
        s(j.get("user_email")),
        created,
        bytes_processed(j).map(|b| b.to_string()),
        s(j.get("status")
            .and_then(|x| x.get("errorResult"))
            .and_then(|x| x.get("message"))),
        s(j.get("configuration")
            .and_then(|c| c.get("query"))
            .and_then(|q| q.get("query"))),
    ]
}

/// Unix milliseconds → `2026-11-21T17:07:07Z`.
fn chrono_iso(ms: i64) -> Option<String> {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    ))
}

/// The REST client of one data source.
pub struct Rest {
    http: reqwest::Client,
    base: String,
    pub project: String,
    pub location: Option<String>,
    pub dataset: Option<String>,
    credentials: Credentials,
    token: tokio::sync::Mutex<Option<(String, Instant)>>,
}

impl std::fmt::Debug for Rest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rest")
            .field("project", &self.project)
            .field("location", &self.location)
            .field("credentials", &self.credentials)
            .finish()
    }
}

fn net(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout
    } else {
        Error::Connection(format!("BigQuery: {e}"))
    }
}

impl Rest {
    pub fn new(
        project: impl Into<String>,
        location: Option<String>,
        dataset: Option<String>,
        credentials: Credentials,
    ) -> Result<Self> {
        let tls = ferrodb_net::client_config(ferrodb_net::TlsMode::VerifyFull, None)
            .map_err(|e| Error::Connection(e.to_string()))?
            .ok_or_else(|| Error::Connection("TLS configuration".into()))?;
        ferrodb_net::install_crypto_provider();
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured((*tls).clone())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Self {
            http,
            base: API.into(),
            project: project.into(),
            location,
            dataset,
            credentials,
            token: tokio::sync::Mutex::new(None),
        })
    }

    pub fn from_config(cfg: &ResolvedConfig) -> Result<Self> {
        let project = cfg
            .prop("project")
            .ok_or_else(|| Error::Connection("BigQuery: the project is required".into()))?;
        Self::new(
            project,
            cfg.prop("location").map(str::to_owned),
            cfg.prop("dataset").map(str::to_owned),
            Credentials::from_config(cfg)?,
        )
    }

    /// Another API root (tests against a local server).
    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    async fn fetch_token(&self) -> Result<(String, Duration)> {
        let parse = |v: Value| -> Result<(String, Duration)> {
            let token = v
                .get("access_token")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Connection("Google token endpoint: no access_token".into()))?
                .to_owned();
            let ttl = v.get("expires_in").and_then(Value::as_u64).unwrap_or(3600);
            Ok((token, Duration::from_secs(ttl)))
        };
        let post = |url: String, form: Vec<(&'static str, String)>| async move {
            let resp = self.http.post(url).form(&form).send().await.map_err(net)?;
            let status = resp.status();
            let body = resp.text().await.map_err(net)?;
            if !status.is_success() {
                return Err(Error::Connection(format!(
                    "Google token endpoint: HTTP {status}: {body}"
                )));
            }
            serde_json::from_str::<Value>(&body).map_err(|e| Error::Connection(e.to_string()))
        };
        match &self.credentials {
            Credentials::Token(t) => Ok((t.clone(), Duration::from_secs(3600))),
            Credentials::ServiceAccount {
                email,
                key,
                token_uri,
            } => {
                let jwt = Credentials::assertion(email, key, token_uri, now_secs())?;
                parse(
                    post(
                        token_uri.clone(),
                        vec![
                            (
                                "grant_type",
                                "urn:ietf:params:oauth:grant-type:jwt-bearer".into(),
                            ),
                            ("assertion", jwt),
                        ],
                    )
                    .await?,
                )
            }
            Credentials::AuthorizedUser {
                client_id,
                client_secret,
                refresh_token,
            } => parse(
                post(
                    GOOGLE_TOKEN_URI.into(),
                    vec![
                        ("grant_type", "refresh_token".into()),
                        ("client_id", client_id.clone()),
                        ("client_secret", client_secret.clone()),
                        ("refresh_token", refresh_token.clone()),
                    ],
                )
                .await?,
            ),
            Credentials::Gcloud => {
                let out = tokio::task::spawn_blocking(|| {
                    gcloud_command()
                        .args(["auth", "print-access-token"])
                        .output()
                })
                .await
                .map_err(|e| Error::Other(e.to_string()))?
                .map_err(|e| Error::Connection(format!("gcloud: {e}")))?;
                if !out.status.success() {
                    return Err(Error::Connection(format!(
                        "gcloud auth print-access-token: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    )));
                }
                // gcloud tokens live one hour; refresh well before.
                Ok((
                    String::from_utf8_lossy(&out.stdout).trim().to_owned(),
                    Duration::from_secs(1800),
                ))
            }
            Credentials::Metadata => {
                let resp = self
                    .http
                    .get(METADATA_TOKEN)
                    .header("Metadata-Flavor", "Google")
                    .timeout(Duration::from_secs(3))
                    .send()
                    .await
                    .map_err(|_| {
                        Error::Connection(
                            "BigQuery: no credentials (no service account key, no application \
                             default credentials, no gcloud, not on Google Cloud)"
                                .into(),
                        )
                    })?;
                parse(resp.json().await.map_err(net)?)
            }
        }
    }

    /// A valid access token (cached until a minute before it expires).
    pub async fn token(&self) -> Result<String> {
        let mut g = self.token.lock().await;
        if let Some((t, until)) = g.as_ref()
            && Instant::now() < *until
        {
            return Ok(t.clone());
        }
        let (t, ttl) = self.fetch_token().await?;
        *g = Some((
            t.clone(),
            Instant::now() + ttl.saturating_sub(Duration::from_secs(60)),
        ));
        Ok(t)
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
        sql: &str,
    ) -> Result<Value> {
        let token = self.token().await?;
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token)
            .query(query);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(net)?;
        let status = resp.status();
        let text = resp.text().await.map_err(net)?;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &text, sql));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| Error::Other(format!("BigQuery: {e}")))
    }

    fn location_query(&self, location: Option<&str>) -> Vec<(&'static str, String)> {
        location
            .or(self.location.as_deref())
            .map(|l| vec![("location", l.to_owned())])
            .unwrap_or_default()
    }

    /// The `jobs.insert` body of a query job.
    pub fn query_job(&self, sql: &str, dry_run: bool) -> Value {
        let mut query = json!({ "query": sql, "useLegacySql": false });
        if let Some(d) = &self.dataset {
            query["defaultDataset"] = json!({ "projectId": self.project, "datasetId": d });
        }
        let mut job = json!({
            "configuration": {
                "dryRun": dry_run,
                "query": query,
                "labels": { "client": "ferrodb" },
            },
        });
        if let Some(l) = &self.location {
            job["jobReference"] = json!({ "projectId": self.project, "location": l });
        }
        job
    }

    /// A dry run of `sql`: the job resource with `totalBytesProcessed`,
    /// the referenced tables and the statement type. Free of charge.
    pub async fn dry_run(&self, sql: &str) -> Result<Value> {
        self.call(
            reqwest::Method::POST,
            &format!("/projects/{}/jobs", self.project),
            &[],
            Some(self.query_job(sql, true)),
            sql,
        )
        .await
    }

    /// Runs `sql` as a job and waits for it (plans with statistics).
    /// Cancelling cancels the job.
    pub async fn run_job(&self, sql: &str, cancel: &CancellationToken) -> Result<Value> {
        let job = self
            .call(
                reqwest::Method::POST,
                &format!("/projects/{}/jobs", self.project),
                &[],
                Some(self.query_job(sql, false)),
                sql,
            )
            .await?;
        let id = job
            .pointer("/jobReference/jobId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("BigQuery: job without an id".into()))?
            .to_owned();
        let location = job
            .pointer("/jobReference/location")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut current = job;
        let mut delay = Duration::from_millis(200);
        loop {
            if current.pointer("/status/state").and_then(Value::as_str) == Some("DONE") {
                if let Some(e) = current
                    .pointer("/status/errorResult/message")
                    .and_then(Value::as_str)
                {
                    return Err(Error::Query(QueryError {
                        range: error_position(e, sql),
                        message: e.to_owned(),
                        code: current
                            .pointer("/status/errorResult/reason")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }));
                }
                return Ok(current);
            }
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.cancelled() => {
                    let _ = self.cancel_job(&id, location.as_deref()).await;
                    return Err(Error::Cancelled);
                }
            }
            delay = (delay * 2).min(Duration::from_secs(2));
            current = self
                .call(
                    reqwest::Method::GET,
                    &format!("/projects/{}/jobs/{id}", self.project),
                    &self.location_query(location.as_deref()),
                    None,
                    sql,
                )
                .await?;
        }
    }

    /// Recent jobs of this user, newest first. `state`: `running`,
    /// `pending`, `done` or `None` for all.
    pub async fn jobs(
        &self,
        state: Option<&str>,
        limit: u32,
        min_creation_ms: Option<u64>,
    ) -> Result<Vec<Value>> {
        let mut q = vec![
            ("maxResults", limit.to_string()),
            ("projection", "full".to_owned()),
        ];
        if let Some(s) = state {
            q.push(("stateFilter", s.to_owned()));
        }
        if let Some(ms) = min_creation_ms {
            q.push(("minCreationTime", ms.to_string()));
        }
        let v = self
            .call(
                reqwest::Method::GET,
                &format!("/projects/{}/jobs", self.project),
                &q,
                None,
                "",
            )
            .await?;
        Ok(v.get("jobs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// `jobs.cancel` (the job stops asynchronously).
    pub async fn cancel_job(&self, job_id: &str, location: Option<&str>) -> Result<()> {
        self.call(
            reqwest::Method::POST,
            &format!("/projects/{}/jobs/{job_id}/cancel", self.project),
            &self.location_query(location),
            None,
            "",
        )
        .await
        .map(|_| ())
    }

    /// Cancels the running jobs of this user whose query text is `sql`,
    /// created after `since_ms` (the ADBC driver does not report its job
    /// id, so a running statement is found by its text).
    pub async fn cancel_matching(&self, sql: &str, since_ms: u64) -> Result<usize> {
        let jobs = self
            .jobs(Some("running"), 50, Some(since_ms.saturating_sub(5000)))
            .await?;
        let mut n = 0;
        for j in jobs {
            let text = j
                .pointer("/configuration/query/query")
                .and_then(Value::as_str);
            if text.map(str::trim) != Some(sql.trim()) {
                continue;
            }
            let (Some(id), loc) = (
                j.pointer("/jobReference/jobId").and_then(Value::as_str),
                j.pointer("/jobReference/location").and_then(Value::as_str),
            ) else {
                continue;
            };
            if self.cancel_job(id, loc).await.is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }
}

/// Shared handle used by the connection and its cancel handle.
pub type SharedRest = Arc<Rest>;

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway 2048-bit RSA key (generated for this test only).
    const TEST_KEY: &str = include_str!("../tests/data/test_rsa_pkcs8.pem");

    #[test]
    fn service_account_jwt_is_signed_with_rs256() {
        let json = json!({
            "type": "service_account",
            "project_id": "p",
            "private_key": TEST_KEY,
            "client_email": "sa@p.iam.gserviceaccount.com",
        })
        .to_string();
        let Credentials::ServiceAccount {
            email,
            key,
            token_uri,
        } = Credentials::from_json(&json).unwrap()
        else {
            panic!()
        };
        assert_eq!(token_uri, GOOGLE_TOKEN_URI);
        let jwt = Credentials::assertion(&email, &key, &token_uri, 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["iss"], "sa@p.iam.gserviceaccount.com");
        assert_eq!(claims["exp"], 1_700_003_600u64);
        assert_eq!(claims["scope"], SCOPE);
        // Verify the signature with the public key of the pair.
        let pair = ring::signature::RsaKeyPair::from_pkcs8(&key).unwrap();
        let public = ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            pair.public().as_ref().to_vec(),
        );
        let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[2])
            .unwrap();
        public
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .unwrap();
        assert!(format!("{:?}", Credentials::from_json(&json).unwrap()) == "ServiceAccount");
    }

    #[test]
    fn errors_jobs_and_dry_runs_from_recorded_responses() {
        let sql = "select x\nfrm t";
        let body = r#"{"error":{"code":400,"message":"Syntax error: Expected end of input but got identifier \"t\" at [2:5]","errors":[{"message":"Syntax error: Expected end of input but got identifier \"t\" at [2:5]","domain":"global","reason":"invalidQuery","location":"q","locationType":"parameter"}],"status":"INVALID_ARGUMENT"}}"#;
        let Error::Query(q) = api_error(400, body, sql) else {
            panic!()
        };
        assert_eq!(q.code.as_deref(), Some("invalidQuery"));
        assert_eq!(&sql[q.range.unwrap()], "t");
        let dry: Value = serde_json::from_str(
            r#"{"kind":"bigquery#job","status":{"state":"DONE"},"statistics":{"creationTime":"1763744827000","totalBytesProcessed":"65935918","query":{"totalBytesProcessed":"65935918","statementType":"SELECT","referencedTables":[{"projectId":"bigquery-public-data","datasetId":"samples","tableId":"shakespeare"}]}},"configuration":{"query":{"query":"select word from `bigquery-public-data.samples.shakespeare`"},"dryRun":true},"jobReference":{"projectId":"p","location":"US"},"user_email":"me@example.com"}"#,
        )
        .unwrap();
        assert_eq!(bytes_processed(&dry), Some(65_935_918));
        let row = job_row(&dry);
        assert_eq!(row[1].as_deref(), Some("US"));
        assert_eq!(row[2].as_deref(), Some("DONE"));
        assert_eq!(row[3].as_deref(), Some("SELECT"));
        assert_eq!(row[5].as_deref(), Some("2025-11-21T17:07:07Z"));
        let r = Rest::new(
            "p",
            Some("EU".into()),
            Some("ds".into()),
            Credentials::Token("t".into()),
        )
        .unwrap();
        let job = r.query_job("select 1", true);
        assert_eq!(job["configuration"]["dryRun"], true);
        assert_eq!(job["jobReference"]["location"], "EU");
        assert_eq!(
            job["configuration"]["query"]["defaultDataset"]["datasetId"],
            "ds"
        );
        assert!(matches!(api_error(401, "{}", ""), Error::Connection(_)));
    }

    /// Live: `FERRODB_TCK_BIGQUERY_PROJECT` (+ ADC or
    /// `FERRODB_TCK_BIGQUERY_CREDENTIALS` = key file path).
    #[tokio::test]
    async fn live_dry_run() {
        let Ok(project) = std::env::var("FERRODB_TCK_BIGQUERY_PROJECT") else {
            return;
        };
        let mut cfg = ResolvedConfig::new("bigquery").with("project", project);
        if let Ok(f) = std::env::var("FERRODB_TCK_BIGQUERY_CREDENTIALS") {
            cfg = cfg.with("credentials_file", f);
        }
        let rest = Rest::from_config(&cfg).unwrap();
        let job = rest
            .dry_run("select word from `bigquery-public-data.samples.shakespeare`")
            .await
            .unwrap();
        assert!(bytes_processed(&job).unwrap() > 0);
    }
}
