//! # ferrodb-mock
//!
//! [`MockBackend`] implements every service of `ferrodb-api` with synthetic
//! data so that the whole GUI can be built and tested before real drivers exist
//! (plan, section 6). It simulates latency, batching, errors with positions,
//! connection drops, cancellation and drivers that must be downloaded.
//!
//! It is runtime-agnostic: background work runs on plain threads and results
//! are delivered through `async-channel`, so the GUI can await them on its own
//! executor. The mock stays in the repository permanently as the UI test base.

// The mock simulates latency by sleeping on its own background threads (never the UI thread).
#![allow(clippy::disallowed_methods)]

mod catalog;
pub mod data;
mod objects;
mod scenario;

use std::{
    collections::HashMap,
    ops::Range,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use ferrodb_api::*;
use futures::{FutureExt, channel::oneshot};

pub use catalog::descriptors as driver_catalog;
pub use scenario::Scenario;

/// Database profiles exercised by the mock (plan, section 6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MockProfile {
    /// Relational with foreign keys and transactions.
    PostgresLike,
    /// Relational, no transactions, cloud dialect with cost estimate.
    BigQueryLike,
    /// Wide-column without joins.
    CassandraLike,
    /// Documents.
    MongoLike,
    /// Key-value.
    RedisLike,
}

/// Maximum items per `Data` event (coalescing, plan 5.5).
const CHUNK: usize = 1000;

/// Resolves after `d` without needing an async runtime.
fn delay(d: Duration) -> impl Future<Output = ()> + Send + 'static {
    let (tx, rx) = oneshot::channel::<()>();
    thread::spawn(move || {
        thread::sleep(d);
        let _ = tx.send(());
    });
    rx.map(|_| ())
}

struct Cursor {
    tx: async_channel::Sender<QueryEvent>,
    kind: CursorKind,
    next: u64,
    total: u64,
    cancel: CancelHandle,
}

#[derive(Clone)]
enum CursorKind {
    Rows(Arc<arrow_schema::Schema>),
    Documents,
    /// Keys matching a glob pattern; positions are scan positions.
    Keys(Arc<str>),
}

/// What a result was produced from (used by the change preview).
#[derive(Clone)]
struct ResultMeta {
    kind: CursorKind,
    container: String,
    language: QueryLanguage,
}

struct Inner {
    sources: Vec<DataSourceInfo>,
    driver_status: HashMap<DriverId, DriverStatus>,
    bridge: BridgeState,
    sessions: HashMap<SessionId, SessionInfo>,
    cursors: HashMap<ResultId, Cursor>,
    results: HashMap<ResultId, ResultMeta>,
    history: Vec<HistoryEntry>,
    ds_subscribers: Vec<async_channel::Sender<DataSourceEvent>>,
    driver_subscribers: Vec<async_channel::Sender<DriverEvent>>,
}

struct Shared {
    inner: Mutex<Inner>,
    scenario: Mutex<Scenario>,
    drivers: Vec<DriverDescriptor>,
    next_id: AtomicU64,
    /// No threads and no latency: everything completes on the caller's
    /// thread (deterministic UI tests, see [`MockBackend::deterministic`]).
    deterministic: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn scenario(&self) -> Scenario {
        *self.scenario.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
    /// Simulated latency for async operations.
    fn delay(&self, d: Duration) -> BoxFuture<'static, ()> {
        if self.deterministic {
            futures::future::ready(()).boxed()
        } else {
            delay(d).boxed()
        }
    }
    /// Simulated latency on a mock worker thread.
    fn sleep(&self, d: Duration) {
        if !self.deterministic {
            thread::sleep(d);
        }
    }
    fn driver(&self, id: &DriverId) -> Option<&DriverDescriptor> {
        self.drivers.iter().find(|d| &d.id == id)
    }
    fn source(&self, id: &DataSourceId) -> Result<DataSourceInfo> {
        self.lock()
            .sources
            .iter()
            .find(|s| &s.config.id == id)
            .cloned()
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }
    fn emit_ds(&self, ev: DataSourceEvent) {
        self.lock()
            .ds_subscribers
            .retain(|s| s.try_send(ev.clone()).is_ok() || !s.is_closed());
    }
    fn emit_driver(&self, ev: DriverEvent) {
        self.lock()
            .driver_subscribers
            .retain(|s| s.try_send(ev.clone()).is_ok() || !s.is_closed());
    }
    fn set_state(&self, id: &DataSourceId, state: ConnectionState) {
        if let Some(s) = self.lock().sources.iter_mut().find(|s| &s.config.id == id) {
            s.state = state.clone();
        }
        self.emit_ds(DataSourceEvent::StateChanged(id.clone(), state));
        self.update_bridge();
    }
    /// The JDBC bridge runs while any JDBC source is connected.
    fn update_bridge(&self) {
        let next = {
            let inner = self.lock();
            let connections = inner
                .sources
                .iter()
                .filter(|s| {
                    s.capabilities.access == AccessLayer::Jdbc
                        && s.state == ConnectionState::Connected
                })
                .count();
            let next = if connections > 0 {
                BridgeState::Running {
                    pid: 4242,
                    connections,
                }
            } else {
                BridgeState::NotStarted
            };
            (inner.bridge != next).then_some(next)
        };
        if let Some(next) = next {
            self.set_bridge(next);
        }
    }
    fn set_bridge(&self, state: BridgeState) {
        self.lock().bridge = state.clone();
        self.emit_driver(DriverEvent::BridgeChanged(state));
    }
    fn session_source(&self, session: SessionId) -> Result<DataSourceInfo> {
        let ds = self
            .lock()
            .sessions
            .get(&session)
            .map(|s| s.data_source.clone())
            .ok_or_else(|| Error::NotFound(session.to_string()))?;
        self.source(&ds)
    }
}

/// The mock implementation of [`Backend`].
#[derive(Clone)]
pub struct MockBackend {
    shared: Arc<Shared>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    /// A backend with the default set of data sources (one per profile plus a
    /// production PostgreSQL and an Oracle source whose driver is not downloaded).
    pub fn new() -> Self {
        Self::with_mode(false)
    }

    /// Like [`Self::new`], but without background threads or latency: every
    /// future and query completes on the calling thread. Used by UI tests,
    /// whose scheduler rejects wake-ups from foreign threads.
    pub fn deterministic() -> Self {
        Self::with_mode(true)
    }

