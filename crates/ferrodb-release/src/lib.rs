//! # ferrodb-release
//!
//! Phase 10 (plan, section 7): crash reports with consent and signed
//! updates with a beta channel, behind the API v8 contracts
//! ([`ferrodb_api::UpdateService`], [`ferrodb_api::CrashReportService`]).
//!
//! - [`install_panic_hook`] (first thing in `main`) and [`log_layer`] (in
//!   the `tracing` subscriber) produce crash reports with masked secrets in
//!   `<data>/crashes`;
//! - [`ReleaseBackend`] wraps any [`Backend`] (core or mock, like
//!   `ferrodb_ai::AiBackend`) and serves `updates()` and `crash_reports()`;
//! - [`manifest`] holds the signed manifest format shared with
//!   `cargo xtask release-manifest`.
//!
//! Build-time configuration (none by default: a developer build checks for
//! nothing and sends nothing):
//!
//! | Variable | Meaning |
//! |---|---|
//! | `FERRODB_UPDATE_URL` | HTTPS URL of the manifest (`<url>.sig` next to it) |
//! | `FERRODB_UPDATE_PUBKEY` | base64 Ed25519 public key (32 bytes) |
//! | `FERRODB_CRASH_ENDPOINT` | HTTPS URL that receives crash reports (POST, JSON) |
//! | `FERRODB_RELEASE_CHANNEL` | channel of this build (`stable` / `beta`), shown in About |

pub mod crash;
mod http;
pub mod logbuf;
pub mod manifest;
pub mod mask;
pub mod settings;
pub mod updates;
pub mod version;

use std::{path::PathBuf, sync::Arc};

use ferrodb_api::{
    AgentService, AssistantService, Backend, CrashReportService, DataSourceService,
    DriverCatalogService, HistoryService, LanguageService, LocalHistoryService, MetadataService,
    QueryService, ScriptService, SearchIndexService, TransferService, UpdateService,
};

pub use crash::{CrashReports, CrashStore, install_panic_hook};
pub use logbuf::log_layer;
pub use mask::mask_secrets;
pub use updates::Updates;

/// Version of this build (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where settings and reports go and what the build was configured with.
#[derive(Clone, Debug)]
pub struct ReleaseOptions {
    /// `release.toml`; `None` = in memory.
    pub config_dir: Option<PathBuf>,
    /// `crashes/`, `updates/`; `None` = reports in memory, downloads in the
    /// temporary directory.
    pub data_dir: Option<PathBuf>,
    pub version: String,
    /// Platform key of the manifest (`windows-x86_64`).
    pub target: String,
    pub update_url: Option<String>,
    /// Raw Ed25519 public key (32 bytes).
    pub public_key: Option<Vec<u8>>,
    pub crash_endpoint: Option<String>,
    /// Plain HTTP to loopback (tests only).
    pub allow_loopback_http: bool,
}

fn build_env(value: Option<&'static str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

impl ReleaseOptions {
    /// The configuration baked into this build, with these directories.
    pub fn from_build(config_dir: Option<PathBuf>, data_dir: Option<PathBuf>) -> Self {
        let public_key = build_env(option_env!("FERRODB_UPDATE_PUBKEY")).and_then(|k| {
            manifest::decode_public_key(&k)
                .inspect_err(|e| tracing::error!(error = %e, "FERRODB_UPDATE_PUBKEY"))
                .ok()
        });
        Self {
            config_dir,
            data_dir,
            version: VERSION.to_owned(),
            target: manifest::current_target(),
            update_url: build_env(option_env!("FERRODB_UPDATE_URL")),
            public_key,
            crash_endpoint: build_env(option_env!("FERRODB_CRASH_ENDPOINT")),
            allow_loopback_http: false,
        }
    }

    /// In memory, nothing configured (tests, mock).
    pub fn in_memory() -> Self {
        Self {
            config_dir: None,
            data_dir: None,
            version: VERSION.to_owned(),
            target: manifest::current_target(),
            update_url: None,
            public_key: None,
            crash_endpoint: None,
            allow_loopback_http: false,
        }
    }
}

/// The channel this build was made for (`FERRODB_RELEASE_CHANNEL`).
pub fn build_channel() -> &'static str {
    match option_env!("FERRODB_RELEASE_CHANNEL") {
        Some("beta") => "beta",
        Some("stable") => "stable",
        _ => "dev",
    }
}

/// Runs blocking work on its own thread; the future resolves with its result.
pub(crate) async fn spawn_blocking<T: Send + 'static>(
    f: impl FnOnce() -> ferrodb_api::Result<T> + Send + 'static,
) -> ferrodb_api::Result<T> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("release-io".into())
        .spawn(move || {
            let _ = tx.try_send(f());
        })
        .map_err(|e| ferrodb_api::Error::Other(e.to_string()))?;
    rx.recv()
        .await
        .map_err(|_| ferrodb_api::Error::Other("worker thread stopped".into()))?
}

/// A [`Backend`] that delegates everything to an inner one and adds updates
/// and crash reports.
pub struct ReleaseBackend {
    inner: Arc<dyn Backend>,
    updates: Updates,
    crashes: CrashReports,
}

impl ReleaseBackend {
    pub fn new(inner: Arc<dyn Backend>, options: ReleaseOptions) -> Arc<Self> {
        let settings = Arc::new(settings::SettingsFile::load(options.config_dir.as_deref()));
        let store = Arc::new(CrashStore::new(
            options.data_dir.as_deref().map(crash::crash_dir),
        ));
        let crashes = CrashReports::new(
            store,
            settings.clone(),
            options.crash_endpoint.clone(),
            options.allow_loopback_http,
        );
        Arc::new(Self {
            inner,
            updates: Updates::new(options, settings),
            crashes,
        })
    }

    pub fn inner(&self) -> &Arc<dyn Backend> {
        &self.inner
    }

    pub fn updates_impl(&self) -> &Updates {
        &self.updates
    }

    pub fn crashes_impl(&self) -> &CrashReports {
        &self.crashes
    }
}

impl Backend for ReleaseBackend {
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
        self.inner.assistant()
    }
    fn agents(&self) -> &dyn AgentService {
        self.inner.agents()
    }
    fn updates(&self) -> &dyn UpdateService {
        &self.updates
    }
    fn crash_reports(&self) -> &dyn CrashReportService {
        &self.crashes
    }
}
