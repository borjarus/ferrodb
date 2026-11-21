//! Couchbase: the operator tree of `EXPLAIN` (`{"plan": {...}, "text":
//! "..."}`) and of a profiled run (`profile.executionTimings`, the same
//! tree with `#stats`).
//!
//! Operators run as pipelines: the children of a `Sequence` feed each
//! other in order, `Parallel` runs its `~child` in parallel streams, joins
//! and nests read their inner side from `~child`, set operations combine
//! `children`. Here a pipeline becomes a chain of nodes, each one consuming
//! the one below it. `PrimaryScan` (every document of the keyspace) gets a
//! warning; the cost-based optimizer's estimates fill cost and rows when
//! the server reports them.

use ferrodb_api::{ExecutionPlan, PlanNode};
use serde_json::Value;

use super::{Result, count, err, num, text};

/// Keys read into the node itself (not listed as properties).
const CONSUMED: &[&str] = &[
    "#operator",
    "~children",
    "~child",
    "children",
    "#stats",
    "#time",
    "optimizer_estimates",
    "index",
    "keyspace",
    "bucket",
    "scope",
    "namespace",
    "condition",
    "as",
];

/// A Go duration (`1.5ms`, `250µs`, `2m3.5s`) in milliseconds.
pub(super) fn go_duration_ms(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0.0;
    let mut rest = s;
    while !rest.is_empty() {
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let (n, tail) = rest.split_at(end);
        let n: f64 = n.parse().ok()?;
        let unit_end = tail
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(tail.len());
        let (unit, next) = tail.split_at(unit_end);
        total += n * match unit {
            "h" => 3_600_000.0,
            "m" => 60_000.0,
            "s" => 1000.0,
            "ms" => 1.0,
            "µs" | "us" | "μs" => 0.001,
            "ns" => 0.000_001,
            _ => return None,
        };
        rest = next;
    }
    Some(total)
}

fn object_of(o: &Value) -> Option<String> {
    let ks = o.get("keyspace").and_then(Value::as_str)?;
    Some(
        match (
            o.get("bucket").and_then(Value::as_str),
            o.get("scope").and_then(Value::as_str),
        ) {
            (Some(b), Some(s)) => format!("{b}.{s}.{ks}"),
            _ => ks.to_owned(),
        },
    )
}

/// One operator (with `input` feeding it).
fn node(o: &Value, input: Option<PlanNode>) -> PlanNode {
    let op = o
        .get("#operator")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned();
    match op.as_str() {
        "Sequence" => {
            let mut cur = input;
            for c in o
                .get("~children")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                cur = Some(node(c, cur));
            }
            return cur.unwrap_or_else(|| PlanNode::new("Sequence"));
        }
        "Parallel" => {
            if let Some(c) = o.get("~child") {
                let mut n = node(c, input);
                n.properties.push(("parallel".into(), "true".into()));
                return n;
            }
        }
        _ => {}
    }
    let mut n = PlanNode::new(op.clone());
    n.object = object_of(o);
    let index = o.get("index").and_then(Value::as_str);
    let alias = o.get("as").and_then(Value::as_str);
    n.detail = match (index, o.get("condition")) {
        (Some(i), _) => Some(match &n.object {
            Some(obj) => format!("{i} on {obj}"),
            None => i.to_owned(),
        }),
        (None, Some(c)) => Some(text(c)),
        _ => n.object.clone(),
    };
    if let Some(a) = alias {
        n.properties.push(("as".into(), a.to_owned()));
    }
    if let Some(o) = o.as_object() {
        for (k, v) in o {
            if !CONSUMED.contains(&k.as_str()) {
                n.properties.push((k.clone(), text(v)));
            }
        }
    }
    if let Some(e) = o.get("optimizer_estimates") {
        n.cost = num(e.get("cost")).unwrap_or(0.0);
        n.rows = num(e.get("cardinality")).map_or(0, |r| r.max(0.0).round() as u64);
    }
    if let Some(st) = o.get("#stats") {
        n.actual_rows = count(st.get("#itemsOut"));
        let ms = ["execTime", "servTime"]
            .iter()
            .filter_map(|k| st.get(*k).and_then(Value::as_str).and_then(go_duration_ms))
            .sum::<f64>();
        n.actual_ms = Some(ms);
        n.loops = Some(1);
        if let Some(i) = st.get("#itemsIn") {
            n.properties.push(("#itemsIn".into(), text(i)));
        }
    }
    let index_lc = op.to_ascii_lowercase();
    if index_lc.starts_with("primaryscan") {
        n.warnings.push(
            "primary index scan: reads every document key of the keyspace; a secondary index \
             matching the filter may help"
                .into(),
        );
    }
    // Inner sides and combined inputs.
    if let Some(input) = input {
        n.children.push(input);
    }
    if let Some(c) = o.get("~child")
        && op != "Parallel"
    {
        n.children.push(node(c, None));
    }
    for c in o
        .get("children")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        n.children.push(node(c, None));
    }
    n
}

/// Times of a profile are per operator: add the inputs' (the shared model
/// counts a node with its inputs).
fn inclusive(n: &mut PlanNode) -> f64 {
    let own = n.actual_ms.unwrap_or(0.0);
    let inputs: f64 = n.children.iter_mut().map(inclusive).sum();
    if n.actual_ms.is_some() {
        n.actual_ms = Some(own + inputs);
    }
    own + inputs
}

