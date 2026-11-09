//! # ferrodb-core
//!
//! The real [`Backend`]: data sources from `data-sources.toml` with secrets
//! in the keyring, sessions on native drivers, script execution with
//! streaming and paging, transactions, read-only enforcement, timeouts,
//! query history and the query log.
//!
//! Concurrency (ADR-002): the core owns a multi-threaded tokio runtime.
//! Every future handed to the UI only awaits a tokio `JoinHandle` or a
//! channel, so GPUI can poll it on its own executor. Each session is an
//! actor task owning its connection; commands are processed in order.

mod config;
mod metadata;
mod services;
mod session;
mod sources;

use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use ferrodb_api::{
    Backend, BoxFuture, ConnectionState, DataSourceConfig, DataSourceEvent, DataSourceId,
    DataSourceService, DriverCatalogService, DriverEvent, Error, HistoryService, LanguageService,
    MetadataService, QueryService, Result, ResultId, SearchEntry, SearchIndexService, SessionId,
};
use ferrodb_drivers::{Connection, Registry};
use ferrodb_lang::BasicLanguageService;
use ferrodb_secrets::SecretStore;
use ferrodb_storage::Storage;
use tokio::runtime::Runtime;

pub use ferrodb_secrets::{KeyringStore, MemoryStore};
pub use ferrodb_storage::{LogEntry, LogSource};

/// Where the core keeps its files and secrets.
pub struct CoreOptions {
    /// `data-sources.toml` lives here; `None` = in memory only (tests).
    pub config_dir: Option<PathBuf>,
    /// `app.db` (history, query log) lives here; `None` = in memory.
    pub data_dir: Option<PathBuf>,
    pub secrets: Box<dyn SecretStore>,
    pub registry: Registry,
}

impl CoreOptions {
    /// Files under the given directories, secrets in the system keyring,
    /// every built-in driver.
    pub fn system(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            config_dir: Some(config_dir),
            data_dir: Some(data_dir),
            secrets: ferrodb_secrets::system_store("FerroDb"),
            registry: Registry::builtin(),
        }
    }

    /// Nothing on disk, secrets in memory (tests).
    pub fn in_memory() -> Self {
        Self {
            config_dir: None,
            data_dir: None,
            secrets: Box::new(MemoryStore::new()),
            registry: Registry::builtin(),
        }
    }
}

/// Cheap to clone; all clones share one core.
#[derive(Clone)]
pub struct CoreBackend {
    inner: Arc<Inner>,
}

impl CoreBackend {
    pub fn new(opts: CoreOptions) -> Result<Self> {
        // Every driver uses rustls with `ring`; make it the process default so
        // libraries calling `ClientConfig::builder()` find it.
        ferrodb_net::install_crypto_provider();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("ferrodb-core")
            .enable_all()
            .build()
            .map_err(|e| Error::Other(format!("tokio runtime: {e}")))?;
        let config_path = opts.config_dir.map(|d| d.join("data-sources.toml"));
        let sources = match &config_path {
            Some(p) => config::load(p)?,
            None => Vec::new(),
        };
        let storage = match &opts.data_dir {
            Some(d) => Storage::open(&d.join("app.db")),
            None => Storage::in_memory(),
        }
        .map_err(|e| Error::Other(format!("app.db: {e}")))?;
        let inner = Arc::new_cyclic(|weak| Inner {
            me: weak.clone(),
            handle: rt.handle().clone(),
            rt: Mutex::new(Some(rt)),
            registry: opts.registry,
            secrets: Arc::from(opts.secrets),
            storage: Arc::new(storage),
            config_path,
            language: BasicLanguageService,
            state: Mutex::new(State {
                sources: sources
                    .into_iter()
                    .map(|c| {
                        (
                            c.id.clone(),
                            SourceEntry {
                                config: c,
                                state: ConnectionState::Disconnected,
                            },
                        )
                    })
                    .collect(),
                ..Default::default()
            }),
            ds_subs: Mutex::new(Vec::new()),
            driver_subs: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        });
        Ok(Self { inner })
    }

    /// The query log (newest first), for diagnostics and the Output panel.
    pub fn query_log(&self, limit: usize) -> Vec<LogEntry> {
        self.inner.storage.query_log(limit).unwrap_or_default()
    }
}