    fn with_mode(deterministic: bool) -> Self {
        let drivers = catalog::descriptors();
        let driver_status = drivers
            .iter()
            .map(|d| {
                let st = match d.access() {
                    AccessLayer::Native => DriverStatus::BuiltIn,
                    _ if matches!(d.id.as_str(), "h2" | "bigquery") => DriverStatus::Downloaded {
                        version: "1.0.0".into(),
                    },
                    _ => DriverStatus::NotDownloaded,
                };
                (d.id.clone(), st)
            })
            .collect();
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                sources: Vec::new(),
                driver_status,
                bridge: BridgeState::NotStarted,
                sessions: HashMap::new(),
                cursors: HashMap::new(),
                results: HashMap::new(),
                history: Vec::new(),
                ds_subscribers: Vec::new(),
                driver_subscribers: Vec::new(),
            }),
            scenario: Mutex::new(Scenario::Normal),
            drivers,
            next_id: AtomicU64::new(1),
            deterministic,
        });
        let backend = Self { shared };
        for (id, name, driver, env, folder) in [
            (
                "shop-dev",
                "shop (dev)",
                "postgresql",
                Environment::Development,
                &["local"][..],
            ),
            (
                "shop-prod",
                "shop (production)",
                "postgresql",
                Environment::Production,
                &["production"][..],
            ),
            (
                "analytics",
                "analytics",
                "bigquery",
                Environment::None,
                &[][..],
            ),
            ("events", "events", "cassandra", Environment::Test, &[][..]),
            (
                "catalog",
                "catalog",
                "mongodb",
                Environment::Development,
                &["local"][..],
            ),
            (
                "cache",
                "cache",
                "redis",
                Environment::Development,
                &["local"][..],
            ),
            (
                "legacy",
                "legacy ERP",
                "oracle",
                Environment::Staging,
                &[][..],
            ),
        ] {
            backend.insert_source(DataSourceConfig {
                id: DataSourceId::new(id),
                name: name.into(),
                driver: DriverId::new(driver),
                folder: folder.iter().map(|s| s.to_string()).collect(),
                environment: env,
                read_only: env == Environment::Production,
                tx_mode: TxMode::Auto,
                properties: vec![("host".into(), "localhost".into())],
                secret_ref: Some(format!("secret://{id}")),
            });
        }
        backend
    }

    /// The data source used for a given profile in the default set.
    pub fn profile_source(profile: MockProfile) -> DataSourceId {
        DataSourceId::new(match profile {
            MockProfile::PostgresLike => "shop-dev",
            MockProfile::BigQueryLike => "analytics",
            MockProfile::CassandraLike => "events",
            MockProfile::MongoLike => "catalog",
            MockProfile::RedisLike => "cache",
        })
    }

    pub fn scenario(&self) -> Scenario {
        self.shared.scenario()
    }

    /// Switches the active scenario (developer menu).
    pub fn set_scenario(&self, scenario: Scenario) {
        *self
            .shared
            .scenario
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = scenario;
        let mut changed = Vec::new();
        {
            let mut inner = self.shared.lock();
            for d in &self.shared.drivers {
                let ready = !(scenario == Scenario::DriverNotDownloaded
                    && d.access() != AccessLayer::Native);
                let st = inner
                    .driver_status
                    .get_mut(&d.id)
                    .expect("status for every driver");
                if !ready && st.is_ready() {
                    *st = DriverStatus::NotDownloaded;
                    changed.push((d.id.clone(), st.clone()));
                }
            }
        }
        for (id, st) in changed {
            self.shared.emit_driver(DriverEvent::StatusChanged(id, st));
        }
    }

    fn insert_source(&self, config: DataSourceConfig) {
        let caps = self
            .shared
            .driver(&config.driver)
            .map(|d| d.capabilities.clone())
            .expect("mock data sources use catalog drivers");
        self.shared.lock().sources.push(DataSourceInfo {
            config,
            capabilities: caps,
            state: ConnectionState::Disconnected,
        });
    }
}

// ---------------------------------------------------------------- Backend

impl Backend for MockBackend {
    fn name(&self) -> &str {
        "mock"
    }
    fn data_sources(&self) -> &dyn DataSourceService {
        self
    }
    fn drivers(&self) -> &dyn DriverCatalogService {
        self
    }
    fn queries(&self) -> &dyn QueryService {
        self
    }
    fn metadata(&self) -> &dyn MetadataService {
        self
    }
    fn language(&self) -> &dyn LanguageService {
        self
    }
    fn history(&self) -> &dyn HistoryService {
        self
    }
    fn search(&self) -> &dyn SearchIndexService {
        self
    }
}

// ---------------------------------------------------------------- data sources

impl DataSourceService for MockBackend {
    fn list(&self) -> Vec<DataSourceInfo> {
        self.shared.lock().sources.clone()
    }

    fn get(&self, id: &DataSourceId) -> Option<DataSourceInfo> {
        self.shared.source(id).ok()
    }

    fn save(&self, config: DataSourceConfig, _secrets: SecretValues) -> BoxFuture<'_, Result<()>> {
        async move {
            let id = config.id.clone();
            let existed = {
                let mut inner = self.shared.lock();
                match inner.sources.iter_mut().find(|s| s.config.id == id) {
                    Some(s) => {
                        s.config = config.clone();
                        true
                    }
                    None => false,
                }
            };
            if existed {
                self.shared.emit_ds(DataSourceEvent::Changed(id));
            } else {
                if self.shared.driver(&config.driver).is_none() {
                    return Err(Error::NotFound(config.driver.to_string()));
                }
                self.insert_source(config);
                self.shared.emit_ds(DataSourceEvent::Added(id));
            }
            Ok(())
        }
        .boxed()
    }

    fn remove(&self, id: &DataSourceId) -> BoxFuture<'_, Result<()>> {
        let id = id.clone();
        async move {
            self.shared.lock().sources.retain(|s| s.config.id != id);
            self.shared.emit_ds(DataSourceEvent::Removed(id));
            Ok(())
        }
        .boxed()
    }

    fn test(
        &self,
        config: DataSourceConfig,
        _secrets: SecretValues,
    ) -> BoxFuture<'_, Result<TestConnectionReport>> {
        async move {
            let scenario = self.shared.scenario();
            self.shared.delay(scenario.latency() * 3).await;
            self.check_connectable(&config.driver, scenario)?;
            Ok(TestConnectionReport {
                server_version: format!("{} (mock) 1.0", config.driver),
                latency_ms: scenario.latency().as_millis() as u64,
            })
        }
        .boxed()
    }

    fn connect(&self, id: &DataSourceId) -> BoxFuture<'_, Result<()>> {
        let id = id.clone();
        async move {
            let src = self.shared.source(&id)?;
            let scenario = self.shared.scenario();
            self.shared.set_state(&id, ConnectionState::Connecting);
            self.shared.delay(scenario.latency() * 5).await;
            match self.check_connectable(&src.config.driver, scenario) {
                Ok(()) => {
                    self.shared.set_state(&id, ConnectionState::Connected);
                    Ok(())
                }
                Err(e) => {
                    self.shared
                        .set_state(&id, ConnectionState::Failed(e.to_string()));
                    Err(e)
                }
            }
        }
        .boxed()
    }

    fn disconnect(&self, id: &DataSourceId) -> BoxFuture<'_, Result<()>> {
        let id = id.clone();
        async move {
            self.shared.set_state(&id, ConnectionState::Disconnected);
            Ok(())
        }
        .boxed()
    }

    fn subscribe(&self) -> async_channel::Receiver<DataSourceEvent> {
        let (tx, rx) = async_channel::unbounded();
        self.shared.lock().ds_subscribers.push(tx);
        rx
    }
}

