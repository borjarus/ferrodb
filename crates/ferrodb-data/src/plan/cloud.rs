//! Execution plans of the analytical databases of phase 8:
//!
//! | Database | Estimate | With `analyze` |
//! |---|---|---|
//! | ClickHouse | `EXPLAIN json = 1, actions = 1, indexes = 1, description = 1` | — (no `EXPLAIN ANALYZE`) |
//! | Snowflake | `EXPLAIN USING JSON` | — |
//! | BigQuery | dry-run job (bytes, referenced tables) | job statistics: `queryPlan` stages |
//!
//! None of them gives row estimates per operator; what they give instead
//! (granules and parts selected by each index, partitions pruned, bytes
//! scanned, records per stage) becomes properties and warnings.

use ferrodb_api::{ExecutionPlan, PlanNode};
use serde_json::{Map, Value};

use super::{Result, common_warnings, count, err, num, parse_json, text};

/// Rows per granule (ClickHouse `index_granularity` default), for the row
/// estimate of a `ReadFromMergeTree` step.
const GRANULE_ROWS: u64 = 8192;

fn pretty(v: &Value, raw: &str) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| raw.to_owned())
}

/// Scalar properties of a JSON object, in order, except `skip`.
fn scalars(o: &Map<String, Value>, skip: &[&str]) -> Vec<(String, String)> {
    o.iter()
        .filter(|(k, v)| {
            !skip.contains(&k.as_str())
                && !v.is_object()
                && !(v.is_array() && v.as_array().is_some_and(|a| a.iter().any(Value::is_object)))
        })
        .map(|(k, v)| (k.clone(), text(v)))
        .collect()
}

// ------------------------------------------------------------- ClickHouse

/// `EXPLAIN json = 1, …` output: `[{"Plan": {"Node Type", "Description",
/// "Plans": […], …}}]`.
pub fn clickhouse_json(raw: &str) -> Result<ExecutionPlan> {
    let v = parse_json(raw)?;
    let top = match &v {
        Value::Array(a) => a.first().ok_or_else(|| err("empty plan"))?,
        other => other,
    };
    let plan = top.get("Plan").ok_or_else(|| err("no \"Plan\" key"))?;
    Ok(ExecutionPlan {
        root: ch_node(plan),
        raw: pretty(&v, raw),
        ..Default::default()
    })
}

const CH_CONSUMED: &[&str] = &[
    "Node Type",
    "Node Id",
    "Description",
    "Plans",
    "Expression",
    "Indexes",
    "Prewhere info",
    "Keys",
    "Aggregates",
    "Sort Description",
];

