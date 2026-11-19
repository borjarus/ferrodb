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
        /// The container in the explorer tree (database → collection),
        /// when known: schema inference, indexes and editing use it.
        node: Option<crate::NodeRef>,
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
    /// Document: remove the field at path (`$unset`).
    UnsetField {
        document: usize,
        path: String,
    },
    /// Document: insert a new document (Extended JSON object).
    InsertDocument {
        json: String,
    },
    /// Document: replace a whole document (Extended JSON object; its `_id`
    /// is kept).
    ReplaceDocument {
        document: usize,
        json: String,
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
    /// The changes run inside the session's open (manual) transaction: they
    /// become permanent with [`QueryService::commit`].
    pub pending_commit: bool,
}

/// How rows of a tabular result are identified when edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowIdentity {
    /// Primary or unique key columns (names in the table).
    Key(Vec<String>),
    /// No usable key: rows are matched by all their column values and every
    /// statement must change exactly one row.
    AllColumns,
}

/// What can be edited in a result (`QueryService::edit_info`).
#[derive(Clone, Debug, PartialEq)]
pub struct EditInfo {
    /// The table (or collection) the result comes from.
    pub object: crate::NodeRef,
    /// Per result column: the table column it shows (`None` = computed,
    /// not editable).
    pub columns: Vec<Option<String>>,
    pub identity: RowIdentity,
    /// Shown above the operation preview (e.g. no key).
    pub warning: Option<String>,
}

/// A foreign key leaving or entering the result's table, for "related rows".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Navigation {
    /// e.g. `orders.customer_id → customers.id` or `order_items → orders`.
    pub label: String,
    /// The related table.
    pub object: crate::NodeRef,
    /// Result columns whose values select the related rows.
    pub columns: Vec<usize>,
    /// Columns of the related table compared with those values.
    pub related_columns: Vec<String>,
    /// `true`: rows of the related table that this row references;
    /// `false`: rows of the related table referencing this row.
    pub outgoing: bool,
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
///
/// One model for every database (plan, phase 7): PostgreSQL, MySQL /
/// MariaDB, SQLite and MongoDB plans are parsed into it, the views (tree,
/// table, diagram) only read it. Fields a database does not report stay
/// at their defaults (`cost` 0, `None`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanNode {
    /// Operation, e.g. `Hash Join`, `Seq Scan`, `COLLSCAN`.
    pub operation: String,
    /// Object or condition, e.g. `orders o`, `o.customer_id = c.id`.
    pub detail: Option<String>,
    /// Estimated total cost (this node and its inputs) in database-specific
    /// units; 0 when the database gives none (SQLite, MongoDB).
    pub cost: f64,
    /// Estimated rows / documents.
    pub rows: u64,
    /// Table, index or collection the node reads.
    pub object: Option<String>,
    /// Rows actually produced (per loop), with `analyze`.
    pub actual_rows: Option<u64>,
    /// Time spent in the node and its inputs, in milliseconds, with
    /// `analyze` (all loops).
    pub actual_ms: Option<f64>,
    /// Times the node was executed (nested loops), with `analyze`.
    pub loops: Option<u64>,
    /// Everything else the database reports (`Filter`, `Index Cond`,
    /// `access_type`, `keysExamined`…), in its order.
    pub properties: Vec<(String, String)>,
    /// Problems worth pointing at: full scans, sorts on disk, estimates far
    /// off the actual rows.
    pub warnings: Vec<String>,
    pub children: Vec<PlanNode>,
}