impl MockBackend {
    fn check_connectable(&self, driver: &DriverId, scenario: Scenario) -> Result<()> {
        if scenario == Scenario::ConnectionError {
            return Err(Error::Connection(
                "could not connect to server: Connection refused (localhost:5432)".into(),
            ));
        }
        if !self.status(driver).is_ready() {
            return Err(Error::DriverMissing(driver.to_string()));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- drivers

impl DriverCatalogService for MockBackend {
    fn list(&self) -> Vec<DriverDescriptor> {
        self.shared.drivers.clone()
    }

    fn get(&self, id: &DriverId) -> Option<DriverDescriptor> {
        self.shared.driver(id).cloned()
    }

    fn status(&self, id: &DriverId) -> DriverStatus {
        self.shared
            .lock()
            .driver_status
            .get(id)
            .cloned()
            .unwrap_or(DriverStatus::NotDownloaded)
    }

    fn versions(&self, id: &DriverId) -> Vec<String> {
        match self.shared.driver(id) {
            Some(d) if d.access() != AccessLayer::Native => {
                vec!["1.2.0".into(), "1.1.3".into(), "1.0.0".into()]
            }
            _ => Vec::new(),
        }
    }

    fn download(&self, id: &DriverId, version: Option<String>) -> BoxFuture<'_, Result<()>> {
        let id = id.clone();
        async move {
            if self.shared.driver(&id).is_none() {
                return Err(Error::NotFound(id.to_string()));
            }
            let version = version.unwrap_or_else(|| "1.2.0".into());
            let step = self.shared.scenario().latency();
            for p in (0..=1000u16).step_by(100) {
                let st = DriverStatus::Downloading {
                    progress_permille: p,
                };
                self.shared
                    .lock()
                    .driver_status
                    .insert(id.clone(), st.clone());
                self.shared
                    .emit_driver(DriverEvent::StatusChanged(id.clone(), st));
                self.shared.delay(step).await;
            }
            let st = DriverStatus::Downloaded { version };
            self.shared
                .lock()
                .driver_status
                .insert(id.clone(), st.clone());
            self.shared.emit_driver(DriverEvent::StatusChanged(id, st));
            Ok(())
        }
        .boxed()
    }

    fn use_custom_artifacts(
        &self,
        id: &DriverId,
        paths: Vec<std::path::PathBuf>,
    ) -> BoxFuture<'_, Result<()>> {
        let id = id.clone();
        async move {
            if self.shared.driver(&id).is_none() {
                return Err(Error::NotFound(id.to_string()));
            }
            let st = DriverStatus::Custom { files: paths.len() };
            self.shared
                .lock()
                .driver_status
                .insert(id.clone(), st.clone());
            self.shared.emit_driver(DriverEvent::StatusChanged(id, st));
            Ok(())
        }
        .boxed()
    }

    fn jdbc_bridge(&self) -> BridgeState {
        self.shared.lock().bridge.clone()
    }

    fn subscribe(&self) -> async_channel::Receiver<DriverEvent> {
        let (tx, rx) = async_channel::unbounded();
        self.shared.lock().driver_subscribers.push(tx);
        rx
    }
}

// ---------------------------------------------------------------- queries

impl QueryService for MockBackend {
    fn open_session(&self, ds: &DataSourceId) -> BoxFuture<'_, Result<SessionId>> {
        let ds = ds.clone();
        async move {
            let src = self.shared.source(&ds)?;
            if src.state != ConnectionState::Connected {
                self.connect(&ds).await?;
            }
            let id = SessionId(self.shared.id());
            self.shared.lock().sessions.insert(
                id,
                SessionInfo {
                    id,
                    data_source: ds,
                    opened_at: SystemTime::now(),
                    in_transaction: src.config.tx_mode == TxMode::Manual,
                    schema: None,
                    busy: false,
                },
            );
            Ok(id)
        }
        .boxed()
    }