impl Backend for CoreBackend {
    fn name(&self) -> &str {
        "core"
    }
    fn data_sources(&self) -> &dyn DataSourceService {
        &*self.inner
    }
    fn drivers(&self) -> &dyn DriverCatalogService {
        &*self.inner
    }
    fn queries(&self) -> &dyn QueryService {
        &*self.inner
    }
    fn metadata(&self) -> &dyn MetadataService {
        &*self.inner
    }
    fn language(&self) -> &dyn LanguageService {
        &self.inner.language
    }
    fn history(&self) -> &dyn HistoryService {
        &*self.inner
    }
    fn search(&self) -> &dyn SearchIndexService {
        &*self.inner
    }
}

pub(crate) struct SourceEntry {
    pub config: DataSourceConfig,
    pub state: ConnectionState,
}

#[derive(Default)]
pub(crate) struct State {
    pub sources: BTreeMap<DataSourceId, SourceEntry>,
    pub sessions: HashMap<SessionId, Arc<session::Session>>,
    /// Paused results waiting for `fetch_more`: result → page credits.
    pub results: HashMap<ResultId, tokio::sync::mpsc::UnboundedSender<usize>>,
    /// Explorer connections, one per connected source.
    pub meta: HashMap<DataSourceId, Arc<tokio::sync::Mutex<Box<dyn Connection>>>>,
    /// Idle connections kept for reuse by new sessions.
    pub pool: HashMap<DataSourceId, Vec<Box<dyn Connection>>>,
    /// Objects seen in the explorer, for Search Everywhere.
    pub seen: Vec<SearchEntry>,
}

pub(crate) struct Inner {
    me: Weak<Inner>,
    handle: tokio::runtime::Handle,
    rt: Mutex<Option<Runtime>>,
    pub registry: Registry,
    pub secrets: Arc<dyn SecretStore>,
    pub storage: Arc<Storage>,
    pub config_path: Option<PathBuf>,
    pub language: BasicLanguageService,
    state: Mutex<State>,
    ds_subs: Mutex<Vec<async_channel::Sender<DataSourceEvent>>>,
    driver_subs: Mutex<Vec<async_channel::Sender<DriverEvent>>>,
    next_id: AtomicU64,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.lock().ok().and_then(|mut r| r.take()) {
            rt.shutdown_background();
        }
    }
}

impl Inner {
    pub fn arc(&self) -> Arc<Inner> {
        self.me.upgrade().expect("core is alive while used")
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn handle(&self) -> &tokio::runtime::Handle {
        &self.handle
    }

    /// Runs `fut` on the core runtime; the returned future only awaits the
    /// join handle, so any executor can poll it.
    pub fn spawn<T, F>(&self, fut: F) -> BoxFuture<'static, Result<T>>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let h = self.handle.spawn(fut);
        Box::pin(async move {
            h.await
                .map_err(|e| Error::Other(format!("core task failed: {e}")))?
        })
    }

    /// Runs blocking `f` (keyring, app.db) on the runtime's blocking pool.
    pub async fn blocking<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        self.handle
            .spawn_blocking(f)
            .await
            .map_err(|e| Error::Other(format!("core task failed: {e}")))?
    }

    pub fn emit_ds(&self, ev: DataSourceEvent) {
        let mut subs = self.ds_subs.lock().unwrap_or_else(|p| p.into_inner());
        subs.retain(|s| s.try_send(ev.clone()).is_ok() || !s.is_closed());
    }

    pub fn set_state(&self, id: &DataSourceId, state: ConnectionState) {
        let changed = {
            let mut st = self.state();
            match st.sources.get_mut(id) {
                Some(e) if e.state != state => {
                    e.state = state.clone();
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.emit_ds(DataSourceEvent::StateChanged(id.clone(), state));
        }
    }

    pub fn subscribe_ds(&self) -> async_channel::Receiver<DataSourceEvent> {
        let (tx, rx) = async_channel::unbounded();
        self.ds_subs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(tx);
        rx
    }

    pub fn subscribe_drivers(&self) -> async_channel::Receiver<DriverEvent> {
        let (tx, rx) = async_channel::unbounded();
        self.driver_subs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(tx);
        rx
    }
}

#[cfg(test)]
mod tests;
