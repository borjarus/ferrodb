use serde::{Deserialize, Serialize};

use crate::{AccessLayer, BoxFuture, Capabilities, DriverId, FamilyId, Result};

/// Grouping used in the "new data source" dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DriverCategory {
    Relational,
    AnalyticalCloud,
    NoSql,
}

/// What must be present on disk before the driver can connect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Requirement {
    /// Java runtime for the JDBC bridge.
    JavaRuntime { min_version: u32 },
    /// A JDBC artifact (Maven coordinates).
    JdbcArtifact {
        coordinates: String,
        version: String,
        sha256: String,
        license: String,
    },
    /// An ADBC shared library.
    AdbcLibrary {
        name: String,
        version: String,
        sha256: String,
        license: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriverStatus {
    /// Compiled into the application (native drivers).
    BuiltIn,
    NotDownloaded,
    Downloading {
        progress_permille: u16,
    },
    Downloaded {
        version: String,
    },
    UpdateAvailable {
        installed: String,
        latest: String,
    },
    Failed(String),
}

impl DriverStatus {
    pub fn is_ready(&self) -> bool {
        matches!(
            self,
            Self::BuiltIn | Self::Downloaded { .. } | Self::UpdateAvailable { .. }
        )
    }
}

/// One field of the connection form generated from the driver description.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverProperty {
    pub key: String,
    pub label: String,
    pub default: Option<String>,
    pub required: bool,
    pub secret: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriverDescriptor {
    pub id: DriverId,
    pub family: FamilyId,
    /// Database product name (proper noun, not localized).
    pub name: String,
    pub category: DriverCategory,
    pub capabilities: Capabilities,
    pub requirements: Vec<Requirement>,
    pub properties: Vec<DriverProperty>,
    pub default_port: Option<u16>,
}

impl DriverDescriptor {
    pub fn access(&self) -> AccessLayer {
        self.capabilities.access
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriverEvent {
    StatusChanged(DriverId, DriverStatus),
}

/// The driver manager (catalog, downloads, verification).
pub trait DriverCatalogService: Send + Sync {
    fn list(&self) -> Vec<DriverDescriptor>;
    fn get(&self, id: &DriverId) -> Option<DriverDescriptor>;
    fn status(&self, id: &DriverId) -> DriverStatus;
    /// Downloads and verifies the driver; progress is reported via [`Self::subscribe`].
    fn download(&self, id: &DriverId) -> BoxFuture<'_, Result<()>>;
    fn subscribe(&self) -> async_channel::Receiver<DriverEvent>;
}
