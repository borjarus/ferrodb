//! Execution plans (plan, phase 7): what each database returns from its
//! `EXPLAIN`, parsed into the shared [`PlanNode`] model.
//!
//! | Database | Estimate | With `analyze` |
//! |---|---|---|
//! | PostgreSQL | `EXPLAIN (FORMAT JSON)` | `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` |
//! | MySQL | `EXPLAIN FORMAT=JSON` | `EXPLAIN ANALYZE` (tree text) |
//! | MariaDB | `EXPLAIN FORMAT=JSON` | `ANALYZE FORMAT=JSON` |
//! | SQLite | `EXPLAIN QUERY PLAN` rows | the same, plus the measured run |
//! | MongoDB | `explain` (`queryPlanner`) | `explain` (`executionStats`) |
//! | DuckDB | `EXPLAIN (FORMAT JSON)` | `EXPLAIN (ANALYZE, FORMAT JSON)` |
//! | Couchbase | `EXPLAIN` | a run with `profile=timings` (Enterprise Edition) |
//!
//! Warnings are added here, the same way for every database: full scans of
//! large inputs, sorts and temporary tables, estimates far off the actual
//! row counts.

use ferrodb_api::{ExecutionPlan, PlanNode};
use serde_json::{Map, Value};

mod couchbase;
pub use couchbase::couchbase_json;

/// Rows below which a full scan is not worth a warning.
pub const SCAN_WARNING_ROWS: u64 = 1000;
/// Estimated vs actual rows differing by this factor get a warning.
const MISESTIMATE_FACTOR: f64 = 10.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanError(pub String);

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot read the execution plan: {}", self.0)
    }
}

impl std::error::Error for PlanError {}

type Result<T> = std::result::Result<T, PlanError>;

fn err(msg: impl Into<String>) -> PlanError {
    PlanError(msg.into())
}

/// A number given as a JSON number or a numeric string (MySQL `cost_info`).
fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Object(o) => {
            // Extended JSON in canonical form: {"$numberLong": "12"}.
            o.values().next().and_then(|x| num(Some(x)))
        }
        _ => None,
    }
}

fn count(v: Option<&Value>) -> Option<u64> {
    num(v).map(|f| f.max(0.0).round() as u64)
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(text).collect::<Vec<_>>().join(", "),
        Value::Null => "null".into(),
        other => other.to_string(),
    }
}

fn parse_json(raw: &str) -> Result<Value> {
    serde_json::from_str(raw.trim()).map_err(|e| err(e.to_string()))
}

/// Adds the warnings that apply to every database.
fn common_warnings(n: &mut PlanNode, full_scan: bool) {
    if full_scan {
        let rows = n.actual_rows.unwrap_or(n.rows);
        if rows >= SCAN_WARNING_ROWS {
            n.warnings.push(format!(
                "full scan reading about {rows} rows; an index may help"
            ));
        }
    }
    if let Some(actual) = n.actual_rows
        && n.rows > 0
        && n.loops.unwrap_or(1) > 0
    {
        let (e, a) = (n.rows as f64, actual.max(1) as f64);
        if (e / a >= MISESTIMATE_FACTOR || a / e >= MISESTIMATE_FACTOR) && e.max(a) >= 100.0 {
            n.warnings.push(format!(
                "estimated {} rows, actual {actual}: statistics may be stale",
                n.rows
            ));
        }
    }
}

// ------------------------------------------------------------- PostgreSQL

/// `EXPLAIN (FORMAT JSON)` output (also DuckDB's, which differs; see
/// [`duckdb_json`]).
pub fn postgres_json(raw: &str) -> Result<ExecutionPlan> {
    let v = parse_json(raw)?;
    let top = match &v {
        Value::Array(a) => a.first().ok_or_else(|| err("empty plan"))?,
        other => other,
    };
    let plan = top.get("Plan").ok_or_else(|| err("no \"Plan\" key"))?;
    let root = pg_node(plan)?;
    Ok(ExecutionPlan {
        analyzed: root.actual_rows.is_some(),
        root,
        raw: serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.to_owned()),
        planning_ms: num(top.get("Planning Time")),
        execution_ms: num(top.get("Execution Time")),
        statement: String::new(),
    })
}

const PG_CONSUMED: &[&str] = &[
    "Node Type",
    "Plans",
    "Total Cost",
    "Plan Rows",
    "Actual Rows",
    "Actual Loops",
    "Actual Total Time",
    "Relation Name",
    "Alias",
    "Index Name",
    "Join Type",
    "Strategy",
    "Scan Direction",
    "Operation",
    "CTE Name",
    "Function Name",
    "Parallel Aware",
    "Async Capable",
];

fn pg_operation(o: &Map<String, Value>) -> String {
    let ty = o.get("Node Type").and_then(Value::as_str).unwrap_or("?");
    let join = o
        .get("Join Type")
        .and_then(Value::as_str)
        .filter(|j| *j != "Inner");
    let parallel = o.get("Parallel Aware").and_then(Value::as_bool) == Some(true);
    let mut op = match ty {
        "Aggregate" => match o.get("Strategy").and_then(Value::as_str) {
            Some("Hashed") => "HashAggregate".to_owned(),
            Some("Sorted") => "GroupAggregate".to_owned(),
            Some("Mixed") => "MixedAggregate".to_owned(),
            _ => "Aggregate".to_owned(),
        },
        "SetOp" => match o.get("Strategy").and_then(Value::as_str) {
            Some("Hashed") => "HashSetOp".to_owned(),
            _ => "SetOp".to_owned(),
        },
        "ModifyTable" => o
            .get("Operation")
            .and_then(Value::as_str)
            .unwrap_or("ModifyTable")
            .to_owned(),
        "Index Scan" | "Index Only Scan"
            if o.get("Scan Direction").and_then(Value::as_str) == Some("Backward") =>
        {
            format!("{ty} Backward")
        }
        other => other.to_owned(),
    };
    if let Some(j) = join {
        op = if op == "Nested Loop" {
            format!("Nested Loop {j} Join")
        } else {
            op.replace(" Join", &format!(" {j} Join"))
        };
    }
    if parallel {
        op = format!("Parallel {op}");
    }
    op
}

