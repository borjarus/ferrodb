//! The MCP tools. Each call: arguments → data source visible to agents →
//! permission layer (policy, consent) → backend → compact JSON; every call
//! is audited. Failures are tool errors (`isError`), never protocol errors.

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use ferrodb_api::{
    AgentAccess, AgentService as _, AuditEntry, AuditOutcome, ConnectionState, DataModel,
    DataSourceId, DataSourceInfo, DdlOptions, ExecMode, ExecOptions, ExplainOptions,
    ExplainSupport, NodeRef, NoticeLevel, ObjectKind, QueryEvent, QueryHandle, ResultChunk,
    ResultId, ResultShape, RiskKind, SampleOptions, Script,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, Tool, ToolAnnotations,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{convert, handler::McpHandler};
use crate::agents::{Agents, Call, Verdict, policy::CallClass};

pub const LIST_DATA_SOURCES: &str = "list_data_sources";
pub const LIST_OBJECTS: &str = "list_objects";
pub const DESCRIBE_OBJECT: &str = "describe_object";
pub const RUN_QUERY: &str = "run_query";
pub const EXPLAIN_QUERY: &str = "explain_query";

/// Upper bound of one `run_query` call (the user can also cancel it).
const QUERY_TIMEOUT: Duration = Duration::from_secs(300);
/// Objects returned by one `list_objects` call.
const MAX_OBJECTS: usize = 1_000;
/// Documents sampled by `describe_object` for a collection.
const SCHEMA_SAMPLE: usize = 200;

fn schema(value: Value) -> Arc<JsonObject> {
    match value {
        Value::Object(map) => Arc::new(map),
        _ => unreachable!("tool schemas are objects"),
    }
}

fn data_source_param() -> Value {
    json!({
        "type": "string",
        "description": "Data source id, as returned by list_data_sources."
    })
}

fn path_param(what: &str) -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": format!(
            "{what} Names from the top of the data source's explorer tree, \
             e.g. [\"shop\", \"public\", \"orders\"] (database, schema, table)."
        )
    })
}

/// The tools, their input schemas and annotations.
pub fn definitions() -> Vec<Tool> {
    let read_only = |title: &str| {
        ToolAnnotations::with_title(title)
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false)
    };
    vec![
        Tool::new(
            LIST_DATA_SOURCES,
            "List the data sources (database connections) of FerroDb that agents may use: \
             id, name, database product, data model (tabular, document, key-value), query \
             language, the access agents have (metadata-only, read-only, read-write), whether \
             the connection itself is read-only and whether it is connected. Use the `id` \
             with the other tools.",
            schema(json!({ "type": "object", "properties": {}, "additionalProperties": false })),
        )
        .annotate(read_only("List data sources")),
        Tool::new(
            LIST_OBJECTS,
            "List the objects under a path of a data source's explorer tree: databases, \
             schemas, tables, views, routines, collections, key prefixes, keys… Without `path` \
             the top level is listed (and the shape of the tree). Each object has a name, a \
             kind, a short detail (type, row estimate) and whether it has children; append \
             its name to `path` to go deeper.",
            schema(json!({
                "type": "object",
                "properties": {
                    "data_source": data_source_param(),
                    "path": path_param("Where to list; omit for the top level."),
                },
                "required": ["data_source"],
                "additionalProperties": false
            })),
        )
        .annotate(read_only("List objects")),
        Tool::new(
            DESCRIBE_OBJECT,
            "Describe one object: the DDL (CREATE statement with columns, keys and indexes) \
             of a table, view or routine of a tabular source, or the inferred schema \
             (field paths, types and how often they occur) of a collection of a document \
             source.",
            schema(json!({
                "type": "object",
                "properties": {
                    "data_source": data_source_param(),
                    "path": path_param("The object."),
                },
                "required": ["data_source", "path"],
                "additionalProperties": false
            })),
        )
        .annotate(read_only("Describe an object")),
        Tool::new(
            RUN_QUERY,
            "Run a query in the data source's own language (its SQL dialect, MongoDB shell \
             syntax, CQL, SQL++, PartiQL or Redis commands; see list_data_sources) and return \
             the first rows / documents / keys as JSON with `truncated` when there are more, \
             plus affected rows, notices and errors. Several statements run in order. \
             Queries that change data or schema are refused unless the user granted \
             read-write access, and then always need the user's approval. The user may have \
             to approve any call in the FerroDb window.",
            schema(json!({
                "type": "object",
                "properties": {
                    "data_source": data_source_param(),
                    "query": { "type": "string", "description": "The query text." },
                    "schema": {
                        "type": "string",
                        "description": "Default schema / database for unqualified names; \
                                        omit to keep the connection's default."
                    },
                    "max_rows": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Rows / documents / keys to return per result \
                                        (capped by the user's policy)."
                    }
                },
                "required": ["data_source", "query"],
                "additionalProperties": false
            })),
        )
        .annotate(
            ToolAnnotations::with_title("Run a query")
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(false),
        ),
        Tool::new(
            EXPLAIN_QUERY,
            "Show the estimated execution plan of a query as an indented tree (operations, \
             objects, estimated cost and rows, warnings such as full scans). The query is \
             not run.",
            schema(json!({
                "type": "object",
                "properties": {
                    "data_source": data_source_param(),
                    "query": { "type": "string", "description": "The query text." },
                    "schema": {
                        "type": "string",
                        "description": "Default schema / database for unqualified names."
                    }
                },
                "required": ["data_source", "query"],
                "additionalProperties": false
            })),
        )
        .annotate(read_only("Explain a query")),
    ]
}

fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// What a tool produced once allowed.
struct Done {
    content: String,
    items: Option<u64>,
    /// The call ran but reported errors (query errors): `isError` with the
    /// content above.
    error: Option<String>,
}

impl Done {
    fn json(value: Value, items: usize) -> Self {
        Self {
            content: value.to_string(),
            items: Some(items as u64),
            error: None,
        }
    }
}

type Outcome = Result<Done, String>;

/// Arguments of a call.
struct Args<'a>(&'a JsonObject);

impl Args<'_> {
    fn string(&self, name: &str) -> Result<Option<String>, String> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("invalid arguments: `{name}` must be a string")),
        }
    }

    fn required(&self, name: &str) -> Result<String, String> {
        self.string(name)?
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| format!("invalid arguments: `{name}` is required"))
    }

    fn path(&self) -> Result<Vec<String>, String> {
        match self.0.get("path") {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| match v {
                    Value::String(s) => Ok(s.clone()),
                    _ => Err("invalid arguments: `path` must be a list of names".to_owned()),
                })
                .collect(),
            // Tolerate "a/b/c" from clients that ignore the schema.
            Some(Value::String(s)) => Ok(s
                .split(['/', '.'])
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()),
            Some(_) => Err("invalid arguments: `path` must be a list of names".into()),
        }
    }

    fn max_rows(&self) -> Result<Option<usize>, String> {
        match self.0.get("max_rows") {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|n| *n > 0)
                .map(|n| Some(n.min(usize::MAX as u64) as usize))
                .ok_or_else(|| "invalid arguments: `max_rows` must be a positive integer".into()),
        }
    }
}

/// Audit data of one call.
struct Audit<'a> {
    agents: &'a Agents,
    client: String,
    tool: &'static str,
    data_source: Option<DataSourceId>,
    text: Option<String>,
}

impl Audit<'_> {
    fn record(
        &self,
        outcome: AuditOutcome,
        items: Option<u64>,
        error: Option<String>,
        elapsed: Option<Duration>,
    ) {
        self.agents.record(AuditEntry {
            at: SystemTime::now(),
            client: self.client.clone(),
            tool: self.tool.to_owned(),
            data_source: self.data_source.clone(),
            text: self.text.clone(),
            outcome,
            items,
            error,
            elapsed_ms: elapsed.map(|d| d.as_millis() as u64),
        });
    }

    /// A call refused before reaching the permission layer.
    fn refuse(&self, message: String) -> CallToolResult {
        self.record(
            AuditOutcome::DeniedByPolicy,
            None,
            Some(message.clone()),
            None,
        );
        tool_error(message)
    }
}