impl PlanNode {
    pub fn new(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            ..Default::default()
        }
    }

    /// Cost of this node without its inputs (never negative).
    pub fn self_cost(&self) -> f64 {
        let inputs: f64 = self.children.iter().map(|c| c.cost).sum();
        (self.cost - inputs).max(0.0)
    }

    /// Time of this node without its inputs, with `analyze`.
    pub fn self_ms(&self) -> Option<f64> {
        let own = self.actual_ms?;
        let inputs: f64 = self.children.iter().filter_map(|c| c.actual_ms).sum();
        Some((own - inputs).max(0.0))
    }

    /// Nodes depth-first with their depth: rows of the table view.
    pub fn flatten(&self) -> Vec<(usize, &PlanNode)> {
        fn walk<'a>(n: &'a PlanNode, depth: usize, out: &mut Vec<(usize, &'a PlanNode)>) {
            out.push((depth, n));
            for c in &n.children {
                walk(c, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        walk(self, 0, &mut out);
        out
    }

    /// Value of a property by name.
    pub fn property(&self, name: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Number of nodes in the plan.
    pub fn len(&self) -> usize {
        1 + self.children.iter().map(PlanNode::len).sum::<usize>()
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

/// What [`QueryService::explain`] should do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExplainOptions {
    /// Run the statement and report actual rows and times (`EXPLAIN
    /// ANALYZE`, MongoDB `executionStats`). Writes are refused: they would
    /// be executed.
    pub analyze: bool,
    /// Default schema / database for the statement (console schema picker).
    pub schema: Option<String>,
}

/// An execution plan with what produced it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecutionPlan {
    pub root: PlanNode,
    /// The statement that was sent (`EXPLAIN (FORMAT JSON) …`).
    pub statement: String,
    /// The plan as the database returned it (JSON or text), for copying.
    pub raw: String,
    /// The plan comes from an actual execution.
    pub analyzed: bool,
    /// Planning time (PostgreSQL) in milliseconds.
    pub planning_ms: Option<f64>,
    /// Total execution time in milliseconds, with `analyze`.
    pub execution_ms: Option<f64>,
}

impl ExecutionPlan {
    /// Every warning of the plan with the node's operation, in tree order.
    pub fn warnings(&self) -> Vec<(String, String)> {
        self.root
            .flatten()
            .into_iter()
            .flat_map(|(_, n)| {
                n.warnings
                    .iter()
                    .map(move |w| (n.operation.clone(), w.clone()))
            })
            .collect()
    }
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
    /// Execution plan of the first statement of `script`, run in the
    /// session (its schema, its open transaction).
    fn explain(
        &self,
        session: SessionId,
        script: Script,
        opts: ExplainOptions,
    ) -> BoxFuture<'_, Result<ExecutionPlan>>;
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

    /// The table / collection behind a result and how its rows are
    /// identified; `Err` explains why the result cannot be edited (a join,
    /// an aggregate, an unknown table…).
    fn edit_info(&self, result: ResultId) -> BoxFuture<'_, Result<EditInfo>> {
        let _ = result;
        Box::pin(async { Err(crate::Error::Unsupported("editing this result".into())) })
    }

    /// Foreign keys of the result's table usable from its rows.
    fn navigations(&self, result: ResultId) -> BoxFuture<'_, Result<Vec<Navigation>>> {
        let _ = result;
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Query listing the rows related to one row: `values` are the cells of
    /// `nav.columns` encoded as for [`Change::SetCell`].
    fn related_script(
        &self,
        result: ResultId,
        nav: Navigation,
        values: Vec<String>,
    ) -> BoxFuture<'_, Result<Script>> {
        let _ = (result, nav, values);
        Box::pin(async { Err(crate::Error::Unsupported("related rows".into())) })
    }

    /// A filter selecting rows whose `column` equals `value` (encoded as for
    /// [`Change::SetCell`]) in the result's language: `"city" = 'Kraków'`,
    /// `"note" IS NULL`, `{ "city": "Kraków" }`.
    fn value_filter(&self, result: ResultId, column: String, value: String) -> Result<String> {
        let _ = result;
        let v: Option<&str> = (value != "null").then_some(value.as_str());
        Ok(match v {
            None => format!("{column} IS NULL"),
            Some(v) if v.starts_with('"') => {
                let inner = v.trim_matches('"').replace('\'', "''");
                format!("{column} = '{inner}'")
            }
            Some(v) => format!("{column} = {v}"),
        })
    }

    /// The UI no longer shows the result: the backend may drop what it kept
    /// for editing it.
    fn release_result(&self, result: ResultId) {
        let _ = result;
    }
}
