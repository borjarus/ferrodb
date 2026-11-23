//! AI and agents (plan, phase 9; version 1.1, optional).
//!
//! - [`AssistantService`]: a natural-language assistant that writes, explains
//!   and fixes queries in the language of a data source (SQL dialects, MQL,
//!   CQL, SQL++, PartiQL, Redis commands), aware of its schema. It never runs
//!   anything: suggested queries go to a console where the user runs them.
//! - [`AgentService`]: the permission layer for external agents and the MCP
//!   server through which they reach the IDE's data sources. Agents are
//!   read-only by default and every call needs the user's consent unless the
//!   user relaxed the policy.
//!
//! Both are optional: [`NoAssistant`] and [`NoAgents`] are the defaults of
//! [`crate::Backend`].

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::{BoxFuture, CancelHandle, DataSourceId, QueryLanguage, Result, RiskKind};

// ---------------------------------------------------------------------------
// Assistant
// ---------------------------------------------------------------------------

/// LLM provider family. The wire protocol is chosen by the family; the
/// endpoint can point at any compatible server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiProvider {
    /// Anthropic Messages API (`/v1/messages`).
    #[default]
    Anthropic,
    /// OpenAI Chat Completions and compatible servers (OpenAI, Azure OpenAI,
    /// LM Studio, vLLM, llama.cpp server…).
    OpenAiCompatible,
    /// Ollama's native API (`/api/chat`), local models.
    Ollama,
}

impl AiProvider {
    pub const ALL: [AiProvider; 3] = [Self::Anthropic, Self::OpenAiCompatible, Self::Ollama];

    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic (Claude)",
            Self::OpenAiCompatible => "OpenAI-compatible",
            Self::Ollama => "Ollama (local)",
        }
    }

    /// Endpoint used when the settings leave it empty.
    pub fn default_endpoint(self) -> &'static str {
        match self {
            Self::Anthropic => "https://api.anthropic.com",
            Self::OpenAiCompatible => "https://api.openai.com/v1",
            Self::Ollama => "http://localhost:11434",
        }
    }

    /// Model used when the settings leave it empty (`None` = must be chosen).
    pub fn default_model(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("claude-opus-5-5"),
            Self::OpenAiCompatible | Self::Ollama => None,
        }
    }

    /// The provider needs an API key (Ollama and local servers do not).
    pub fn needs_key(self) -> bool {
        matches!(self, Self::Anthropic)
    }
}

/// Assistant settings (stored in the configuration directory; the API key
/// lives in the system keyring and is never returned).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub enabled: bool,
    pub provider: AiProvider,
    /// Empty = [`AiProvider::default_endpoint`].
    pub endpoint: String,
    /// Empty = [`AiProvider::default_model`].
    pub model: String,
    /// Upper bound of one answer, in tokens.
    pub max_tokens: u32,
    /// Send table / collection / column names and types with the question.
    /// Without it the model only sees the database product and the question.
    pub share_schema: bool,
    /// Send a few sample values per column (never rows of query results).
    /// Off by default.
    pub share_samples: bool,
    /// Upper bound of the schema part of a prompt, in characters.
    pub schema_budget: usize,
    /// Read-only: an API key is stored for this provider.
    #[serde(skip)]
    pub has_key: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: AiProvider::default(),
            endpoint: String::new(),
            model: String::new(),
            max_tokens: 16_000,
            share_schema: true,
            share_samples: false,
            schema_budget: 24_000,
            has_key: false,
        }
    }
}

impl AiSettings {
    pub fn effective_endpoint(&self) -> &str {
        if self.endpoint.trim().is_empty() {
            self.provider.default_endpoint()
        } else {
            self.endpoint.trim()
        }
    }

    pub fn effective_model(&self) -> Option<&str> {
        if self.model.trim().is_empty() {
            self.provider.default_model()
        } else {
            Some(self.model.trim())
        }
    }
}

/// What the user asks the assistant to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistantTask {
    /// Write a query from a description ("top 10 customers by revenue").
    Generate,
    /// Explain the query in `context`.
    Explain,
    /// Fix the query in `context` that failed with `error`.
    Fix { error: String },
    /// Suggest a faster equivalent of the query in `context` (and indexes).
    Optimize,
    /// Free conversation about the data source.
    Chat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssistantRequest {
    /// The data source the question is about; its language, dialect and
    /// schema shape the prompt. `None` = no database context.
    pub data_source: Option<DataSourceId>,
    /// Current schema / database of the console.
    pub schema: Option<String>,
    pub task: AssistantTask,
    /// The user's words.
    pub prompt: String,
    /// Query text the task is about (selection or statement under cursor).
    pub context: Option<String>,
    /// Earlier turns of the same conversation, oldest first.
    pub history: Vec<ChatTurn>,
}

