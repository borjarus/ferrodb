use crate::{
    DataSourceService, DriverCatalogService, HistoryService, LanguageService, MetadataService,
    QueryService, SearchIndexService, TransferService,
};

/// The only point where the UI meets the logic.
///
/// Implemented by `ferrodb_mock::MockBackend` (phases 1–2) and by
/// `ferrodb-core` (from phase 3). Views hold an `Arc<dyn Backend>` and never
/// know which one they talk to.
pub trait Backend: Send + Sync + 'static {
    /// Human-readable backend name shown in the status bar (e.g. "mock").
    fn name(&self) -> &str;
    fn data_sources(&self) -> &dyn DataSourceService;
    fn drivers(&self) -> &dyn DriverCatalogService;
    fn queries(&self) -> &dyn QueryService;
    fn metadata(&self) -> &dyn MetadataService;
    fn language(&self) -> &dyn LanguageService;
    fn history(&self) -> &dyn HistoryService;
    fn search(&self) -> &dyn SearchIndexService;
    /// Export, import and copying (phase 6).
    fn transfers(&self) -> &dyn TransferService {
        &crate::NoTransfers
    }
    /// Local History of consoles and files (phase 7). The default keeps it
    /// in memory for the life of the process.
    fn local_history(&self) -> &dyn crate::LocalHistoryService {
        static MEMORY: std::sync::LazyLock<crate::MemoryLocalHistory> =
            std::sync::LazyLock::new(crate::MemoryLocalHistory::new);
        &*MEMORY
    }
    /// Natural-language assistant (phase 9, optional).
    fn assistant(&self) -> &dyn crate::AssistantService {
        &crate::NoAssistant
    }
    /// Permission layer for agents and the MCP server (phase 9, optional).
    fn agents(&self) -> &dyn crate::AgentService {
        &crate::NoAgents
    }
    /// Extension scripts: export formats and aggregates (phase 9).
    fn scripts(&self) -> &dyn crate::ScriptService {
        &crate::NoScripts
    }
    /// Signed application updates and the beta channel (phase 10).
    fn updates(&self) -> &dyn crate::UpdateService {
        &crate::NoUpdates
    }
    /// Crash reports, sent only with consent (phase 10).
    fn crash_reports(&self) -> &dyn crate::CrashReportService {
        &crate::NoCrashReports
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The contract must stay dyn-compatible: this fails to compile otherwise.
    #[allow(dead_code)]
    fn assert_dyn(_: &dyn Backend) {}

    #[test]
    fn capabilities_roundtrip_json() {
        use crate::*;
        let caps = Capabilities {
            data_model: DataModel::Document,
            language: QueryLanguage::Mql,
            access: AccessLayer::Native,
            language_level: LanguageLevel::A,
            transactions: TxSupport::MultiStatement,
            schema: SchemaKind::Inferred,
            joins: false,
            explain: ExplainSupport::Structured,
            server_cancel: true,
            editable_results: true,
            cost_estimate: false,
            auth_methods: vec![AuthMethod::Password, AuthMethod::X509],
        };
        let json = serde_json::to_string(&caps).unwrap();
        let back: Capabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(caps, back);
    }
}