    fn close_session(&self, session: SessionId) -> BoxFuture<'_, Result<()>> {
        async move {
            self.shared.lock().sessions.remove(&session);
            Ok(())
        }
        .boxed()
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        let mut v: Vec<_> = self.shared.lock().sessions.values().cloned().collect();
        v.sort_by_key(|s| s.id);
        v
    }

    fn estimate_cost(
        &self,
        session: SessionId,
        script: Script,
    ) -> BoxFuture<'_, Result<Option<u64>>> {
        async move {
            let src = self.shared.session_source(session)?;
            self.shared.delay(self.shared.scenario().latency()).await;
            if !src.capabilities.cost_estimate {
                return Ok(None);
            }
            // Deterministic "bytes processed" derived from the text.
            let h = data::mix(script.text.len() as u64 + 17);
            Ok(Some(50_000_000 + h % 5_000_000_000))
        }
        .boxed()
    }

    fn explain(&self, session: SessionId, script: Script) -> BoxFuture<'_, Result<PlanNode>> {
        async move {
            let src = self.shared.session_source(session)?;
            self.shared.delay(self.shared.scenario().latency()).await;
            if src.capabilities.explain == ExplainSupport::None {
                return Err(Error::Unsupported("explain".into()));
            }
            Ok(objects::plan(src.capabilities.data_model, &script.text))
        }
        .boxed()
    }

    fn key_value(&self, session: SessionId, key: String) -> BoxFuture<'_, Result<KeyValue>> {
        async move {
            let src = self.shared.session_source(session)?;
            if src.capabilities.data_model != DataModel::KeyValue {
                return Err(Error::Unsupported("key_value".into()));
            }
            self.shared.delay(self.shared.scenario().latency()).await;
            Ok(data::key_value(&key))
        }
        .boxed()
    }

    fn preview_changes(
        &self,
        result: ResultId,
        changes: ChangeSet,
    ) -> BoxFuture<'_, Result<Vec<String>>> {
        async move {
            let meta = self
                .shared
                .lock()
                .results
                .get(&result)
                .cloned()
                .ok_or_else(|| Error::NotFound(result.to_string()))?;
            Ok(changes
                .changes
                .iter()
                .map(|c| change_statement(&meta, c))
                .collect())
        }
        .boxed()
    }

    fn execute(&self, session: SessionId, script: Script, opts: ExecOptions) -> QueryHandle {
        // Bounded: a slow consumer blocks the mock thread (backpressure). The
        // deterministic mode runs inline, so it must never block.
        let (tx, rx) = if self.shared.deterministic {
            async_channel::unbounded()
        } else {
            async_channel::bounded(64)
        };
        let cancel = CancelHandle::new();
        let handle = QueryHandle {
            id: QueryId(self.shared.id()),
            events: rx,
            cancel: cancel.clone(),
        };
        let this = self.clone();
        if self.shared.deterministic {
            this.run_script(session, script, opts, tx, cancel);
        } else {
            thread::Builder::new()
                .name("mock-query".into())
                .spawn(move || this.run_script(session, script, opts, tx, cancel))
                .expect("spawn mock query thread");
        }
        handle
    }

    fn fetch_more(&self, result: ResultId, items: usize) -> BoxFuture<'_, Result<()>> {
        async move {
            let (tx, kind, from, to, cancel) = {
                let inner = self.shared.lock();
                let c = inner
                    .cursors
                    .get(&result)
                    .ok_or_else(|| Error::NotFound(result.to_string()))?;
                (
                    c.tx.clone(),
                    c.kind.clone(),
                    c.next,
                    (c.next + items as u64).min(c.total),
                    c.cancel.clone(),
                )
            };
            let (sent, tx) = if self.shared.deterministic {
                (self.stream_items(result, &kind, from, to, &tx, &cancel), tx)
            } else {
                let this = self.clone();
                thread::spawn(move || {
                    let sent = this.stream_items(result, &kind, from, to, &tx, &cancel);
                    (sent, tx)
                })
                .join()
                .map_err(|_| Error::Other("mock thread panicked".into()))?
            };
            let has_more = {
                let mut inner = self.shared.lock();
                let c = inner
                    .cursors
                    .get_mut(&result)
                    .ok_or_else(|| Error::NotFound(result.to_string()))?;
                c.next = sent;
                c.next < c.total
            };
            let _ = tx.send(QueryEvent::ResultPaused { result, has_more }).await;
            if !has_more {
                self.shared.lock().cursors.remove(&result);
            }
            Ok(())
        }
        .boxed()
    }

    fn apply_changes(
        &self,
        result: ResultId,
        changes: ChangeSet,
    ) -> BoxFuture<'_, Result<ApplyReport>> {
        async move {
            self.shared.delay(self.shared.scenario().latency()).await;
            let statements = self.preview_changes(result, changes.clone()).await?;
            Ok(ApplyReport {
                applied: changes.changes.len(),
                statements,
            })
        }
        .boxed()
    }

    fn commit(&self, _session: SessionId) -> BoxFuture<'_, Result<()>> {
        self.shared
            .delay(self.shared.scenario().latency())
            .map(Ok)
            .boxed()
    }

    fn rollback(&self, _session: SessionId) -> BoxFuture<'_, Result<()>> {
        self.shared
            .delay(self.shared.scenario().latency())
            .map(Ok)
            .boxed()
    }
}

impl MockBackend {
    /// Sends items `[from, to)` in chunks; returns the next offset (stops early on cancel).
    fn stream_items(
        &self,
        result: ResultId,
        kind: &CursorKind,
        from: u64,
        to: u64,
        tx: &async_channel::Sender<QueryEvent>,
        cancel: &CancelHandle,
    ) -> u64 {
        let mut at = from;
        if let CursorKind::Keys(pattern) = kind {
            // Scan positions: `to - from` keys are examined, matches are sent.
            while at < to {
                if cancel.is_cancelled() {
                    break;
                }
                let end = (at + CHUNK as u64).min(to);
                let keys: Vec<KeyEntry> = (at..end)
                    .map(data::key)
                    .filter(|k| glob_match(pattern, &k.key))
                    .collect();
                at = end;
                if !keys.is_empty()
                    && tx
                        .send_blocking(QueryEvent::Data {
                            result,
                            chunk: ResultChunk::Keys(Arc::new(keys)),
                        })
                        .is_err()
                {
                    break;
                }
            }
            return at;
        }
        while at < to {
            if cancel.is_cancelled() {
                break;
            }
            let n = ((to - at) as usize).min(CHUNK);
            let chunk = match kind {
                CursorKind::Rows(schema) => {
                    ResultChunk::Rows(Arc::new(data::table_batch(schema, at, n)))
                }
                CursorKind::Documents => {
                    if self.shared.scenario() == Scenario::FiveMbDocument && at == 0 {
                        let mut docs = vec![data::big_document(5 * 1024 * 1024)];
                        docs.extend((1..n as u64).map(data::document));
                        ResultChunk::Documents(Arc::new(docs))
                    } else {
                        ResultChunk::Documents(Arc::new(
                            (at..at + n as u64).map(data::document).collect(),
                        ))
                    }
                }
                CursorKind::Keys(_) => unreachable!("handled above"),
            };
            // Bounded channel: blocks this background thread when the UI lags (backpressure).
            if tx
                .send_blocking(QueryEvent::Data { result, chunk })
                .is_err()
            {
                break;
            }
            at += n as u64;
        }
        at
    }

