//! SQL Server XML showplans (`SET SHOWPLAN_XML ON` for the estimate,
//! `SET STATISTICS XML ON` for the actual plan) → the shared plan model.
//!
//! A statement becomes a node (`SELECT`, `UPDATE`…) whose child is the
//! top `RelOp`; every `RelOp` nested in an operator element is a child.
//! Estimates come from the `RelOp` attributes (`EstimatedTotalSubtreeCost`,
//! `EstimateRows` per execution), actuals from `RunTimeCountersPerThread`
//! summed over threads. Warnings: the plan's own (`SpillToTempDb`,
//! `NoJoinPredicate`, `PlanAffectingConvert`, `ColumnsWithNoStatistics`,
//! memory grants), missing indexes with a suggested `CREATE INDEX`, full
//! scans of large inputs, lookups executed many times, and estimates far
//! off the actual rows.

use ferrodb_api::{ExecutionPlan, PlanNode};
use roxmltree::{Document, Node};

use crate::plan::{PlanError, SCAN_WARNING_ROWS, common_warnings};

type Result<T> = std::result::Result<T, PlanError>;

fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attribute(name)
}

fn num(n: Node<'_, '_>, name: &str) -> Option<f64> {
    attr(n, name).and_then(|v| v.trim().parse().ok())
}

fn is(n: Node<'_, '_>, name: &str) -> bool {
    n.is_element() && n.tag_name().name() == name
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| is(*c, name))
}

/// Descendants of `n` that are not inside a nested `RelOp`.
fn own<'a, 'i>(n: Node<'a, 'i>, out: &mut Vec<Node<'a, 'i>>) {
    for c in n.children().filter(Node::is_element) {
        out.push(c);
        if !is(c, "RelOp") {
            own(c, out);
        }
    }
}

/// `RelOp`s directly below `n` (through operator elements).
fn child_ops<'a, 'i>(n: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
    let mut all = Vec::new();
    own(n, &mut all);
    all.into_iter().filter(|c| is(*c, "RelOp")).collect()
}

/// `[db].[dbo].[orders]` without the brackets the plan puts everywhere.
fn unbracket(s: &str) -> String {
    s.trim_start_matches('[')
        .trim_end_matches(']')
        .replace("]]", "]")
}

/// `dbo.orders` (+ ` o` alias) and the index of an `Object` element.
fn object(o: Node<'_, '_>) -> (Option<String>, Option<String>) {
    let table = attr(o, "Table").map(unbracket);
    let schema = attr(o, "Schema").map(unbracket);
    let alias = attr(o, "Alias")
        .map(unbracket)
        .filter(|a| Some(a) != table.as_ref());
    let target = table.map(|t| {
        let t = match schema {
            Some(s) => format!("{s}.{t}"),
            None => t,
        };
        match alias {
            Some(a) => format!("{t} {a}"),
            None => t,
        }
    });
    (target, attr(o, "Index").map(unbracket))
}