fn pg_node(v: &Value) -> Result<PlanNode> {
    let o = v
        .as_object()
        .ok_or_else(|| err("plan node is not an object"))?;
    let s = |k: &str| o.get(k).and_then(Value::as_str);
    let relation = s("Relation Name");
    let alias = s("Alias").filter(|a| Some(*a) != relation);
    let index = s("Index Name");
    let target = relation
        .map(|r| match alias {
            Some(a) => format!("{r} {a}"),
            None => r.to_owned(),
        })
        .or_else(|| s("CTE Name").map(str::to_owned))
        .or_else(|| s("Function Name").map(|f| format!("{f}()")))
        .or_else(|| s("Alias").map(str::to_owned));
    let detail = match (index, target) {
        (Some(i), Some(t)) => Some(format!("using {i} on {t}")),
        (Some(i), None) => Some(format!("on {i}")),
        (None, Some(t)) => Some(format!("on {t}")),
        (None, None) => None,
    };
    let loops = count(o.get("Actual Loops"));
    let actual_ms = num(o.get("Actual Total Time")).map(|t| t * loops.unwrap_or(1).max(1) as f64);
    let mut n = PlanNode {
        operation: pg_operation(o),
        detail,
        cost: num(o.get("Total Cost")).unwrap_or(0.0),
        rows: count(o.get("Plan Rows")).unwrap_or(0),
        object: index.or(relation).map(str::to_owned),
        actual_rows: count(o.get("Actual Rows")),
        actual_ms,
        loops,
        properties: o
            .iter()
            .filter(|(k, _)| !PG_CONSUMED.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), text(v)))
            .collect(),
        warnings: Vec::new(),
        children: o
            .get("Plans")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(pg_node).collect::<Result<Vec<_>>>())
            .transpose()?
            .unwrap_or_default(),
    };
    let ty = o.get("Node Type").and_then(Value::as_str).unwrap_or("");
    common_warnings(&mut n, ty == "Seq Scan");
    if let Some(m) = n.property("Sort Method")
        && m.contains("external")
    {
        n.warnings.push(format!(
            "sort spilled to disk ({m}); consider more work_mem"
        ));
    }
    if let (Some(removed), Some(actual)) = (count(o.get("Rows Removed by Filter")), n.actual_rows)
        && removed >= SCAN_WARNING_ROWS
        && removed > actual.saturating_mul(10)
    {
        n.warnings.push(format!(
            "filter removed {removed} rows to keep {actual}: an index on the filtered columns may help"
        ));
    }
    Ok(n)
}

// ------------------------------------------------------------------ DuckDB

/// DuckDB `EXPLAIN (FORMAT JSON)`: an array of operator trees with `name`,
/// `children` and `extra_info`. `EXPLAIN (ANALYZE, FORMAT JSON)` returns the
/// profile: operators with `operator_name` / `type`, `operator_timing` /
/// `timing` (seconds) and `operator_cardinality` / `intermediate_rows`,
/// under `children` (1.x) or `operator` (2.x), wrapped in `RESULT_COLLECTOR`
/// / `EXECUTE` / `EXPLAIN_ANALYZE` nodes that are skipped here.
pub fn duckdb_json(raw: &str) -> Result<ExecutionPlan> {
    let v = parse_json(raw)?;
    let nodes: Vec<&Value> = match &v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) if o.contains_key("operator") => o
            .get("operator")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        Value::Object(o) if o.contains_key("children") && !o.contains_key("name") => o
            .get("children")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        other => vec![other],
    };
    /// Wrapper operators of a profile, without meaning for the query.
    fn unwrap(v: &Value) -> &Value {
        let name = v
            .get("operator_type")
            .or_else(|| v.get("type"))
            .or_else(|| v.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let kids = v.get("children").and_then(Value::as_array);
        match kids {
            Some(k)
                if k.len() == 1
                    && matches!(
                        name,
                        "RESULT_COLLECTOR" | "EXECUTE" | "EXPLAIN_ANALYZE" | "QUERY"
                    ) =>
            {
                unwrap(&k[0])
            }
            _ => v,
        }
    }
    let mut roots: Vec<PlanNode> = nodes.into_iter().map(|n| duck_node(unwrap(n))).collect();
    let root = match roots.len() {
        0 => return Err(err("empty plan")),
        1 => roots.remove(0),
        _ => PlanNode {
            operation: "Query".into(),
            children: roots,
            ..Default::default()
        },
    };
    let execution_ms = num(v.get("latency")).map(|s| s * 1000.0);
    Ok(ExecutionPlan {
        analyzed: root.actual_rows.is_some(),
        root,
        raw: serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.to_owned()),
        planning_ms: None,
        execution_ms,
        statement: String::new(),
    })
}