    fn run_script(
        &self,
        session: SessionId,
        script: Script,
        opts: ExecOptions,
        tx: async_channel::Sender<QueryEvent>,
        cancel: CancelHandle,
    ) {
        let send = |ev| tx.send_blocking(ev).is_ok();
        let scenario = self.shared.scenario();
        let Ok(src) = self.shared.session_source(session) else {
            send(QueryEvent::Error(QueryError {
                message: "session is closed".into(),
                code: None,
                range: None,
            }));
            send(QueryEvent::Finished);
            return;
        };
        let caps = &src.capabilities;
        if scenario == Scenario::JdbcProcessFailure && caps.access == AccessLayer::Jdbc {
            self.shared.set_bridge(BridgeState::Restarting);
            self.shared.sleep(scenario.latency());
            self.shared.update_bridge();
            send(QueryEvent::Error(QueryError {
                message: "JDBC bridge process terminated unexpectedly; restarting".into(),
                code: Some("FERRODB-JDBC".into()),
                range: None,
            }));
            send(QueryEvent::Finished);
            return;
        }
        let doc = DocSnapshot {
            text: script.text.as_str().into(),
            language: caps.language.clone(),
            data_source: Some(src.config.id.clone()),
            version: 0,
        };
        let statements = self.statements(&doc);
        for (index, span) in statements.into_iter().enumerate() {
            if cancel.is_cancelled() {
                send(QueryEvent::Error(QueryError {
                    message: "cancelled".into(),
                    code: None,
                    range: None,
                }));
                break;
            }
            let range = span.range.start + script.base_offset..span.range.end + script.base_offset;
            let text = &script.text[span.range.clone()];
            let started = Instant::now();
            if !send(QueryEvent::StatementStarted {
                index,
                range: range.clone(),
            }) {
                return;
            }
            if caps.cost_estimate && opts.estimate_cost {
                send(QueryEvent::CostEstimate {
                    bytes: 1_234_567_890,
                });
            }
            self.shared.sleep(scenario.latency());
            if let Some(err) = Self::simulated_error(text, &range) {
                send(QueryEvent::Error(err));
                self.shared.lock().history.push(HistoryEntry {
                    data_source: src.config.id.clone(),
                    text: text.to_owned(),
                    executed_at: SystemTime::now(),
                    elapsed: started.elapsed(),
                    succeeded: false,
                });
                break;
            }
            let lower = text.trim_start().to_ascii_lowercase();
            let is_write = [
                "insert",
                "update",
                "delete",
                "set ",
                "db.coll.insert",
                "del ",
            ]
            .iter()
            .any(|w| lower.starts_with(w));
            if is_write {
                if src.config.read_only {
                    send(QueryEvent::Error(QueryError {
                        message: Error::ReadOnly.to_string(),
                        code: None,
                        range: Some(range.clone()),
                    }));
                    break;
                }
                send(QueryEvent::StatementFinished {
                    index,
                    affected: Some(1),
                    elapsed: started.elapsed(),
                });
                continue;
            }
            let result = ResultId(self.shared.id());
            let container = container_name(text, &caps.language);
            let (kind, shape, total) = match caps.data_model {
                DataModel::Tabular => {
                    let schema = data::table_schema(scenario.table_columns());
                    (
                        CursorKind::Rows(schema.clone()),
                        ResultShape::Table(schema),
                        scenario.table_rows(),
                    )
                }
                DataModel::Document => (
                    CursorKind::Documents,
                    ResultShape::Documents {
                        container: Some(container.clone()),
                    },
                    scenario.documents(),
                ),
                DataModel::KeyValue => {
                    let pattern = key_pattern(text);
                    (
                        CursorKind::Keys(pattern.as_str().into()),
                        ResultShape::Keys { pattern },
                        scenario.keys(),
                    )
                }
            };
            self.shared.lock().results.insert(
                result,
                ResultMeta {
                    kind: kind.clone(),
                    container,
                    language: caps.language.clone(),
                },
            );
            let title = text
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("-- "))
                .map(str::to_owned);
            send(QueryEvent::ResultStarted {
                result,
                shape,
                title,
            });
            let first = (opts.page_size as u64).min(total);
            let next = self.stream_items(result, &kind, 0, first, &tx, &cancel);
            let has_more = next < total;
            if has_more {
                self.shared.lock().cursors.insert(
                    result,
                    Cursor {
                        tx: tx.clone(),
                        kind,
                        next,
                        total,
                        cancel: cancel.clone(),
                    },
                );
            }
            send(QueryEvent::ResultPaused { result, has_more });
            send(QueryEvent::StatementFinished {
                index,
                affected: None,
                elapsed: started.elapsed(),
            });
            self.shared.lock().history.push(HistoryEntry {
                data_source: src.config.id.clone(),
                text: text.to_owned(),
                executed_at: SystemTime::now(),
                elapsed: started.elapsed(),
                succeeded: true,
            });
        }
        send(QueryEvent::Finished);
    }

    /// Any statement mentioning `error` fails, pointing at that word.
    fn simulated_error(text: &str, range: &Range<usize>) -> Option<QueryError> {
        let pos = text.to_ascii_lowercase().find("error")?;
        let start = range.start + pos;
        Some(QueryError {
            message: format!("syntax error at or near \"{}\"", &text[pos..pos + 5]),
            code: Some("42601".into()),
            range: Some(start..start + 5),
        })
    }
}

/// Collection / table targeted by a statement: `db.<name>.…` for MQL,
/// otherwise the identifier after `FROM` / `UPDATE` / `INTO`.
fn container_name(text: &str, lang: &QueryLanguage) -> String {
    if *lang == QueryLanguage::Mql
        && let Some(rest) = text.trim_start().strip_prefix("db.")
    {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            return name;
        }
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    words
        .windows(2)
        .find(|w| ["from", "update", "into"].contains(&w[0].to_ascii_lowercase().as_str()))
        .map(|w| {
            w[1].trim_matches(|c: char| c == '"' || c == '`' || c == ';' || c == '(')
                .to_owned()
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "customers".into())
}

/// Pattern of `SCAN … MATCH <p>` / `KEYS <p>`; `*` otherwise.
fn key_pattern(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    words
        .windows(2)
        .find(|w| w[0].eq_ignore_ascii_case("match") || w[0].eq_ignore_ascii_case("keys"))
        .map(|w| w[1].trim_matches('"').to_owned())
        .unwrap_or_else(|| "*".into())
}

/// Redis-style glob: `*` any run, `?` one character (no classes).
pub fn glob_match(pattern: &str, s: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    pattern == "*" || go(pattern.as_bytes(), s.as_bytes())
}

/// JSON value (as sent by the UI) → SQL literal.
fn sql_literal(json: &str) -> String {
    let v = json.trim();
    if v == "null" {
        "NULL".into()
    } else if let Some(inner) = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        format!("'{}'", inner.replace("\\\"", "\"").replace('\'', "''"))
    } else {
        v.to_owned()
    }
}

