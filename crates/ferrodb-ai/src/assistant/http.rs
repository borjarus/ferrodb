//! HTTP plumbing shared by the providers: the client (rustls with `ring` and
//! the platform verifier, no aws-lc / OpenSSL), error bodies, and a line
//! reader over a streamed body that stops when the request is cancelled.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use reqwest::StatusCode;

use super::AiError;

/// Polled while waiting for the network.
pub(crate) type Cancelled<'a> = &'a (dyn Fn() -> bool + Send + Sync);

pub(crate) fn client() -> Result<reqwest::Client, AiError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls_error = |e: &dyn std::fmt::Display| AiError(format!("TLS configuration: {e}"));
    let verifier =
        rustls_platform_verifier::Verifier::new(provider.clone()).map_err(|e| tls_error(&e))?;
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| tls_error(&e))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .connect_timeout(Duration::from_secs(15))
        // Between two chunks: Anthropic pings, local models may load slowly.
        .read_timeout(Duration::from_secs(300))
        .user_agent(concat!("FerroDb/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| AiError(format!("HTTP client: {e}")))
}

/// `endpoint` + `path` without a doubled slash.
pub(crate) fn join(endpoint: &str, path: &str) -> String {
    format!(
        "{}/{}",
        endpoint.trim().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Runs `fut` unless the request is cancelled first (`None`).
pub(crate) async fn unless_cancelled<F: Future>(
    fut: F,
    cancelled: Cancelled<'_>,
) -> Option<F::Output> {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut fut = std::pin::pin!(fut);
    loop {
        if cancelled() {
            return None;
        }
        tokio::select! {
            out = &mut fut => return Some(out),
            _ = tick.tick() => {}
        }
    }
}

pub(crate) fn send_error(e: reqwest::Error) -> AiError {
    if e.is_connect() {
        AiError(format!("Cannot connect to the AI provider: {e}"))
    } else if e.is_timeout() {
        AiError(format!("The AI provider did not answer in time: {e}"))
    } else {
        AiError(format!("Request to the AI provider failed: {e}"))
    }
}

/// The response, or an error built from a non-success status and its body.
pub(crate) async fn check(resp: reqwest::Response) -> Result<reqwest::Response, AiError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(status_error(status, &body))
}

/// A message for an HTTP error. Understands the error bodies of Anthropic
/// (`{"type":"error","error":{"type","message"}}`), OpenAI
/// (`{"error":{"message"}}`) and Ollama (`{"error":"…"}`).
pub(crate) fn status_error(status: StatusCode, body: &str) -> AiError {
    let detail = error_detail(body).unwrap_or_else(|| {
        let b = body.trim();
        if b.is_empty() {
            status.canonical_reason().unwrap_or("").to_owned()
        } else {
            b.chars().take(300).collect()
        }
    });
    let code = status.as_u16();
    let what = match code {
        401 => "Invalid API key",
        403 => "Permission denied",
        404 => "Not found (check the endpoint and the model)",
        413 => "Request too large",
        429 => "Rate limited, try again later",
        500..=503 => "The AI provider failed",
        529 => "The AI provider is overloaded, try again later",
        _ => "The AI provider rejected the request",
    };
    AiError(format!("{what} (HTTP {code}): {detail}"))
}

fn error_detail(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let e = v.get("error").unwrap_or(&v);
    if let Some(s) = e.as_str() {
        return Some(s.to_owned());
    }
    let message = e.get("message")?.as_str()?;
    Some(match e.get("type").and_then(|t| t.as_str()) {
        Some(t) => format!("{message} ({t})"),
        None => message.to_owned(),
    })
}

/// How [`read_lines`] ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Read {
    /// `on_line` asked to stop.
    Done,
    /// The body ended.
    Eof,
    /// The request was cancelled; the response is dropped (the connection
    /// closes).
    Cancelled,
}

/// Calls `on_line` with every line of the body (without the line break)
/// until it returns `true`, the body ends or the request is cancelled.
pub(crate) async fn read_lines(
    resp: reqwest::Response,
    cancelled: Cancelled<'_>,
    mut on_line: impl FnMut(&str) -> Result<bool, AiError>,
) -> Result<Read, AiError> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if cancelled() {
            return Ok(Read::Cancelled);
        }
        tokio::select! {
            chunk = stream.next() => match chunk {
                None => {
                    if !buf.is_empty() {
                        let line = String::from_utf8_lossy(&buf).into_owned();
                        if on_line(line.trim_end_matches('\r'))? {
                            return Ok(Read::Done);
                        }
                    }
                    return Ok(Read::Eof);
                }
                Some(Err(e)) => {
                    return Err(AiError(format!(
                        "The connection failed while reading the answer: {e}"
                    )));
                }
                Some(Ok(bytes)) => {
                    buf.extend_from_slice(&bytes);
                    while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=pos).collect();
                        let line = String::from_utf8_lossy(&line[..pos]);
                        if on_line(line.trim_end_matches('\r'))? {
                            return Ok(Read::Done);
                        }
                    }
                }
            },
            _ = tick.tick() => {}
        }
    }
}