fn duck_node(v: &Value) -> PlanNode {
    let name = v
        .get("name")
        .or_else(|| v.get("operator_name"))
        .or_else(|| v.get("operator_type"))
        .or_else(|| v.get("type"))
        .map(text)
        .unwrap_or_else(|| "?".into())
        .trim()
        .to_owned();
    let extra = v.get("extra_info");
    let mut properties: Vec<(String, String)> = match extra {
        Some(Value::Object(o)) => o.iter().map(|(k, v)| (k.clone(), text(v))).collect(),
        Some(Value::String(s)) if !s.trim().is_empty() => vec![("Info".into(), s.trim().into())],
        _ => Vec::new(),
    };
    let object = properties
        .iter()
        .find(|(k, _)| k == "Table" || k == "Function")
        .map(|(_, v)| v.clone());
    let estimated = properties
        .iter()
        .find(|(k, _)| k == "Estimated Cardinality")
        .and_then(|(_, v)| v.trim_start_matches('~').trim().parse::<u64>().ok());
    properties.retain(|(k, _)| k != "Estimated Cardinality");
    let mut n = PlanNode {
        detail: object.as_ref().map(|o| format!("on {o}")),
        operation: name,
        rows: estimated.unwrap_or(0),
        object,
        actual_rows: count(v.get("operator_cardinality"))
            .or_else(|| count(v.get("intermediate_rows"))),
        actual_ms: num(v.get("operator_timing"))
            .or_else(|| num(v.get("timing")))
            .map(|s| s * 1000.0),
        properties,
        children: v
            .get("children")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(duck_node).collect())
            .unwrap_or_default(),
        ..Default::default()
    };
    // DuckDB times are per operator: make them cumulative like the others.
    if n.actual_ms.is_some() {
        let inputs: f64 = n.children.iter().filter_map(|c| c.actual_ms).sum();
        n.actual_ms = n.actual_ms.map(|t| t + inputs);
    }
    let scan = n.operation.contains("SEQ_SCAN") || n.operation.contains("TABLE_SCAN");
    let has_filter = n.properties.iter().any(|(k, _)| k == "Filters");
    common_warnings(&mut n, scan && !has_filter);
    n
}

// -------------------------------------------------------- MySQL / MariaDB

/// `EXPLAIN FORMAT=JSON` of MySQL, and `EXPLAIN` / `ANALYZE FORMAT=JSON` of
/// MariaDB (`r_rows`, `r_total_time_ms`).
pub fn mysql_json(raw: &str) -> Result<ExecutionPlan> {
    let v = parse_json(raw)?;
    let qb = v
        .get("query_block")
        .ok_or_else(|| err("no \"query_block\" key"))?;
    let root = my_block(qb);
    Ok(ExecutionPlan {
        analyzed: root.flatten().iter().any(|(_, n)| n.actual_rows.is_some()),
        execution_ms: num(qb.get("r_total_time_ms")),
        root,
        raw: serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.to_owned()),
        planning_ms: None,
        statement: String::new(),
    })
}

fn my_cost(o: &Value) -> Option<f64> {
    let ci = o.get("cost_info");
    num(ci.and_then(|c| c.get("query_cost")))
        .or_else(|| num(ci.and_then(|c| c.get("prefix_cost"))))
        .or_else(|| num(ci.and_then(|c| c.get("sort_cost"))))
        .or_else(|| num(o.get("cost")))
}

fn my_block(qb: &Value) -> PlanNode {
    let id = qb.get("select_id").map(text).unwrap_or_else(|| "1".into());
    let mut n = PlanNode {
        operation: "Query block".into(),
        detail: Some(format!("#{id}")),
        cost: my_cost(qb).unwrap_or(0.0),
        ..Default::default()
    };
    if let Some(m) = qb.get("message").map(text) {
        n.properties.push(("message".into(), m));
    }
    n.children = my_children(qb);
    if n.rows == 0 {
        n.rows = n.children.iter().map(|c| c.rows).max().unwrap_or(0);
    }
    n
}

/// The operations nested in a query block or operation object.
fn my_children(o: &Value) -> Vec<PlanNode> {
    let Some(map) = o.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (k, v) in map {
        match k.as_str() {
            "table" => out.push(my_table(v)),
            "nested_loop" => {
                let tables: Vec<PlanNode> = v
                    .as_array()
                    .map(|a| a.iter().flat_map(my_children).collect())
                    .unwrap_or_default();
                if tables.len() == 1 {
                    out.extend(tables);
                } else {
                    let mut nl = PlanNode {
                        operation: "Nested loop".into(),
                        cost: tables.iter().map(|t| t.cost).fold(0.0, f64::max),
                        rows: tables.last().map(|t| t.rows).unwrap_or(0),
                        children: tables,
                        ..Default::default()
                    };
                    nl.actual_rows = nl.children.last().and_then(|t| t.actual_rows);
                    out.push(nl);
                }
            }
            "ordering_operation" | "grouping_operation" | "duplicates_removal" | "windowing"
            | "filesort" | "temporary_table" | "read_sorted_file" | "having_condition" => {
                out.push(my_operation(k, v))
            }
            "union_result" => {
                let mut u = PlanNode::new(if v.get("table_name").is_some() {
                    "Union"
                } else {
                    "Union result"
                });
                u.detail = v.get("table_name").map(text);
                u.cost = my_cost(v).unwrap_or(0.0);
                for spec in v
                    .get("query_specifications")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(qb) = spec.get("query_block") {
                        u.children.push(my_block(qb));
                    }
                }
                out.push(u);
            }
            "attached_subqueries"
            | "optimized_away_subqueries"
            | "order_by_subqueries"
            | "group_by_subqueries"
            | "subqueries" => {
                for s in v.as_array().into_iter().flatten() {
                    if let Some(qb) = s.get("query_block") {
                        let mut b = my_block(qb);
                        b.operation = "Subquery".into();
                        out.push(b);
                    }
                }
            }
            "query_block" => out.push(my_block(v)),
            _ => {}
        }
    }
    out
}

fn my_operation(key: &str, v: &Value) -> PlanNode {
    let flag = |k: &str| v.get(k).and_then(Value::as_bool) == Some(true);
    let mut n = PlanNode::new(match key {
        "ordering_operation" => "Ordering",
        "grouping_operation" => "Grouping",
        "duplicates_removal" => "Duplicates removal",
        "windowing" => "Window",
        "filesort" | "read_sorted_file" => "Filesort",
        "temporary_table" => "Temporary table",
        "having_condition" => "Having",
        other => other,
    });
    n.cost = my_cost(v).unwrap_or(0.0);
    if let Some(k) = v.get("sort_key") {
        n.detail = Some(text(k));
    }
    if flag("using_filesort") || key == "filesort" {
        n.warnings.push(
            "sorts rows (filesort): an index on the ORDER BY / GROUP BY columns may help".into(),
        );
    }
    if flag("using_temporary_table") || key == "temporary_table" {
        n.warnings.push("uses a temporary table".into());
    }
    if let Some(r) = count(v.get("r_output_rows")).or_else(|| count(v.get("r_rows"))) {
        n.actual_rows = Some(r);
    }
    n.actual_ms = num(v.get("r_total_time_ms"));
    n.children = my_children(v);
    if n.cost == 0.0 {
        n.cost = n.children.iter().map(|c| c.cost).fold(0.0, f64::max);
    }
    n.rows = n.children.iter().map(|c| c.rows).max().unwrap_or(0);
    n
}

