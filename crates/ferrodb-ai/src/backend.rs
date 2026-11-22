//! [`AiBackend`]: a [`Backend`] that delegates everything to an inner one and
//! adds the assistant and the agents.

use std::{path::PathBuf, sync::Arc};

use ferrodb_api::{
    AgentService, AssistantService, Backend, DataSourceService, DriverCatalogService,
    HistoryService, LanguageService, LocalHistoryService, MetadataService, QueryService,
    ScriptService, SearchIndexService, TransferService,
};
use ferrodb_secrets::SecretStore;

use crate::{agents::Agents, assistant::Assistant};

/// Where settings, secrets and the audit log go.
#[derive(Clone)]
pub struct AiOptions {
    /// `ai.toml` and `agents.toml` are kept here; `None` = in memory (tests).
    pub config_dir: Option<PathBuf>,
    /// The audit log (`agents-audit.jsonl`) is kept here; `None` = in memory.
    pub data_dir: Option<PathBuf>,
    /// API keys and the MCP token.
    pub secrets: Arc<dyn SecretStore>,
}

impl AiOptions {
    /// In memory only (tests, mock).
    pub fn in_memory() -> Self {
        Self {
            config_dir: None,
            data_dir: None,
            secrets: Arc::new(ferrodb_secrets::MemoryStore::new()),
        }
    }
}

pub struct AiBackend {
    inner: Arc<dyn Backend>,
    assistant: Arc<Assistant>,
    agents: Arc<Agents>,
}

impl AiBackend {
    pub fn new(inner: Arc<dyn Backend>, options: AiOptions) -> Arc<Self> {
        let assistant = Assistant::new(inner.clone(), options.clone());
        let agents = Agents::new(inner.clone(), options);
        Arc::new(Self {
            inner,
            assistant,
            agents,
        })
    }

    pub fn inner(&self) -> &Arc<dyn Backend> {
        &self.inner
    }

    pub fn assistant_impl(&self) -> &Arc<Assistant> {
        &self.assistant
    }

    pub fn agents_impl(&self) -> &Arc<Agents> {
        &self.agents
    }
}

impl Backend for AiBackend {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn data_sources(&self) -> &dyn DataSourceService {
        self.inner.data_sources()
    }
    fn drivers(&self) -> &dyn DriverCatalogService {
        self.inner.drivers()
    }
    fn queries(&self) -> &dyn QueryService {
        self.inner.queries()
    }
    fn metadata(&self) -> &dyn MetadataService {
        self.inner.metadata()
    }
    fn language(&self) -> &dyn LanguageService {
        self.inner.language()
    }
    fn history(&self) -> &dyn HistoryService {
        self.inner.history()
    }
    fn search(&self) -> &dyn SearchIndexService {
        self.inner.search()
    }
    fn transfers(&self) -> &dyn TransferService {
        self.inner.transfers()
    }
    fn local_history(&self) -> &dyn LocalHistoryService {
        self.inner.local_history()
    }
    fn scripts(&self) -> &dyn ScriptService {
        self.inner.scripts()
    }
    fn assistant(&self) -> &dyn AssistantService {
        &*self.assistant
    }
    fn agents(&self) -> &dyn AgentService {
        &*self.agents
    }
}