fn ch_node(v: &Value) -> PlanNode {
    let Some(o) = v.as_object() else {
        return PlanNode::new("?");
    };
    let mut n = PlanNode::new(
        o.get("Node Type")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned(),
    );
    n.detail = o
        .get("Description")
        .and_then(Value::as_str)
        .map(str::to_owned);
    n.properties = scalars(o, CH_CONSUMED);
    if let Some(keys) = o.get("Keys").and_then(Value::as_array) {
        n.properties
            .push(("Keys".into(), text(&Value::Array(keys.clone()))));
    }
    if let Some(aggs) = o.get("Aggregates").and_then(Value::as_array) {
        let names: Vec<String> = aggs
            .iter()
            .filter_map(|a| a.get("Name").and_then(Value::as_str).map(str::to_owned))
            .collect();
        n.properties.push(("Aggregates".into(), names.join(", ")));
    }
    if let Some(sort) = o.get("Sort Description").and_then(Value::as_array) {
        let cols: Vec<String> = sort
            .iter()
            .map(|s| {
                let c = s.get("Column").and_then(Value::as_str).unwrap_or("?");
                if s.get("Ascending").and_then(Value::as_bool) == Some(false) {
                    format!("{c} DESC")
                } else {
                    c.to_owned()
                }
            })
            .collect();
        n.properties.push(("Sort".into(), cols.join(", ")));
    }
    if let Some(outputs) = o
        .get("Expression")
        .and_then(|e| e.get("Outputs"))
        .and_then(Value::as_array)
    {
        let names: Vec<String> = outputs
            .iter()
            .filter_map(|x| x.get("Name").and_then(Value::as_str).map(str::to_owned))
            .collect();
        n.properties.push(("Outputs".into(), names.join(", ")));
    }
    if let Some(f) = o
        .get("Prewhere info")
        .and_then(|p| p.get("Prewhere filter"))
        .and_then(|p| p.get("Prewhere filter column"))
        .and_then(Value::as_str)
    {
        n.properties.push(("Prewhere".into(), f.to_owned()));
    }
    if n.operation.starts_with("ReadFrom") {
        n.object = n.detail.clone();
        let granules = count(o.get("Granules"));
        if let Some(g) = granules {
            n.rows = g * GRANULE_ROWS;
        }
        let mut filtered = false;
        for idx in o
            .get("Indexes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let ty = idx.get("Type").and_then(Value::as_str).unwrap_or("index");
            let (ip, sp) = (
                count(idx.get("Initial Parts")),
                count(idx.get("Selected Parts")),
            );
            let (ig, sg) = (
                count(idx.get("Initial Granules")),
                count(idx.get("Selected Granules")),
            );
            let cond = idx.get("Condition").and_then(Value::as_str).unwrap_or("");
            if sg < ig || sp < ip {
                filtered = true;
            }
            let name = idx
                .get("Name")
                .and_then(Value::as_str)
                .map(|x| format!("{ty} {x}"))
                .unwrap_or_else(|| ty.to_owned());
            n.properties.push((
                format!("Index {name}"),
                format!(
                    "{}/{} parts, {}/{} granules{}",
                    sp.unwrap_or(0),
                    ip.unwrap_or(0),
                    sg.unwrap_or(0),
                    ig.unwrap_or(0),
                    if cond.is_empty() || cond == "true" {
                        String::new()
                    } else {
                        format!(", {cond}")
                    }
                ),
            ));
        }
        // No index skipped anything: every granule is read.
        common_warnings(&mut n, !filtered);
    }
    n.children = o
        .get("Plans")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(ch_node).collect())
        .unwrap_or_default();
    if n.operation == "Sorting" && n.children.iter().any(|c| c.rows >= 1_000_000) {
        n.warnings.push(
            "sorts more than a million rows; an ORDER BY matching the sorting key avoids it".into(),
        );
    }
    n
}

// -------------------------------------------------------------- Snowflake

/// `EXPLAIN USING JSON` output: `{"GlobalStats": {…}, "Operations":
/// [[{"id", "parent", "operation", "objects", "expressions",
/// "partitionsAssigned", "partitionsTotal", "bytesAssigned"}, …]]}`.
pub fn snowflake_json(raw: &str) -> Result<ExecutionPlan> {
    let v = parse_json(raw)?;
    let ops: Vec<&Value> = v
        .get("Operations")
        .and_then(Value::as_array)
        .ok_or_else(|| err("no \"Operations\" key"))?
        .iter()
        .flat_map(|step| match step {
            Value::Array(a) => a.iter().collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect();
    if ops.is_empty() {
        return Err(err("empty plan"));
    }
    fn node(op: &Value) -> PlanNode {
        let o = op.as_object().cloned().unwrap_or_default();
        let mut n = PlanNode::new(
            o.get("operation")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned(),
        );
        n.object = o
            .get("objects")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(text).collect::<Vec<_>>().join(", "))
            .filter(|s| !s.is_empty());
        n.detail = o
            .get("expressions")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(text).collect::<Vec<_>>().join(", "))
            .filter(|s| !s.is_empty());
        n.properties = scalars(&o, &["id", "parent", "operation", "objects", "expressions"]);
        let (assigned, total) = (
            count(o.get("partitionsAssigned")),
            count(o.get("partitionsTotal")),
        );
        if let (Some(a), Some(t)) = (assigned, total)
            && a == t
            && t > 10
        {
            n.warnings.push(format!(
                "reads all {t} micro-partitions: no pruning; a filter on the clustering key may help"
            ));
        }
        n
    }
    let id = |op: &Value| count(op.get("id"));
    let parent = |op: &Value| count(op.get("parent"));
    fn build(
        i: usize,
        ops: &[&Value],
        id: &dyn Fn(&Value) -> Option<u64>,
        parent: &dyn Fn(&Value) -> Option<u64>,
        depth: usize,
    ) -> PlanNode {
        let mut n = node(ops[i]);
        if depth < 256 {
            let me = id(ops[i]);
            n.children = (0..ops.len())
                .filter(|&j| j != i && me.is_some() && parent(ops[j]) == me)
                .map(|j| build(j, ops, id, parent, depth + 1))
                .collect();
        }
        n
    }
    let roots: Vec<usize> = (0..ops.len())
        .filter(|&i| parent(ops[i]).is_none())
        .collect();
    let mut trees: Vec<PlanNode> = roots
        .iter()
        .map(|&i| build(i, &ops, &id, &parent, 0))
        .collect();
    let mut root = match trees.len() {
        0 => build(0, &ops, &id, &parent, 0),
        1 => trees.remove(0),
        _ => PlanNode {
            operation: "Query".into(),
            children: trees,
            ..Default::default()
        },
    };
    if let Some(g) = v.get("GlobalStats").and_then(Value::as_object) {
        root.properties
            .extend(g.iter().map(|(k, v)| (format!("global {k}"), text(v))));
    }
    Ok(ExecutionPlan {
        root,
        raw: pretty(&v, raw),
        ..Default::default()
    })
}