fn access_label(access: &str) -> &'static str {
    match access {
        "ALL" => "Full table scan",
        "index" => "Full index scan",
        "range" => "Index range scan",
        "ref" => "Index lookup",
        "eq_ref" => "Unique index lookup",
        "ref_or_null" => "Index lookup or NULL",
        "const" | "system" => "Constant lookup",
        "fulltext" => "Full-text lookup",
        "index_merge" => "Index merge",
        "unique_subquery" | "index_subquery" => "Subquery index lookup",
        "hash" => "Hash lookup",
        _ => "Table access",
    }
}

const MY_CONSUMED: &[&str] = &[
    "table_name",
    "access_type",
    "key",
    "cost_info",
    "rows_examined_per_scan",
    "rows_produced_per_join",
    "rows",
    "r_rows",
    "r_loops",
    "r_total_time_ms",
    "r_table_time_ms",
    "loops",
    "cost",
    "materialized_from_subquery",
    "used_columns",
];

fn my_table(v: &Value) -> PlanNode {
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    let table = s("table_name").unwrap_or("?");
    let access = s("access_type").unwrap_or("");
    let key = s("key");
    let rows = count(v.get("rows_produced_per_join"))
        .or_else(|| count(v.get("rows_examined_per_scan")))
        .or_else(|| count(v.get("rows")))
        .unwrap_or(0);
    let loops = count(v.get("r_loops"));
    let mut n = PlanNode {
        operation: access_label(access).into(),
        detail: Some(match key {
            Some(k) => format!("on {table} using {k}"),
            None => format!("on {table}"),
        }),
        cost: my_cost(v).unwrap_or(0.0),
        rows,
        object: Some(table.to_owned()),
        actual_rows: count(v.get("r_rows")),
        actual_ms: num(v.get("r_total_time_ms")).or_else(|| num(v.get("r_table_time_ms"))),
        loops,
        properties: v
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(k, _)| !MY_CONSUMED.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), text(v)))
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    };
    if let Some(ci) = v.get("cost_info").and_then(Value::as_object) {
        for (k, c) in ci {
            n.properties.push((k.clone(), text(c)));
        }
    }
    if let Some(m) = v
        .get("materialized_from_subquery")
        .and_then(|m| m.get("query_block"))
    {
        let mut b = my_block(m);
        b.operation = "Materialized subquery".into();
        n.children.push(b);
    }
    let examined = count(v.get("rows_examined_per_scan")).unwrap_or(rows);
    let mut probe = n.clone();
    probe.rows = examined;
    common_warnings(&mut probe, access == "ALL");
    n.warnings = probe.warnings;
    if access == "index" && examined >= SCAN_WARNING_ROWS {
        n.warnings
            .push(format!("reads the whole index ({examined} rows)"));
    }
    n
}

/// `EXPLAIN ANALYZE` / `EXPLAIN FORMAT=TREE` of MySQL 8: one line per
/// operation, children indented by four spaces:
///
/// ```text
/// -> Nested loop inner join  (cost=1.55 rows=3) (actual time=0.05..0.07 rows=3 loops=1)
///     -> Table scan on c  (cost=0.55 rows=3) (actual time=0.03..0.04 rows=3 loops=1)
/// ```
pub fn mysql_tree(raw: &str) -> Result<ExecutionPlan> {
    let mut stack: Vec<(usize, PlanNode)> = Vec::new();
    let mut roots: Vec<PlanNode> = Vec::new();
    fn attach(stack: &mut Vec<(usize, PlanNode)>, roots: &mut Vec<PlanNode>, upto: usize) {
        while stack.last().is_some_and(|(d, _)| *d >= upto) {
            let (_, n) = stack.pop().expect("checked");
            match stack.last_mut() {
                Some((_, p)) => p.children.push(n),
                None => roots.push(n),
            }
        }
    }
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let Some(body) = trimmed.strip_prefix("->") else {
            // A continuation of the previous line (long conditions wrap).
            if let Some((_, n)) = stack.last_mut()
                && !trimmed.is_empty()
            {
                let d = n.detail.get_or_insert_with(String::new);
                d.push(' ');
                d.push_str(trimmed);
            }
            continue;
        };
        let depth = (line.len() - trimmed.len()) / 4;
        attach(&mut stack, &mut roots, depth);
        stack.push((depth, tree_line(body.trim())));
    }
    attach(&mut stack, &mut roots, 0);
    let root = match roots.len() {
        0 => return Err(err("no plan lines")),
        1 => roots.remove(0),
        _ => PlanNode {
            operation: "Query".into(),
            children: roots,
            ..Default::default()
        },
    };
    Ok(ExecutionPlan {
        analyzed: root.actual_rows.is_some(),
        execution_ms: root.actual_ms,
        root,
        raw: raw.to_owned(),
        planning_ms: None,
        statement: String::new(),
    })
}

