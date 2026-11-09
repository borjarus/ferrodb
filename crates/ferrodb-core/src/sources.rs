//! Data sources: configuration, secrets, connecting, the connection pool.

use std::{sync::Arc, time::Instant};

use ferrodb_api::{
    AccessLayer, BoxFuture, Capabilities, ConnectionState, DataModel, DataSourceConfig,
    DataSourceEvent, DataSourceId, DataSourceInfo, DataSourceService, Error, ExplainSupport,
    LanguageLevel, QueryLanguage, Result, SchemaKind, SecretValues, TestConnectionReport,
    TxSupport,
};
use ferrodb_drivers::{Connection, Driver, ResolvedConfig};
use ferrodb_secrets::Secrets;

use crate::{Inner, SourceEntry, config};

/// Idle connections kept per source.
const POOL_SIZE: usize = 2;

/// Capabilities shown for a source whose driver is not in this build.
fn unavailable_capabilities() -> Capabilities {
    Capabilities {
        data_model: DataModel::Tabular,
        language: QueryLanguage::sql("generic"),
        access: AccessLayer::Native,
        language_level: LanguageLevel::C,
        transactions: TxSupport::None,
        schema: SchemaKind::Strict,
        joins: true,
        explain: ExplainSupport::None,
        server_cancel: false,
        editable_results: false,
        cost_estimate: false,
        auth_methods: Vec::new(),
    }
}

fn secret_ref(config: &DataSourceConfig) -> String {
    config
        .secret_ref
        .clone()
        .unwrap_or_else(|| ferrodb_secrets::reference(config.id.as_str()))
}

impl Inner {
    pub fn driver_for(&self, config: &DataSourceConfig) -> Result<Arc<dyn Driver>> {
        self.registry.get(&config.driver).ok_or_else(|| {
            Error::Unsupported(format!(
                "driver `{}` is not available in this build yet",
                config.driver
            ))
        })
    }

    pub fn source_config(&self, id: &DataSourceId) -> Result<DataSourceConfig> {
        self.state()
            .sources
            .get(id)
            .map(|e| e.config.clone())
            .ok_or_else(|| Error::NotFound(format!("data source {id}")))
    }

    fn info(&self, e: &SourceEntry) -> DataSourceInfo {
        DataSourceInfo {
            capabilities: self
                .registry
                .get(&e.config.driver)
                .map(|d| d.capabilities())
                .unwrap_or_else(unavailable_capabilities),
            config: e.config.clone(),
            state: e.state.clone(),
        }
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = &self.config_path else {
            return Ok(());
        };
        let configs: Vec<DataSourceConfig> = self
            .state()
            .sources
            .values()
            .map(|e| e.config.clone())
            .collect();
        config::save(path, &configs)
    }

    /// Stored secrets overlaid with non-empty `typed` ones.
    async fn resolve(
        &self,
        config: &DataSourceConfig,
        typed: SecretValues,
    ) -> Result<ResolvedConfig> {
        let store = self.secrets.clone();
        let r = secret_ref(config);
        let stored = self
            .blocking(move || store.get(&r).map_err(|e| Error::Other(e.to_string())))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cannot read secrets");
                Secrets::new()
            });
        let mut secrets = SecretValues(stored.into_iter().collect());
        for (k, v) in typed.0 {
            if !v.is_empty() {
                secrets.0.retain(|(sk, _)| sk != &k);
                secrets.0.push((k, v));
            }
        }
        ResolvedConfig::from_config(config, secrets)
    }

    /// A fresh connection (no pool), configured for the source.
    pub async fn open_connection(
        &self,
        config: &DataSourceConfig,
        typed: SecretValues,
    ) -> Result<Box<dyn Connection>> {
        let driver = self.driver_for(config)?;
        let resolved = self.resolve(config, typed).await?;
        let timeout = resolved.connect_timeout;
        let mut conn = tokio::time::timeout(timeout, driver.connect(&resolved))
            .await
            .map_err(|_| Error::Connection(format!("timed out after {} s", timeout.as_secs())))??;
        if config.read_only && driver.variant().quirks().read_only_sessions {
            conn.set_read_only(true).await?;
        }
        Ok(conn)
    }

    /// A pooled connection if a healthy one is idle, otherwise a new one.
    pub async fn checkout(&self, id: &DataSourceId) -> Result<Box<dyn Connection>> {
        let config = self.source_config(id)?;
        loop {
            let pooled = self.state().pool.get_mut(id).and_then(Vec::pop);
            let Some(mut conn) = pooled else { break };
            if conn.is_alive() && conn.ping().await.is_ok() {
                return Ok(conn);
            }
        }
        self.set_state(id, ConnectionState::Connecting);
        match self.open_connection(&config, SecretValues::new()).await {
            Ok(c) => {
                self.set_state(id, ConnectionState::Connected);
                Ok(c)
            }
            Err(e) => {
                self.set_state(id, ConnectionState::Failed(e.to_string()));
                Err(e)
            }
        }
    }

    /// Returns a connection to the pool (or drops it).
    pub fn checkin(&self, id: &DataSourceId, conn: Box<dyn Connection>) {
        if !conn.is_alive() {
            return;
        }
        let mut st = self.state();
        if !st.sources.contains_key(id) {
            return;
        }
        let pool = st.pool.entry(id.clone()).or_default();
        if pool.len() < POOL_SIZE {
            pool.push(conn);
        }
    }

    /// Closes sessions, the explorer connection and pooled connections.
    async fn close_all(&self, id: &DataSourceId) {
        let sessions: Vec<_> = {
            let mut st = self.state();
            st.meta.remove(id);
            st.pool.remove(id);
            st.sessions
                .values()
                .filter(|s| &s.data_source == id)
                .cloned()
                .collect()
        };
        for s in sessions {
            self.close_session_now(s.id).await;
        }
    }
}

