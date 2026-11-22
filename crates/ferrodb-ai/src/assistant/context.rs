//! Schema-aware context: which objects of the data source go into the prompt.
//!
//! Objects come from the metadata snapshot (the explorer catalog); when none
//! is cached, the tree is listed with `children` (bounded in depth, count
//! and time). Tables, views and collections are ranked by relevance to the
//! question and the console's query (identifier overlap with singular /
//! plural and snake / camel case splitting, foreign-key neighbours of hits,
//! objects named in the query first), the most relevant ones are completed
//! with their columns and keys, and everything is rendered compactly until
//! the schema budget is spent. Only metadata is read: never rows.

use std::{collections::HashSet, time::Duration};

use ferrodb_api::{
    AiSettings, AssistantRequest, AssistantTask, Backend, ChatRole, DataModel, DataSourceId,
    DbObject, MetadataService, NodeRef, ObjectKind, QueryLanguage,
};
use futures::StreamExt;

use super::http::Cancelled;

/// What the prompt knows about the data source.
#[derive(Clone, Debug)]
pub(crate) struct SourceContext {
    pub data_source: Option<DataSourceId>,
    pub source_name: Option<String>,
    /// Database product, e.g. `PostgreSQL`; `None` without a data source.
    pub product: Option<String>,
    pub data_model: Option<DataModel>,
    pub language: QueryLanguage,
    pub joins: bool,
    /// The console's schema / database.
    pub console_schema: Option<String>,
    /// `AiSettings::share_schema`.
    pub shared: bool,
    /// Rendered schema; `None` when not shared or not available.
    pub schema_text: Option<String>,
    /// Objects whose definitions are in `schema_text`, most relevant first.
    pub objects: Vec<String>,
}

/// Tables listed with their columns and keys before ranking stops mattering.
const ENRICH_ALL_UP_TO: usize = 40;
/// Relevant tables completed with `children` in a large schema.
const ENRICH_TOP: usize = 25;
const ENRICH_TIME: Duration = Duration::from_secs(6);
const CALL_TIMEOUT: Duration = Duration::from_secs(4);
/// Tree listing when the snapshot is empty.
const WALK_TIME: Duration = Duration::from_secs(15);
const WALK_DEPTH: usize = 5;
const WALK_PER_LEVEL: usize = 8;
const WALK_CONTAINERS: usize = 24;
const WALK_OBJECTS: usize = 5_000;
/// Columns shown per table.
const MAX_COLUMNS: usize = 80;

pub(crate) async fn build(
    backend: &dyn Backend,
    req: &AssistantRequest,
    settings: &AiSettings,
    cancelled: Cancelled<'_>,
) -> SourceContext {
    let console_schema = req.schema.clone().filter(|s| !s.trim().is_empty());
    let info = req
        .data_source
        .as_ref()
        .and_then(|ds| backend.data_sources().get(ds));
    let Some(info) = info else {
        return SourceContext {
            data_source: None,
            source_name: None,
            product: None,
            data_model: None,
            language: QueryLanguage::sql("generic"),
            joins: true,
            console_schema,
            shared: settings.share_schema,
            schema_text: None,
            objects: Vec::new(),
        };
    };
    let ds = info.config.id.clone();
    let caps = &info.capabilities;
    let product = backend
        .drivers()
        .get(&info.config.driver)
        .map(|d| d.name)
        .unwrap_or_else(|| info.config.driver.to_string());
    let mut ctx = SourceContext {
        data_source: Some(ds.clone()),
        source_name: Some(info.config.name.clone()),
        product: Some(product),
        data_model: Some(caps.data_model),
        language: caps.language.clone(),
        joins: caps.joins,
        console_schema: console_schema.clone(),
        shared: settings.share_schema,
        schema_text: None,
        objects: Vec::new(),
    };
    if !settings.share_schema {
        return ctx;
    }
    let meta = backend.metadata();
    let objects = load_objects(
        meta,
        &ds,
        caps.data_model,
        console_schema.as_deref(),
        cancelled,
    )
    .await;
    if objects.is_empty() || cancelled() {
        return ctx;
    }
    let separator = meta
        .hierarchy(&ds)
        .key_separator
        .clone()
        .unwrap_or_else(|| ":".into());
    let mut model = Model::new(objects, caps.data_model, separator);
    let terms = Terms::new(req);
    let cs = console_schema.as_deref();

    model.rank(&terms, cs, false);
    let first = model.to_enrich();
    enrich(meta, &mut model, first, cancelled).await;
    model.rank(&terms, cs, true);
    let second = model.to_enrich();
    enrich(meta, &mut model, second, cancelled).await;
    model.rank(&terms, cs, true);

    let (text, objects) = model.render(settings.schema_budget);
    ctx.schema_text = (!text.trim().is_empty()).then_some(text);
    ctx.objects = objects;
    ctx
}

