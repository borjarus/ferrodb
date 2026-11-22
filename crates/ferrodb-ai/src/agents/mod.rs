//! Permission layer for agents (plan, phase 9.2).
//!
//! Every MCP tool call goes through [`Agents::authorize`]: hidden data sources
//! do not exist for agents, the policy refuses what the access level does not
//! allow, and the rest is allowed or asked according to the consent mode
//! (see [`policy::decide`]). Every call ends up in the audit log.
//!
//! The policy and the MCP settings live in `<config_dir>/agents.toml`, the
//! bearer token of the MCP server in the secret store (`secret://mcp`).

pub mod audit;
pub mod consent;
pub mod policy;

#[cfg(test)]
mod tests;

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use ferrodb_api::{
    AgentAccess, AgentEvent, AgentPolicy, AgentService, AuditEntry, AuditOutcome, Backend,
    BoxFuture, ConsentDecision, ConsentRequest, DataSourceId, DataSourceInfo, Error,
    McpConnectionInfo, McpSettings, McpStatus, Result, RiskKind,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use self::{
    audit::AuditLog,
    consent::{ConsentBroker, ConsentOutcome},
    policy::{CallClass, PolicyDecision},
};
use crate::{AiOptions, mcp};

/// Settings file in the configuration directory.
pub const SETTINGS_FILE: &str = "agents.toml";
/// Secret reference of the MCP bearer token (key [`TOKEN_KEY`]).
pub const TOKEN_REF: &str = "secret://mcp";
pub const TOKEN_KEY: &str = "token";

/// Contents of `agents.toml`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsFile {
    pub policy: AgentPolicy,
    pub mcp: McpSettings,
}

impl AgentsFile {
    /// Reads `<config_dir>/agents.toml`; a missing file gives the defaults.
    pub fn load(config_dir: &Path) -> Result<Self> {
        let path = config_dir.join(SETTINGS_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).map_err(|e| Error::Other(format!("{}: {e}", path.display())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::Other(format!("read {}: {e}", path.display()))),
        }
    }

    fn save(&self, config_dir: &Path) -> Result<()> {
        let path = config_dir.join(SETTINGS_FILE);
        let text = toml::to_string_pretty(self).map_err(|e| Error::Other(e.to_string()))?;
        std::fs::create_dir_all(config_dir)
            .and_then(|()| std::fs::write(&path, text))
            .map_err(|e| Error::Other(format!("write {}: {e}", path.display())))
    }
}

/// Subscribers of [`AgentEvent`]s; closed receivers are dropped on the next
/// event.
#[derive(Default)]
pub struct Events(Mutex<Vec<async_channel::Sender<AgentEvent>>>);

impl Events {
    pub fn subscribe(&self) -> async_channel::Receiver<AgentEvent> {
        let (tx, rx) = async_channel::unbounded();
        self.0.lock().unwrap().push(tx);
        rx
    }

    pub fn emit(&self, event: AgentEvent) {
        self.0
            .lock()
            .unwrap()
            .retain(|tx| tx.try_send(event.clone()).is_ok());
    }
}

/// A tool call as the permission layer sees it.
#[derive(Clone, Debug)]
pub struct Call {
    /// MCP session (see [`Agents::new_session`]).
    pub session: u64,
    pub client: String,
    pub tool: &'static str,
    /// The data source, already checked to be visible to agents.
    pub data_source: Option<DataSourceInfo>,
    pub text: Option<String>,
    pub class: CallClass,
    pub risks: Vec<RiskKind>,
    /// One line for the consent dialog.
    pub summary: String,
}

/// What [`Agents::authorize`] decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allowed(AuditOutcome),
    Refused {
        outcome: AuditOutcome,
        /// Shown to the agent as the tool error.
        message: String,
    },
}

/// State of the MCP server.
#[derive(Default)]
struct ServerSlot {
    running: Option<mcp::Running>,
    error: Option<String>,
    /// Connected clients: (MCP session, client name).
    clients: Vec<(u64, String)>,
}

pub struct Agents {
    backend: Arc<dyn Backend>,
    options: AiOptions,
    settings: Mutex<AgentsFile>,
    events: Events,
    consent: ConsentBroker,
    audit: AuditLog,
    token: Mutex<Option<String>>,
    server: Mutex<ServerSlot>,
    next_session: AtomicU64,
    me: Weak<Agents>,
}