/// `key=value` pairs inside `( … )` groups after the operation text.
fn tree_line(body: &str) -> PlanNode {
    let (head, stats) = match body.find("  (") {
        Some(i) => (&body[..i], &body[i..]),
        None => match body.rfind(" (cost=") {
            Some(i) => (&body[..i], &body[i..]),
            None => (body, ""),
        },
    };
    let (operation, detail) = split_operation(head);
    let mut n = PlanNode {
        operation,
        detail,
        ..Default::default()
    };
    let mut groups = stats.split('(').skip(1);
    for g in groups.by_ref() {
        let g = g.trim().trim_end_matches(')').trim();
        if g == "never executed" {
            n.actual_rows = Some(0);
            n.loops = Some(0);
            continue;
        }
        let actual = g.starts_with("actual");
        for kv in g.split_whitespace() {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            match (actual, k) {
                (false, "cost") => n.cost = v.parse().unwrap_or(0.0),
                (false, "rows") => n.rows = v.parse::<f64>().map(|f| f as u64).unwrap_or(0),
                (true, "time") => {
                    let last = v.split("..").last().unwrap_or(v);
                    n.actual_ms = last.parse().ok();
                }
                (true, "rows") => n.actual_rows = v.parse::<f64>().ok().map(|f| f.round() as u64),
                (true, "loops") => n.loops = v.parse().ok(),
                _ => {}
            }
        }
    }
    if let (Some(t), Some(l)) = (n.actual_ms, n.loops) {
        n.actual_ms = Some(t * l.max(1) as f64);
    }
    let lower = n.operation.to_ascii_lowercase();
    n.object = n
        .detail
        .as_deref()
        .and_then(|d| d.strip_prefix("on "))
        .and_then(|d| d.split_whitespace().next())
        .map(str::to_owned);
    common_warnings(&mut n, lower == "table scan");
    if lower.starts_with("sort") && !lower.contains("index") {
        n.warnings
            .push("sorts rows: an index on the sort columns may help".into());
    }
    if lower.contains("temporary table") {
        n.warnings.push("uses a temporary table".into());
    }
    n
}

/// `Table scan on c` → (`Table scan`, `on c`); `Filter: (x > 1)` →
/// (`Filter`, `(x > 1)`).
fn split_operation(head: &str) -> (String, Option<String>) {
    if let Some((op, rest)) = head.split_once(": ") {
        return (op.trim().to_owned(), Some(rest.trim().to_owned()));
    }
    for sep in [" on ", " using "] {
        if let Some(i) = head.find(sep) {
            return (
                head[..i].trim().to_owned(),
                Some(head[i + 1..].trim().to_owned()),
            );
        }
    }
    (head.trim().to_owned(), None)
}

// ------------------------------------------------------------------ SQLite

/// `EXPLAIN QUERY PLAN` rows: (`id`, `parent`, `detail`).
pub fn sqlite_rows(rows: &[(i64, i64, String)]) -> Result<ExecutionPlan> {
    fn build(id: i64, rows: &[(i64, i64, String)]) -> Vec<PlanNode> {
        rows.iter()
            .filter(|(_, p, _)| *p == id)
            .map(|(i, _, d)| {
                let mut n = sqlite_node(d);
                n.children = build(*i, rows);
                n
            })
            .collect()
    }
    let children = build(0, rows);
    if children.is_empty() {
        return Err(err("no plan rows"));
    }
    let root = PlanNode {
        operation: "QUERY PLAN".into(),
        children,
        ..Default::default()
    };
    let raw = rows
        .iter()
        .map(|(i, p, d)| format!("{i}\t{p}\t{d}"))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ExecutionPlan {
        root,
        raw,
        ..Default::default()
    })
}

fn sqlite_node(detail: &str) -> PlanNode {
    let d = detail.trim();
    let mut words = d.split_whitespace();
    let first = words.next().unwrap_or_default();
    let mut n = PlanNode::default();
    match first {
        "SCAN" | "SEARCH" => {
            let mut rest: Vec<&str> = words.collect();
            // Older versions: `SCAN TABLE t`.
            if rest.first() == Some(&"TABLE") {
                rest.remove(0);
            }
            let object = rest.first().copied().unwrap_or_default();
            n.operation = first.to_owned();
            n.object = (!object.is_empty()).then(|| object.to_owned());
            n.detail = Some(rest.join(" "));
            let indexed = d.contains("INDEX") || d.contains("PRIMARY KEY");
            if first == "SCAN" && !indexed && !object.starts_with("CONSTANT") {
                n.warnings
                    .push("full table scan: an index on the filtered columns may help".into());
            }
        }
        _ if d.starts_with("USE TEMP B-TREE") => {
            n.operation = "USE TEMP B-TREE".into();
            n.detail = Some(d["USE TEMP B-TREE".len()..].trim().to_owned());
            n.warnings
                .push("builds a temporary b-tree: an index could avoid the sort / grouping".into());
        }
        _ => {
            // `CO-ROUTINE x`, `MATERIALIZE x`, `COMPOUND QUERY`, `CORRELATED
            // SCALAR SUBQUERY 1`…: upper-case words are the operation.
            let op: Vec<&str> = d
                .split_whitespace()
                .take_while(|w| {
                    w.chars().any(char::is_alphabetic) && w.chars().all(|c| !c.is_lowercase())
                })
                .collect();
            let op = if op.is_empty() {
                d.to_owned()
            } else {
                op.join(" ")
            };
            let rest = d[op.len().min(d.len())..].trim();
            n.operation = op;
            n.detail = (!rest.is_empty()).then(|| rest.to_owned());
        }
    }
    n
}

// ----------------------------------------------------------------- MongoDB