/// The statement / command the mock "runs" for one change, in the language
/// of the source (this is what the operation preview shows).
fn change_statement(meta: &ResultMeta, change: &Change) -> String {
    let c = &meta.container;
    let column = |ix: usize| match &meta.kind {
        CursorKind::Rows(schema) => schema
            .fields()
            .get(ix)
            .map(|f| f.name().clone())
            .unwrap_or_else(|| format!("col{ix}")),
        _ => format!("col{ix}"),
    };
    let doc_id = |ix: usize| {
        data::document(ix as u64)
            .get_str("_id")
            .map(str::to_owned)
            .unwrap_or_default()
    };
    match (change, &meta.language) {
        (
            Change::SetCell {
                row,
                column: col,
                value,
            },
            _,
        ) => format!(
            "UPDATE {c} SET {} = {} WHERE id = {};",
            column(*col),
            sql_literal(value),
            row + 1
        ),
        (Change::InsertRow { values }, _) => {
            let cols: Vec<String> = (0..values.len()).map(column).collect();
            let vals: Vec<String> = values
                .iter()
                .map(|v| v.as_deref().map_or("NULL".into(), sql_literal))
                .collect();
            format!(
                "INSERT INTO {c} ({}) VALUES ({});",
                cols.join(", "),
                vals.join(", ")
            )
        }
        (Change::DeleteRow { row }, _) => format!("DELETE FROM {c} WHERE id = {};", row + 1),
        (
            Change::SetField {
                document,
                path,
                value,
            },
            QueryLanguage::SqlPlusPlus,
        ) => format!(
            "UPDATE {c} USE KEYS \"{}\" SET {path} = {value};",
            doc_id(*document)
        ),
        (
            Change::SetField {
                document,
                path,
                value,
            },
            QueryLanguage::PartiQl,
        ) => format!(
            "UPDATE \"{c}\" SET {path} = {value} WHERE pk = '{}';",
            doc_id(*document)
        ),
        (
            Change::SetField {
                document,
                path,
                value,
            },
            _,
        ) => format!(
            "db.{c}.updateOne({{ _id: \"{}\" }}, {{ $set: {{ \"{path}\": {value} }} }})",
            doc_id(*document)
        ),
        (Change::DeleteDocument { document }, QueryLanguage::Mql) => {
            format!("db.{c}.deleteOne({{ _id: \"{}\" }})", doc_id(*document))
        }
        (Change::DeleteDocument { document }, _) => {
            format!("DELETE FROM {c} WHERE pk = '{}';", doc_id(*document))
        }
        (Change::SetKey { key, value }, _) => format!("SET {key} {value:?}"),
        (Change::SetKeyMember { key, member, value }, _) => match data::key_value(key).kind() {
            KeyType::Hash => format!("HSET {key} {member} {value:?}"),
            KeyType::List => format!("LSET {key} {member} {value:?}"),
            KeyType::ZSet => format!("ZADD {key} {value} {member:?}"),
            KeyType::Set => format!("SREM {key} {member:?}\nSADD {key} {value:?}"),
            _ => format!("SET {key} {value:?}"),
        },
        (
            Change::ExpireKey {
                key,
                ttl_secs: Some(s),
            },
            _,
        ) => format!("EXPIRE {key} {s}"),
        (
            Change::ExpireKey {
                key,
                ttl_secs: None,
            },
            _,
        ) => format!("PERSIST {key}"),
        (Change::DeleteKey { key }, _) => format!("DEL {key}"),
    }
}

// ---------------------------------------------------------------- metadata

impl MetadataService for MockBackend {
    fn hierarchy(&self, ds: &DataSourceId) -> Arc<HierarchySpec> {
        let (driver, model) = self
            .shared
            .source(ds)
            .map(|s| (s.config.driver.0, s.capabilities.data_model))
            .unwrap_or_else(|_| ("postgresql".into(), DataModel::Tabular));
        Arc::new(objects::hierarchy(&driver, model))
    }

    fn children(&self, node: NodeRef) -> BoxFuture<'_, Result<Vec<DbObject>>> {
        async move {
            let src = self.shared.source(&node.data_source)?;
            let scenario = self.shared.scenario();
            self.shared.delay(scenario.latency()).await;
            if scenario == Scenario::ConnectionError {
                return Err(Error::ConnectionLost);
            }
            Ok(objects::children(
                src.config.driver.as_str(),
                src.capabilities.data_model,
                scenario,
                &node,
            ))
        }
        .boxed()
    }

    fn snapshot(&self, _ds: &DataSourceId) -> Option<Arc<SchemaSnapshot>> {
        None
    }

    fn refresh(&self, _ds: &DataSourceId, _scope: RefreshScope) -> BoxFuture<'_, Result<()>> {
        self.shared
            .delay(self.shared.scenario().latency())
            .map(Ok)
            .boxed()
    }

    fn infer_schema(
        &self,
        _container: NodeRef,
        sample: SampleOptions,
    ) -> BoxFuture<'_, Result<InferredSchema>> {
        async move {
            self.shared
                .delay(self.shared.scenario().latency() * 4)
                .await;
            let f = |path: &str, ty: &str, presence: f32| FieldStats {
                path: path.into(),
                types: vec![(ty.into(), 1.0)],
                presence,
                samples: vec![],
            };
            Ok(InferredSchema {
                sampled: sample.sample_size,
                fields: vec![
                    f("_id", "objectId", 1.0),
                    f("name", "string", 1.0),
                    f("age", "int", 1.0),
                    f("address.city", "string", 0.97),
                    f("address.zip", "string", 0.97),
                    f("tags", "array", 1.0),
                    f("balance", "double", 1.0),
                    f("active", "bool", 1.0),
                ],
            })
        }
        .boxed()
    }

    fn definition(
        &self,
        objects: Vec<NodeRef>,
        _opts: DdlOptions,
    ) -> BoxFuture<'_, Result<String>> {
        async move {
            let schema = data::table_schema(self.shared.scenario().table_columns());
            let mut out = String::new();
            for o in objects {
                let name = o.name().unwrap_or("object");
                out.push_str(&format!("CREATE TABLE {name} (\n"));
                // Production lags behind development by one column (schema compare demo).
                let lagging = o.data_source.as_str().contains("prod");
                let n = schema.fields().len() - usize::from(lagging && schema.fields().len() > 1);
                let cols: Vec<String> = schema
                    .fields()
                    .iter()
                    .take(n)
                    .map(|f| format!("    {} {}", f.name(), f.data_type()))
                    .collect();
                out.push_str(&cols.join(",\n"));
                out.push_str("\n);\n\n");
            }
            Ok(out)
        }
        .boxed()
    }
}

// ---------------------------------------------------------------- language

const SQL_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "JOIN", "LEFT", "INNER", "ON", "GROUP", "BY", "ORDER", "LIMIT",
    "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "AND", "OR", "NOT",
    "NULL", "AS", "DISTINCT", "HAVING", "WITH", "UNION",
];

impl LanguageService for MockBackend {
    fn complete(&self, doc: DocSnapshot, offset: usize) -> BoxFuture<'_, Vec<CompletionItem>> {
        async move {
            let text = &doc.text[..offset.min(doc.text.len())];
            let start = text
                .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
                .map(|i| i + 1)
                .unwrap_or(0);
            let prefix = text[start..].to_ascii_lowercase();
            let mut items: Vec<CompletionItem> = SQL_KEYWORDS
                .iter()
                .filter(|k| k.to_ascii_lowercase().starts_with(&prefix))
                .map(|k| CompletionItem {
                    label: k.to_string(),
                    kind: CompletionKind::Keyword,
                    insert_text: k.to_string(),
                    detail: None,
                    replace: start..offset,
                })
                .collect();
            items.extend(
                ["customers", "orders", "order_items", "products"]
                    .iter()
                    .filter(|t| t.starts_with(&prefix))
                    .map(|t| CompletionItem {
                        label: t.to_string(),
                        kind: CompletionKind::Table,
                        insert_text: t.to_string(),
                        detail: Some("public".into()),
                        replace: start..offset,
                    }),
            );
            items
        }
        .boxed()
    }

    fn diagnostics(&self, doc: DocSnapshot) -> BoxFuture<'_, Vec<Diagnostic>> {
        async move {
            self.statements(&doc)
                .into_iter()
                .filter_map(|s| {
                    Self::simulated_error(&doc.text[s.range.clone()], &s.range).map(|e| {
                        Diagnostic {
                            range: e.range.unwrap_or(s.range),
                            severity: Severity::Error,
                            message: e.message,
                        }
                    })
                })
                .collect()
        }
        .boxed()
    }

    fn statements(&self, doc: &DocSnapshot) -> Vec<StatementSpan> {
        split_statements(&doc.text)
    }

    fn parameters(&self, text: &str, lang: &QueryLanguage) -> Vec<Parameter> {
        ferrodb_lang::BasicLanguageService.parameters(text, lang)
    }

    fn risks(&self, text: &str, lang: &QueryLanguage, all_modifications: bool) -> Vec<Risk> {
        ferrodb_lang::BasicLanguageService.risks(text, lang, all_modifications)
    }

    fn format(&self, text: &str, _lang: &QueryLanguage, style: &FormatStyle) -> Result<String> {
        if !style.uppercase_keywords {
            return Ok(text.to_owned());
        }
        Ok(text
            .split(' ')
            .map(|w| {
                if SQL_KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(w)) {
                    w.to_ascii_uppercase()
                } else {
                    w.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" "))
    }
}