impl Agents {
    /// Loads the settings and starts the MCP server when it is enabled.
    pub fn new(backend: Arc<dyn Backend>, options: AiOptions) -> Arc<Self> {
        let settings = match &options.config_dir {
            Some(dir) => AgentsFile::load(dir).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "agents settings ignored");
                AgentsFile::default()
            }),
            None => AgentsFile::default(),
        };
        let enabled = settings.mcp.enabled;
        let agents = Arc::new_cyclic(|me| Self {
            backend,
            audit: AuditLog::new(options.data_dir.clone()),
            options,
            settings: Mutex::new(settings),
            events: Events::default(),
            consent: ConsentBroker::default(),
            token: Mutex::new(None),
            server: Mutex::new(ServerSlot::default()),
            next_session: AtomicU64::new(0),
            me: me.clone(),
        });
        if enabled {
            agents.start_server();
        }
        agents
    }

    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    fn persist(&self) -> Result<()> {
        match &self.options.config_dir {
            Some(dir) => self.settings.lock().unwrap().save(dir),
            None => Ok(()),
        }
    }

    // ------------------------------------------------------------ token

    /// The bearer token, generated and stored on first use.
    pub fn token(&self) -> Result<String> {
        let mut cached = self.token.lock().unwrap();
        if let Some(t) = &*cached {
            return Ok(t.clone());
        }
        let secrets = &self.options.secrets;
        let stored = secrets
            .get(TOKEN_REF)
            .map_err(|e| Error::Other(e.to_string()))?
            .get(TOKEN_KEY)
            .filter(|t| !t.is_empty())
            .cloned();
        let token = match stored {
            Some(t) => t,
            None => {
                let t = new_token()?;
                self.store_token(&t)?;
                t
            }
        };
        *cached = Some(token.clone());
        Ok(token)
    }

    fn store_token(&self, token: &str) -> Result<()> {
        let secrets = [(TOKEN_KEY.to_owned(), token.to_owned())].into();
        self.options
            .secrets
            .set(TOKEN_REF, &secrets)
            .map_err(|e| Error::Other(e.to_string()))
    }

    // ------------------------------------------------------------ server

    /// Starts the server when the settings enable it (the previous one must
    /// be stopped). Binding happens here, so a busy port shows at once.
    fn start_server(&self) {
        let settings = self.mcp_settings();
        {
            let mut slot = self.server.lock().unwrap();
            slot.error = None;
            if settings.enabled {
                match self.token() {
                    Err(e) => slot.error = Some(format!("cannot store the access token: {e}")),
                    Ok(_) => match mcp::start(self.me.clone(), settings.port) {
                        Ok(running) => {
                            tracing::info!(url = %running.url(), "MCP server started");
                            slot.running = Some(running);
                        }
                        Err(e) => {
                            let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                                format!("port {} is already in use", settings.port)
                            } else {
                                format!("cannot listen on 127.0.0.1:{}: {e}", settings.port)
                            };
                            tracing::warn!(error = %msg, "MCP server not started");
                            slot.error = Some(msg);
                        }
                    },
                }
            }
        }
        self.emit_status();
    }

    /// Stops the server; the returned task ends once its listener is closed.
    fn stop_server(&self) -> Option<tokio::task::JoinHandle<()>> {
        let running = {
            let mut slot = self.server.lock().unwrap();
            slot.clients.clear();
            slot.running.take()
        }?;
        running.cancel.cancel();
        tracing::info!("MCP server stopped");
        Some(running.task)
    }

    async fn restart_server(&self) {
        if let Some(task) = self.stop_server() {
            task.abort();
            let _ = task.await;
        }
        self.start_server();
    }

    fn emit_status(&self) {
        self.events.emit(AgentEvent::Status(self.status()));
    }

    // ------------------------------------------------------------ sessions

    /// A new MCP session (one per connected client).
    pub fn new_session(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The client of `session` introduced itself (`initialize`).
    pub fn client_connected(&self, session: u64, name: String) {
        {
            let mut slot = self.server.lock().unwrap();
            slot.clients.retain(|(s, _)| *s != session);
            slot.clients.push((session, name));
        }
        self.emit_status();
    }

    /// `session` ended: its grants are forgotten.
    pub fn session_ended(&self, session: u64) {
        self.consent.end_session(session);
        let changed = {
            let mut slot = self.server.lock().unwrap();
            let before = slot.clients.len();
            slot.clients.retain(|(s, _)| *s != session);
            before != slot.clients.len()
        };
        if changed {
            self.emit_status();
        }
    }

    // ------------------------------------------------------------ permission

    /// The data source if it exists and is not hidden from agents.
    pub fn visible_source(&self, id: &DataSourceId) -> Option<DataSourceInfo> {
        let info = self.backend.data_sources().get(id)?;
        (self.policy().access(id) != AgentAccess::Hidden).then_some(info)
    }

    /// Applies the policy to `call`, asking the user when it says so.
    /// `cancel` is the client's cancellation of the call.
    pub async fn authorize(&self, call: &Call, cancel: &CancellationToken) -> Verdict {
        let policy = self.policy();
        let (access, read_only) = match &call.data_source {
            Some(ds) => (policy.access(&ds.config.id), ds.config.read_only),
            // Calls about no particular source (listing the sources).
            None => (AgentAccess::MetadataOnly, false),
        };
        match policy::decide(access, policy.consent, read_only, call.class) {
            PolicyDecision::NotFound => Verdict::Refused {
                outcome: AuditOutcome::DeniedByPolicy,
                message: "data source not found".into(),
            },
            PolicyDecision::Deny(message) => Verdict::Refused {
                outcome: AuditOutcome::DeniedByPolicy,
                message,
            },
            PolicyDecision::Allow => Verdict::Allowed(AuditOutcome::AllowedByPolicy),
            PolicyDecision::Ask => {
                let request = ConsentRequest {
                    id: 0,
                    client: call.client.clone(),
                    tool: call.tool.to_owned(),
                    data_source: call.data_source.as_ref().map(|d| d.config.id.clone()),
                    data_source_name: call.data_source.as_ref().map(|d| d.config.name.clone()),
                    text: call.text.clone(),
                    modifies: call.class == CallClass::Modify,
                    risks: call.risks.clone(),
                    summary: call.summary.clone(),
                    at: SystemTime::now(),
                };
                let timeout = Duration::from_secs(policy.consent_timeout_secs.max(1));
                match self
                    .consent
                    .ask(&self.events, call.session, request, timeout, cancel)
                    .await
                {
                    ConsentOutcome::Allowed | ConsentOutcome::AllowedForSession => {
                        Verdict::Allowed(AuditOutcome::AllowedByUser)
                    }
                    ConsentOutcome::Denied => Verdict::Refused {
                        outcome: AuditOutcome::DeniedByUser,
                        message: "denied by the user".into(),
                    },
                    ConsentOutcome::TimedOut => Verdict::Refused {
                        outcome: AuditOutcome::TimedOut,
                        message: format!(
                            "the user did not answer within {} s; the call was not run",
                            timeout.as_secs()
                        ),
                    },
                    ConsentOutcome::Cancelled => Verdict::Refused {
                        outcome: AuditOutcome::DeniedByUser,
                        message: "cancelled by the client".into(),
                    },
                }
            }
        }
    }

    /// Writes an audit entry and tells the subscribers.
    pub fn record(&self, entry: AuditEntry) {
        self.audit.append(&entry);
        self.events.emit(AgentEvent::Audit(entry));
    }
}