/// `explain` output of `find` / `aggregate` / `count` / `distinct`
/// (relaxed Extended JSON), with or without `executionStats`.
pub fn mongo(v: &Value) -> Result<ExecutionPlan> {
    let mut execution_ms = None;
    let root = if let Some(stages) = v.get("stages").and_then(Value::as_array) {
        // An aggregation not fully pushed down: each stage consumes the
        // previous one; `$cursor` holds the query plan.
        let mut prev: Option<PlanNode> = None;
        for st in stages {
            let Some((name, body)) = st.as_object().and_then(|o| {
                o.iter()
                    .find(|(k, _)| k.starts_with('$'))
                    .map(|(k, b)| (k.clone(), b))
            }) else {
                continue;
            };
            let mut n = if name == "$cursor" {
                let (n, ms) = mongo_cursor(body)?;
                execution_ms = execution_ms.or(ms);
                n
            } else {
                PlanNode {
                    operation: name.clone(),
                    detail: Some(compact(body)),
                    ..Default::default()
                }
            };
            if name != "$cursor" {
                n.actual_rows = count(st.get("nReturned"));
                n.actual_ms = num(st.get("executionTimeMillisEstimate"));
                if name == "$sort" && st.get("usedDisk").and_then(Value::as_bool) == Some(true) {
                    n.warnings.push("sort spilled to disk".into());
                }
                if let Some(p) = prev.take() {
                    n.children.push(p);
                }
            }
            prev = Some(n);
        }
        prev.ok_or_else(|| err("no stages"))?
    } else if v.get("queryPlanner").is_some() || v.get("executionStats").is_some() {
        let (n, ms) = mongo_cursor(v)?;
        execution_ms = ms;
        n
    } else if let Some(shards) = v.get("shards").and_then(Value::as_object) {
        let mut root = PlanNode::new("SHARD_MERGE");
        for (name, s) in shards {
            let mut p = mongo(s)?.root;
            p.detail = Some(match p.detail.take() {
                Some(d) => format!("{d} (shard {name})"),
                None => format!("shard {name}"),
            });
            root.children.push(p);
        }
        root
    } else {
        return Err(err("no queryPlanner, executionStats or stages"));
    };
    Ok(ExecutionPlan {
        analyzed: root.flatten().iter().any(|(_, n)| n.actual_rows.is_some()),
        root,
        raw: serde_json::to_string_pretty(v).unwrap_or_default(),
        planning_ms: None,
        execution_ms,
        statement: String::new(),
    })
}

fn compact(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 120 {
        format!("{}…", s.chars().take(119).collect::<String>())
    } else {
        s
    }
}

/// A `queryPlanner` / `executionStats` pair: the executed tree when there
/// is one (it carries the counters), else the winning plan.
fn mongo_cursor(v: &Value) -> Result<(PlanNode, Option<f64>)> {
    let qp = v.get("queryPlanner");
    let ns = qp
        .and_then(|q| q.get("namespace"))
        .and_then(Value::as_str)
        .map(|ns| ns.split_once('.').map_or(ns, |(_, c)| c).to_owned());
    if let Some(es) = v.get("executionStats")
        && let Some(stages) = es.get("executionStages")
    {
        let mut root = mongo_stage(stages, ns.as_deref());
        let ms = num(es.get("executionTimeMillis"));
        if let Some(ms) = ms {
            root.actual_ms = Some(root.actual_ms.unwrap_or(0.0).max(ms));
        }
        root.actual_rows = count(es.get("nReturned")).or(root.actual_rows);
        let docs = count(es.get("totalDocsExamined")).unwrap_or(0);
        let keys = count(es.get("totalKeysExamined")).unwrap_or(0);
        let returned = root.actual_rows.unwrap_or(0);
        root.properties
            .push(("totalDocsExamined".into(), docs.to_string()));
        root.properties
            .push(("totalKeysExamined".into(), keys.to_string()));
        if docs >= SCAN_WARNING_ROWS && docs > returned.saturating_mul(10) {
            root.warnings.push(format!(
                "examined {docs} documents to return {returned}: an index matching the filter may help"
            ));
        }
        return Ok((root, ms));
    }
    let wp = qp
        .and_then(|q| q.get("winningPlan"))
        .ok_or_else(|| err("no winningPlan"))?;
    // Slot-based engine: {queryPlan: {...}, slotBasedPlan: {...}}.
    let wp = wp.get("queryPlan").unwrap_or(wp);
    let mut root = mongo_stage(wp, ns.as_deref());
    if let Some(r) = qp
        .and_then(|q| q.get("rejectedPlans"))
        .and_then(Value::as_array)
    {
        root.properties
            .push(("rejectedPlans".into(), r.len().to_string()));
    }
    Ok((root, None))
}

const MONGO_CONSUMED: &[&str] = &[
    "stage",
    "inputStage",
    "inputStages",
    "nReturned",
    "executionTimeMillisEstimate",
    "indexName",
    "keyPattern",
    "filter",
    "planNodeId",
];