// ------------------------------------------------------------------ loading

fn is_container(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Database
            | ObjectKind::Schema
            | ObjectKind::Keyspace
            | ObjectKind::Bucket
            | ObjectKind::Scope
            | ObjectKind::Project
            | ObjectKind::Dataset
            | ObjectKind::Folder
    )
}

fn is_table_like(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::Collection
    )
}

const SYSTEM_CONTAINERS: &[&str] = &[
    "information_schema",
    "pg_catalog",
    "pg_toast",
    "sys",
    "system",
    "mysql",
    "performance_schema",
    "admin",
    "config",
    "local",
];

fn is_system(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SYSTEM_CONTAINERS.contains(&n.as_str()) || n.starts_with("system_") || n.starts_with("pg_")
}

async fn load_objects(
    meta: &dyn MetadataService,
    ds: &DataSourceId,
    model: DataModel,
    console_schema: Option<&str>,
    cancelled: Cancelled<'_>,
) -> Vec<DbObject> {
    if let Some(snapshot) = meta.snapshot(ds)
        && snapshot
            .objects
            .iter()
            .any(|o| is_table_like(o.kind) || o.kind == ObjectKind::KeyPrefix)
    {
        return snapshot.objects.clone();
    }
    walk(meta, ds, model, console_schema, cancelled).await
}

/// Lists containers and their objects with `children`, the console's
/// schema first, system containers last.
async fn walk(
    meta: &dyn MetadataService,
    ds: &DataSourceId,
    model: DataModel,
    console_schema: Option<&str>,
    cancelled: Cancelled<'_>,
) -> Vec<DbObject> {
    let deadline = tokio::time::Instant::now() + WALK_TIME;
    let per_level = if model == DataModel::KeyValue {
        1
    } else {
        WALK_PER_LEVEL
    };
    let mut out = Vec::new();
    let mut level = vec![NodeRef::root(ds.clone())];
    let mut expanded = 0;
    'walk: for _ in 0..WALK_DEPTH {
        let rank = |n: &NodeRef| {
            let name = n.name().unwrap_or_default();
            if console_schema.is_some_and(|cs| name.eq_ignore_ascii_case(cs)) {
                0
            } else if is_system(name) {
                2
            } else {
                1
            }
        };
        level.sort_by_key(|n| rank(n));
        let mut next = Vec::new();
        for node in level.into_iter().take(per_level) {
            if expanded >= WALK_CONTAINERS || out.len() >= WALK_OBJECTS || cancelled() {
                break 'walk;
            }
            expanded += 1;
            match tokio::time::timeout_at(deadline, meta.children(node)).await {
                Ok(Ok(children)) => {
                    for c in children {
                        if is_container(c.kind) {
                            next.push(c.node.clone());
                        }
                        out.push(c);
                    }
                }
                Ok(Err(e)) => tracing::debug!("assistant context: {e}"),
                Err(_) => break 'walk,
            }
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    out
}

/// Completes `indices` with their columns and keys (`children`), in
/// parallel, within [`ENRICH_TIME`].
async fn enrich(
    meta: &dyn MetadataService,
    model: &mut Model,
    indices: Vec<usize>,
    cancelled: Cancelled<'_>,
) {
    if indices.is_empty() {
        return;
    }
    let jobs: Vec<(usize, NodeRef)> = indices
        .into_iter()
        .map(|i| (i, model.entries[i].node.clone()))
        .collect();
    let deadline = tokio::time::Instant::now() + ENRICH_TIME;
    let mut results = futures::stream::iter(jobs)
        .map(|(i, node)| async move {
            (
                i,
                tokio::time::timeout(CALL_TIMEOUT, meta.children(node)).await,
            )
        })
        .buffer_unordered(8);
    loop {
        if cancelled() {
            break;
        }
        match tokio::time::timeout_at(deadline, results.next()).await {
            Ok(Some((i, Ok(Ok(children))))) => model.entries[i].apply(&children),
            Ok(Some((i, _))) => model.entries[i].enriched = true,
            Ok(None) | Err(_) => break,
        }
    }
}

// ------------------------------------------------------------------ terms

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "by", "for", "to", "in", "on", "at", "and", "or", "with", "from",
    "all", "show", "get", "list", "give", "find", "me", "my", "what", "which", "who", "how",
    "many", "much", "is", "are", "was", "be", "it", "that", "this", "these", "those", "each",
    "per", "every", "query", "select", "where", "please", "want", "need", "write", "make",
];

