//! Text plans → the shared plan model:
//!
//! - CockroachDB `EXPLAIN (VERBOSE)` / `EXPLAIN ANALYZE (VERBOSE)`: a tree
//!   of `• operator` lines with `key: value` properties (CockroachDB has no
//!   JSON plan format);
//! - PostgreSQL text format, as Amazon Redshift prints it (`XN Seq Scan on t
//!   (cost=0.00..2.02 rows=202 width=17)` with `->` children), with
//!   Redshift's data movement (`DS_BCAST_INNER`, `DS_DIST_BOTH`…) flagged.

use ferrodb_api::{ExecutionPlan, PlanNode};

use crate::plan::{PlanError, SCAN_WARNING_ROWS, common_warnings};

type Result<T> = std::result::Result<T, PlanError>;

/// `1,000 (100% of the table; …)` → 1000.
fn leading_count(v: &str) -> Option<u64> {
    let digits: String = v
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// `1.2ms`, `350µs`, `2s` → milliseconds.
fn duration_ms(v: &str) -> Option<f64> {
    let v = v.trim();
    let split = v.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (n, unit) = v.split_at(split);
    let n: f64 = n.parse().ok()?;
    Some(match unit.trim() {
        "ns" => n / 1e6,
        "µs" | "us" => n / 1e3,
        "ms" => n,
        "s" => n * 1e3,
        "m" => n * 60e3,
        _ => return None,
    })
}

fn attach(stack: &mut Vec<(usize, PlanNode)>, roots: &mut Vec<PlanNode>, upto: usize) {
    while stack.last().is_some_and(|(d, _)| *d >= upto) {
        let (_, n) = stack.pop().expect("checked");
        match stack.last_mut() {
            Some((_, p)) => p.children.push(n),
            None => roots.push(n),
        }
    }
}

fn finish_crdb(n: &mut PlanNode) {
    for c in &mut n.children {
        finish_crdb(c);
    }
    let full = n
        .properties
        .iter()
        .any(|(k, v)| k == "spans" && v.starts_with("FULL SCAN"));
    common_warnings(n, full);
    for (k, v) in &n.properties {
        if (k.contains("disk usage") || k.contains("disk spill")) && !v.starts_with('0') {
            n.warnings.push(format!("spilled to disk ({k}: {v})"));
        }
    }
    if (n.operation == "lookup join" || n.operation.starts_with("index join"))
        && let Some(rows) = n
            .actual_rows
            .or(Some(n.rows))
            .filter(|r| *r >= SCAN_WARNING_ROWS * 10)
    {
        n.warnings
            .push(format!("{} looks up {rows} rows one by one", n.operation));
    }
}

/// CockroachDB `EXPLAIN (VERBOSE)` / `EXPLAIN ANALYZE (VERBOSE)` output
/// (the `info` rows joined with newlines).
pub fn cockroach_text(raw: &str) -> Result<ExecutionPlan> {
    let mut stack: Vec<(usize, PlanNode)> = Vec::new();
    let mut roots: Vec<PlanNode> = Vec::new();
    let mut top: Vec<(String, String)> = Vec::new();
    let mut recommendations: Vec<String> = Vec::new();
    let mut in_recs = false;
    for line in raw.lines() {
        if let Some(pos) = line.find('•') {
            in_recs = false;
            let depth = line[..pos].chars().count();
            attach(&mut stack, &mut roots, depth);
            let op = line[pos + '•'.len_utf8()..].trim().to_owned();
            stack.push((depth, PlanNode::new(op)));
            continue;
        }
        let body = line
            .trim_start_matches(|c: char| c.is_whitespace() || "│├└─".contains(c))
            .trim_end();
        if body.is_empty() {
            continue;
        }
        if body.starts_with("index recommendations") {
            in_recs = true;
            continue;
        }
        if in_recs {
            if let Some(sql) = body.strip_prefix("SQL command:") {
                recommendations.push(format!("index recommendation: {}", sql.trim()));
            }
            continue;
        }
        let Some((k, v)) = body.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim().to_owned(), v.trim().to_owned());
        match stack.last_mut() {
            None => top.push((k, v)),
            Some((_, n)) => match k.as_str() {
                "estimated row count" => n.rows = leading_count(&v).unwrap_or(0),
                "actual row count" => {
                    n.actual_rows = leading_count(&v);
                    n.loops = Some(1);
                }
                "execution time" => n.actual_ms = duration_ms(&v),
                "table" => {
                    let (t, i) = v.split_once('@').unwrap_or((&v, ""));
                    n.detail = Some(if i.is_empty() {
                        format!("on {t}")
                    } else {
                        format!("{i} on {t}")
                    });
                    n.object = Some(v.clone());
                }
                _ => n.properties.push((k, v)),
            },
        }
    }
    attach(&mut stack, &mut roots, 0);
    let mut root = match roots.len() {
        0 => return Err(PlanError("no plan nodes".into())),
        1 => roots.remove(0),
        _ => PlanNode {
            operation: "plan".into(),
            children: roots,
            ..Default::default()
        },
    };
    finish_crdb(&mut root);
    root.warnings.extend(recommendations);
    let get = |k: &str| top.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str());
    let execution_ms = get("execution time").and_then(duration_ms);
    let planning_ms = get("planning time").and_then(duration_ms);
    let analyzed = root.actual_rows.is_some() || execution_ms.is_some();
    if root.actual_ms.is_none() {
        root.actual_ms = execution_ms;
    }
    for (k, v) in top {
        if !matches!(k.as_str(), "execution time" | "planning time") {
            root.properties.push((k, v));
        }
    }
    Ok(ExecutionPlan {
        root,
        statement: String::new(),
        raw: raw.to_owned(),
        analyzed,
        planning_ms,
        execution_ms,
    })
}