/// Plan warnings of a `Warnings` element, as text.
fn warning_texts(w: Node<'_, '_>) -> Vec<String> {
    let mut out = Vec::new();
    for c in w.children().filter(Node::is_element) {
        match c.tag_name().name() {
            "SpillToTempDb" => out.push(format!(
                "spilled to tempdb (spill level {}): the memory grant was too small",
                attr(c, "SpillLevel").unwrap_or("?")
            )),
            "SortSpillDetails" | "HashSpillDetails" | "ExchangeSpillDetails" => out.push(format!(
                "{} spilled {} pages to tempdb",
                c.tag_name()
                    .name()
                    .trim_end_matches("SpillDetails")
                    .to_lowercase(),
                attr(c, "WritesToTempDb").unwrap_or("?")
            )),
            "NoJoinPredicate" => out.push("no join predicate: a cartesian product".into()),
            "PlanAffectingConvert" => out.push(format!(
                "implicit conversion {} may prevent {}",
                attr(c, "Expression").unwrap_or_default(),
                match attr(c, "ConvertIssue") {
                    Some("Seek Plan") => "an index seek",
                    _ => "a good cardinality estimate",
                }
            )),
            "ColumnsWithNoStatistics" => {
                let cols: Vec<String> = c
                    .descendants()
                    .filter(|d| is(*d, "ColumnReference"))
                    .filter_map(|d| attr(d, "Column").map(unbracket))
                    .collect();
                out.push(format!("columns without statistics: {}", cols.join(", ")));
            }
            "MemoryGrantWarning" => out.push(format!(
                "memory grant warning ({}): granted {} KB, used {} KB",
                attr(c, "GrantWarningKind").unwrap_or("?"),
                attr(c, "GrantedMemory").unwrap_or("?"),
                attr(c, "MaxUsedMemory").unwrap_or("?")
            )),
            "UnmatchedIndexes" => {
                out.push("a filtered index could not be used (parameterized query)".into())
            }
            "WaitForWait" => out.push("waited for a resource (see WaitStats)".into()),
            other => out.push(other.to_owned()),
        }
    }
    if attr(w, "NoJoinPredicate") == Some("true") || attr(w, "NoJoinPredicate") == Some("1") {
        out.push("no join predicate: a cartesian product".into());
    }
    out
}