impl AssistantRequest {
    pub fn generate(data_source: Option<DataSourceId>, prompt: impl Into<String>) -> Self {
        Self {
            data_source,
            schema: None,
            task: AssistantTask::Generate,
            prompt: prompt.into(),
            context: None,
            history: Vec::new(),
        }
    }
}

/// A query found in an answer, checked by the language service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestedQuery {
    pub text: String,
    pub language: QueryLanguage,
    /// The query changes data or schema (it is never run automatically).
    pub modifies: bool,
    /// Risks the console would ask to confirm (`DELETE` without `WHERE`…).
    pub risks: Vec<RiskKind>,
    /// Syntax problems reported by the language service (messages).
    pub problems: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cached_input: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssistantAnswer {
    /// Full answer (Markdown).
    pub text: String,
    /// Code blocks of the answer in the data source's language, in order.
    pub queries: Vec<SuggestedQuery>,
    pub usage: Option<TokenUsage>,
    /// Model that answered.
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistantEvent {
    /// What was sent along with the question (shown so the user knows).
    Context {
        /// Objects whose definitions were included.
        objects: Vec<String>,
        /// Size of the prompt in characters.
        chars: usize,
    },
    /// Next piece of the answer text.
    Delta(String),
    Finished(AssistantAnswer),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct AssistantHandle {
    pub events: async_channel::Receiver<AssistantEvent>,
    pub cancel: CancelHandle,
}

impl AssistantHandle {
    /// A handle whose only event is `Failed(message)`.
    pub fn failed(message: impl Into<String>) -> Self {
        let (tx, rx) = async_channel::bounded(1);
        let _ = tx.try_send(AssistantEvent::Failed(message.into()));
        Self {
            events: rx,
            cancel: CancelHandle::new(),
        }
    }
}

pub trait AssistantService: Send + Sync {
    /// The assistant is compiled in (false for [`NoAssistant`]).
    fn available(&self) -> bool {
        true
    }
    fn settings(&self) -> AiSettings;
    /// Saves the settings; `api_key`: `Some("")` removes the stored key,
    /// `None` keeps it.
    fn save_settings(
        &self,
        settings: AiSettings,
        api_key: Option<String>,
    ) -> BoxFuture<'_, Result<()>>;
    /// Checks the provider with a tiny request; returns the model's name.
    fn test(&self) -> BoxFuture<'_, Result<String>>;
    /// Models offered by the provider (for the settings picker).
    fn models(&self) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    /// Streams an answer. Fails at once when the assistant is disabled.
    fn ask(&self, request: AssistantRequest) -> AssistantHandle;
}

/// Default: no assistant.
pub struct NoAssistant;

impl AssistantService for NoAssistant {
    fn available(&self) -> bool {
        false
    }
    fn settings(&self) -> AiSettings {
        AiSettings::default()
    }
    fn save_settings(
        &self,
        _settings: AiSettings,
        _api_key: Option<String>,
    ) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(crate::Error::Unsupported("AI assistant".into())) })
    }
    fn test(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async { Err(crate::Error::Unsupported("AI assistant".into())) })
    }
    fn ask(&self, _request: AssistantRequest) -> AssistantHandle {
        AssistantHandle::failed("The AI assistant is not available in this build.")
    }
}

// ---------------------------------------------------------------------------
// Agents: permission layer and MCP server
// ---------------------------------------------------------------------------

/// What agents may do on a data source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentAccess {
    /// Invisible to agents.
    Hidden,
    /// Metadata only: listings, definitions, no queries.
    MetadataOnly,
    /// Metadata and read-only queries (the default).
    #[default]
    ReadOnly,
    /// Queries that change data or schema too (always with consent).
    ReadWrite,
}

/// When the user is asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsentMode {
    /// Every tool call (the default).
    #[default]
    EveryCall,
    /// Queries only; listings and definitions are allowed without asking.
    QueriesOnly,
    /// Writes only; read-only calls are allowed without asking.
    WritesOnly,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentPolicy {
    /// Access of sources not listed in `sources`.
    pub default_access: AgentAccess,
    pub sources: Vec<(DataSourceId, AgentAccess)>,
    pub consent: ConsentMode,
    /// Rows / documents / keys returned by one query call.
    pub max_rows: usize,
    /// Seconds a consent request waits before it is denied.
    pub consent_timeout_secs: u64,
}

impl Default for AgentPolicy {
    fn default() -> Self {
        Self {
            default_access: AgentAccess::ReadOnly,
            sources: Vec::new(),
            consent: ConsentMode::EveryCall,
            max_rows: 200,
            consent_timeout_secs: 120,
        }
    }
}