/// Lower-case words of an identifier or a sentence: split on non-alphanumeric
/// characters and on camel-case boundaries (`orderItems`, `HTTPServer`).
pub(crate) fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in s.split(|c: char| !c.is_alphanumeric()) {
        let chars: Vec<char> = part.chars().collect();
        let mut cur = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let prev = i.checked_sub(1).map(|p| chars[p]);
            let next = chars.get(i + 1).copied();
            let boundary = c.is_uppercase()
                && prev.is_some_and(|p| {
                    p.is_lowercase()
                        || p.is_ascii_digit()
                        || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
                });
            if boundary && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.extend(c.to_lowercase());
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

/// Crude singular of an English word (`categories` → `category`).
pub(crate) fn stem(w: &str) -> String {
    let n = w.len();
    if n > 4 && w.ends_with("ies") {
        format!("{}y", &w[..n - 3])
    } else if n > 4
        && (w.ends_with("sses") || w.ends_with("xes") || w.ends_with("ches") || w.ends_with("shes"))
    {
        w[..n - 2].to_owned()
    } else if n > 3
        && w.ends_with('s')
        && !w.ends_with("ss")
        && !w.ends_with("us")
        && !w.ends_with("is")
    {
        w[..n - 1].to_owned()
    } else {
        w.to_owned()
    }
}

fn stems(s: &str) -> Vec<String> {
    words(s).iter().map(|w| stem(w)).collect()
}

/// Words of the question and identifiers of the console's query.
pub(crate) struct Terms {
    stems: HashSet<String>,
    /// Lower-case identifiers of the context query (`orders`, `public`).
    named: HashSet<String>,
}

impl Terms {
    pub(crate) fn new(req: &AssistantRequest) -> Self {
        let mut text = req.prompt.clone();
        for t in req.history.iter().filter(|t| t.role == ChatRole::User) {
            text.push(' ');
            text.push_str(&t.text);
        }
        if let AssistantTask::Fix { error } = &req.task {
            text.push(' ');
            text.push_str(error);
        }
        let query = req.context.as_deref().unwrap_or_default();
        text.push(' ');
        text.push_str(query);
        let stems = words(&text)
            .into_iter()
            .filter(|w| w.chars().count() > 1 && !STOPWORDS.contains(&w.as_str()))
            .map(|w| stem(&w))
            .collect();
        let named = query
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        Self { stems, named }
    }
}

// ------------------------------------------------------------------ model

#[derive(Clone, Debug, PartialEq)]
struct Fk {
    name: String,
    /// Referencing columns; empty when the catalog does not say.
    columns: Vec<String>,
    target: String,
    target_columns: Vec<String>,
}

#[derive(Clone, Debug)]
struct Entry {
    node: NodeRef,
    kind: ObjectKind,
    name: String,
    /// Names of the containers above, outermost first.
    containers: Vec<String>,
    /// Comment (or another short note) from the catalog.
    detail: Option<String>,
    columns: Vec<(String, Option<String>)>,
    pk: Vec<String>,
    fks: Vec<Fk>,
    /// `children` was asked (keys known, or nothing more to know).
    enriched: bool,
    score: f64,
}

fn list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|c| c.trim().trim_matches(['"', '`', '[', ']']).to_owned())
        .filter(|c| !c.is_empty())
        .collect()
}

