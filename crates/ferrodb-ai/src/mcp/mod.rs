//! MCP server (plan, phase 9.3) and the `ferrodb mcp` stdio bridge.
//!
//! The server speaks the streamable HTTP transport of MCP (`rmcp`) on
//! `http://127.0.0.1:<port>/mcp`, plain HTTP/1 on the loopback interface
//! only. Every request must carry `Authorization: Bearer <token>` (401
//! otherwise) and requests whose `Host` or `Origin` is not a loopback name
//! are rejected (403), so a web page cannot reach the server through DNS
//! rebinding. Tools go through the permission layer ([`crate::agents`]).

mod bridge;
mod convert;
mod handler;
mod tools;

#[cfg(test)]
mod tests;

use std::{
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Weak},
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderMap, Request, Response, StatusCode, header};
use http_body_util::{BodyExt as _, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;

pub use self::bridge::run_stdio_bridge;
use self::handler::McpHandler;
use crate::{agents::Agents, runtime::runtime};

/// Path of the MCP endpoint.
pub const PATH: &str = "/mcp";

type Service = StreamableHttpService<McpHandler, LocalSessionManager>;
type Body = BoxBody<Bytes, Infallible>;

/// A running server.
pub struct Running {
    pub(crate) cancel: CancellationToken,
    pub(crate) task: tokio::task::JoinHandle<()>,
    addr: SocketAddr,
}

impl Running {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{PATH}", self.addr.port())
    }
}

/// Binds `127.0.0.1:port` (0 = any free port) and serves MCP on the shared
/// runtime until [`Running::cancel`] is cancelled.
pub(crate) fn start(agents: Weak<Agents>, port: u16) -> std::io::Result<Running> {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let rt = runtime();
    let _guard = rt.enter();
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let cancel = CancellationToken::new();
    // rmcp validates Host and Origin as well; the checks below come first
    // so that they also guard the bearer token check.
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(["localhost", "127.0.0.1", "::1"])
        .with_allowed_origins(["http://localhost:*", "http://127.0.0.1:*", "http://[::1]:*"])
        .with_cancellation_token(cancel.child_token());
    let factory = agents.clone();
    let service: Service = StreamableHttpService::new(
        move || Ok(McpHandler::new(factory.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let task = rt.spawn(accept(listener, service, agents, cancel.clone()));
    Ok(Running { cancel, task, addr })
}

async fn accept(
    listener: tokio::net::TcpListener,
    service: Service,
    agents: Weak<Agents>,
    cancel: CancellationToken,
) {
    loop {
        let stream = tokio::select! {
            r = listener.accept() => match r {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::debug!(error = %e, "MCP accept");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = cancel.cancelled() => break,
        };
        let service = service.clone();
        let agents = agents.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let serve = hyper::service::service_fn(move |req| {
                let service = service.clone();
                let agents = agents.clone();
                async move { Ok::<_, Infallible>(handle(req, &service, &agents).await) }
            });
            let conn = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), serve);
            tokio::select! {
                r = conn => if let Err(e) = r {
                    tracing::debug!(error = %e, "MCP connection");
                },
                // Stopping the server drops open connections (SSE streams too).
                _ = cancel.cancelled() => {}
            }
        });
    }
}

async fn handle(
    req: Request<Incoming>,
    service: &Service,
    agents: &Weak<Agents>,
) -> Response<Body> {
    if req.uri().path() != PATH {
        return plain(StatusCode::NOT_FOUND, "not found; the MCP endpoint is /mcp");
    }
    if let Err(reason) = check_origin(req.headers()) {
        return plain(StatusCode::FORBIDDEN, reason);
    }
    let token = match agents.upgrade().map(|a| a.token()) {
        Some(Ok(t)) => t,
        _ => {
            return plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "the MCP server is stopping",
            );
        }
    };
    if !bearer_matches(req.headers(), &token) {
        let mut res = plain(StatusCode::UNAUTHORIZED, "missing or invalid bearer token");
        res.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            http::HeaderValue::from_static("Bearer"),
        );
        return res;
    }
    service.handle(req).await
}

fn plain(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut res = Response::new(Full::new(Bytes::from_static(text.as_bytes())).boxed());
    *res.status_mut() = status;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    res
}

/// `127.0.0.1`, `localhost` or `::1`, with or without a port / brackets.
fn is_loopback_authority(authority: &str) -> bool {
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// Rejects requests whose `Host` or `Origin` is not loopback (DNS
/// rebinding: a page of another site resolving its name to 127.0.0.1).
fn check_origin(headers: &HeaderMap) -> Result<(), &'static str> {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !is_loopback_authority(host) {
        return Err("forbidden: the Host header must name the loopback interface");
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let ok = origin
            .to_str()
            .ok()
            .and_then(|o| o.parse::<http::Uri>().ok())
            .is_some_and(|uri| {
                matches!(uri.scheme_str(), Some("http" | "https"))
                    && uri
                        .authority()
                        .is_some_and(|a| is_loopback_authority(a.as_str()))
            });
        if !ok {
            return Err("forbidden: requests from other origins are not allowed");
        }
    }
    Ok(())
}

/// `Authorization: Bearer <token>`, compared in constant time.
fn bearer_matches(headers: &HeaderMap, token: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some((scheme, given)) = value.trim().split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return false;
    }
    let (a, b) = (given.trim().as_bytes(), token.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
