use serde::{Deserialize, Serialize};

use crate::{BoxFuture, Capabilities, DataSourceId, DriverId, Result};

/// Environment marker shown as a color everywhere the source appears.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Environment {
    #[default]
    None,
    Development,
    Test,
    Staging,
    Production,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TxMode {
    #[default]
    Auto,
    Manual,
}

/// Persisted configuration of a data source. Never contains secrets: those are
/// referenced as `secret://<id>` and live in the system keyring.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DataSourceConfig {
    pub id: DataSourceId,
    pub name: String,
    pub driver: DriverId,
    /// Folder path in the explorer, e.g. `["clients", "acme"]`.
    #[serde(default)]
    pub folder: Vec<String>,
    #[serde(default)]
    pub environment: Environment,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub tx_mode: TxMode,
    /// Driver-specific properties (host, port, database, URI…).
    #[serde(default)]
    pub properties: Vec<(String, String)>,
    /// Reference to the secret in the keyring, never the secret itself.
    pub secret_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DataSourceInfo {
    pub config: DataSourceConfig,
    pub capabilities: Capabilities,
    pub state: ConnectionState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TestConnectionReport {
    pub server_version: String,
    pub latency_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DataSourceEvent {
    Added(DataSourceId),
    Removed(DataSourceId),
    Changed(DataSourceId),
    StateChanged(DataSourceId, ConnectionState),
}

/// Secret values typed into the connection form (password, token, key…),
/// keyed by [`crate::DriverProperty::key`]. They travel from the form to the
/// keyring and are never persisted in configuration files nor printed:
/// `Debug` redacts the values.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretValues(pub Vec<(String, String)>);

impl SecretValues {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.push((key.into(), value.into()));
        self
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SecretValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|(k, _)| (k, "***")))
            .finish()
    }
}

pub trait DataSourceService: Send + Sync {
    fn list(&self) -> Vec<DataSourceInfo>;
    fn get(&self, id: &DataSourceId) -> Option<DataSourceInfo>;
    /// Persists the configuration. Non-empty `secrets` replace the stored
    /// ones; keys absent or empty keep what the keyring already holds.
    fn save(&self, config: DataSourceConfig, secrets: SecretValues) -> BoxFuture<'_, Result<()>>;
    fn remove(&self, id: &DataSourceId) -> BoxFuture<'_, Result<()>>;
    /// Connects with an unsaved configuration. Missing secrets fall back to
    /// the ones stored for `config.id` (editing an existing source).
    fn test(
        &self,
        config: DataSourceConfig,
        secrets: SecretValues,
    ) -> BoxFuture<'_, Result<TestConnectionReport>>;
    fn connect(&self, id: &DataSourceId) -> BoxFuture<'_, Result<()>>;
    fn disconnect(&self, id: &DataSourceId) -> BoxFuture<'_, Result<()>>;
    /// Change notifications for the explorer and status bar.
    fn subscribe(&self) -> async_channel::Receiver<DataSourceEvent>;
}
