//! Driver catalog (built-in native drivers only in phase 3), history and
//! search.

use std::path::PathBuf;

use ferrodb_api::{
    BoxFuture, BridgeState, DriverCatalogService, DriverDescriptor, DriverEvent, DriverId,
    DriverStatus, Error, HistoryEntry, HistoryFilter, HistoryService, Result, SearchEntry,
    SearchIndexService, SearchTarget,
};

use crate::Inner;

impl DriverCatalogService for Inner {
    fn list(&self) -> Vec<DriverDescriptor> {
        self.registry
            .list()
            .iter()
            .map(|d| d.descriptor())
            .collect()
    }

    fn get(&self, id: &DriverId) -> Option<DriverDescriptor> {
        self.registry.get(id).map(|d| d.descriptor())
    }

    fn status(&self, id: &DriverId) -> DriverStatus {
        if self.registry.get(id).is_some() {
            DriverStatus::BuiltIn
        } else {
            DriverStatus::NotDownloaded
        }
    }

    fn versions(&self, _id: &DriverId) -> Vec<String> {
        Vec::new()
    }

    fn download(&self, id: &DriverId, _version: Option<String>) -> BoxFuture<'_, Result<()>> {
        let built_in = self.registry.get(id).is_some();
        let id = id.clone();
        Box::pin(async move {
            if built_in {
                Ok(())
            } else {
                Err(Error::Unsupported(format!(
                    "downloading drivers (`{id}`) arrives with the driver manager"
                )))
            }
        })
    }

    fn use_custom_artifacts(
        &self,
        _id: &DriverId,
        _paths: Vec<PathBuf>,
    ) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            Err(Error::Unsupported(
                "custom driver files (JDBC bridge)".into(),
            ))
        })
    }

    fn jdbc_bridge(&self) -> BridgeState {
        BridgeState::NotStarted
    }

    fn subscribe(&self) -> async_channel::Receiver<DriverEvent> {
        self.subscribe_drivers()
    }
}

impl HistoryService for Inner {
    fn record(&self, entry: HistoryEntry) {
        let storage = self.storage.clone();
        self.handle().spawn_blocking(move || {
            if let Err(e) = storage.record_history(&entry) {
                tracing::warn!(error = %e, "history write failed");
            }
        });
    }

    fn query(&self, filter: HistoryFilter) -> BoxFuture<'_, Result<Vec<HistoryEntry>>> {
        let storage = self.storage.clone();
        let me = self.arc();
        self.spawn(async move {
            me.blocking(move || {
                storage
                    .history(&filter)
                    .map_err(|e| Error::Other(e.to_string()))
            })
            .await
        })
    }
}

impl SearchIndexService for Inner {
    fn entries(&self) -> Vec<SearchEntry> {
        let st = self.state();
        st.sources
            .values()
            .map(|e| SearchEntry {
                label: e.config.name.clone(),
                location: e.config.folder.join(" / "),
                kind: None,
                target: SearchTarget::DataSource(e.config.id.clone()),
            })
            .chain(st.seen.iter().cloned())
            .collect()
    }
}
