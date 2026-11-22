//! Driver catalog (built-in native drivers, JDBC drivers whose files come
//! from the driver manager), history and search.

use std::path::PathBuf;

use ferrodb_api::{
    BoxFuture, BridgeState, DriverCatalogService, DriverDescriptor, DriverEvent, DriverId,
    DriverStatus, Error, HistoryEntry, HistoryFilter, HistoryService, Result, SearchEntry,
    SearchIndexService, SearchTarget,
};

use crate::Inner;

impl Inner {
    /// Drivers whose files come from the driver manager (JDBC).
    fn managed(&self, id: &DriverId) -> bool {
        self.registry
            .get(id)
            .is_some_and(|d| d.family().as_str() == "jdbc")
    }
}

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
        if self.managed(id) {
            self.driver_manager.status(id.as_str())
        } else if self.registry.get(id).is_some() {
            DriverStatus::BuiltIn
        } else {
            DriverStatus::NotDownloaded
        }
    }

    fn versions(&self, id: &DriverId) -> Vec<String> {
        if self.managed(id) {
            self.driver_manager.versions(id.as_str())
        } else {
            Vec::new()
        }
    }

    fn download(&self, id: &DriverId, version: Option<String>) -> BoxFuture<'_, Result<()>> {
        let managed = self.managed(id);
        let built_in = self.registry.get(id).is_some();
        let manager = self.driver_manager.clone();
        let id = id.clone();
        let me = self.arc();
        self.spawn(async move {
            if !managed {
                return if built_in {
                    Ok(())
                } else {
                    Err(Error::NotFound(format!("driver `{id}`")))
                };
            }
            me.blocking(move || {
                manager.download(id.as_str(), version.as_deref())?;
                // The bridge needs Java: fetch a runtime when none is installed.
                if manager.java().is_none() {
                    manager.download_java()?;
                }
                Ok(())
            })
            .await
        })
    }

    /// Local files: verified against the catalog when they are one of its
    /// versions (offline installation), otherwise used as custom files
    /// (level C, unverified).
    fn use_custom_artifacts(
        &self,
        id: &DriverId,
        paths: Vec<PathBuf>,
    ) -> BoxFuture<'_, Result<()>> {
        let managed = self.managed(id);
        let manager = self.driver_manager.clone();
        let id = id.clone();
        let me = self.arc();
        self.spawn(async move {
            if !managed {
                return Err(Error::Unsupported(format!(
                    "driver `{id}` is built in: it takes no driver files"
                )));
            }
            me.blocking(move || {
                let in_catalog = manager.catalog().get(id.as_str()).is_some();
                if in_catalog && manager.install_offline(id.as_str(), None, &paths).is_ok() {
                    return Ok(());
                }
                manager.use_custom(id.as_str(), &paths, None)?;
                Ok(())
            })
            .await
        })
    }

    fn jdbc_bridge(&self) -> BridgeState {
        self.bridge.state()
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
        let mut out: Vec<SearchEntry> = self
            .state()
            .sources
            .values()
            .map(|e| SearchEntry {
                label: e.config.name.clone(),
                location: e.config.folder.join(" / "),
                kind: None,
                target: SearchTarget::DataSource(e.config.id.clone()),
            })
            .collect();
        // Tables, views, routines, collections of every cached catalog.
        out.extend(self.catalog_entries());
        out
    }
}
