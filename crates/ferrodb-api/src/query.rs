use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

use crate::{BoxFuture, DataSourceId, QueryError, QueryId, Result, ResultId, SessionId};

/// Text submitted for execution, already narrowed to what should run
/// (statement under cursor, selection or whole script).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Script {
    pub text: String,
    /// Offset of `text` inside the console document, so that ranges in events
    /// can be mapped back to the editor.
    pub base_offset: usize,
}

impl Script {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            base_offset: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecMode {
    #[default]
    StatementUnderCursor,
    Selection,
    WholeScript,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecOptions {
    pub mode: ExecMode,
    /// Items in the first page (rows, documents or keys). Default 500.
    pub page_size: usize,
    pub timeout: Option<Duration>,
    /// Ask for a cost estimate first (sources with `cost_estimate`).
    pub estimate_cost: bool,
    /// Default schema / database for the statements (console schema picker);
    /// `None` keeps the session's current one.
    pub schema: Option<String>,
    /// Values of the script's parameters (`:name`, `?`, `$1`) by name, as
    /// returned by `LanguageService::parameters`. `None` = SQL `NULL`.
    pub params: Vec<(String, Option<String>)>,
    /// The user confirmed the risks reported by `LanguageService::risks`.
    pub confirmed: bool,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            mode: ExecMode::default(),
            page_size: 500,
            timeout: None,
            estimate_cost: false,
            schema: None,
            params: Vec::new(),
            confirmed: false,
        }
    }
}

/// Cooperative cancellation shared by the UI and the executor. Executors map it
/// to the server mechanism (cancel request, `KILL QUERY`, `killOp`…).
#[derive(Clone, Debug, Default)]
pub struct CancelHandle(Arc<AtomicBool>);