fn mongo_stage(v: &Value, ns: Option<&str>) -> PlanNode {
    let stage = v.get("stage").map(text).unwrap_or_else(|| "?".into());
    let index = v.get("indexName").and_then(Value::as_str);
    let mut n = PlanNode {
        operation: stage.clone(),
        actual_rows: count(v.get("nReturned")),
        actual_ms: num(v.get("executionTimeMillisEstimate")),
        properties: v
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(k, _)| !MONGO_CONSUMED.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), text(v)))
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    };
    n.detail = match (index, v.get("keyPattern"), v.get("filter")) {
        (Some(i), Some(k), _) => Some(format!("{i} {}", compact(k))),
        (Some(i), None, _) => Some(i.to_owned()),
        (None, _, Some(f)) => Some(compact(f)),
        _ => None,
    };
    n.object = index.map(str::to_owned).or_else(|| {
        (stage == "COLLSCAN")
            .then(|| ns.map(str::to_owned))
            .flatten()
    });
    if let Some(i) = v.get("inputStage") {
        n.children.push(mongo_stage(i, ns));
    }
    for i in v
        .get("inputStages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        n.children.push(mongo_stage(i, ns));
    }
    match stage.as_str() {
        "COLLSCAN" => n.warnings.push("collection scan: no index is used".into()),
        "SORT" => {
            if v.get("usedDisk").and_then(Value::as_bool) == Some(true) {
                n.warnings.push("sort spilled to disk".into());
            } else {
                n.warnings
                    .push("in-memory sort: an index on the sort fields may help".into());
            }
        }
        _ => {}
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: &str = r#"[{"Plan": {"Node Type": "Hash Join", "Join Type": "Left", "Total Cost": 120.5,
        "Plan Rows": 1000, "Hash Cond": "(o.customer_id = c.id)",
        "Actual Rows": 980, "Actual Loops": 1, "Actual Total Time": 4.5,
        "Plans": [
          {"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "o", "Total Cost": 80.0,
           "Plan Rows": 5000, "Actual Rows": 5000, "Actual Loops": 1, "Actual Total Time": 2.0,
           "Filter": "(total > 10)", "Rows Removed by Filter": 20},
          {"Node Type": "Hash", "Total Cost": 30.0, "Plan Rows": 100, "Actual Rows": 100,
           "Actual Loops": 1, "Actual Total Time": 1.0,
           "Plans": [{"Node Type": "Index Scan", "Index Name": "customers_pkey",
              "Relation Name": "customers", "Alias": "c", "Scan Direction": "Forward",
              "Total Cost": 25.0, "Plan Rows": 2, "Actual Rows": 100, "Actual Loops": 1,
              "Actual Total Time": 0.8, "Index Cond": "(id > 5)"}]}
        ]}, "Planning Time": 0.3, "Execution Time": 5.1}]"#;

    #[test]
    fn postgres_plan() {
        let p = postgres_json(PG).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.planning_ms, Some(0.3));
        assert_eq!(p.execution_ms, Some(5.1));
        let r = &p.root;
        assert_eq!(r.operation, "Hash Left Join");
        assert_eq!(r.property("Hash Cond"), Some("(o.customer_id = c.id)"));
        assert_eq!(r.children.len(), 2);
        let scan = &r.children[0];
        assert_eq!(scan.operation, "Seq Scan");
        assert_eq!(scan.detail.as_deref(), Some("on orders o"));
        assert_eq!(scan.object.as_deref(), Some("orders"));
        assert!(
            scan.warnings[0].contains("full scan"),
            "{:?}",
            scan.warnings
        );
        let idx = &r.children[1].children[0];
        assert_eq!(
            idx.detail.as_deref(),
            Some("using customers_pkey on customers c")
        );
        assert!(
            idx.warnings.iter().any(|w| w.contains("estimated 2 rows")),
            "{:?}",
            idx.warnings
        );
        assert!((r.self_cost() - 10.5).abs() < 1e-9);
        assert_eq!(r.self_ms(), Some(1.5));
        assert_eq!(r.len(), 4);
        assert_eq!(p.warnings().len(), 2);
    }

    #[test]
    fn postgres_aggregate_names() {
        let p = postgres_json(
            r#"[{"Plan": {"Node Type": "Aggregate", "Strategy": "Hashed", "Total Cost": 1,
                "Plan Rows": 1, "Group Key": ["a", "b"]}}]"#,
        )
        .unwrap();
        assert_eq!(p.root.operation, "HashAggregate");
        assert_eq!(p.root.property("Group Key"), Some("a, b"));
        assert!(!p.analyzed);
    }

    #[test]
    fn mysql_json_plan() {
        let raw = r#"{"query_block": {"select_id": 1, "cost_info": {"query_cost": "1207.25"},
          "ordering_operation": {"using_filesort": true,
            "nested_loop": [
              {"table": {"table_name": "c", "access_type": "ALL", "rows_examined_per_scan": 5000,
                 "rows_produced_per_join": 5000, "filtered": "100.00",
                 "cost_info": {"read_cost": "7.25", "eval_cost": "500.00", "prefix_cost": "507.25"}}},
              {"table": {"table_name": "o", "access_type": "ref", "key": "ix_customer",
                 "rows_examined_per_scan": 1, "rows_produced_per_join": 5000,
                 "cost_info": {"prefix_cost": "1207.25"}, "attached_condition": "(o.total > 10)"}}
            ]}}}"#;
        let p = mysql_json(raw).unwrap();
        assert!(!p.analyzed);
        let r = &p.root;
        assert_eq!(r.operation, "Query block");
        assert_eq!(r.cost, 1207.25);
        let ord = &r.children[0];
        assert_eq!(ord.operation, "Ordering");
        assert!(ord.warnings[0].contains("filesort"));
        let nl = &ord.children[0];
        assert_eq!(nl.operation, "Nested loop");
        assert_eq!(nl.children[0].operation, "Full table scan");
        assert!(!nl.children[0].warnings.is_empty());
        assert_eq!(nl.children[1].operation, "Index lookup");
        assert_eq!(
            nl.children[1].detail.as_deref(),
            Some("on o using ix_customer")
        );
        assert_eq!(
            nl.children[1].property("attached_condition"),
            Some("(o.total > 10)")
        );
    }

    #[test]
    fn mariadb_analyze() {
        let raw = r#"{"query_block": {"select_id": 1, "r_loops": 1, "r_total_time_ms": 0.42,
          "table": {"table_name": "t", "access_type": "ALL", "r_loops": 1, "rows": 3,
            "r_rows": 3, "r_table_time_ms": 0.01, "filtered": 100, "r_filtered": 100}}}"#;
        let p = mysql_json(raw).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.execution_ms, Some(0.42));
        assert_eq!(p.root.children[0].actual_rows, Some(3));
    }

    #[test]
    fn mysql_tree_analyze() {
        let raw = "-> Nested loop inner join  (cost=1.55 rows=3) (actual time=0.05..0.07 rows=3 loops=1)\n    -> Table scan on c  (cost=0.55 rows=3) (actual time=0.03..0.04 rows=3 loops=1)\n    -> Single-row index lookup on o using PRIMARY (id=c.id)  (cost=0.28 rows=1) (actual time=0.01..0.01 rows=1 loops=3)\n";
        let p = mysql_tree(raw).unwrap();
        assert!(p.analyzed);
        let r = &p.root;
        assert_eq!(r.operation, "Nested loop inner join");
        assert_eq!(r.cost, 1.55);
        assert_eq!(r.actual_ms, Some(0.07));
        assert_eq!(r.children.len(), 2);
        assert_eq!(r.children[0].operation, "Table scan");
        assert_eq!(r.children[0].object.as_deref(), Some("c"));
        let l = &r.children[1];
        assert_eq!(l.operation, "Single-row index lookup");
        assert_eq!(l.loops, Some(3));
        assert!((l.actual_ms.unwrap() - 0.03).abs() < 1e-9);
    }

    #[test]
    fn sqlite_plan() {
        let rows = vec![
            (2, 0, "SCAN o".to_owned()),
            (
                4,
                0,
                "SEARCH c USING INTEGER PRIMARY KEY (rowid=?)".to_owned(),
            ),
            (12, 0, "USE TEMP B-TREE FOR ORDER BY".to_owned()),
            (15, 0, "CORRELATED SCALAR SUBQUERY 1".to_owned()),
            (18, 15, "SEARCH x USING INDEX ix (a=?)".to_owned()),
        ];
        let p = sqlite_rows(&rows).unwrap();
        let r = &p.root;
        assert_eq!(r.children.len(), 4);
        assert_eq!(r.children[0].operation, "SCAN");
        assert_eq!(r.children[0].object.as_deref(), Some("o"));
        assert_eq!(r.children[0].warnings.len(), 1);
        assert!(r.children[1].warnings.is_empty());
        assert_eq!(r.children[2].operation, "USE TEMP B-TREE");
        assert_eq!(r.children[3].operation, "CORRELATED SCALAR SUBQUERY");
        assert_eq!(r.children[3].children[0].object.as_deref(), Some("x"));
        assert!(sqlite_rows(&[]).is_err());
    }

    #[test]
    fn mongo_find_with_stats() {
        let v: Value = serde_json::from_str(
            r#"{"queryPlanner": {"namespace": "shop.orders",
                 "winningPlan": {"stage": "FETCH", "inputStage": {"stage": "IXSCAN",
                   "indexName": "status_1", "keyPattern": {"status": 1}}},
                 "rejectedPlans": []},
               "executionStats": {"nReturned": 5, "executionTimeMillis": 3,
                 "totalKeysExamined": 5, "totalDocsExamined": 5,
                 "executionStages": {"stage": "FETCH", "nReturned": 5,
                   "executionTimeMillisEstimate": 1, "docsExamined": 5,
                   "inputStage": {"stage": "IXSCAN", "nReturned": 5, "indexName": "status_1",
                     "keyPattern": {"status": 1}, "keysExamined": 5}}}}"#,
        )
        .unwrap();
        let p = mongo(&v).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.execution_ms, Some(3.0));
        assert_eq!(p.root.operation, "FETCH");
        assert_eq!(p.root.actual_ms, Some(3.0));
        let ix = &p.root.children[0];
        assert_eq!(ix.object.as_deref(), Some("status_1"));
        assert_eq!(ix.property("keysExamined"), Some("5"));
    }

    #[test]
    fn mongo_collscan_and_pipeline() {
        let v: Value = serde_json::from_str(
            r#"{"stages": [
                 {"$cursor": {"queryPlanner": {"namespace": "shop.orders",
                    "winningPlan": {"queryPlan": {"stage": "COLLSCAN", "filter": {"total": {"$gt": 10}},
                      "direction": "forward"}}}}},
                 {"$group": {"_id": "$status", "n": {"$sum": 1}}, "nReturned": 3,
                  "executionTimeMillisEstimate": 2},
                 {"$sort": {"sortKey": {"n": -1}}, "nReturned": 3, "usedDisk": true}
               ]}"#,
        )
        .unwrap();
        let p = mongo(&v).unwrap();
        let r = &p.root;
        assert_eq!(r.operation, "$sort");
        assert_eq!(r.warnings, vec!["sort spilled to disk".to_owned()]);
        let g = &r.children[0];
        assert_eq!(g.operation, "$group");
        let scan = &g.children[0];
        assert_eq!(scan.operation, "COLLSCAN");
        assert_eq!(scan.object.as_deref(), Some("orders"));
        assert!(scan.warnings[0].contains("collection scan"));
    }

    #[test]
    fn duckdb_plan() {
        let raw = r#"[{"name": "PROJECTION", "children": [{"name": "SEQ_SCAN ",
            "children": [], "extra_info": {"Table": "orders", "Projections": ["id", "total"],
            "Estimated Cardinality": "5000"}}], "extra_info": {"Projections": ["id"]}}]"#;
        let p = duckdb_json(raw).unwrap();
        assert_eq!(p.root.operation, "PROJECTION");
        let scan = &p.root.children[0];
        assert_eq!(scan.operation, "SEQ_SCAN");
        assert_eq!(scan.rows, 5000);
        assert_eq!(scan.object.as_deref(), Some("orders"));
        assert_eq!(scan.warnings.len(), 1);
    }

    #[test]
    fn duckdb_profile() {
        let raw = r##"{"latency": 0.002, "operator": [{"type": "RESULT_COLLECTOR", "timing": 0.0,
            "children": [{"type": "EXPLAIN_ANALYZE", "children": [{"type": "HASH_GROUP_BY",
              "timing": 0.0003, "intermediate_rows": 2, "extra_info": {"Groups": "#0",
              "Estimated Cardinality": "1"},
              "children": [{"type": "TABLE_SCAN", "timing": 0.0001, "intermediate_rows": 3,
                "extra_info": {"Function": "READ_CSV", "Estimated Cardinality": "3"},
                "children": []}]}]}]}]}"##;
        let p = duckdb_json(raw).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.execution_ms, Some(2.0));
        assert_eq!(p.root.operation, "HASH_GROUP_BY");
        assert_eq!(p.root.actual_rows, Some(2));
        let scan = &p.root.children[0];
        assert_eq!(scan.object.as_deref(), Some("READ_CSV"));
        assert!((p.root.actual_ms.unwrap() - 0.4).abs() < 1e-9);
    }

    #[test]
    fn bad_input_is_an_error() {
        assert!(postgres_json("not json").is_err());
        assert!(mysql_json("{}").is_err());
        assert!(mysql_tree("").is_err());
        assert!(mongo(&serde_json::json!({})).is_err());
    }
}