/// `XN Hash Join DS_BCAST_INNER  (cost=112.50..129.07 rows=576 width=17)`.
fn pg_text_line(body: &str) -> PlanNode {
    let (head, stats) = match body.find("  (cost=").or_else(|| body.find(" (cost=")) {
        Some(i) => (body[..i].trim(), &body[i..]),
        None => (body.trim(), ""),
    };
    let (op, detail) = match head.find(" on ") {
        Some(i) => (head[..i].to_owned(), Some(head[i + 1..].to_owned())),
        None => (head.to_owned(), None),
    };
    let mut n = PlanNode::new(op);
    n.object = detail
        .as_deref()
        .and_then(|d| d.strip_prefix("on "))
        .and_then(|d| d.split_whitespace().next())
        .map(str::to_owned);
    n.detail = detail;
    for group in stats.split('(').skip(1) {
        let g = group.trim().trim_end_matches(')');
        let actual = g.starts_with("actual");
        for kv in g.split_whitespace() {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            match (actual, k) {
                (false, "cost") => {
                    n.cost = v
                        .split("..")
                        .last()
                        .and_then(|c| c.parse().ok())
                        .unwrap_or(0.0)
                }
                (false, "rows") => n.rows = v.parse().unwrap_or(0),
                (false, "width") => n.properties.push(("Plan Width".into(), v.to_owned())),
                (true, "time") => n.actual_ms = v.split("..").last().and_then(|t| t.parse().ok()),
                (true, "rows") => n.actual_rows = v.parse().ok(),
                (true, "loops") => n.loops = v.parse().ok(),
                _ => {}
            }
        }
    }
    let up = n.operation.to_ascii_uppercase();
    common_warnings(&mut n, up.ends_with("SEQ SCAN"));
    for (code, what) in [
        ("DS_BCAST_INNER", "broadcasts the inner table to every node"),
        ("DS_DIST_BOTH", "redistributes both tables"),
        (
            "DS_DIST_ALL_INNER",
            "sends the inner table to a single slice",
        ),
    ] {
        if up.contains(code) {
            n.warnings.push(format!(
                "{code}: the join {what}; distribution keys on the join columns avoid it"
            ));
        }
    }
    if up.contains("NESTED LOOP") {
        n.warnings
            .push("nested loop join: check the join condition".into());
    }
    n
}