impl AgentPolicy {
    pub fn access(&self, ds: &DataSourceId) -> AgentAccess {
        self.sources
            .iter()
            .find(|(id, _)| id == ds)
            .map(|(_, a)| *a)
            .unwrap_or(self.default_access)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    /// Start the MCP server with the IDE.
    pub enabled: bool,
    /// Port on 127.0.0.1 (the server never listens on other interfaces).
    pub port: u16,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 7391,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpStatus {
    pub running: bool,
    /// Endpoint of the streamable HTTP transport, e.g. `http://127.0.0.1:7391/mcp`.
    pub url: Option<String>,
    /// Names of connected clients (from their `initialize`).
    pub clients: Vec<String>,
    pub error: Option<String>,
}

/// What an MCP client needs to connect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConnectionInfo {
    pub url: String,
    /// Bearer token (`Authorization: Bearer …`).
    pub token: String,
    /// Ready-to-paste client configuration (JSON with `mcpServers`), using the
    /// `ferrodb mcp` stdio bridge.
    pub client_config: String,
}

/// A tool call waiting for the user's decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsentRequest {
    pub id: u64,
    /// Client name, e.g. `claude-code`.
    pub client: String,
    /// MCP tool, e.g. `run_query`.
    pub tool: String,
    pub data_source: Option<DataSourceId>,
    /// Data source name as shown in the explorer.
    pub data_source_name: Option<String>,
    /// Query text, when the call runs one.
    pub text: Option<String>,
    /// The call changes data or schema.
    pub modifies: bool,
    pub risks: Vec<RiskKind>,
    /// One line for the dialog ("Run a query on Sales (PostgreSQL)").
    pub summary: String,
    pub at: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsentDecision {
    AllowOnce,
    /// Allow the same tool on the same data source for this client until it
    /// disconnects (never for calls that modify).
    AllowForSession,
    Deny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditOutcome {
    /// Allowed by the policy without asking.
    AllowedByPolicy,
    AllowedByUser,
    DeniedByPolicy,
    DeniedByUser,
    /// Nobody answered within the timeout.
    TimedOut,
}

/// One tool call of an agent, as kept in the audit log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at: SystemTime,
    pub client: String,
    pub tool: String,
    pub data_source: Option<DataSourceId>,
    pub text: Option<String>,
    pub outcome: AuditOutcome,
    /// Rows / items returned.
    pub items: Option<u64>,
    pub error: Option<String>,
    pub elapsed_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEvent {
    ConsentRequested(ConsentRequest),
    /// Decided by the user, timed out or cancelled by the client.
    ConsentClosed {
        id: u64,
    },
    Audit(AuditEntry),
    Status(McpStatus),
}

pub trait AgentService: Send + Sync {
    fn available(&self) -> bool {
        true
    }
    fn policy(&self) -> AgentPolicy;
    fn save_policy(&self, policy: AgentPolicy) -> BoxFuture<'_, Result<()>>;
    fn mcp_settings(&self) -> McpSettings;
    /// Saves and applies (starts / stops / restarts the server).
    fn save_mcp_settings(&self, settings: McpSettings) -> BoxFuture<'_, Result<()>>;
    fn status(&self) -> McpStatus;
    /// `None` while the server is not running.
    fn connection_info(&self) -> Option<McpConnectionInfo>;
    /// Replaces the bearer token; connected clients must reconnect.
    fn regenerate_token(&self) -> BoxFuture<'_, Result<()>>;
    fn subscribe(&self) -> async_channel::Receiver<AgentEvent>;
    /// Requests still waiting.
    fn pending(&self) -> Vec<ConsentRequest>;
    fn decide(&self, id: u64, decision: ConsentDecision);
    /// Newest first.
    fn audit(&self, limit: usize) -> BoxFuture<'_, Result<Vec<AuditEntry>>>;
}

/// Default: no agents, no MCP server.
pub struct NoAgents;

impl AgentService for NoAgents {
    fn available(&self) -> bool {
        false
    }
    fn policy(&self) -> AgentPolicy {
        AgentPolicy::default()
    }
    fn save_policy(&self, _policy: AgentPolicy) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(crate::Error::Unsupported("agents".into())) })
    }
    fn mcp_settings(&self) -> McpSettings {
        McpSettings::default()
    }
    fn save_mcp_settings(&self, _settings: McpSettings) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(crate::Error::Unsupported("MCP server".into())) })
    }
    fn status(&self) -> McpStatus {
        McpStatus::default()
    }
    fn connection_info(&self) -> Option<McpConnectionInfo> {
        None
    }
    fn regenerate_token(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(crate::Error::Unsupported("MCP server".into())) })
    }
    fn subscribe(&self) -> async_channel::Receiver<AgentEvent> {
        // Closed channel: nothing will ever arrive.
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