/// The plan of an `EXPLAIN` row or of a profiled run.
pub fn couchbase_json(v: &Value) -> Result<ExecutionPlan> {
    let (tree, analyzed) = if let Some(p) = v.get("plan") {
        (p, false)
    } else if let Some(t) = v
        .get("profile")
        .and_then(|p| p.get("executionTimings"))
        .or_else(|| v.get("executionTimings"))
    {
        (t, true)
    } else if v.get("#operator").is_some() {
        (v, false)
    } else {
        return Err(err("no plan or executionTimings"));
    };
    let mut root = node(tree, None);
    let execution_ms = if analyzed {
        Some(inclusive(&mut root))
    } else {
        None
    };
    if analyzed && let Some(r) = count(v.get("rows")) {
        root.actual_rows = root.actual_rows.or(Some(r));
    }
    Ok(ExecutionPlan {
        root,
        statement: v
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        raw: serde_json::to_string_pretty(v).unwrap_or_default(),
        analyzed,
        planning_ms: None,
        execution_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `EXPLAIN` recorded from Couchbase Server 8.0 (community): covered
    /// index scan, grouping, ordering.
    const EXPLAIN: &str = r##"{"plan":{"#operator":"Sequence","~children":[{"#operator":"Sequence","~children":[{"#operator":"IndexScan3","as":"d","bucket":"tck","covers":["cover ((`d`.`z`))","cover ((meta(`d`).`id`))"],"filter_covers":{"cover (((`d`.`z`) is not missing))":true},"index":"ix_z","index_id":"4487bdc7c681ad8c","index_projection":{"entry_keys":[0]},"keyspace":"docs","namespace":"default","scope":"app","spans":[{"exact":true,"range":[{"inclusion":0,"index_key":"`z`","low":"1"}]}],"using":"gsi"},{"#operator":"Parallel","~child":{"#operator":"Sequence","~children":[{"#operator":"Filter","condition":"(1 < cover ((`d`.`z`)))"},{"#operator":"InitialGroup","aggregates":["count(*)"],"flags":4,"group_keys":["cover ((`d`.`z`))"]}]}},{"#operator":"IntermediateGroup","aggregates":["count(*)"],"flags":4,"group_keys":["cover ((`d`.`z`))"]},{"#operator":"FinalGroup","aggregates":["count(*)"],"flags":4,"group_keys":["cover ((`d`.`z`))"]},{"#operator":"Parallel","~child":{"#operator":"Sequence","~children":[{"#operator":"InitialProject","preserve_order":true,"result_terms":[{"expr":"cover ((`d`.`z`))"},{"as":"n","expr":"count(*)"}]}]}}]},{"#operator":"Order","flags":6,"limit":"10","sort_terms":[{"desc":"\"desc\"","expr":"`n`"}]},{"#operator":"Limit","expr":"10"}]},"text":"SELECT d.z, COUNT(*) AS n FROM tck.app.docs d WHERE z > 1 GROUP BY d.z ORDER BY n DESC LIMIT 10"}"##;

    #[test]
    fn explain_becomes_a_chain() {
        let v: Value = serde_json::from_str(EXPLAIN).unwrap();
        let p = couchbase_json(&v).unwrap();
        let ops: Vec<String> = p
            .root
            .flatten()
            .iter()
            .map(|(d, n)| format!("{d}:{}", n.operation))
            .collect();
        assert_eq!(
            ops,
            vec![
                "0:Limit",
                "1:Order",
                "2:InitialProject",
                "3:FinalGroup",
                "4:IntermediateGroup",
                "5:InitialGroup",
                "6:Filter",
                "7:IndexScan3",
            ]
        );
        let scan = p.root.flatten().last().unwrap().1.clone();
        assert_eq!(scan.object.as_deref(), Some("tck.app.docs"));
        assert_eq!(scan.detail.as_deref(), Some("ix_z on tck.app.docs"));
        assert!(scan.property("spans").is_some());
        assert!(p.warnings().is_empty());
        assert!(p.statement.starts_with("SELECT d.z"));
        assert!(!p.analyzed);
    }

    #[test]
    fn primary_scans_and_profiles() {
        let v: Value = serde_json::from_str(
            r##"{"rows": 3, "profile": {"executionTimings": {"#operator":"Sequence","#stats":{"#phaseSwitches":1,"execTime":"1.5µs"},"~children":[
                {"#operator":"PrimaryScan3","#stats":{"#itemsOut":3,"execTime":"200µs","servTime":"1.3ms"},"index":"#primary","keyspace":"docs","bucket":"tck","scope":"app"},
                {"#operator":"Fetch","#stats":{"#itemsIn":3,"#itemsOut":3,"execTime":"1ms","servTime":"2ms"},"keyspace":"docs","bucket":"tck","scope":"app"},
                {"#operator":"NestedLoopJoin","on_clause":"a = b","~child":{"#operator":"IndexScan3","index":"ix","keyspace":"other"}}
            ]}}}"##,
        )
        .unwrap();
        let p = couchbase_json(&v).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.root.operation, "NestedLoopJoin");
        assert_eq!(p.root.children.len(), 2);
        let fetch = &p.root.children[0];
        assert_eq!(fetch.operation, "Fetch");
        assert_eq!(fetch.actual_rows, Some(3));
        // 1 ms + 2 ms of the fetch plus 0.2 + 1.3 ms of the scan below it.
        assert!((fetch.actual_ms.unwrap() - 4.5).abs() < 1e-9);
        assert_eq!(p.warnings().len(), 1);
        assert!(p.warnings()[0].1.contains("primary index"));
        assert_eq!(go_duration_ms("1m2.5s"), Some(62_500.0));
        assert!(couchbase_json(&serde_json::json!({"x": 1})).is_err());
    }
}