impl CancelHandle {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Returned immediately by [`QueryService::execute`]; events stream in.
#[derive(Clone, Debug)]
pub struct QueryHandle {
    pub id: QueryId,
    pub events: async_channel::Receiver<QueryEvent>,
    pub cancel: CancelHandle,
}

#[derive(Clone, Debug)]
pub enum QueryEvent {
    StatementStarted {
        index: usize,
        range: Range<usize>,
    },
    CostEstimate {
        bytes: u64,
    },
    ResultStarted {
        result: ResultId,
        shape: ResultShape,
        title: Option<String>,
    },
    Data {
        result: ResultId,
        chunk: ResultChunk,
    },
    /// No more data for this result unless `fetch_more` is called.
    ResultPaused {
        result: ResultId,
        has_more: bool,
    },
    Notice(ServerNotice),
    StatementFinished {
        index: usize,
        affected: Option<u64>,
        elapsed: Duration,
    },
    Error(QueryError),
    Finished,
}

#[derive(Clone, Debug)]
pub enum ResultShape {
    Table(Arc<arrow_schema::Schema>),
    /// Collection / DynamoDB table / Couchbase keyspace.
    Documents {
        container: Option<String>,
    },
    Keys {
        pattern: String,
    },
}

#[derive(Clone, Debug)]
pub enum ResultChunk {
    Rows(Arc<arrow_array::RecordBatch>),
    Documents(Arc<Vec<bson::RawDocumentBuf>>),
    Keys(Arc<Vec<KeyEntry>>),
}

impl ResultChunk {
    pub fn len(&self) -> usize {
        match self {
            Self::Rows(b) => b.num_rows(),
            Self::Documents(d) => d.len(),
            Self::Keys(k) => k.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyType {
    String,
    Hash,
    List,
    Set,
    ZSet,
    Stream,
    Json,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEntry {
    pub key: String,
    pub kind: KeyType,
    /// Remaining time to live; `None` = no expiry.
    pub ttl: Option<Duration>,
    /// Size in bytes or elements, when known.
    pub size: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerNotice {
    pub level: NoticeLevel,
    pub message: String,
}

/// A cell / field / key change made in a result view (filled in phase 6).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChangeSet {
    pub changes: Vec<Change>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Tabular: set column `column` of row `row` (index in the result) to a JSON-encoded value.
    SetCell {
        row: usize,
        column: usize,
        value: String,
    },
    InsertRow {
        values: Vec<Option<String>>,
    },
    DeleteRow {
        row: usize,
    },
    /// Document: replace field at path with an Extended JSON value.
    SetField {
        document: usize,
        path: String,
        value: String,
    },
    DeleteDocument {
        document: usize,
    },
    /// Key-value: set the whole value of a string / JSON key.
    SetKey {
        key: String,
        value: String,
    },
    /// Key-value: set one member of a collection key — hash field, list
    /// index, zset member (value = score), set member (value = new member).
    SetKeyMember {
        key: String,
        member: String,
        value: String,
    },
    /// Key-value: change the TTL (`None` = persist).
    ExpireKey {
        key: String,
        ttl_secs: Option<u64>,
    },
    DeleteKey {
        key: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub applied: usize,
    /// Generated statements/commands shown in the operation preview.
    pub statements: Vec<String>,
}

/// Value of one key (Redis), loaded on demand by the key browser.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyValue {
    String(String),
    Hash(Vec<(String, String)>),
    List(Vec<String>),
    Set(Vec<String>),
    /// Members with scores, ordered by score.
    ZSet(Vec<(String, f64)>),
    Stream(Vec<StreamEntry>),
    /// RedisJSON document as JSON text.
    Json(String),
}

impl KeyValue {
    pub fn kind(&self) -> KeyType {
        match self {
            Self::String(_) => KeyType::String,
            Self::Hash(_) => KeyType::Hash,
            Self::List(_) => KeyType::List,
            Self::Set(_) => KeyType::Set,
            Self::ZSet(_) => KeyType::ZSet,
            Self::Stream(_) => KeyType::Stream,
            Self::Json(_) => KeyType::Json,
        }
    }

    /// Number of elements (1 for strings and JSON).
    pub fn len(&self) -> usize {
        match self {
            Self::String(_) | Self::Json(_) => 1,
            Self::Hash(v) => v.len(),
            Self::List(v) | Self::Set(v) => v.len(),
            Self::ZSet(v) => v.len(),
            Self::Stream(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamEntry {
    pub id: String,
    pub fields: Vec<(String, String)>,
}

/// An open session, listed in the Services panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: SessionId,
    pub data_source: DataSourceId,
    pub opened_at: SystemTime,
    /// A manual-commit transaction is open.
    pub in_transaction: bool,
    /// Current schema / database, when known.
    pub schema: Option<String>,
    /// A statement is running in this session.
    pub busy: bool,
}

/// One node of an execution plan (sources with `explain != None`).
#[derive(Clone, Debug, PartialEq)]
pub struct PlanNode {
    /// Operation, e.g. `Hash Join`, `Seq Scan`, `COLLSCAN`.
    pub operation: String,
    /// Object or condition, e.g. `orders o`, `o.customer_id = c.id`.
    pub detail: Option<String>,
    /// Estimated cost in database-specific units.
    pub cost: f64,
    /// Estimated rows / documents.
    pub rows: u64,
    pub children: Vec<PlanNode>,
}

pub trait QueryService: Send + Sync {
    fn open_session(&self, ds: &DataSourceId) -> BoxFuture<'_, Result<SessionId>>;
    fn close_session(&self, session: SessionId) -> BoxFuture<'_, Result<()>>;
    /// Open sessions (Services panel).
    fn sessions(&self) -> Vec<SessionInfo>;
    fn execute(&self, session: SessionId, script: Script, opts: ExecOptions) -> QueryHandle;
    /// Requests up to `items` more items for a paused result; they arrive as
    /// further `Data` events on the original handle.
    fn fetch_more(&self, result: ResultId, items: usize) -> BoxFuture<'_, Result<()>>;
    /// Cost of running `script` without running it (sources with
    /// `cost_estimate`), in bytes processed. `None` when unknown.
    fn estimate_cost(
        &self,
        session: SessionId,
        script: Script,
    ) -> BoxFuture<'_, Result<Option<u64>>>;
    /// Execution plan of the first statement of `script`.
    fn explain(&self, session: SessionId, script: Script) -> BoxFuture<'_, Result<PlanNode>>;
    /// Value of one key (key-value sources).
    fn key_value(&self, session: SessionId, key: String) -> BoxFuture<'_, Result<KeyValue>>;
    /// Statements / commands that [`Self::apply_changes`] would run, for the
    /// operation preview. Nothing is executed.
    fn preview_changes(
        &self,
        result: ResultId,
        changes: ChangeSet,
    ) -> BoxFuture<'_, Result<Vec<String>>>;
    fn apply_changes(
        &self,
        result: ResultId,
        changes: ChangeSet,
    ) -> BoxFuture<'_, Result<ApplyReport>>;
    fn commit(&self, session: SessionId) -> BoxFuture<'_, Result<()>>;
    fn rollback(&self, session: SessionId) -> BoxFuture<'_, Result<()>>;
}