/// `→ customers(id)` (detail) and `FK (customer_id) → customers` (name of an
/// unnamed key).
fn parse_fk(name: &str, detail: Option<&str>) -> Option<Fk> {
    let target_part = detail
        .and_then(|d| d.rsplit_once('→').map(|(_, t)| t))
        .or_else(|| name.rsplit_once('→').map(|(_, t)| t))?
        .trim();
    let (target, target_columns) = match target_part.split_once('(') {
        Some((t, cols)) => (t.trim(), list(cols.trim_end_matches(')'))),
        None => (target_part, Vec::new()),
    };
    if target.is_empty() {
        return None;
    }
    let columns = name
        .strip_prefix("FK (")
        .and_then(|rest| rest.split_once(')'))
        .map(|(cols, _)| list(cols))
        .unwrap_or_default();
    Some(Fk {
        name: name.to_owned(),
        columns,
        target: target.to_owned(),
        target_columns,
    })
}

impl Entry {
    fn apply(&mut self, children: &[DbObject]) {
        let columns: Vec<_> = children
            .iter()
            .filter(|c| matches!(c.kind, ObjectKind::Column | ObjectKind::Field))
            .map(|c| (c.name.clone(), c.detail.clone()))
            .collect();
        if !columns.is_empty() {
            self.columns = columns;
        }
        for c in children {
            match c.kind {
                ObjectKind::PrimaryKey => {
                    self.pk = c.detail.as_deref().map(list).unwrap_or_default();
                }
                ObjectKind::ForeignKey => {
                    if let Some(fk) = parse_fk(&c.name, c.detail.as_deref())
                        && !self.fks.contains(&fk)
                    {
                        self.fks.push(fk);
                    }
                }
                _ => {}
            }
        }
        self.enriched = true;
    }

    /// Whether `target` (`customers`, `public.customers`) names this entry.
    fn is_target(&self, target: &str) -> bool {
        let parts: Vec<&str> = target
            .split('.')
            .map(|p| p.trim().trim_matches(['"', '`', '[', ']']))
            .collect();
        let Some((last, qualifier)) = parts.split_last() else {
            return false;
        };
        last.eq_ignore_ascii_case(&self.name)
            && match (qualifier.last(), self.containers.last()) {
                (Some(q), Some(c)) => q.eq_ignore_ascii_case(c),
                _ => true,
            }
    }
}

struct Model {
    data_model: DataModel,
    separator: String,
    entries: Vec<Entry>,
    /// Functions and procedures: (qualified name, signature / return type).
    routines: Vec<(String, Option<String>)>,
    /// Key prefixes: (database, prefix path, detail).
    prefixes: Vec<(String, String, Option<String>)>,
}