/// Splits on `;` outside single/double quotes; trims whitespace from spans and
/// drops empty statements. (The real splitter lives in `ferrodb-lang`.)
pub fn split_statements(text: &str) -> Vec<StatementSpan> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let push = |out: &mut Vec<StatementSpan>, s: usize, e: usize| {
        let slice = &text[s..e];
        let lead = slice.len() - slice.trim_start().len();
        let trail = slice.len() - slice.trim_end().len();
        if lead + trail < slice.len() {
            out.push(StatementSpan {
                range: s + lead..e - trail,
            });
        }
    };
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, ';') => {
                push(&mut out, start, i);
                start = i + 1;
            }
            _ => {}
        }
    }
    push(&mut out, start, text.len());
    out
}

// ---------------------------------------------------------------- history & search

impl HistoryService for MockBackend {
    fn record(&self, entry: HistoryEntry) {
        self.shared.lock().history.push(entry);
    }

    fn query(&self, filter: HistoryFilter) -> BoxFuture<'_, Result<Vec<HistoryEntry>>> {
        async move {
            let inner = self.shared.lock();
            let mut v: Vec<_> = inner
                .history
                .iter()
                .rev()
                .filter(|h| {
                    filter
                        .data_source
                        .as_ref()
                        .is_none_or(|d| &h.data_source == d)
                })
                .filter(|h| {
                    filter
                        .text
                        .as_ref()
                        .is_none_or(|t| h.text.contains(t.as_str()))
                })
                .cloned()
                .collect();
            if let Some(l) = filter.limit {
                v.truncate(l);
            }
            Ok(v)
        }
        .boxed()
    }
}

