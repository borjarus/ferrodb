//! Execution plans of the JDBC databases (plan, phase 8):
//!
//! | Database | Source | Model |
//! |---|---|---|
//! | Oracle | `EXPLAIN PLAN SET STATEMENT_ID = … FOR …` + `PLAN_TABLE` rows | structured ([`oracle_plan_table`]) |
//! | H2 | `EXPLAIN` / `EXPLAIN ANALYZE` | text ([`text_plan`]) |
//! | HyperSQL | `EXPLAIN PLAN FOR` | text |
//! | Derby | runtime statistics of a measured run | text |
//! | Db2 | `EXPLAIN PLAN FOR` + `EXPLAIN_OPERATOR` | text |
//! | Vertica, Hive | `EXPLAIN` | text |
//! | Exasol | profiling of a measured run | text |
//! | Sybase ASE | `set showplan on` / `set noexec on` messages | text |
//!
//! Text plans become a tree by indentation, so the plan view can fold
//! them; lines that read a whole table get the usual full-scan warning.

use ferrodb_api::{ExecutionPlan, PlanNode};

use crate::plan::{PlanError, SCAN_WARNING_ROWS};

type Result<T> = std::result::Result<T, PlanError>;

/// One row of Oracle's `PLAN_TABLE` (the columns the explain request reads).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OracleRow {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub depth: i64,
    pub operation: String,
    pub options: Option<String>,
    pub object_owner: Option<String>,
    pub object_name: Option<String>,
    pub object_alias: Option<String>,
    pub object_type: Option<String>,
    pub optimizer: Option<String>,
    pub cost: Option<f64>,
    pub cardinality: Option<u64>,
    pub bytes: Option<u64>,
    pub cpu_cost: Option<u64>,
    pub io_cost: Option<u64>,
    pub time: Option<u64>,
    pub access_predicates: Option<String>,
    pub filter_predicates: Option<String>,
    pub partition_start: Option<String>,
    pub partition_stop: Option<String>,
}

fn oracle_node(r: &OracleRow) -> PlanNode {
    let operation = match r.options.as_deref().filter(|o| !o.is_empty()) {
        Some(o) => format!("{} {o}", r.operation),
        None => r.operation.clone(),
    };
    let object = r.object_name.as_ref().map(|n| match &r.object_owner {
        Some(o) => format!("{o}.{n}"),
        None => n.clone(),
    });
    let mut props: Vec<(String, String)> = Vec::new();
    let mut push = |k: &str, v: Option<String>| {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            props.push((k.to_owned(), v));
        }
    };
    push("Access predicates", r.access_predicates.clone());
    push("Filter predicates", r.filter_predicates.clone());
    push("Alias", r.object_alias.clone());
    push("Object type", r.object_type.clone());
    push("Bytes", r.bytes.map(|v| v.to_string()));
    push("CPU cost", r.cpu_cost.map(|v| v.to_string()));
    push("I/O cost", r.io_cost.map(|v| v.to_string()));
    push("Time (s)", r.time.map(|v| v.to_string()));
    push("Optimizer", r.optimizer.clone());
    if r.partition_start.is_some() || r.partition_stop.is_some() {
        push(
            "Partitions",
            Some(format!(
                "{} – {}",
                r.partition_start.as_deref().unwrap_or("?"),
                r.partition_stop.as_deref().unwrap_or("?")
            )),
        );
    }
    let mut n = PlanNode {
        operation,
        detail: r
            .access_predicates
            .clone()
            .or_else(|| r.filter_predicates.clone()),
        cost: r.cost.unwrap_or(0.0),
        rows: r.cardinality.unwrap_or(0),
        object,
        properties: props,
        ..Default::default()
    };
    let rows = n.rows;
    match (r.operation.as_str(), r.options.as_deref()) {
        ("TABLE ACCESS", Some("FULL")) | ("INDEX", Some("FULL SCAN" | "FAST FULL SCAN"))
            if rows >= SCAN_WARNING_ROWS =>
        {
            n.warnings.push(format!(
                "full scan reading about {rows} rows; an index may help"
            ));
        }
        ("MERGE JOIN", Some("CARTESIAN")) => n
            .warnings
            .push("Cartesian product: a join condition may be missing".into()),
        ("SORT", Some("ORDER BY" | "GROUP BY")) if rows >= SCAN_WARNING_ROWS => n
            .warnings
            .push("sorts many rows: an index could deliver them in order".into()),
        _ => {}
    }
    n
}

