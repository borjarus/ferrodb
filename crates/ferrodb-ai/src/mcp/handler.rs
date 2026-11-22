//! The MCP server handler: one instance per MCP session (client connection).
//! Its tools reach the backend only through the permission layer.

use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

use ferrodb_api::{DataSourceId, SessionId};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, Implementation, InitializeRequestParams,
        InitializeResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
        ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
};

use crate::{agents::Agents, runtime::runtime};

const INSTRUCTIONS: &str = "FerroDb is a database IDE. These tools reach the data sources \
configured in it: list them with `list_data_sources`, browse their objects with \
`list_objects`, read definitions with `describe_object`, run queries in each source's own \
language with `run_query` and inspect plans with `explain_query`. Agents are read-only by \
default and the user may have to approve each call in the FerroDb window, so a call can \
take a while or be denied.";

/// Per-session state: the client's name and the backend sessions opened
/// for its queries (one per data source). Dropped when the MCP session ends.
pub(crate) struct McpSession {
    pub id: u64,
    agents: Weak<Agents>,
    pub client: Mutex<String>,
    queries: Mutex<HashMap<DataSourceId, QuerySlot>>,
}

/// A backend session; the async lock keeps one statement at a time.
pub(crate) type QuerySlot = Arc<tokio::sync::Mutex<Option<SessionId>>>;

impl McpSession {
    fn new(agents: Weak<Agents>) -> Self {
        let id = agents.upgrade().map_or(0, |a| a.new_session());
        Self {
            id,
            agents,
            client: Mutex::new("unknown client".into()),
            queries: Mutex::new(HashMap::new()),
        }
    }

    pub fn client(&self) -> String {
        self.client.lock().unwrap().clone()
    }

    pub fn query_slot(&self, ds: &DataSourceId) -> QuerySlot {
        self.queries
            .lock()
            .unwrap()
            .entry(ds.clone())
            .or_default()
            .clone()
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        let Some(agents) = self.agents.upgrade() else {
            return;
        };
        agents.session_ended(self.id);
        let slots: Vec<QuerySlot> = self
            .queries
            .get_mut()
            .unwrap()
            .drain()
            .map(|(_, s)| s)
            .collect();
        if slots.is_empty() {
            return;
        }
        runtime().spawn(async move {
            for slot in slots {
                let session = slot.lock().await.take();
                if let Some(session) = session
                    && let Err(e) = agents.backend().queries().close_session(session).await
                {
                    tracing::debug!(error = %e, "closing an agent session");
                }
            }
        });
    }
}

pub(crate) struct McpHandler {
    pub(crate) agents: Weak<Agents>,
    pub(crate) session: Arc<McpSession>,
}

impl McpHandler {
    pub fn new(agents: Weak<Agents>) -> Self {
        Self {
            session: Arc::new(McpSession::new(agents.clone())),
            agents,
        }
    }
}

impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ferrodb", env!("CARGO_PKG_VERSION")))
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
            .with_instructions(INSTRUCTIONS)
    }

    /// Versions with an `initialize` handshake only: consent grants and
    /// query sessions belong to an MCP session.
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(
            &ProtocolVersion::LATEST_WITH_INITIALIZE,
        ))
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let name = request.client_info.name.trim();
        let name = if name.is_empty() {
            "unknown client".to_owned()
        } else {
            name.to_owned()
        };
        *self.session.client.lock().unwrap() = name.clone();
        if let Some(agents) = self.agents.upgrade() {
            agents.client_connected(self.session.id, name);
        }
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(super::tools::definitions()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        Ok(self.call(request, context.ct).await.into())
    }
}