fn product(agents: &Agents, ds: &DataSourceInfo) -> String {
    agents
        .backend()
        .drivers()
        .get(&ds.config.driver)
        .map(|d| d.name)
        .unwrap_or_else(|| ds.config.driver.to_string())
}

fn access_label(access: AgentAccess) -> &'static str {
    match access {
        AgentAccess::Hidden => "hidden",
        AgentAccess::MetadataOnly => "metadata-only",
        AgentAccess::ReadOnly => "read-only",
        AgentAccess::ReadWrite => "read-write",
    }
}

fn model_label(model: DataModel) -> &'static str {
    match model {
        DataModel::Tabular => "tabular",
        DataModel::Document => "document",
        DataModel::KeyValue => "key-value",
    }
}

fn state_label(state: &ConnectionState) -> &'static str {
    match state {
        ConnectionState::Disconnected => "disconnected",
        ConnectionState::Connecting => "connecting",
        ConnectionState::Connected => "connected",
        ConnectionState::Failed(_) => "failed",
    }
}

async fn ensure_connected(agents: &Agents, ds: &DataSourceInfo) -> Result<(), String> {
    let state = agents
        .backend()
        .data_sources()
        .get(&ds.config.id)
        .map(|d| d.state);
    if state != Some(ConnectionState::Connected) {
        agents
            .backend()
            .data_sources()
            .connect(&ds.config.id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The node at `names` (cached metadata first, else listing level by level).
async fn resolve(agents: &Agents, ds: &DataSourceId, names: &[String]) -> Result<NodeRef, String> {
    let metadata = agents.backend().metadata();
    let matches = |node: &NodeRef| {
        node.path.len() == names.len() && node.path.iter().zip(names).all(|((_, n), m)| n == m)
    };
    if let Some(snapshot) = metadata.snapshot(ds)
        && let Some(o) = snapshot.objects.iter().find(|o| matches(&o.node))
    {
        return Ok(o.node.clone());
    }
    let mut node = NodeRef::root(ds.clone());
    for name in names {
        let children = metadata
            .children(node.clone())
            .await
            .map_err(|e| e.to_string())?;
        let found = children
            .iter()
            .find(|c| &c.name == name)
            .or_else(|| children.iter().find(|c| c.name.eq_ignore_ascii_case(name)));
        match found {
            Some(c) => node = c.node.clone(),
            None => {
                return Err(format!("`{name}` not found under {}", display_path(&node)));
            }
        }
    }
    Ok(node)
}

fn display_path(node: &NodeRef) -> String {
    if node.path.is_empty() {
        "the top level".into()
    } else {
        node.path
            .iter()
            .map(|(_, n)| n.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }
}

fn kind_label(kind: ObjectKind) -> String {
    format!("{kind:?}")
}

impl McpHandler {
    pub(crate) async fn call(
        &self,
        request: CallToolRequestParams,
        cancel: CancellationToken,
    ) -> CallToolResult {
        let Some(agents) = self.agents.upgrade() else {
            return tool_error("FerroDb is shutting down");
        };
        let empty = JsonObject::new();
        let args = Args(request.arguments.as_ref().unwrap_or(&empty));
        let tool = match request.name.as_ref() {
            LIST_DATA_SOURCES => LIST_DATA_SOURCES,
            LIST_OBJECTS => LIST_OBJECTS,
            DESCRIBE_OBJECT => DESCRIBE_OBJECT,
            RUN_QUERY => RUN_QUERY,
            EXPLAIN_QUERY => EXPLAIN_QUERY,
            other => return tool_error(format!("unknown tool `{other}`")),
        };
        let mut audit = Audit {
            agents: &agents,
            client: self.session.client(),
            tool,
            data_source: None,
            text: None,
        };
        if tool == LIST_DATA_SOURCES {
            let call = Call {
                session: self.session.id,
                client: audit.client.clone(),
                tool,
                data_source: None,
                text: None,
                class: CallClass::Metadata,
                risks: Vec::new(),
                summary: "List the data sources".into(),
            };
            return self
                .guarded(&audit, call, &cancel, || async {
                    Ok(list_data_sources(&agents))
                })
                .await;
        }

        // Every other tool is about one data source.
        let id = match args.required("data_source") {
            Ok(id) => DataSourceId::new(id),
            Err(e) => return audit.refuse(e),
        };
        audit.data_source = Some(id.clone());
        let Some(ds) = agents.visible_source(&id) else {
            return audit.refuse(format!(
                "data source `{id}` not found (see list_data_sources)"
            ));
        };
        let label = format!("{} ({})", ds.config.name, product(&agents, &ds));
        match tool {
            LIST_OBJECTS | DESCRIBE_OBJECT => {
                let path = match args.path() {
                    Ok(p) => p,
                    Err(e) => return audit.refuse(e),
                };
                if tool == DESCRIBE_OBJECT && path.is_empty() {
                    return audit.refuse("invalid arguments: `path` is required".into());
                }
                let shown = if path.is_empty() {
                    "the top level".to_owned()
                } else {
                    path.join(".")
                };
                audit.text = (!path.is_empty()).then(|| path.join("."));
                let summary = if tool == LIST_OBJECTS {
                    format!("List the objects in {shown} of {label}")
                } else {
                    format!("Describe {shown} in {label}")
                };
                let call = Call {
                    session: self.session.id,
                    client: audit.client.clone(),
                    tool,
                    data_source: Some(ds.clone()),
                    text: audit.text.clone(),
                    class: CallClass::Metadata,
                    risks: Vec::new(),
                    summary,
                };
                self.guarded(&audit, call, &cancel, || async {
                    ensure_connected(&agents, &ds).await?;
                    let node = resolve(&agents, &ds.config.id, &path).await?;
                    if tool == LIST_OBJECTS {
                        list_objects(&agents, &ds, node).await
                    } else {
                        describe_object(&agents, &ds, node).await
                    }
                })
                .await
            }
            _ => {
                let query = match args.required("query") {
                    Ok(q) => q,
                    Err(e) => return audit.refuse(e),
                };
                let (schema, max_rows) = match (args.string("schema"), args.max_rows()) {
                    (Ok(s), Ok(m)) => (s, m),
                    (Err(e), _) | (_, Err(e)) => return audit.refuse(e),
                };
                audit.text = Some(query.clone());
                let language = &ds.capabilities.language;
                let mut risks: Vec<RiskKind> = Vec::new();
                for r in agents.backend().language().risks(&query, language, true) {
                    if !risks.contains(&r.kind) {
                        risks.push(r.kind);
                    }
                }
                let modifies = !risks.is_empty();
                let (class, summary) = if tool == EXPLAIN_QUERY {
                    // Explaining never runs the statement.
                    (CallClass::Read, format!("Explain a query on {label}"))
                } else if modifies {
                    (
                        CallClass::Modify,
                        format!("Run a query that changes data or schema on {label}"),
                    )
                } else {
                    (CallClass::Read, format!("Run a query on {label}"))
                };
                let call = Call {
                    session: self.session.id,
                    client: audit.client.clone(),
                    tool,
                    data_source: Some(ds.clone()),
                    text: Some(query.clone()),
                    class,
                    risks: if tool == EXPLAIN_QUERY {
                        Vec::new()
                    } else {
                        risks
                    },
                    summary,
                };
                let limit = agents.policy().max_rows;
                let page_size = max_rows.unwrap_or(limit).min(limit).max(1);
                self.guarded(&audit, call, &cancel, || async {
                    ensure_connected(&agents, &ds).await?;
                    if tool == EXPLAIN_QUERY {
                        self.explain(&agents, &ds, query, schema).await
                    } else {
                        let opts = ExecOptions {
                            mode: ExecMode::WholeScript,
                            page_size,
                            timeout: Some(QUERY_TIMEOUT),
                            schema,
                            // Only reached after the user approved it.
                            confirmed: class == CallClass::Modify,
                            ..ExecOptions::default()
                        };
                        self.run_query(&agents, &ds, query, opts, &cancel).await
                    }
                })
                .await
            }
        }
    }

    /// Authorizes `call`, runs `exec` when allowed and audits the call.
    async fn guarded<F, Fut>(
        &self,
        audit: &Audit<'_>,
        call: Call,
        cancel: &CancellationToken,
        exec: F,
    ) -> CallToolResult
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Outcome>,
    {
        let outcome = match audit.agents.authorize(&call, cancel).await {
            Verdict::Refused { outcome, message } => {
                audit.record(outcome, None, Some(message.clone()), None);
                return tool_error(message);
            }
            Verdict::Allowed(outcome) => outcome,
        };
        let started = Instant::now();
        // Queries handle cancellation themselves (they cancel the statement).
        let result = if call.tool == RUN_QUERY {
            exec().await
        } else {
            tokio::select! {
                r = exec() => r,
                _ = cancel.cancelled() => Err("cancelled by the client".into()),
            }
        };
        let elapsed = Some(started.elapsed());
        match result {
            Ok(done) => {
                audit.record(outcome, done.items, done.error.clone(), elapsed);
                if done.error.is_some() {
                    CallToolResult::error(vec![ContentBlock::text(done.content)])
                } else {
                    CallToolResult::success(vec![ContentBlock::text(done.content)])
                }
            }
            Err(message) => {
                audit.record(outcome, None, Some(message.clone()), elapsed);
                tool_error(message)
            }
        }
    }

    async fn run_query(
        &self,
        agents: &Agents,
        ds: &DataSourceInfo,
        query: String,
        opts: ExecOptions,
        cancel: &CancellationToken,
    ) -> Outcome {
        let queries = agents.backend().queries();
        let slot = self.session.query_slot(&ds.config.id);
        let mut session = slot.lock().await;
        let id = match *session {
            Some(id) => id,
            None => {
                let id = queries
                    .open_session(&ds.config.id)
                    .await
                    .map_err(|e| e.to_string())?;
                *session = Some(id);
                id
            }
        };
        let page_size = opts.page_size;
        let handle = queries.execute(id, Script::new(query), opts);
        let collected = collect(handle, page_size, cancel).await;
        for r in &collected.results {
            queries.release_result(r.id);
        }
        Ok(collected.into_done())
    }

    async fn explain(
        &self,
        agents: &Agents,
        ds: &DataSourceInfo,
        query: String,
        schema: Option<String>,
    ) -> Outcome {
        if ds.capabilities.explain == ExplainSupport::None {
            return Err("this data source has no execution plans".into());
        }
        let queries = agents.backend().queries();
        let slot = self.session.query_slot(&ds.config.id);
        let mut session = slot.lock().await;
        let id = match *session {
            Some(id) => id,
            None => {
                let id = queries
                    .open_session(&ds.config.id)
                    .await
                    .map_err(|e| e.to_string())?;
                *session = Some(id);
                id
            }
        };
        let plan = queries
            .explain(
                id,
                Script::new(query),
                ExplainOptions {
                    analyze: false,
                    schema,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(Done {
            content: convert::plan_text(&plan),
            items: Some(plan.root.len() as u64),
            error: None,
        })
    }
}

fn list_data_sources(agents: &Agents) -> Done {
    let policy = agents.policy();
    let sources: Vec<Value> = agents
        .backend()
        .data_sources()
        .list()
        .into_iter()
        .filter_map(|ds| {
            let access = policy.access(&ds.config.id);
            (access != AgentAccess::Hidden).then(|| {
                json!({
                    "id": ds.config.id,
                    "name": ds.config.name,
                    "product": product(agents, &ds),
                    "driver": ds.config.driver,
                    "data_model": model_label(ds.capabilities.data_model),
                    "query_language": ds.capabilities.language.display_name(),
                    "access": access_label(access),
                    "read_only_connection": ds.config.read_only,
                    "state": state_label(&ds.state),
                })
            })
        })
        .collect();
    let n = sources.len();
    Done::json(json!({ "data_sources": sources }), n)
}

async fn list_objects(agents: &Agents, ds: &DataSourceInfo, node: NodeRef) -> Outcome {
    let metadata = agents.backend().metadata();
    let children = metadata
        .children(node.clone())
        .await
        .map_err(|e| e.to_string())?;
    let total = children.len();
    let objects: Vec<Value> = children
        .into_iter()
        .take(MAX_OBJECTS)
        .map(|o| {
            json!({
                "name": o.name,
                "kind": kind_label(o.kind),
                "detail": o.detail,
                "has_children": o.has_children,
            })
        })
        .collect();
    let mut out = json!({
        "path": node.path.iter().map(|(_, n)| n).collect::<Vec<_>>(),
        "objects": objects,
        "truncated": total > MAX_OBJECTS,
    });
    if node.path.is_empty() {
        let spec = metadata.hierarchy(&ds.config.id);
        out["hierarchy"] = spec
            .levels
            .iter()
            .map(|l| {
                json!({
                    "kind": kind_label(l.kind),
                    "children": l.children.iter().map(|k| kind_label(*k)).collect::<Vec<_>>(),
                })
            })
            .collect();
    }
    Ok(Done::json(out, total.min(MAX_OBJECTS)))
}

async fn describe_object(agents: &Agents, ds: &DataSourceInfo, node: NodeRef) -> Outcome {
    let metadata = agents.backend().metadata();
    let kind = node.kind();
    let object = display_path(&node);
    let inferred = ds.capabilities.data_model == DataModel::Document
        && matches!(kind, Some(ObjectKind::Collection | ObjectKind::Table));
    if inferred {
        let schema = metadata
            .infer_schema(
                node,
                SampleOptions {
                    sample_size: SCHEMA_SAMPLE,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        // Field paths and types only: sample values are data.
        let fields: Vec<Value> = schema
            .fields
            .iter()
            .map(|f| {
                json!({
                    "path": f.path,
                    "types": f.types.iter().map(|(t, share)| json!({ "type": t, "share": share })).collect::<Vec<_>>(),
                    "presence": f.presence,
                })
            })
            .collect();
        let n = fields.len();
        return Ok(Done::json(
            json!({
                "object": object,
                "kind": kind.map(kind_label),
                "sampled_documents": schema.sampled,
                "fields": fields,
            }),
            n,
        ));
    }
    let ddl = metadata
        .definition(vec![node], DdlOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    Ok(Done::json(
        json!({ "object": object, "kind": kind.map(kind_label), "definition": ddl }),
        1,
    ))
}

/// One result of a query, as returned to the agent.
struct Collected {
    results: Vec<ResultJson>,
    affected: Option<u64>,
    notices: Vec<String>,
    errors: Vec<String>,
}

struct ResultJson {
    id: ResultId,
    title: Option<String>,
    shape: &'static str,
    columns: Option<Value>,
    items: Vec<Value>,
    truncated: bool,
}

impl Collected {
    fn into_done(self) -> Done {
        let items: usize = self.results.iter().map(|r| r.items.len()).sum();
        let results: Vec<Value> = self
            .results
            .into_iter()
            .map(|r| {
                let mut v = json!({
                    "kind": r.shape,
                    "count": r.items.len(),
                    "truncated": r.truncated,
                });
                if let Some(t) = r.title {
                    v["title"] = t.into();
                }
                if let Some(c) = r.columns {
                    v["columns"] = c;
                }
                let key = match r.shape {
                    "table" => "rows",
                    "documents" => "documents",
                    _ => "keys",
                };
                v[key] = Value::Array(r.items);
                v
            })
            .collect();
        let mut out = json!({ "results": results });
        if let Some(n) = self.affected {
            out["affected_rows"] = n.into();
        }
        if !self.notices.is_empty() {
            out["notices"] = self.notices.into();
        }
        let error = (!self.errors.is_empty()).then(|| self.errors.join("; "));
        if !self.errors.is_empty() {
            out["errors"] = self.errors.into();
        }
        Done {
            content: out.to_string(),
            items: Some(items as u64),
            error,
        }
    }
}

/// Reads the events of a query until it finishes: the first `page_size`
/// items of each result. Cancels the query when the client cancels the
/// call or it runs past [`QUERY_TIMEOUT`].
async fn collect(handle: QueryHandle, page_size: usize, cancel: &CancellationToken) -> Collected {
    let mut out = Collected {
        results: Vec::new(),
        affected: None,
        notices: Vec::new(),
        errors: Vec::new(),
    };
    let deadline = tokio::time::sleep(QUERY_TIMEOUT + Duration::from_secs(5));
    tokio::pin!(deadline);
    let mut stopped: Option<&str> = None;
    loop {
        let event = if stopped.is_some() {
            // Drain until the backend acknowledges the cancellation.
            match tokio::time::timeout(Duration::from_secs(5), handle.events.recv()).await {
                Ok(Ok(e)) => e,
                _ => break,
            }
        } else {
            tokio::select! {
                e = handle.events.recv() => match e {
                    Ok(e) => e,
                    Err(_) => break,
                },
                _ = cancel.cancelled() => {
                    handle.cancel.cancel();
                    stopped = Some("cancelled by the client");
                    continue;
                }
                _ = &mut deadline => {
                    handle.cancel.cancel();
                    stopped = Some("timed out");
                    continue;
                }
            }
        };
        match event {
            QueryEvent::ResultStarted {
                result,
                shape,
                title,
            } => {
                let (shape, columns) = match &shape {
                    ResultShape::Table(schema) => ("table", Some(convert::columns(schema))),
                    ResultShape::Documents { .. } => ("documents", None),
                    ResultShape::Keys { .. } => ("keys", None),
                };
                out.results.push(ResultJson {
                    id: result,
                    title,
                    shape,
                    columns,
                    items: Vec::new(),
                    truncated: false,
                });
            }
            QueryEvent::Data { result, chunk } => {
                let Some(r) = out.results.iter_mut().find(|r| r.id == result) else {
                    continue;
                };
                let room = page_size.saturating_sub(r.items.len());
                let left = match &chunk {
                    ResultChunk::Rows(batch) => convert::batch_rows(batch, room, &mut r.items),
                    ResultChunk::Documents(docs) => {
                        r.items
                            .extend(docs.iter().take(room).map(convert::document));
                        docs.len().saturating_sub(room)
                    }
                    ResultChunk::Keys(keys) => {
                        r.items.extend(keys.iter().take(room).map(convert::key));
                        keys.len().saturating_sub(room)
                    }
                };
                r.truncated |= left > 0;
            }
            QueryEvent::ResultPaused { result, has_more } => {
                if let Some(r) = out.results.iter_mut().find(|r| r.id == result) {
                    r.truncated |= has_more;
                }
            }
            QueryEvent::Notice(n) => out.notices.push(match n.level {
                NoticeLevel::Info => n.message,
                NoticeLevel::Warning => format!("warning: {}", n.message),
            }),
            QueryEvent::StatementFinished {
                affected: Some(n), ..
            } => *out.affected.get_or_insert(0) += n,
            QueryEvent::Error(e) => out.errors.push(e.to_string()),
            QueryEvent::Finished => break,
            QueryEvent::StatementStarted { .. }
            | QueryEvent::CostEstimate { .. }
            | QueryEvent::StatementFinished { affected: None, .. } => {}
        }
    }
    if let Some(reason) = stopped
        && !out.errors.iter().any(|e| e == reason)
    {
        out.errors.push(reason.to_owned());
    }
    out
}