/// `PLAN_TABLE` rows of one statement → the plan tree (root: id 0).
pub fn oracle_plan_table(rows: &[OracleRow]) -> Result<ExecutionPlan> {
    fn build(id: i64, rows: &[OracleRow]) -> Vec<PlanNode> {
        rows.iter()
            .filter(|r| r.parent_id == Some(id))
            .map(|r| {
                let mut n = oracle_node(r);
                n.children = build(r.id, rows);
                n
            })
            .collect()
    }
    let top = rows
        .iter()
        .find(|r| r.parent_id.is_none())
        .ok_or_else(|| PlanError("PLAN_TABLE has no rows for the statement".into()))?;
    let mut root = oracle_node(top);
    root.children = build(top.id, rows);
    // DBMS_XPLAN-like text for copying.
    let mut raw = String::from("Id | Operation | Name | Rows | Cost\n");
    for r in rows {
        raw.push_str(&format!(
            "{:>3} | {}{}{} | {} | {} | {}\n",
            r.id,
            "  ".repeat(r.depth.max(0) as usize),
            r.operation,
            r.options
                .as_deref()
                .map(|o| format!(" {o}"))
                .unwrap_or_default(),
            r.object_name.as_deref().unwrap_or(""),
            r.cardinality.map(|c| c.to_string()).unwrap_or_default(),
            r.cost.map(|c| c.to_string()).unwrap_or_default(),
        ));
        for (k, v) in [
            ("access", &r.access_predicates),
            ("filter", &r.filter_predicates),
        ] {
            if let Some(v) = v {
                raw.push_str(&format!("    {} - {k}({v})\n", r.id));
            }
        }
    }
    Ok(ExecutionPlan {
        root,
        raw,
        ..Default::default()
    })
}