// --------------------------------------------------------------- BigQuery

/// A BigQuery job resource (`jobs.insert` / `jobs.get`): the stages of
/// `statistics.query.queryPlan` linked by `inputStages`, or, for a dry run,
/// the bytes the query would process and the tables it reads.
pub fn bigquery_job(raw: &str) -> Result<ExecutionPlan> {
    let v = parse_json(raw)?;
    let stats = v
        .get("statistics")
        .ok_or_else(|| err("no \"statistics\""))?;
    let q = stats.get("query");
    let bytes = count(
        stats
            .get("totalBytesProcessed")
            .or_else(|| q.and_then(|q| q.get("totalBytesProcessed"))),
    );
    let stages = q
        .and_then(|q| q.get("queryPlan"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let dry = v
        .pointer("/configuration/dryRun")
        .and_then(Value::as_bool)
        .unwrap_or(stages.is_empty());
    let mut root = if stages.is_empty() {
        let mut root = PlanNode::new(if dry { "Query (dry run)" } else { "Query" });
        root.children = q
            .and_then(|q| q.get("referencedTables"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|t| {
                let name = ["projectId", "datasetId", "tableId"]
                    .iter()
                    .filter_map(|k| t.get(*k).and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(".");
                let mut n = PlanNode::new("Table read");
                n.object = Some(name);
                n
            })
            .collect();
        root
    } else {
        bq_tree(&stages)?
    };
    if let Some(b) = bytes {
        root.properties
            .push(("bytes processed".into(), b.to_string()));
        if b >= 1 << 40 {
            root.warnings.push(format!(
                "processes {:.1} TiB: partition or cluster filters would reduce the cost",
                b as f64 / (1u64 << 40) as f64
            ));
        }
    }
    if let Some(t) = q
        .and_then(|q| q.get("statementType"))
        .and_then(Value::as_str)
    {
        root.properties
            .push(("statement type".into(), t.to_owned()));
    }
    for (key, label) in [
        ("totalSlotMs", "slot ms"),
        ("totalBytesBilled", "bytes billed"),
        ("cacheHit", "cache hit"),
    ] {
        if let Some(x) = q.and_then(|q| q.get(key)).or_else(|| stats.get(key)) {
            root.properties.push((label.into(), text(x)));
        }
    }
    let execution_ms = match (num(stats.get("startTime")), num(stats.get("endTime"))) {
        (Some(a), Some(b)) if b >= a && !dry => Some(b - a),
        _ => None,
    };
    Ok(ExecutionPlan {
        analyzed: !dry && !stages.is_empty(),
        root,
        raw: pretty(&v, raw),
        planning_ms: None,
        execution_ms,
        statement: String::new(),
    })
}

fn bq_stage(s: &Value) -> PlanNode {
    let o = s.as_object().cloned().unwrap_or_default();
    let mut n = PlanNode::new(
        o.get("name")
            .and_then(Value::as_str)
            .unwrap_or("stage")
            .to_owned(),
    );
    let steps: Vec<(String, Vec<String>)> = o
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|st| {
            (
                st.get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_owned(),
                st.get("substeps")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(text).collect())
                    .unwrap_or_default(),
            )
        })
        .collect();
    n.detail = Some(
        steps
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    )
    .filter(|d| !d.is_empty());
    n.object = steps
        .iter()
        .flat_map(|(_, subs)| subs.iter())
        .find_map(|s| s.strip_prefix("FROM ").map(str::to_owned));
    for (k, subs) in &steps {
        n.properties.push((k.clone(), subs.join("; ")));
    }
    let records_read = count(o.get("recordsRead"));
    let records_written = count(o.get("recordsWritten"));
    n.rows = records_written.unwrap_or(0);
    n.actual_rows = records_written;
    if let (Some(a), Some(b)) = (num(o.get("startMs")), num(o.get("endMs")))
        && b >= a
    {
        n.actual_ms = Some(b - a);
    }
    n.properties.extend(scalars(
        &o,
        &[
            "name",
            "id",
            "steps",
            "inputStages",
            "recordsWritten",
            "startMs",
            "endMs",
        ],
    ));
    if let (Some(r), Some(w)) = (records_read, records_written)
        && r >= 1_000_000
        && w > 0
        && r / w.max(1) >= 1000
    {
        n.warnings.push(format!(
            "reads {r} records to produce {w}: a more selective filter or partitioning may help"
        ));
    }
    n
}

fn bq_tree(stages: &[Value]) -> Result<PlanNode> {
    let id = |s: &Value| s.get("id").map(text);
    let inputs = |s: &Value| -> Vec<String> {
        s.get("inputStages")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(text).collect())
            .unwrap_or_default()
    };
    let referenced: std::collections::HashSet<String> = stages.iter().flat_map(inputs).collect();
    fn build(
        s: &Value,
        stages: &[Value],
        id: &dyn Fn(&Value) -> Option<String>,
        inputs: &dyn Fn(&Value) -> Vec<String>,
        depth: usize,
    ) -> PlanNode {
        let mut n = bq_stage(s);
        if depth < 256 {
            n.children = inputs(s)
                .iter()
                .filter_map(|i| stages.iter().find(|x| id(x).as_deref() == Some(i.as_str())))
                .map(|c| build(c, stages, id, inputs, depth + 1))
                .collect();
        }
        n
    }
    let roots: Vec<&Value> = stages
        .iter()
        .filter(|s| id(s).is_none_or(|i| !referenced.contains(&i)))
        .collect();
    let mut trees: Vec<PlanNode> = roots
        .iter()
        .map(|s| build(s, stages, &id, &inputs, 0))
        .collect();
    Ok(match trees.len() {
        0 => return Err(err("query plan without a final stage")),
        1 => trees.remove(0),
        _ => PlanNode {
            operation: "Query".into(),
            children: trees,
            ..Default::default()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from ClickHouse 26.10 (`EXPLAIN json = 1, actions = 1,
    // indexes = 1, description = 1`), shortened.
    const CH: &str = r#"[{"Plan": {"Node Type": "Expression", "Node Id": "Expression_9",
        "Description": "Project names",
        "Expression": {"Inputs": [], "Actions": [], "Outputs": [{"Name": "v", "Type": "String"}, {"Name": "count()", "Type": "UInt64"}], "Positions": [2, 1]},
        "Plans": [{"Node Type": "Limit", "Description": "preliminary LIMIT", "Limit": 10, "Offset": 0,
          "Plans": [{"Node Type": "Sorting", "Description": "Sorting for ORDER BY",
            "Sort Description": [{"Column": "__table1.v", "Ascending": true, "With Fill": false}], "Limit": 10,
            "Plans": [{"Node Type": "Aggregating", "Keys": ["__table1.v"],
              "Aggregates": [{"Name": "count()", "Function": {"Name": "count"}, "Arguments": []}],
              "Plans": [{"Node Type": "ReadFromMergeTree", "Description": "tck.ev", "Read Type": "Default",
                "Parts": 2, "Granules": 2,
                "Prewhere info": {"Need filter": true, "Prewhere filter": {"Prewhere filter column": "greater(__table1.id, 100_UInt8)"}},
                "Indexes": [
                  {"Type": "Partition", "Condition": "true", "Initial Parts": 2, "Selected Parts": 2, "Initial Granules": 2, "Selected Granules": 2},
                  {"Type": "PrimaryKey", "Keys": ["id"], "Condition": "(id in [101, +Inf))", "Initial Parts": 2, "Selected Parts": 2, "Initial Granules": 2, "Selected Granules": 2}
                ]}]}]}]}]}}]"#;

    #[test]
    fn clickhouse_plan() {
        let p = clickhouse_json(CH).unwrap();
        let flat = p.root.flatten();
        let ops: Vec<&str> = flat.iter().map(|(_, n)| n.operation.as_str()).collect();
        assert_eq!(
            ops,
            [
                "Expression",
                "Limit",
                "Sorting",
                "Aggregating",
                "ReadFromMergeTree"
            ]
        );
        let read = flat[4].1;
        assert_eq!(read.object.as_deref(), Some("tck.ev"));
        assert_eq!(read.rows, 2 * GRANULE_ROWS);
        assert!(
            read.properties
                .iter()
                .any(|(k, v)| k == "Index PrimaryKey" && v.contains("2/2 granules"))
        );
        assert!(read.properties.iter().any(|(k, _)| k == "Prewhere"));
        assert!(
            read.warnings.iter().any(|w| w.contains("full scan")),
            "{:?}",
            read.warnings
        );
        assert!(
            flat[3]
                .1
                .properties
                .contains(&("Aggregates".into(), "count()".into()))
        );
        assert_eq!(p.root.detail.as_deref(), Some("Project names"));
    }

    #[test]
    fn snowflake_plan() {
        let raw = r#"{"GlobalStats":{"partitionsTotal":120,"partitionsAssigned":120,"bytesAssigned":1048576},
          "Operations":[[
            {"id":0,"operation":"Result","expressions":["COUNT(*)"]},
            {"id":1,"parent":0,"operation":"Aggregate","expressions":["aggExprs: [COUNT(*)]"]},
            {"id":2,"parent":1,"operation":"TableScan","objects":["DB.PUBLIC.ORDERS"],"expressions":["ID"],"partitionsAssigned":120,"partitionsTotal":120,"bytesAssigned":1048576}
          ]]}"#;
        let p = snowflake_json(raw).unwrap();
        assert_eq!(p.root.operation, "Result");
        let scan = &p.root.children[0].children[0];
        assert_eq!(scan.operation, "TableScan");
        assert_eq!(scan.object.as_deref(), Some("DB.PUBLIC.ORDERS"));
        assert!(scan.warnings[0].contains("no pruning"));
        assert!(
            p.root
                .properties
                .iter()
                .any(|(k, _)| k == "global bytesAssigned")
        );
    }

    #[test]
    fn bigquery_stages_and_dry_run() {
        let job = r#"{"configuration":{"query":{"query":"select corpus, count(*) from t group by 1"}},
          "statistics":{"startTime":"1763744827000","endTime":"1763744828500","totalBytesProcessed":"2097152",
            "query":{"statementType":"SELECT","totalSlotMs":"431","totalBytesProcessed":"2097152","queryPlan":[
              {"name":"S00: Input","id":"0","startMs":"1763744827100","endMs":"1763744827600","recordsRead":"164656","recordsWritten":"42",
               "steps":[{"kind":"READ","substeps":["$1:corpus","FROM bigquery-public-data.samples.shakespeare"]},{"kind":"AGGREGATE","substeps":["GROUP BY $30 := $1"]},{"kind":"WRITE","substeps":["$30","TO __stage00_output"]}]},
              {"name":"S01: Output","id":"1","inputStages":["0"],"recordsRead":"42","recordsWritten":"42",
               "steps":[{"kind":"READ","substeps":["FROM __stage00_output"]},{"kind":"WRITE","substeps":["TO __stage01_output"]}]}
            ]}}}"#;
        let p = bigquery_job(job).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.root.operation, "S01: Output");
        let input = &p.root.children[0];
        assert_eq!(input.operation, "S00: Input");
        assert_eq!(
            input.object.as_deref(),
            Some("bigquery-public-data.samples.shakespeare")
        );
        assert_eq!(input.actual_rows, Some(42));
        assert_eq!(input.actual_ms, Some(500.0));
        assert_eq!(p.execution_ms, Some(1500.0));
        assert!(
            p.root
                .properties
                .contains(&("bytes processed".into(), "2097152".into()))
        );

        let dry = r#"{"configuration":{"dryRun":true,"query":{"query":"select 1"}},
          "statistics":{"totalBytesProcessed":"65935918","query":{"statementType":"SELECT","totalBytesProcessed":"65935918",
          "referencedTables":[{"projectId":"bigquery-public-data","datasetId":"samples","tableId":"shakespeare"}]}}}"#;
        let p = bigquery_job(dry).unwrap();
        assert!(!p.analyzed);
        assert_eq!(p.root.operation, "Query (dry run)");
        assert_eq!(
            p.root.children[0].object.as_deref(),
            Some("bigquery-public-data.samples.shakespeare")
        );
    }
}
