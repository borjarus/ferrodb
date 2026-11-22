//! Permission layer for agents. (Phase 9.2 — stub, to be implemented.)

use std::sync::Arc;

use ferrodb_api::{
    AgentEvent, AgentPolicy, AgentService, AuditEntry, Backend, BoxFuture, ConsentDecision,
    ConsentRequest, Error, McpConnectionInfo, McpSettings, McpStatus, Result,
};

use crate::AiOptions;

pub struct Agents {
    #[allow(dead_code)]
    backend: Arc<dyn Backend>,
    #[allow(dead_code)]
    options: AiOptions,
}

impl Agents {
    pub fn new(backend: Arc<dyn Backend>, options: AiOptions) -> Arc<Self> {
        Arc::new(Self { backend, options })
    }
}

impl AgentService for Agents {
    fn policy(&self) -> AgentPolicy {
        AgentPolicy::default()
    }
    fn save_policy(&self, _policy: AgentPolicy) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(Error::Unsupported("not implemented yet".into())) })
    }
    fn mcp_settings(&self) -> McpSettings {
        McpSettings::default()
    }
    fn save_mcp_settings(&self, _settings: McpSettings) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(Error::Unsupported("not implemented yet".into())) })
    }
    fn status(&self) -> McpStatus {
        McpStatus::default()
    }
    fn connection_info(&self) -> Option<McpConnectionInfo> {
        None
    }
    fn regenerate_token(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(Error::Unsupported("not implemented yet".into())) })
    }
    fn subscribe(&self) -> async_channel::Receiver<AgentEvent> {
        let (_tx, rx) = async_channel::unbounded();
        rx
    }
    fn pending(&self) -> Vec<ConsentRequest> {
        Vec::new()
    }
    fn decide(&self, _id: u64, _decision: ConsentDecision) {}
    fn audit(&self, _limit: usize) -> BoxFuture<'_, Result<Vec<AuditEntry>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