impl DataSourceService for Inner {
    fn list(&self) -> Vec<DataSourceInfo> {
        self.state()
            .sources
            .values()
            .map(|e| self.info(e))
            .collect()
    }

    fn get(&self, id: &DataSourceId) -> Option<DataSourceInfo> {
        self.state().sources.get(id).map(|e| self.info(e))
    }

    fn save(
        &self,
        mut config: DataSourceConfig,
        secrets: SecretValues,
    ) -> BoxFuture<'_, Result<()>> {
        let me = self.arc();
        self.spawn(async move {
            let r = secret_ref(&config);
            config.secret_ref = Some(r.clone());
            let typed: Vec<(String, String)> = secrets
                .0
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .collect();
            if !typed.is_empty() {
                let store = me.secrets.clone();
                me.blocking(move || {
                    let mut stored = store.get(&r).map_err(|e| Error::Other(e.to_string()))?;
                    stored.extend(typed);
                    store
                        .set(&r, &stored)
                        .map_err(|e| Error::Other(e.to_string()))
                })
                .await?;
            }
            let id = config.id.clone();
            let existed = {
                let mut st = me.state();
                let existed = st.sources.contains_key(&id);
                let state = st
                    .sources
                    .get(&id)
                    .map(|e| e.state.clone())
                    .unwrap_or(ConnectionState::Disconnected);
                st.sources.insert(id.clone(), SourceEntry { config, state });
                // New settings apply to new connections.
                st.pool.remove(&id);
                existed
            };
            me.persist()?;
            me.emit_ds(if existed {
                DataSourceEvent::Changed(id)
            } else {
                DataSourceEvent::Added(id)
            });
            Ok(())
        })
    }

    fn remove(&self, id: &DataSourceId) -> BoxFuture<'_, Result<()>> {
        let me = self.arc();
        let id = id.clone();
        self.spawn(async move {
            me.close_all(&id).await;
            let removed = me.state().sources.remove(&id);
            let Some(entry) = removed else {
                return Err(Error::NotFound(format!("data source {id}")));
            };
            me.persist()?;
            let store = me.secrets.clone();
            let storage = me.storage.clone();
            let r = secret_ref(&entry.config);
            let id2 = id.clone();
            me.blocking(move || {
                if let Err(e) = store.delete(&r) {
                    tracing::warn!(error = %e, "cannot delete secrets");
                }
                let _ = storage.forget(&id2);
                Ok(())
            })
            .await?;
            me.emit_ds(DataSourceEvent::Removed(id));
            Ok(())
        })
    }

    fn test(
        &self,
        config: DataSourceConfig,
        secrets: SecretValues,
    ) -> BoxFuture<'_, Result<TestConnectionReport>> {
        let me = self.arc();
        self.spawn(async move {
            let started = Instant::now();
            let mut conn = me.open_connection(&config, secrets).await?;
            conn.ping().await?;
            Ok(TestConnectionReport {
                server_version: conn.server_version(),
                latency_ms: started.elapsed().as_millis() as u64,
            })
        })
    }

    fn connect(&self, id: &DataSourceId) -> BoxFuture<'_, Result<()>> {
        let me = self.arc();
        let id = id.clone();
        self.spawn(async move {
            me.meta_connection(&id).await?;
            Ok(())
        })
    }

    fn disconnect(&self, id: &DataSourceId) -> BoxFuture<'_, Result<()>> {
        let me = self.arc();
        let id = id.clone();
        self.spawn(async move {
            me.close_all(&id).await;
            me.set_state(&id, ConnectionState::Disconnected);
            Ok(())
        })
    }

    fn subscribe(&self) -> async_channel::Receiver<DataSourceEvent> {
        self.subscribe_ds()
    }
}