impl SearchIndexService for MockBackend {
    fn entries(&self) -> Vec<SearchEntry> {
        let scenario = self.shared.scenario();
        let mut out = Vec::new();
        for src in self.shared.lock().sources.clone() {
            out.push(SearchEntry {
                label: src.config.name.clone(),
                location: src.config.driver.to_string(),
                kind: None,
                target: SearchTarget::DataSource(src.config.id.clone()),
            });
            // Index two levels below the root, plus objects of the first container.
            let driver = src.config.driver.as_str();
            let model = src.capabilities.data_model;
            let mut frontier = vec![NodeRef::root(src.config.id.clone())];
            for _ in 0..3 {
                let mut next = Vec::new();
                for node in frontier.iter().take(4) {
                    for o in objects::children(driver, model, scenario, node) {
                        let location = std::iter::once(src.config.name.as_str())
                            .chain(node.path.iter().map(|(_, n)| n.as_str()))
                            .collect::<Vec<_>>()
                            .join(" / ");
                        out.push(SearchEntry {
                            label: o.name.clone(),
                            location,
                            kind: Some(o.kind),
                            target: SearchTarget::Object(o.node.clone()),
                        });
                        if o.has_children {
                            next.push(o.node);
                        }
                    }
                }
                frontier = next;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    fn collect(h: &QueryHandle) -> Vec<QueryEvent> {
        let mut v = Vec::new();
        while let Ok(ev) = h.events.recv_blocking() {
            let done = matches!(ev, QueryEvent::Finished);
            v.push(ev);
            if done {
                break;
            }
        }
        v
    }

    #[test]
    fn splitter_respects_quotes() {
        let s = split_statements("select ';'; select 2 ;  ; ");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].range, 0..10);
    }

    #[test]
    fn each_profile_produces_its_model() {
        let b = MockBackend::new();
        for (p, expect) in [
            (MockProfile::PostgresLike, DataModel::Tabular),
            (MockProfile::BigQueryLike, DataModel::Tabular),
            (MockProfile::CassandraLike, DataModel::Tabular),
            (MockProfile::MongoLike, DataModel::Document),
            (MockProfile::RedisLike, DataModel::KeyValue),
        ] {
            let ds = MockBackend::profile_source(p);
            let caps = DataSourceService::get(&b, &ds).unwrap().capabilities;
            assert_eq!(caps.data_model, expect, "{p:?}");
            let s = block_on(b.open_session(&ds)).unwrap();
            let h = b.execute(s, Script::new("select 1"), ExecOptions::default());
            let evs = collect(&h);
            let items: usize = evs
                .iter()
                .map(|e| {
                    if let QueryEvent::Data { chunk, .. } = e {
                        chunk.len()
                    } else {
                        0
                    }
                })
                .sum();
            let total = match expect {
                DataModel::Document => Scenario::Normal.documents(),
                DataModel::KeyValue => Scenario::Normal.keys(),
                DataModel::Tabular => Scenario::Normal.table_rows(),
            };
            assert_eq!(items as u64, total.min(500), "{p:?}: first page");
            let shape_ok = evs.iter().any(|e| {
                let QueryEvent::ResultStarted { shape, .. } = e else {
                    return false;
                };
                matches!(
                    (shape, expect),
                    (ResultShape::Table(_), DataModel::Tabular)
                        | (ResultShape::Documents { .. }, DataModel::Document)
                        | (ResultShape::Keys { .. }, DataModel::KeyValue)
                )
            });
            assert!(shape_ok, "{p:?}");
        }
    }

    #[test]
    fn errors_carry_position_and_stop_script() {
        let b = MockBackend::new();
        let s = block_on(b.open_session(&MockBackend::profile_source(MockProfile::PostgresLike)))
            .unwrap();
        let h = b.execute(
            s,
            Script::new("select 1; select error; select 3"),
            ExecOptions::default(),
        );
        let evs = collect(&h);
        let err = evs.iter().find_map(|e| {
            if let QueryEvent::Error(e) = e {
                Some(e.clone())
            } else {
                None
            }
        });
        assert_eq!(err.unwrap().range, Some(17..22));
        let started = evs
            .iter()
            .filter(|e| matches!(e, QueryEvent::StatementStarted { .. }))
            .count();
        assert_eq!(started, 2);
    }

    #[test]
    fn fetch_more_pages_through_million_rows() {
        let b = MockBackend::new();
        b.set_scenario(Scenario::MillionRows);
        let s = block_on(b.open_session(&MockBackend::profile_source(MockProfile::PostgresLike)))
            .unwrap();
        let h = b.execute(s, Script::new("select * from big"), ExecOptions::default());
        let evs = collect(&h);
        let result = evs
            .iter()
            .find_map(|e| {
                if let QueryEvent::ResultStarted { result, .. } = e {
                    Some(*result)
                } else {
                    None
                }
            })
            .unwrap();
        let rx = h.events.clone();
        let drain = thread::spawn(move || {
            let mut n = 0;
            while let Ok(ev) = rx.recv_blocking() {
                match ev {
                    QueryEvent::Data { chunk, .. } => n += chunk.len(),
                    QueryEvent::ResultPaused { .. } => break,
                    _ => {}
                }
            }
            n
        });
        block_on(b.fetch_more(result, 10_000)).unwrap();
        assert_eq!(drain.join().unwrap(), 10_000);
    }

    #[test]
    fn missing_driver_blocks_connection_until_downloaded() {
        let b = MockBackend::new();
        let legacy = DataSourceId::new("legacy");
        assert_eq!(
            block_on(b.connect(&legacy)),
            Err(Error::DriverMissing("oracle".into()))
        );
        block_on(b.download(&DriverId::new("oracle"), None)).unwrap();
        block_on(b.connect(&legacy)).unwrap();
    }

    #[test]
    fn read_only_source_rejects_writes() {
        let b = MockBackend::new();
        let s = block_on(b.open_session(&DataSourceId::new("shop-prod"))).unwrap();
        let h = b.execute(s, Script::new("delete from orders"), ExecOptions::default());
        assert!(
            collect(&h)
                .iter()
                .any(|e| matches!(e, QueryEvent::Error(_)))
        );
    }

    fn first_result(h: &QueryHandle) -> (ResultId, Vec<QueryEvent>) {
        let evs = collect(h);
        let r = evs
            .iter()
            .find_map(|e| match e {
                QueryEvent::ResultStarted { result, .. } => Some(*result),
                _ => None,
            })
            .expect("a result");
        (r, evs)
    }

    #[test]
    fn key_scan_honours_match_pattern() {
        let b = MockBackend::new();
        let s =
            block_on(b.open_session(&MockBackend::profile_source(MockProfile::RedisLike))).unwrap();
        let h = b.execute(
            s,
            Script::new("SCAN 0 MATCH app:user:*"),
            ExecOptions::default(),
        );
        let (_, evs) = first_result(&h);
        let keys: Vec<KeyEntry> = evs
            .into_iter()
            .flat_map(|e| match e {
                QueryEvent::Data {
                    chunk: ResultChunk::Keys(k),
                    ..
                } => k.as_ref().clone(),
                _ => vec![],
            })
            .collect();
        assert!(!keys.is_empty());
        assert!(keys.iter().all(|k| k.key.starts_with("app:user:")));
        assert!(keys.iter().all(|k| k.kind == KeyType::Hash));
        // Values agree with the type reported by the scan.
        let v = block_on(b.key_value(s, keys[0].key.clone())).unwrap();
        assert_eq!(v.kind(), KeyType::Hash);
    }

    #[test]
    fn change_preview_speaks_the_source_language() {
        let b = MockBackend::new();
        let pg = block_on(b.open_session(&MockBackend::profile_source(MockProfile::PostgresLike)))
            .unwrap();
        let (r, _) = first_result(&b.execute(
            pg,
            Script::new("select * from orders"),
            ExecOptions::default(),
        ));
        let sql = block_on(b.preview_changes(
            r,
            ChangeSet {
                changes: vec![Change::SetCell {
                    row: 4,
                    column: 1,
                    value: "\"O'Hara\"".into(),
                }],
            },
        ))
        .unwrap();
        assert_eq!(sql, ["UPDATE orders SET name = 'O''Hara' WHERE id = 5;"]);

        let mongo =
            block_on(b.open_session(&MockBackend::profile_source(MockProfile::MongoLike))).unwrap();
        let (r, _) = first_result(&b.execute(
            mongo,
            Script::new("db.products.find({})"),
            ExecOptions::default(),
        ));
        let cmds = block_on(b.preview_changes(
            r,
            ChangeSet {
                changes: vec![Change::DeleteDocument { document: 0 }],
            },
        ))
        .unwrap();
        assert!(
            cmds[0].starts_with("db.products.deleteOne({ _id: \""),
            "{cmds:?}"
        );
    }

    #[test]
    fn cost_estimate_only_for_capable_sources() {
        let b = MockBackend::new();
        let bq = block_on(b.open_session(&MockBackend::profile_source(MockProfile::BigQueryLike)))
            .unwrap();
        let pg = block_on(b.open_session(&MockBackend::profile_source(MockProfile::PostgresLike)))
            .unwrap();
        assert!(
            block_on(b.estimate_cost(bq, Script::new("select 1")))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            block_on(b.estimate_cost(pg, Script::new("select 1"))).unwrap(),
            None
        );
        assert_eq!(b.sessions().len(), 2);
    }

    #[test]
    fn jdbc_bridge_runs_while_jdbc_sources_are_connected() {
        let b = MockBackend::new();
        assert_eq!(b.jdbc_bridge(), BridgeState::NotStarted);
        block_on(b.download(&DriverId::new("oracle"), Some("1.1.3".into()))).unwrap();
        assert_eq!(
            DriverCatalogService::status(&b, &DriverId::new("oracle")),
            DriverStatus::Downloaded {
                version: "1.1.3".into()
            }
        );
        block_on(b.connect(&DataSourceId::new("legacy"))).unwrap();
        assert!(matches!(
            b.jdbc_bridge(),
            BridgeState::Running { connections: 1, .. }
        ));
        block_on(b.disconnect(&DataSourceId::new("legacy"))).unwrap();
        assert_eq!(b.jdbc_bridge(), BridgeState::NotStarted);
    }

    #[test]
    fn glob() {
        assert!(glob_match("app:*:1?", "app:user:12"));
        assert!(!glob_match("app:*:1?", "app:user:123"));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn search_index_contains_tables_and_collections() {
        let b = MockBackend::new();
        let e = b.entries();
        assert!(
            e.iter()
                .any(|x| x.label == "customers" && x.kind == Some(ObjectKind::Table))
        );
        assert!(e.iter().any(|x| x.kind == Some(ObjectKind::Collection)));
    }
}
