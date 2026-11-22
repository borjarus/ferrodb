//! `ferrodb mcp`: a stdio MCP server for clients that launch a command
//! (Claude Code, Claude Desktop…). It forwards every JSON-RPC message
//! between stdin/stdout and the MCP endpoint of the running IDE: requests,
//! responses, notifications and server-to-client messages alike. The IDE
//! keeps the permission layer; the bridge only carries the bearer token.

use std::{net::Ipv4Addr, path::Path, time::Duration};

use ferrodb_api::{Error, Result};
use rmcp::{
    RoleClient, RoleServer,
    transport::{
        StreamableHttpClientTransport, Transport, async_rw::AsyncRwTransport,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use tokio::task::JoinSet;

use super::PATH;
use crate::{
    agents::{AgentsFile, TOKEN_KEY, TOKEN_REF},
    runtime::runtime,
};

/// Service name of the system keyring entries (the same as the app's).
const KEYRING_SERVICE: &str = "FerroDb";

/// Runs the bridge until stdin closes. The port comes from
/// `<config_dir>/agents.toml`, the token from the system keyring.
///
/// Fails with a message for stderr when the IDE does not run or its MCP
/// server is disabled. Logs must go to stderr: stdout is the protocol.
pub fn run_stdio_bridge(config_dir: &Path) -> Result<()> {
    let settings = AgentsFile::load(config_dir)?;
    if !settings.mcp.enabled {
        return Err(Error::Other(
            "the MCP server of FerroDb is disabled: enable it in FerroDb (Settings → Agents) \
             and keep FerroDb running"
                .into(),
        ));
    }
    let port = settings.mcp.port;
    let store = ferrodb_secrets::system_store(KEYRING_SERVICE);
    let token = store
        .get(TOKEN_REF)
        .map_err(|e| Error::Other(format!("cannot read the MCP token: {e}")))?
        .get(TOKEN_KEY)
        .cloned()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            Error::Other(
                "no MCP token in the system keyring: start FerroDb with the MCP server enabled"
                    .into(),
            )
        })?;
    runtime().block_on(async move {
        let probe = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
        )
        .await;
        if !matches!(probe, Ok(Ok(_))) {
            return Err(Error::Other(format!(
                "FerroDb is not running or its MCP server is not listening on \
                 127.0.0.1:{port}: start FerroDb (Settings → Agents shows the server's state)"
            )));
        }
        let url = format!("http://127.0.0.1:{port}{PATH}");
        let (stdin, stdout) = rmcp::transport::stdio();
        forward(
            AsyncRwTransport::new_server(stdin, stdout),
            http_transport(&url, &token),
        )
        .await
        .map_err(Error::Other)
    })
}

/// A client transport to the IDE's endpoint.
pub(crate) fn http_transport(url: &str, token: &str) -> impl Transport<RoleClient> + 'static {
    // reqwest may be built with `rustls-no-provider` (feature unification
    // with the drivers): give it the workspace's ring provider. The
    // connection itself is plain HTTP on loopback.
    let _ = rustls::crypto::ring::default_provider().install_default();
    StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token.to_owned()),
    )
}

/// Forwards messages between the stdio side (`local`, where the MCP client
/// is) and the IDE (`remote`) until either side closes. Returns `Ok` when
/// the client closed stdin, `Err` when the IDE went away.
pub(crate) async fn forward<L, R>(mut local: L, mut remote: R) -> std::result::Result<(), String>
where
    L: Transport<RoleServer>,
    R: Transport<RoleClient>,
{
    // Sends to the IDE are HTTP requests that may stay open while a tool
    // waits for consent: they run concurrently, like in rmcp's own service.
    let mut sends: JoinSet<std::result::Result<(), String>> = JoinSet::new();
    let result = loop {
        tokio::select! {
            message = local.receive() => match message {
                Some(m) => {
                    let send = remote.send(m);
                    sends.spawn(async move { send.await.map_err(|e| e.to_string()) });
                }
                None => break Ok(()),
            },
            message = remote.receive() => match message {
                Some(m) => {
                    if let Err(e) = local.send(m).await {
                        break Err(format!("cannot write to stdout: {e}"));
                    }
                }
                None => break Err("the FerroDb MCP server closed the connection".to_owned()),
            },
            Some(done) = sends.join_next(), if !sends.is_empty() => {
                if let Ok(Err(e)) = done {
                    tracing::warn!(error = %e, "forwarding a message to FerroDb");
                }
            }
        }
    };
    sends.abort_all();
    let _ = remote.close().await;
    let _ = local.close().await;
    result
}