/// Leading width of a line, counting tree drawing (`|`, `+-`, `->`) as
/// indentation.
fn indent(line: &str) -> (usize, &str) {
    let mut width = 0;
    let mut rest = line;
    loop {
        if let Some(r) = rest.strip_prefix(' ') {
            width += 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('\t') {
            width += 4;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("->") {
            width += 2;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("+-") {
            width += 2;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('|') {
            width += 1;
            rest = r;
        } else {
            break;
        }
    }
    (width, rest.trim_end())
}

fn reads_whole_table(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    [
        "tablescan",
        "table scan",
        "full scan",
        "seq scan",
        "table access full",
    ]
    .iter()
    .any(|p| l.contains(p))
        && !l.contains("index")
}

/// A text plan → tree by indentation. `title` names the root (`H2 plan`).
pub fn text_plan(raw: &str, title: &str) -> Result<ExecutionPlan> {
    let lines: Vec<(usize, String)> = raw
        .lines()
        .map(indent)
        .filter(|(_, t)| !t.is_empty())
        .map(|(w, t)| (w, t.to_owned()))
        .collect();
    if lines.is_empty() {
        return Err(PlanError("the database returned an empty plan".into()));
    }
    // Stack of (indent, node); finished children are folded into parents.
    let mut stack: Vec<(usize, PlanNode)> = vec![(0, PlanNode::new(title))];
    for (w, text) in lines {
        let w = w + 1; // below the synthetic root
        let mut n = PlanNode::new(text.clone());
        if reads_whole_table(&text) {
            n.warnings
                .push("full table scan: an index on the filtered columns may help".into());
        }
        while stack.len() > 1 && stack.last().is_some_and(|(sw, _)| *sw >= w) {
            let (_, done) = stack.pop().expect("non-empty");
            stack.last_mut().expect("root").1.children.push(done);
        }
        stack.push((w, n));
    }
    while stack.len() > 1 {
        let (_, done) = stack.pop().expect("non-empty");
        stack.last_mut().expect("root").1.children.push(done);
    }
    let root = stack.pop().expect("root").1;
    Ok(ExecutionPlan {
        root,
        raw: raw.trim_end().to_owned(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PLAN_TABLE` of `select * from emp e join dept d on d.id = e.dept_id
    /// where e.sal > 1000` on Oracle 23ai Free.
    fn oracle_rows() -> Vec<OracleRow> {
        vec![
            OracleRow {
                id: 0,
                operation: "SELECT STATEMENT".into(),
                optimizer: Some("ALL_ROWS".into()),
                cost: Some(7.0),
                cardinality: Some(5000),
                ..Default::default()
            },
            OracleRow {
                id: 1,
                parent_id: Some(0),
                depth: 1,
                operation: "HASH JOIN".into(),
                cost: Some(7.0),
                cardinality: Some(5000),
                access_predicates: Some("\"D\".\"ID\"=\"E\".\"DEPT_ID\"".into()),
                ..Default::default()
            },
            OracleRow {
                id: 2,
                parent_id: Some(1),
                depth: 2,
                operation: "TABLE ACCESS".into(),
                options: Some("FULL".into()),
                object_owner: Some("HR".into()),
                object_name: Some("DEPT".into()),
                cost: Some(2.0),
                cardinality: Some(4),
                ..Default::default()
            },
            OracleRow {
                id: 3,
                parent_id: Some(1),
                depth: 2,
                operation: "TABLE ACCESS".into(),
                options: Some("FULL".into()),
                object_owner: Some("HR".into()),
                object_name: Some("EMP".into()),
                cost: Some(5.0),
                cardinality: Some(5000),
                filter_predicates: Some("\"E\".\"SAL\">1000".into()),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn oracle_plan_tree() {
        let p = oracle_plan_table(&oracle_rows()).unwrap();
        assert_eq!(p.root.operation, "SELECT STATEMENT");
        let join = &p.root.children[0];
        assert_eq!(join.operation, "HASH JOIN");
        assert_eq!(join.children.len(), 2);
        let emp = &join.children[1];
        assert_eq!(emp.operation, "TABLE ACCESS FULL");
        assert_eq!(emp.object.as_deref(), Some("HR.EMP"));
        assert_eq!(emp.rows, 5000);
        assert_eq!(emp.warnings.len(), 1, "a full scan of 5000 rows");
        assert!(join.children[0].warnings.is_empty(), "4 rows: no warning");
        assert!(p.raw.contains("TABLE ACCESS FULL | EMP"));
        assert!(oracle_plan_table(&[]).is_err());
    }

    #[test]
    fn text_plans_fold_by_indentation() {
        // Vertica EXPLAIN (abridged).
        let raw = " Access Path:\n +-GROUPBY HASH (LOCAL RESEGMENT GROUPS) [Cost: 3, Rows: 1]\n |  Group By: t.a\n | +---> STORAGE ACCESS for t [Cost: 2, Rows: 10K] (PATH ID: 2)\n |       Projection: public.t_super\n";
        let p = text_plan(raw, "Vertica plan").unwrap();
        assert_eq!(p.root.operation, "Vertica plan");
        let access = &p.root.children[0];
        assert_eq!(access.operation, "Access Path:");
        let group = &access.children[0];
        assert!(group.operation.starts_with("GROUPBY HASH"));
        assert!(!group.children.is_empty());
        // H2: the plan is a statement with a scan comment.
        let h2 = text_plan(
            "SELECT\n    \"A\"\nFROM \"PUBLIC\".\"T\"\n    /* PUBLIC.T.tableScan */",
            "H2 plan",
        )
        .unwrap();
        assert!(!h2.warnings().is_empty());
        assert!(text_plan("\n\n", "x").is_err());
    }
}