impl Drop for Agents {
    fn drop(&mut self) {
        if let Some(running) = self.server.get_mut().unwrap().running.take() {
            running.cancel.cancel();
            running.task.abort();
        }
    }
}

/// 32 random bytes, hex-encoded.
fn new_token() -> Result<String> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::Other("no secure random source".into()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `{"mcpServers":{"ferrodb":{"command":"<exe>","args":["mcp"]}}}`.
fn client_config(exe: &Path) -> String {
    let config = serde_json::json!({
        "mcpServers": {
            "ferrodb": {
                "command": exe.to_string_lossy(),
                "args": ["mcp"],
            }
        }
    });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

impl AgentService for Agents {
    fn policy(&self) -> AgentPolicy {
        self.settings.lock().unwrap().policy.clone()
    }

    fn save_policy(&self, policy: AgentPolicy) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.settings.lock().unwrap().policy = policy;
            self.persist()
        })
    }

    fn mcp_settings(&self) -> McpSettings {
        self.settings.lock().unwrap().mcp.clone()
    }

    fn save_mcp_settings(&self, settings: McpSettings) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let changed = {
                let mut s = self.settings.lock().unwrap();
                let changed = s.mcp != settings;
                s.mcp = settings;
                changed
            };
            let saved = self.persist();
            let running = self.server.lock().unwrap().running.is_some();
            let enabled = self.mcp_settings().enabled;
            // Restart on any change, and retry a server that failed to start.
            if changed || running != enabled {
                self.restart_server().await;
            }
            saved
        })
    }

    fn status(&self) -> McpStatus {
        let slot = self.server.lock().unwrap();
        McpStatus {
            running: slot.running.is_some(),
            url: slot.running.as_ref().map(mcp::Running::url),
            clients: slot.clients.iter().map(|(_, n)| n.clone()).collect(),
            error: slot.error.clone(),
        }
    }

    fn connection_info(&self) -> Option<McpConnectionInfo> {
        let url = self.status().url?;
        let token = self.token().ok()?;
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ferrodb"));
        Some(McpConnectionInfo {
            url,
            token,
            client_config: client_config(&exe),
        })
    }

    fn regenerate_token(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let token = new_token()?;
            self.store_token(&token)?;
            *self.token.lock().unwrap() = Some(token);
            // Sessions opened with the old token end with the old server.
            if self.server.lock().unwrap().running.is_some() {
                self.restart_server().await;
            }
            Ok(())
        })
    }

    fn subscribe(&self) -> async_channel::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    fn pending(&self) -> Vec<ConsentRequest> {
        self.consent.pending()
    }

    fn decide(&self, id: u64, decision: ConsentDecision) {
        self.consent.decide(id, decision);
    }

    fn audit(&self, limit: usize) -> BoxFuture<'_, Result<Vec<AuditEntry>>> {
        Box::pin(async move { self.audit.read(limit) })
    }
}