/// `CREATE INDEX` suggested by a `MissingIndexGroup`.
fn missing_indexes(qp: Node<'_, '_>) -> Vec<String> {
    let Some(mi) = child(qp, "MissingIndexes") else {
        return Vec::new();
    };
    mi.children()
        .filter(|g| is(*g, "MissingIndexGroup"))
        .flat_map(|g| {
            let impact = attr(g, "Impact").unwrap_or("?").to_owned();
            g.children()
                .filter(|m| is(*m, "MissingIndex"))
                .map(move |m| {
                    let mut keys = Vec::new();
                    let mut include = Vec::new();
                    for cg in m.children().filter(|c| is(*c, "ColumnGroup")) {
                        let cols = cg
                            .children()
                            .filter(|c| is(*c, "Column"))
                            .filter_map(|c| attr(c, "Name").map(str::to_owned));
                        if attr(cg, "Usage") == Some("INCLUDE") {
                            include.extend(cols);
                        } else {
                            keys.extend(cols);
                        }
                    }
                    let mut stmt = format!(
                        "CREATE INDEX [ix_missing] ON {}.{} ({})",
                        attr(m, "Schema").unwrap_or("[dbo]"),
                        attr(m, "Table").unwrap_or("?"),
                        keys.join(", ")
                    );
                    if !include.is_empty() {
                        stmt.push_str(&format!(" INCLUDE ({})", include.join(", ")));
                    }
                    format!("missing index (estimated impact {impact}%): {stmt}")
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `ScalarString` of the first `ScalarOperator` under `n`.
fn scalar(n: Node<'_, '_>) -> Option<String> {
    n.descendants()
        .find(|d| is(*d, "ScalarOperator"))
        .and_then(|d| attr(d, "ScalarString").map(str::to_owned))
}

const OP_ATTRS: &[&str] = &[
    "LogicalOp",
    "EstimateIO",
    "EstimateCPU",
    "AvgRowSize",
    "EstimatedExecutionMode",
    "Parallel",
    "EstimateRebinds",
    "EstimateRewinds",
    "EstimatedRowsRead",
    "TableCardinality",
];

fn rel_op(n: Node<'_, '_>) -> PlanNode {
    let physical = attr(n, "PhysicalOp").unwrap_or("?").to_owned();
    let logical = attr(n, "LogicalOp").unwrap_or_default();
    let mut own_nodes = Vec::new();
    own(n, &mut own_nodes);
    let obj = own_nodes
        .iter()
        .find(|c| is(**c, "Object"))
        .map(|o| object(*o));
    let (target, index) = obj.unwrap_or((None, None));
    let detail = match (&index, &target) {
        (Some(i), Some(t)) => Some(format!("{i} on {t}")),
        (None, Some(t)) => Some(format!("on {t}")),
        _ => None,
    };
    let mut props: Vec<(String, String)> = OP_ATTRS
        .iter()
        .filter_map(|k| attr(n, k).map(|v| ((*k).to_owned(), v.to_owned())))
        .collect();
    for (tag, label) in [
        ("SeekPredicates", "Seek Predicate"),
        ("Predicate", "Predicate"),
        ("HashKeysBuild", "Hash Keys Build"),
        ("HashKeysProbe", "Hash Keys Probe"),
        ("ProbeResidual", "Probe Residual"),
        ("OrderBy", "Order By"),
    ] {
        if let Some(e) = own_nodes.iter().find(|c| is(**c, tag)) {
            let text = match tag {
                "OrderBy" | "HashKeysBuild" | "HashKeysProbe" => e
                    .descendants()
                    .filter(|d| is(*d, "ColumnReference"))
                    .filter_map(|d| attr(d, "Column").map(unbracket))
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => scalar(*e).unwrap_or_default(),
            };
            if !text.is_empty() {
                props.push((label.to_owned(), text));
            }
        }
    }
    // Actuals: summed over threads.
    let counters: Vec<Node<'_, '_>> = child(n, "RunTimeInformation")
        .map(|r| {
            r.children()
                .filter(|c| is(*c, "RunTimeCountersPerThread"))
                .collect()
        })
        .unwrap_or_default();
    let (mut actual_rows, mut executions, mut elapsed): (Option<u64>, Option<u64>, Option<f64>) =
        (None, None, None);
    for c in &counters {
        if let Some(r) = num(*c, "ActualRows") {
            *actual_rows.get_or_insert(0) += r as u64;
        }
        if let Some(e) = num(*c, "ActualExecutions") {
            *executions.get_or_insert(0) += e as u64;
        }
        if let Some(t) = num(*c, "ActualElapsedms") {
            elapsed = Some(elapsed.unwrap_or(0.0).max(t));
        }
        for k in ["ActualLogicalReads", "ActualPhysicalReads", "ActualSpills"] {
            if let Some(v) = attr(*c, k).filter(|v| *v != "0") {
                props.push((k.to_owned(), v.to_owned()));
            }
        }
    }
    let loops = executions;
    let mut node = PlanNode {
        operation: if logical.is_empty() || logical == physical {
            physical.clone()
        } else {
            format!("{physical} ({logical})")
        },
        detail,
        cost: num(n, "EstimatedTotalSubtreeCost").unwrap_or(0.0),
        rows: num(n, "EstimateRows")
            .map(|r| r.round() as u64)
            .unwrap_or(0),
        object: index.clone().or(target.clone()),
        actual_rows: actual_rows.map(|r| match loops {
            Some(l) if l > 1 => r / l,
            _ => r,
        }),
        actual_ms: elapsed,
        loops,
        properties: props,
        warnings: Vec::new(),
        children: child_ops(n).into_iter().map(rel_op).collect(),
    };
    if let Some(w) = own_nodes.iter().find(|c| is(**c, "Warnings")) {
        node.warnings.extend(warning_texts(*w));
    }
    let full_scan = matches!(
        physical.as_str(),
        "Table Scan" | "Clustered Index Scan" | "Index Scan" | "Columnstore Index Scan"
    ) && !own_nodes.iter().any(|c| is(*c, "SeekPredicates"));
    common_warnings(&mut node, full_scan);
    if matches!(physical.as_str(), "Key Lookup" | "RID Lookup") {
        let execs = loops.unwrap_or_else(|| {
            (num(n, "EstimateRebinds").unwrap_or(0.0)
                + num(n, "EstimateRewinds").unwrap_or(0.0)
                + 1.0) as u64
        });
        if execs >= SCAN_WARNING_ROWS {
            node.warnings.push(format!(
                "{physical} executed {execs} times: a covering index (INCLUDE columns) may help"
            ));
        }
    }
    if physical == "Sort" && node.rows >= SCAN_WARNING_ROWS {
        node.warnings
            .push("sorts rows: an index providing the order may help".into());
    }
    node
}

fn statement(s: Node<'_, '_>) -> PlanNode {
    let kind = attr(s, "StatementType").unwrap_or("Statement").to_owned();
    let mut n = PlanNode {
        operation: kind,
        detail: attr(s, "StatementText").map(|t| t.trim().to_owned()),
        cost: num(s, "StatementSubTreeCost").unwrap_or(0.0),
        rows: num(s, "StatementEstRows")
            .map(|r| r.round() as u64)
            .unwrap_or(0),
        ..Default::default()
    };
    for k in [
        "StatementOptmLevel",
        "StatementOptmEarlyAbortReason",
        "CardinalityEstimationModelVersion",
        "QueryHash",
        "QueryPlanHash",
    ] {
        if let Some(v) = attr(s, k) {
            n.properties.push((k.to_owned(), v.to_owned()));
        }
    }
    if let Some(qp) = child(s, "QueryPlan") {
        for k in [
            "DegreeOfParallelism",
            "CompileTime",
            "CompileCPU",
            "CompileMemory",
            "CachedPlanSize",
        ] {
            if let Some(v) = attr(qp, k) {
                n.properties.push((k.to_owned(), v.to_owned()));
            }
        }
        if let Some(mg) = child(qp, "MemoryGrantInfo")
            && let Some(v) = attr(mg, "GrantedMemory")
        {
            n.properties.push(("GrantedMemory KB".into(), v.to_owned()));
        }
        if let Some(t) = child(qp, "QueryTimeStats") {
            n.actual_ms = num(t, "ElapsedTime");
            if let Some(v) = attr(t, "CpuTime") {
                n.properties.push(("CpuTime ms".into(), v.to_owned()));
            }
        }
        n.warnings.extend(missing_indexes(qp));
        if let Some(w) = child(qp, "Warnings") {
            n.warnings.extend(warning_texts(w));
        }
        n.children = qp
            .children()
            .filter(|c| is(*c, "RelOp"))
            .map(rel_op)
            .collect();
        if n.actual_ms.is_none() {
            n.actual_ms = n.children.first().and_then(|c| c.actual_ms);
        }
        n.actual_rows = n.children.first().and_then(|c| c.actual_rows);
        if n.actual_rows.is_some() {
            n.loops = Some(1);
        }
    }
    n
}

/// One or more `ShowPlanXML` documents (one per statement row).
pub fn sqlserver_xml(raw: &str) -> Result<ExecutionPlan> {
    let raw = raw.trim();
    let doc = Document::parse(raw).map_err(|e| PlanError(e.to_string()))?;
    let stmts: Vec<PlanNode> = doc
        .descendants()
        .filter(|n| is(*n, "StmtSimple") || is(*n, "StmtCond") || is(*n, "StmtCursor"))
        .filter(|n| child(*n, "QueryPlan").is_some() || is(*n, "StmtSimple"))
        .map(statement)
        .collect();
    let compile: f64 = doc
        .descendants()
        .filter(|n| is(*n, "QueryPlan"))
        .filter_map(|n| num(n, "CompileTime"))
        .sum();
    let mut root = match stmts.len() {
        0 => return Err(PlanError("no statement in the showplan".into())),
        1 => stmts.into_iter().next().expect("one"),
        _ => PlanNode {
            operation: "Batch".into(),
            cost: stmts.iter().map(|s| s.cost).sum(),
            children: stmts,
            ..Default::default()
        },
    };
    let analyzed = doc.descendants().any(|n| is(n, "RunTimeInformation"));
    if root.operation == "Batch" && analyzed {
        root.actual_ms = Some(root.children.iter().filter_map(|c| c.actual_ms).sum());
    }
    Ok(ExecutionPlan {
        execution_ms: if analyzed { root.actual_ms } else { None },
        analyzed,
        root,
        raw: raw.to_owned(),
        planning_ms: (compile > 0.0).then_some(compile),
        statement: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded from SQL Server 2022 (`SET SHOWPLAN_XML ON`), trimmed.
    const ESTIMATED: &str = r#"<ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan" Version="1.564" Build="16.0.4135.4"><BatchSequence><Batch><Statements><StmtSimple StatementText="select o.id, c.name from dbo.orders o join dbo.customers c on c.id = o.customer_id where o.code = 42" StatementId="1" StatementCompId="1" StatementType="SELECT" StatementSubTreeCost="0.512" StatementEstRows="12" StatementOptmLevel="FULL" CardinalityEstimationModelVersion="160"><QueryPlan DegreeOfParallelism="1" CachedPlanSize="40" CompileTime="3" CompileCPU="3" CompileMemory="320"><MissingIndexes><MissingIndexGroup Impact="87.5"><MissingIndex Database="[shop]" Schema="[dbo]" Table="[orders]"><ColumnGroup Usage="EQUALITY"><Column Name="[code]" ColumnId="3"/></ColumnGroup><ColumnGroup Usage="INCLUDE"><Column Name="[customer_id]" ColumnId="2"/></ColumnGroup></MissingIndex></MissingIndexGroup></MissingIndexes><Warnings><PlanAffectingConvert ConvertIssue="Seek Plan" Expression="CONVERT_IMPLICIT(int,[o].[code],0)=(42)"/></Warnings><RelOp NodeId="0" PhysicalOp="Hash Match" LogicalOp="Inner Join" EstimateRows="12" EstimateIO="0" EstimateCPU="0.02" AvgRowSize="27" EstimatedTotalSubtreeCost="0.512" Parallel="0" EstimateRebinds="0" EstimateRewinds="0" EstimatedExecutionMode="Row"><OutputList/><Hash><DefinedValues/><HashKeysBuild><ColumnReference Database="[shop]" Schema="[dbo]" Table="[orders]" Alias="[o]" Column="customer_id"/></HashKeysBuild><HashKeysProbe><ColumnReference Database="[shop]" Schema="[dbo]" Table="[customers]" Alias="[c]" Column="id"/></HashKeysProbe><RelOp NodeId="1" PhysicalOp="Clustered Index Scan" LogicalOp="Clustered Index Scan" EstimateRows="12" EstimateIO="0.31" EstimateCPU="0.11" AvgRowSize="15" EstimatedTotalSubtreeCost="0.42" TableCardinality="100000" Parallel="0" EstimateRebinds="0" EstimateRewinds="0" EstimatedExecutionMode="Row"><OutputList/><IndexScan Ordered="0" ForcedIndex="0" NoExpandHint="0"><DefinedValues/><Object Database="[shop]" Schema="[dbo]" Table="[orders]" Index="[PK_orders]" Alias="[o]" IndexKind="Clustered" Storage="RowStore"/><Predicate><ScalarOperator ScalarString="CONVERT_IMPLICIT(int,[shop].[dbo].[orders].[code] as [o].[code],0)=(42)"/></Predicate></IndexScan></RelOp><RelOp NodeId="2" PhysicalOp="Clustered Index Scan" LogicalOp="Clustered Index Scan" EstimateRows="1000" EstimateIO="0.01" EstimateCPU="0.001" AvgRowSize="19" EstimatedTotalSubtreeCost="0.07" TableCardinality="1000" Parallel="0" EstimateRebinds="0" EstimateRewinds="0" EstimatedExecutionMode="Row"><OutputList/><Warnings><ColumnsWithNoStatistics><ColumnReference Database="[shop]" Schema="[dbo]" Table="[customers]" Column="name"/></ColumnsWithNoStatistics></Warnings><IndexScan Ordered="0"><DefinedValues/><Object Database="[shop]" Schema="[dbo]" Table="[customers]" Index="[PK_customers]" Alias="[c]" IndexKind="Clustered" Storage="RowStore"/></IndexScan></RelOp></Hash></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;

    /// Recorded `SET STATISTICS XML ON` output, trimmed.
    const ACTUAL: &str = r#"<ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan" Version="1.564" Build="16.0.4135.4"><BatchSequence><Batch><Statements><StmtSimple StatementText="select * from t order by v" StatementId="1" StatementCompId="1" StatementType="SELECT" StatementSubTreeCost="1.2" StatementEstRows="50"><QueryPlan DegreeOfParallelism="1" CompileTime="1"><QueryTimeStats CpuTime="40" ElapsedTime="52"/><RelOp NodeId="0" PhysicalOp="Sort" LogicalOp="Sort" EstimateRows="50" EstimatedTotalSubtreeCost="1.2" Parallel="0"><OutputList/><Warnings><SpillToTempDb SpillLevel="1" SpilledThreadCount="1"/><SortSpillDetails GrantedMemoryKb="1024" UsedMemoryKb="1024" WritesToTempDb="420" ReadsFromTempDb="420"/></Warnings><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="20000" ActualExecutions="1" ActualElapsedms="51" ActualSpills="1"/></RunTimeInformation><Sort Distinct="0"><OrderBy><OrderByColumn Ascending="1"><ColumnReference Column="v"/></OrderByColumn></OrderBy><RelOp NodeId="1" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="50" EstimatedTotalSubtreeCost="0.3" Parallel="0"><OutputList/><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="20000" ActualExecutions="1" ActualElapsedms="12" ActualLogicalReads="90"/></RunTimeInformation><TableScan Ordered="0"><Object Database="[d]" Schema="[dbo]" Table="[t]"/></TableScan></RelOp></Sort></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;

    #[test]
    fn estimated_plan_with_warnings() {
        let p = sqlserver_xml(ESTIMATED).unwrap();
        assert!(!p.analyzed);
        assert_eq!(p.planning_ms, Some(3.0));
        let root = &p.root;
        assert_eq!(root.operation, "SELECT");
        assert_eq!(root.cost, 0.512);
        assert!(
            root.warnings.iter().any(|w| w.contains(
                "CREATE INDEX [ix_missing] ON [dbo].[orders] ([code]) INCLUDE ([customer_id])"
            )),
            "{:?}",
            root.warnings
        );
        assert!(
            root.warnings
                .iter()
                .any(|w| w.contains("implicit conversion"))
        );
        let join = &root.children[0];
        assert_eq!(join.operation, "Hash Match (Inner Join)");
        assert_eq!(join.children.len(), 2);
        assert!(
            join.properties
                .iter()
                .any(|(k, v)| k == "Hash Keys Build" && v == "customer_id")
        );
        let orders = &join.children[0];
        assert_eq!(orders.detail.as_deref(), Some("PK_orders on dbo.orders o"));
        assert_eq!(orders.object.as_deref(), Some("PK_orders"));
        assert!(orders.properties.iter().any(|(k, _)| k == "Predicate"));
        let customers = &join.children[1];
        assert!(customers.warnings.iter().any(|w| w.contains("full scan")));
        assert!(
            customers
                .warnings
                .iter()
                .any(|w| w.contains("without statistics: name"))
        );
    }

    #[test]
    fn actual_plan_with_spills_and_misestimates() {
        let p = sqlserver_xml(ACTUAL).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.execution_ms, Some(52.0));
        let sort = &p.root.children[0];
        assert_eq!(sort.actual_rows, Some(20_000));
        assert_eq!(sort.loops, Some(1));
        assert!(
            sort.warnings
                .iter()
                .any(|w| w.contains("spilled to tempdb"))
        );
        assert!(
            sort.warnings
                .iter()
                .any(|w| w.contains("estimated 50 rows, actual 20000"))
        );
        let scan = &sort.children[0];
        assert_eq!(scan.operation, "Table Scan");
        assert!(scan.warnings.iter().any(|w| w.contains("full scan")));
        assert!(
            scan.properties
                .iter()
                .any(|(k, v)| k == "ActualLogicalReads" && v == "90")
        );
        assert!(sqlserver_xml("<x/>").is_err());
    }
}