/// PostgreSQL text-format `EXPLAIN` (Amazon Redshift).
pub fn postgres_text(raw: &str) -> Result<ExecutionPlan> {
    let mut stack: Vec<(usize, PlanNode)> = Vec::new();
    let mut roots: Vec<PlanNode> = Vec::new();
    let mut top: Vec<(String, String)> = Vec::new();
    let mut footer = Vec::new();
    for line in raw.lines() {
        if line.trim_start().starts_with("---") {
            let t = line.trim().trim_matches('-').trim();
            if !t.is_empty() {
                footer.push(format!("{t}: run ANALYZE"));
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let node = if let Some(b) = trimmed.strip_prefix("->") {
            Some((indent + 2, b.trim()))
        } else if stack.is_empty() && roots.is_empty() && trimmed.contains("(cost=") {
            Some((0, trimmed))
        } else {
            None
        };
        match node {
            Some((depth, body)) => {
                attach(&mut stack, &mut roots, depth);
                stack.push((depth, pg_text_line(body)));
            }
            None => {
                let (k, v) = trimmed.split_once(": ").unwrap_or((trimmed, ""));
                match stack.last_mut() {
                    Some((_, n)) => n
                        .properties
                        .push((k.trim().to_owned(), v.trim().to_owned())),
                    None => top.push((k.trim().to_owned(), v.trim().to_owned())),
                }
            }
        }
    }
    attach(&mut stack, &mut roots, 0);
    let mut root = match roots.len() {
        0 => return Err(PlanError("no plan lines".into())),
        1 => roots.remove(0),
        _ => PlanNode {
            operation: "Query".into(),
            children: roots,
            ..Default::default()
        },
    };
    root.properties.extend(top);
    root.warnings.extend(footer);
    let ms = |k: &str| {
        root.properties
            .iter()
            .find(|(x, _)| x == k)
            .and_then(|(_, v)| v.trim_end_matches(" ms").parse().ok())
    };
    let (planning_ms, execution_ms) = (ms("Planning Time"), ms("Execution Time"));
    Ok(ExecutionPlan {
        analyzed: root.actual_rows.is_some(),
        root,
        statement: String::new(),
        raw: raw.to_owned(),
        planning_ms,
        execution_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded from CockroachDB 24.x `EXPLAIN ANALYZE (VERBOSE)`.
    const CRDB: &str = "planning time: 1ms
execution time: 6ms
distribution: local
vectorized: true
rows decoded from KV: 20,000 (1.6 MiB, 1 gRPC calls)
maximum memory usage: 2.1 MiB

• sort
│ columns: (id, v)
│ nodes: n1
│ actual row count: 20,000
│ execution time: 4ms
│ estimated max sql temp disk usage: 0 B
│ estimated row count: 20,000
│ order: +v
│
└── • scan
      columns: (id, v)
      nodes: n1
      actual row count: 20,000
      execution time: 2ms
      KV rows decoded: 20,000
      estimated row count: 20,000 (100% of the table; stats collected 3 minutes ago)
      table: t@t_pkey
      spans: FULL SCAN

index recommendations: 1
1. type: index creation
   SQL command: CREATE INDEX ON t (v) STORING (id);";

    #[test]
    fn cockroach_tree() {
        let p = cockroach_text(CRDB).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.planning_ms, Some(1.0));
        assert_eq!(p.execution_ms, Some(6.0));
        let sort = &p.root;
        assert_eq!(sort.operation, "sort");
        assert_eq!(sort.actual_rows, Some(20_000));
        assert_eq!(sort.actual_ms, Some(4.0));
        assert!(
            sort.properties
                .iter()
                .any(|(k, v)| k == "order" && v == "+v")
        );
        assert!(sort.properties.iter().any(|(k, _)| k == "distribution"));
        assert!(
            sort.warnings
                .iter()
                .any(|w| w.contains("CREATE INDEX ON t (v)"))
        );
        let scan = &sort.children[0];
        assert_eq!(scan.operation, "scan");
        assert_eq!(scan.detail.as_deref(), Some("t_pkey on t"));
        assert_eq!(scan.rows, 20_000);
        assert!(scan.warnings.iter().any(|w| w.contains("full scan")));
        assert!(cockroach_text("").is_err());
    }

    /// Recorded Redshift `EXPLAIN`.
    const REDSHIFT: &str = "XN HashAggregate  (cost=131.97..133.41 rows=576 width=17)
  ->  XN Hash Join DS_BCAST_INNER  (cost=112.50..129.07 rows=576 width=17)
        Hash Cond: (\"outer\".venueid = \"inner\".venueid)
        ->  XN Seq Scan on venue  (cost=0.00..2.02 rows=202 width=17)
        ->  XN Hash  (cost=87.98..87.98 rows=4798 width=8)
              ->  XN Seq Scan on event  (cost=0.00..47.98 rows=4798 width=8)
----- Tables missing statistics: venue -----";

    #[test]
    fn redshift_text() {
        let p = postgres_text(REDSHIFT).unwrap();
        assert!(!p.analyzed);
        let agg = &p.root;
        assert_eq!(agg.operation, "XN HashAggregate");
        assert_eq!(agg.cost, 133.41);
        let join = &agg.children[0];
        assert!(
            join.warnings
                .iter()
                .any(|w| w.starts_with("DS_BCAST_INNER"))
        );
        assert!(join.properties.iter().any(|(k, _)| k == "Hash Cond"));
        assert_eq!(join.children.len(), 2);
        let event = &join.children[1].children[0];
        assert_eq!(event.object.as_deref(), Some("event"));
        assert_eq!(event.rows, 4798);
        assert!(event.warnings.iter().any(|w| w.contains("full scan")));
        assert!(
            agg.warnings
                .iter()
                .any(|w| w.contains("missing statistics: venue"))
        );
    }

    /// Recorded from Greenplum 7.0 `EXPLAIN (FORMAT JSON)` (trimmed): the
    /// PostgreSQL parser shows Motion nodes with their slices.
    #[test]
    fn greenplum_motions() {
        let raw = r#"[{"Plan": {"Node Type": "Gather Motion", "Senders": 2, "Receivers": 1, "Slice": 1,
          "Segments": 2, "Total Cost": 862.52, "Plan Rows": 1, "Plans": [
          {"Node Type": "Hash Join", "Join Type": "Inner", "Total Cost": 862.52, "Plan Rows": 1,
           "Hash Cond": "(gpf.v = gpd.code)", "Plans": [
            {"Node Type": "Seq Scan", "Relation Name": "gpf", "Alias": "gpf", "Total Cost": 431.05, "Plan Rows": 2500},
            {"Node Type": "Hash", "Total Cost": 431.0, "Plan Rows": 1, "Plans": [
              {"Node Type": "Broadcast Motion", "Senders": 2, "Receivers": 2, "Slice": 2,
               "Total Cost": 431.0, "Plan Rows": 5000, "Plans": [
                {"Node Type": "Seq Scan", "Relation Name": "gpd", "Alias": "gpd", "Total Cost": 431.0, "Plan Rows": 1}]}]}]}]}}]"#;
        let p = crate::plan::postgres_json(raw).unwrap();
        assert_eq!(p.root.operation, "Gather Motion");
        assert!(
            p.root
                .properties
                .iter()
                .any(|(k, v)| k == "Senders" && v == "2")
        );
        let motion = &p.root.children[0].children[1].children[0];
        assert_eq!(motion.operation, "Broadcast Motion");
        assert!(
            motion.warnings.iter().any(|w| w.contains("every segment")),
            "{:?}",
            motion.warnings
        );
        assert!(
            motion
                .properties
                .iter()
                .any(|(k, v)| k == "Slice" && v == "2")
        );
    }
}