impl Model {
    fn new(objects: Vec<DbObject>, data_model: DataModel, separator: String) -> Self {
        let containers = |n: &NodeRef| -> Vec<String> {
            n.path[..n.path.len().saturating_sub(1)]
                .iter()
                .filter(|(k, _)| is_container(*k))
                .map(|(_, name)| name.clone())
                .collect()
        };
        let mut entries: Vec<Entry> = Vec::new();
        let mut routines = Vec::new();
        let mut prefixes = Vec::new();
        let mut seen = HashSet::new();
        for o in &objects {
            if is_table_like(o.kind) {
                if seen.insert(o.node.clone()) {
                    entries.push(Entry {
                        node: o.node.clone(),
                        kind: o.kind,
                        name: o.name.clone(),
                        containers: containers(&o.node),
                        detail: o.detail.clone(),
                        columns: Vec::new(),
                        pk: Vec::new(),
                        fks: Vec::new(),
                        enriched: false,
                        score: 0.0,
                    });
                }
            } else if matches!(o.kind, ObjectKind::Function | ObjectKind::Procedure) {
                let mut q = containers(&o.node).pop().unwrap_or_default();
                if !q.is_empty() {
                    q.push('.');
                }
                q.push_str(&o.name);
                if !routines.iter().any(|(r, _)| *r == q) {
                    routines.push((q, o.detail.clone()));
                }
            } else if o.kind == ObjectKind::KeyPrefix {
                let db = o
                    .node
                    .path
                    .iter()
                    .find(|(k, _)| is_container(*k))
                    .map(|(_, n)| n.clone())
                    .unwrap_or_default();
                let path: Vec<&str> = o
                    .node
                    .path
                    .iter()
                    .filter(|(k, _)| *k == ObjectKind::KeyPrefix)
                    .map(|(_, n)| n.as_str())
                    .collect();
                prefixes.push((db, path.join(&separator), o.detail.clone()));
            }
        }
        // Columns and keys listed in the snapshot itself.
        let index: std::collections::HashMap<NodeRef, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.node.clone(), i))
            .collect();
        let mut children: Vec<Vec<DbObject>> = vec![Vec::new(); entries.len()];
        for o in objects {
            if is_table_like(o.kind) || o.node.path.len() < 2 {
                continue;
            }
            let parent = NodeRef {
                data_source: o.node.data_source.clone(),
                path: o.node.path[..o.node.path.len() - 1].to_vec(),
            };
            if let Some(&i) = index.get(&parent) {
                children[i].push(o);
            }
        }
        for (e, c) in entries.iter_mut().zip(children) {
            if !c.is_empty() {
                e.apply(&c);
                // Keys are not in the snapshot: still worth asking.
                e.enriched = false;
            }
        }
        Self {
            data_model,
            separator,
            entries,
            routines,
            prefixes,
        }
    }

    fn base_score(e: &Entry, terms: &Terms, console_schema: Option<&str>) -> f64 {
        const GENERIC: &[&str] = &["id", "name", "created_at", "updated_at"];
        let mut s = 0.0;
        if terms.named.contains(&e.name.to_lowercase()) {
            s += 100.0;
        }
        let ws = stems(&e.name);
        if !ws.is_empty() {
            let hits = ws.iter().filter(|w| terms.stems.contains(*w)).count();
            if hits == ws.len() {
                s += 30.0 + 5.0 * (ws.len() - 1) as f64;
            } else {
                s += 8.0 * hits as f64 / ws.len() as f64;
            }
        }
        let column_hits = e
            .columns
            .iter()
            .filter(|(c, _)| !GENERIC.contains(&c.to_lowercase().as_str()))
            .filter(|(c, _)| {
                let cw = stems(c);
                !cw.is_empty() && cw.iter().all(|w| terms.stems.contains(w))
            })
            .count();
        s += (3 * column_hits).min(12) as f64;
        // The console's schema first: same-named tables elsewhere rank lower.
        match (console_schema, e.containers.last()) {
            (Some(cs), Some(c)) if c.eq_ignore_ascii_case(cs) => s + 1.0,
            (Some(_), Some(_)) => s * 0.75,
            _ => s,
        }
    }

    /// Scores and sorts the entries, most relevant first; with `neighbours`
    /// foreign-key neighbours of relevant tables are pulled up too.
    fn rank(&mut self, terms: &Terms, console_schema: Option<&str>, neighbours: bool) {
        let base: Vec<f64> = self
            .entries
            .iter()
            .map(|e| Self::base_score(e, terms, console_schema))
            .collect();
        let mut bonus = vec![0.0f64; base.len()];
        if neighbours {
            for (i, e) in self.entries.iter().enumerate() {
                if base[i] < 8.0 {
                    continue;
                }
                let mut lift = |j: usize, v: f64| {
                    if j != i {
                        bonus[j] = bonus[j].max(v);
                    }
                };
                for fk in &e.fks {
                    for (j, o) in self.entries.iter().enumerate() {
                        if o.is_target(&fk.target) {
                            lift(j, (base[i] * 0.3).max(10.0));
                        }
                    }
                }
                for (j, o) in self.entries.iter().enumerate() {
                    if o.fks.iter().any(|f| e.is_target(&f.target)) {
                        lift(j, (base[i] * 0.2).max(6.0));
                    }
                }
                // `customer_id` → `customers`.
                for (c, _) in &e.columns {
                    let w = words(c);
                    if w.len() < 2 || w.last().map(String::as_str) != Some("id") {
                        continue;
                    }
                    let prefix: Vec<String> = w[..w.len() - 1].iter().map(|x| stem(x)).collect();
                    for (j, o) in self.entries.iter().enumerate() {
                        if stems(&o.name) == prefix {
                            lift(j, 10.0);
                        }
                    }
                }
            }
        }
        for (e, (b, n)) in self.entries.iter_mut().zip(base.into_iter().zip(bonus)) {
            e.score = b + n;
        }
        // Stable: equal scores keep the catalog order.
        self.entries.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Entries worth completing with `children` (ranked order).
    fn to_enrich(&self) -> Vec<usize> {
        let n = if self.entries.len() <= ENRICH_ALL_UP_TO {
            self.entries.len()
        } else {
            self.entries
                .iter()
                .filter(|e| e.score > 0.0)
                .count()
                .clamp(10, ENRICH_TOP)
        };
        (0..n.min(self.entries.len()))
            .filter(|&i| !self.entries[i].enriched)
            .collect()
    }

    fn qualified(&self, e: &Entry) -> String {
        let mut parts: Vec<&str> = match self.data_model {
            DataModel::Tabular => e
                .containers
                .last()
                .map(String::as_str)
                .into_iter()
                .collect(),
            DataModel::Document => e.containers.iter().map(String::as_str).collect(),
            DataModel::KeyValue => Vec::new(),
        };
        parts.push(&e.name);
        parts.join(".")
    }

    fn render_entry(&self, e: &Entry, qualified: &str) -> String {
        let kind = match e.kind {
            ObjectKind::View => "view",
            ObjectKind::MaterializedView => "materialized view",
            ObjectKind::Collection => "collection",
            _ => "table",
        };
        let norm = |s: &str| s.replace('_', "").to_lowercase();
        // Foreign keys whose referencing column is known or can be guessed.
        let mut attached: Vec<(usize, String)> = Vec::new();
        let mut loose: Vec<String> = Vec::new();
        for fk in &e.fks {
            let target = fk.target.rsplit('.').next().unwrap_or(&fk.target);
            let target_col = |k: usize| fk.target_columns.get(k).cloned().unwrap_or_default();
            let mut columns: Vec<usize> = fk
                .columns
                .iter()
                .filter_map(|c| {
                    e.columns
                        .iter()
                        .position(|(n, _)| n.eq_ignore_ascii_case(c))
                })
                .collect();
            if fk.columns.is_empty() && fk.target_columns.len() == 1 {
                let tc = &fk.target_columns[0];
                let singular = stems(target).join("");
                let guesses = [
                    norm(&format!("{singular}{tc}")),
                    norm(&format!("{target}{tc}")),
                ];
                if let Some(p) = e
                    .columns
                    .iter()
                    .position(|(n, _)| guesses.contains(&norm(n)))
                {
                    columns.push(p);
                }
            }
            if !columns.is_empty() && columns.len() == fk.columns.len().max(1) {
                for (k, c) in columns.into_iter().enumerate() {
                    let tc = target_col(k);
                    let to = if tc.is_empty() {
                        target.to_owned()
                    } else {
                        format!("{target}.{tc}")
                    };
                    attached.push((c, to));
                }
            } else {
                let from = if fk.columns.is_empty() {
                    fk.name.clone()
                } else {
                    format!("fk({})", fk.columns.join(", "))
                };
                loose.push(format!(
                    "{from}→{}({})",
                    fk.target,
                    fk.target_columns.join(", ")
                ));
            }
        }
        let mut items: Vec<String> = e
            .columns
            .iter()
            .take(MAX_COLUMNS)
            .enumerate()
            .map(|(i, (name, ty))| {
                let mut s = name.clone();
                if let Some(ty) = ty.as_deref().filter(|t| !t.is_empty()) {
                    s.push(' ');
                    s.push_str(ty);
                }
                if e.pk.iter().any(|p| p.eq_ignore_ascii_case(name)) {
                    s.push_str(" pk");
                }
                for (_, to) in attached.iter().filter(|(c, _)| *c == i) {
                    s.push_str(" fk→");
                    s.push_str(to);
                }
                s
            })
            .collect();
        if e.columns.len() > MAX_COLUMNS {
            items.push(format!("…+{} more", e.columns.len() - MAX_COLUMNS));
        }
        if e.columns.is_empty() && !e.pk.is_empty() {
            items.push(format!("pk({})", e.pk.join(", ")));
        }
        items.extend(loose);
        let mut line = format!("{kind} {qualified}");
        if !items.is_empty() {
            line.push('(');
            line.push_str(&items.join(", "));
            line.push(')');
        }
        if let Some(d) = e.detail.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            line.push_str("  -- ");
            line.extend(d.chars().take(120).filter(|c| *c != '\n'));
        }
        line
    }

    /// Text within `budget` characters and the objects defined in it.
    fn render(&self, budget: usize) -> (String, Vec<String>) {
        let mut out = Out {
            text: String::new(),
            used: 0,
            budget,
        };
        let mut objects = Vec::new();
        let mut rest = Vec::new();
        let mut seen = HashSet::new();
        for e in &self.entries {
            let q = self.qualified(e);
            if !seen.insert(q.to_lowercase()) {
                continue;
            }
            // Once a definition does not fit, the rest are listed by name
            // (in rank order) rather than skipping to smaller, less relevant ones.
            if rest.is_empty() && out.push(&self.render_entry(e, &q)) {
                objects.push(q);
            } else {
                rest.push(q);
            }
        }
        if !rest.is_empty() {
            let mut line = String::from("Not shown in detail: ");
            let head = line.len();
            for (i, q) in rest.iter().enumerate() {
                let more = format!(", …+{} more", rest.len() - i);
                if !out.fits(&line, q.chars().count() + more.chars().count() + 2) {
                    if line.len() > head {
                        line.push_str(&more);
                    }
                    break;
                }
                if line.len() > head {
                    line.push_str(", ");
                }
                line.push_str(q);
            }
            if line.len() > head {
                out.push(&line);
            }
        }
        if !self.routines.is_empty() {
            let mut line = String::from("Functions and procedures: ");
            let head = line.len();
            for (n, d) in &self.routines {
                let item = match d {
                    Some(d) if d.starts_with('(') => format!("{n}{d}"),
                    Some(d) => format!("{n} → {d}"),
                    None => n.clone(),
                };
                if !out.fits(&line, item.chars().count() + 2) {
                    break;
                }
                if line.len() > head {
                    line.push_str("; ");
                }
                line.push_str(&item);
            }
            if line.len() > head {
                out.push(&line);
            }
        }
        let mut by_db: Vec<(&str, Vec<(String, String)>)> = Vec::new();
        for (db, prefix, detail) in &self.prefixes {
            let pattern = format!("{prefix}{}*", self.separator);
            let item = match detail {
                Some(d) => format!("{pattern} ({d})"),
                None => pattern.clone(),
            };
            match by_db.iter_mut().find(|(d, _)| d == db) {
                Some((_, v)) => v.push((pattern, item)),
                None => by_db.push((db, vec![(pattern, item)])),
            }
        }
        for (db, items) in by_db {
            let mut line = if db.is_empty() {
                "Key prefixes: ".to_owned()
            } else {
                format!("Key prefixes in {db}: ")
            };
            let head = line.len();
            let mut patterns = Vec::new();
            for (pattern, item) in items {
                if !out.fits(&line, item.chars().count() + 2) {
                    break;
                }
                if line.len() > head {
                    line.push_str(", ");
                }
                line.push_str(&item);
                patterns.push(if db.is_empty() {
                    pattern
                } else {
                    format!("{db} {pattern}")
                });
            }
            if line.len() > head && out.push(&line) {
                objects.extend(patterns);
            }
        }
        (out.text, objects)
    }
}

/// Lines within a character budget.
struct Out {
    text: String,
    used: usize,
    budget: usize,
}

impl Out {
    /// `line` plus `extra` characters (and the line break) still fit.
    fn fits(&self, line: &str, extra: usize) -> bool {
        self.used + line.chars().count() + extra < self.budget
    }

    fn push(&mut self, line: &str) -> bool {
        if !self.fits(line, 0) {
            return false;
        }
        self.used += line.chars().count() + 1;
        self.text.push_str(line);
        self.text.push('\n');
        true
    }
}
