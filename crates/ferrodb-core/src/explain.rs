//! Execution plans (plan, phase 7): the `EXPLAIN` statement of each
//! database and the parser of its output (`ferrodb_data::plan`). The
//! session actor runs the statement on its own connection, so the plan
//! sees the session's schema, temporary tables and open transaction.

use std::time::Instant;

use bson::{Bson, Document, doc};
use ferrodb_api::{Error, ExecutionPlan, PlanNode, Result};
use ferrodb_data::{plan, plan_jdbc};
use ferrodb_drivers::{Request, sink::CollectedOutput};
use ferrodb_lang::{
    mql::{self, Op},
    sql::{self, Flavor, StatementClass},
};

/// How the output of the explain statement is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    PgJson,
    MySqlJson,
    MySqlTree,
    SqliteRows,
    DuckJson,
    Mongo,
    /// Couchbase `EXPLAIN` row / profiled run (documents).
    Couchbase,
    /// SQL Server XML showplan (one document per statement row).
    MssqlXml,
    /// CockroachDB `EXPLAIN (VERBOSE)` tree text.
    CrdbText,
    /// PostgreSQL text format (Redshift).
    PgText,
    /// Oracle `PLAN_TABLE` rows (JDBC native call `explain_plan`).
    OraclePlan,
    /// A text plan of a JDBC database (native call `explain_text`), with
    /// the title of the plan's root.
    TextPlan(&'static str),
}

/// What the session runs for a plan.
#[derive(Debug)]
pub(crate) struct Explain {
    pub request: Request,
    /// The statement shown in the plan view.
    pub statement: String,
    pub format: Format,
    /// SQLite has no `EXPLAIN ANALYZE`: the statement itself is run and
    /// timed after the plan was read.
    pub measure: Option<Request>,
}

/// The first statement of a script.
pub(crate) fn first_statement(text: &str, dialect: &str, mql: bool) -> Option<String> {
    if mql {
        return mql::split(text)
            .first()
            .map(|s| s.text(text).trim().to_owned());
    }
    let flavor = Flavor::for_dialect(dialect);
    sql::split(text, &flavor)
        .into_iter()
        .map(|s| s.text(text).trim().to_owned())
        .find(|s| !s.is_empty())
}

pub(crate) fn request(dialect: &str, mql: bool, stmt: &str, analyze: bool) -> Result<Explain> {
    if mql {
        return mongo_request(stmt, analyze);
    }
    let class = sql::classify(stmt, dialect);
    if analyze && class == StatementClass::Modification {
        return Err(Error::Unsupported(
            "EXPLAIN ANALYZE runs the statement: refused for a statement that changes data; \
             use the estimated plan"
                .into(),
        ));
    }
    let stmt = stmt.trim().trim_end_matches(';').trim().to_owned();
    if dialect == "sqlpp" {
        return couchbase_request(stmt, analyze);
    }
    let (text, format) = match dialect {
        "postgresql" | "greenplum" => (
            if analyze {
                format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {stmt}")
            } else {
                format!("EXPLAIN (FORMAT JSON) {stmt}")
            },
            Format::PgJson,
        ),
        "mysql" => {
            if analyze {
                (format!("EXPLAIN ANALYZE {stmt}"), Format::MySqlTree)
            } else {
                (format!("EXPLAIN FORMAT=JSON {stmt}"), Format::MySqlJson)
            }
        }
        "mariadb" => (
            if analyze {
                format!("ANALYZE FORMAT=JSON {stmt}")
            } else {
                format!("EXPLAIN FORMAT=JSON {stmt}")
            },
            Format::MySqlJson,
        ),
        "sqlite" => (format!("EXPLAIN QUERY PLAN {stmt}"), Format::SqliteRows),
        "cockroachdb" => (
            if analyze {
                format!("EXPLAIN ANALYZE (VERBOSE) {stmt}")
            } else {
                format!("EXPLAIN (VERBOSE) {stmt}")
            },
            Format::CrdbText,
        ),
        "redshift" => {
            if analyze {
                return Err(Error::Unsupported(
                    "Redshift has no EXPLAIN ANALYZE; use the estimated plan (actual figures are \
                     in SVL_QUERY_SUMMARY after a run)"
                        .into(),
                ));
            }
            (format!("EXPLAIN {stmt}"), Format::PgText)
        }
        // The SET options must be alone in their batch: the driver runs
        // them around the statement (native call `explain`).
        "tsql" => {
            let statement = format!(
                "SET {} ON;\n{stmt}",
                if analyze {
                    "STATISTICS XML"
                } else {
                    "SHOWPLAN_XML"
                }
            );
            return Ok(Explain {
                request: Request::native(
                    "explain",
                    &[
                        ("statement", stmt.as_str()),
                        ("analyze", if analyze { "true" } else { "false" }),
                    ],
                ),
                statement,
                format: Format::MssqlXml,
                measure: None,
            });
        }
        "duckdb" => (
            if analyze {
                format!("EXPLAIN (ANALYZE, FORMAT JSON) {stmt}")
            } else {
                format!("EXPLAIN (FORMAT JSON) {stmt}")
            },
            Format::DuckJson,
        ),
        // JDBC databases: the driver runs the explain statements (Oracle
        // needs two, Derby and Exasol a measured run) and returns the rows.
        "oracle" => {
            return Ok(Explain {
                request: Request::native("explain_plan", &[("sql", &stmt)]),
                statement: format!("EXPLAIN PLAN FOR {stmt}"),
                format: Format::OraclePlan,
                measure: None,
            });
        }
        "h2" | "hsqldb" | "derby" | "db2" | "vertica" | "exasol" | "hiveql" | "sybase" => {
            let (title, statement) = match dialect {
                "h2" if analyze => ("H2 plan", format!("EXPLAIN ANALYZE {stmt}")),
                "h2" => ("H2 plan", format!("EXPLAIN {stmt}")),
                "hsqldb" => ("HyperSQL plan", format!("EXPLAIN PLAN FOR {stmt}")),
                "derby" => (
                    "Derby runtime statistics",
                    format!("CALL SYSCS_UTIL.SYSCS_SET_RUNTIMESTATISTICS(1); {stmt}"),
                ),
                "db2" => ("Db2 plan", format!("EXPLAIN PLAN FOR {stmt}")),
                "vertica" => ("Vertica plan", format!("EXPLAIN {stmt}")),
                "exasol" => (
                    "Exasol profile",
                    format!("ALTER SESSION SET PROFILE = 'ON'; {stmt}"),
                ),
                "hiveql" => ("Hive plan", format!("EXPLAIN {stmt}")),
                _ => (
                    "Sybase ASE showplan",
                    format!("SET SHOWPLAN ON; SET NOEXEC ON; {stmt}"),
                ),
            };
            let analyze_arg = if analyze { "true" } else { "false" };
            return Ok(Explain {
                request: Request::native(
                    "explain_text",
                    &[("sql", &stmt), ("analyze", analyze_arg)],
                ),
                statement,
                format: Format::TextPlan(title),
                measure: None,
            });
        }
        other => {
            return Err(Error::Unsupported(format!(
                "execution plans of the `{other}` dialect"
            )));
        }
    };
    let measure =
        (analyze && format == Format::SqliteRows).then(|| Request::Text { text: stmt.clone() });
    Ok(Explain {
        request: Request::Text { text: text.clone() },
        statement: text,
        format,
        measure,
    })
}

/// Couchbase: `EXPLAIN` (the plan the query service would run); with
/// `analyze` the statement runs with `profile=timings` and its rows are
/// discarded (profiling needs Couchbase Enterprise Edition).
fn couchbase_request(stmt: String, analyze: bool) -> Result<Explain> {
    if analyze {
        return Ok(Explain {
            request: Request::native("profile", &[("statement", &stmt)]),
            statement: stmt,
            format: Format::Couchbase,
            measure: None,
        });
    }
    let text = format!("EXPLAIN {stmt}");
    Ok(Explain {
        request: Request::Text { text: text.clone() },
        statement: text,
        format: Format::Couchbase,
        measure: None,
    })
}

/// `explain` command for a MongoDB read: `find`, `aggregate`,
/// `countDocuments`, `distinct`.
fn mongo_request(stmt: &str, analyze: bool) -> Result<Explain> {
    let cmd = mql::parse(stmt)
        .map_err(|e| Error::Unsupported(format!("MongoDB statement: {}", e.message)))?;
    let inner = match &cmd.op {
        Op::Find {
            collection,
            filter,
            projection,
            sort,
            skip,
            limit,
            options,
            one,
            count,
        } => {
            if *count {
                doc! { "count": collection, "query": filter.clone() }
            } else {
                let mut d = doc! { "find": collection, "filter": filter.clone() };
                if let Some(p) = projection {
                    d.insert("projection", p.clone());
                }
                if let Some(s) = sort {
                    d.insert("sort", s.clone());
                }
                if let Some(s) = skip {
                    d.insert("skip", *s as i64);
                }
                match (one, limit) {
                    (true, _) => {
                        d.insert("limit", 1i64);
                    }
                    (false, Some(l)) => {
                        d.insert("limit", *l);
                    }
                    _ => {}
                }
                for (k, v) in options {
                    if matches!(k.as_str(), "hint" | "collation" | "maxTimeMS" | "comment") {
                        d.insert(k.clone(), v.clone());
                    }
                }
                d
            }
        }
        Op::Aggregate {
            collection: Some(collection),
            pipeline,
            options,
        } => {
            if pipeline
                .iter()
                .any(|s| s.contains_key("$out") || s.contains_key("$merge"))
            {
                return Err(Error::Unsupported(
                    "explaining a pipeline that writes ($out / $merge)".into(),
                ));
            }
            let mut d = doc! {
                "aggregate": collection,
                "pipeline": pipeline.iter().cloned().map(Bson::Document).collect::<Vec<_>>(),
                "cursor": {},
            };
            for (k, v) in options {
                if matches!(k.as_str(), "hint" | "collation" | "allowDiskUse") {
                    d.insert(k.clone(), v.clone());
                }
            }
            d
        }
        Op::CountDocuments {
            collection, filter, ..
        } => {
            doc! { "count": collection, "query": filter.clone() }
        }
        Op::Distinct {
            collection,
            field,
            filter,
        } => doc! { "distinct": collection, "key": field, "query": filter.clone() },
        _ => {
            return Err(Error::Unsupported(
                "execution plans of this MongoDB statement (find, aggregate, countDocuments \
                 and distinct have one)"
                    .into(),
            ));
        }
    };
    let verbosity = if analyze {
        "executionStats"
    } else {
        "queryPlanner"
    };
    let command: Document = doc! { "explain": inner, "verbosity": verbosity };
    let statement = format!(
        "db.runCommand({})",
        Bson::Document(command.clone()).into_relaxed_extjson()
    );
    Ok(Explain {
        request: Request::Mql(mql::Command {
            database: cmd.database.clone(),
            op: Op::RunCommand {
                command,
                admin: false,
            },
        }),
        statement,
        format: Format::Mongo,
        measure: None,
    })
}

/// Text of the first column of every row.
fn first_column(out: &CollectedOutput) -> Vec<String> {
    ferrodb_meta::rows::rows(&out.results)
        .iter()
        .filter_map(|r| r.values().first().cloned().flatten())
        .collect()
}

/// Text of the last column of every row (DuckDB: `explain_key`,
/// `explain_value`).
fn last_column(out: &CollectedOutput) -> Vec<String> {
    ferrodb_meta::rows::rows(&out.results)
        .iter()
        .filter_map(|r| r.values().last().cloned().flatten())
        .collect()
}

pub(crate) fn parse(format: Format, out: &CollectedOutput) -> Result<ExecutionPlan> {
    let bad = |e: plan::PlanError| Error::Other(e.to_string());
    match format {
        Format::PgJson => plan::postgres_json(&first_column(out).join("\n")).map_err(bad),
        Format::MySqlJson => plan::mysql_json(&first_column(out).join("\n")).map_err(bad),
        Format::MySqlTree => plan::mysql_tree(&first_column(out).join("\n")).map_err(bad),
        Format::DuckJson => plan::duckdb_json(&last_column(out).join("\n")).map_err(bad),
        Format::MssqlXml => {
            let plans = first_column(out);
            let first = plans
                .first()
                .ok_or_else(|| Error::Other("the server returned no plan".into()))?;
            plan::sqlserver_xml(first).map_err(bad)
        }
        Format::CrdbText => plan::cockroach_text(&first_column(out).join("\n")).map_err(bad),
        Format::PgText => plan::postgres_text(&first_column(out).join("\n")).map_err(bad),
        Format::SqliteRows => {
            let rows: Vec<(i64, i64, String)> = ferrodb_meta::rows::rows(&out.results)
                .iter()
                .map(|r| {
                    (
                        r.int("id").unwrap_or(0),
                        r.int("parent").unwrap_or(0),
                        r.string("detail"),
                    )
                })
                .collect();
            plan::sqlite_rows(&rows).map_err(bad)
        }
        Format::OraclePlan => {
            let rows: Vec<plan_jdbc::OracleRow> = ferrodb_meta::rows::rows(&out.results)
                .iter()
                .map(|r| {
                    let num = |k: &str| r.get(k).and_then(|v| v.trim().parse::<f64>().ok());
                    let count = |k: &str| num(k).map(|v| v.max(0.0) as u64);
                    plan_jdbc::OracleRow {
                        id: r.int("id").unwrap_or(0),
                        parent_id: r.int("parent_id"),
                        depth: r.int("depth").unwrap_or(0),
                        operation: r.string("operation"),
                        options: r.text("options"),
                        object_owner: r.text("object_owner"),
                        object_name: r.text("object_name"),
                        object_alias: r.text("object_alias"),
                        object_type: r.text("object_type"),
                        optimizer: r.text("optimizer"),
                        cost: num("cost"),
                        cardinality: count("cardinality"),
                        bytes: count("bytes"),
                        cpu_cost: count("cpu_cost"),
                        io_cost: count("io_cost"),
                        time: count("time"),
                        access_predicates: r.text("access_predicates"),
                        filter_predicates: r.text("filter_predicates"),
                        partition_start: r.text("partition_start"),
                        partition_stop: r.text("partition_stop"),
                    }
                })
                .collect();
            plan_jdbc::oracle_plan_table(&rows).map_err(bad)
        }
        Format::TextPlan(title) => {
            plan_jdbc::text_plan(&first_column(out).join("\n"), title).map_err(bad)
        }
        Format::Mongo => {
            let raw = out
                .documents
                .first()
                .ok_or_else(|| Error::Other("explain returned no document".into()))?;
            let d = Document::try_from(raw.as_ref())
                .map_err(|e| Error::Other(format!("explain output: {e}")))?;
            plan::mongo(&Bson::Document(d).into_relaxed_extjson()).map_err(bad)
        }
        Format::Couchbase => {
            let raw = out
                .documents
                .first()
                .ok_or_else(|| Error::Other("EXPLAIN returned no plan".into()))?;
            let d =
                Document::try_from(raw.as_ref()).map_err(|e| Error::Other(format!("plan: {e}")))?;
            if d.contains_key("rows") && !d.contains_key("profile") {
                return Err(Error::Unsupported(
                    "actual timings: the server returned no profile (profiling needs \
                     Couchbase Enterprise Edition); use the estimated plan"
                        .into(),
                ));
            }
            plan::couchbase_json(&Bson::Document(d).into_relaxed_extjson()).map_err(bad)
        }
    }
}

/// SQLite `analyze`: the measured run of the statement on the root.
pub(crate) fn add_measurement(plan: &mut ExecutionPlan, rows: u64, started: Instant) {
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    plan.analyzed = true;
    plan.execution_ms = Some(ms);
    let root: &mut PlanNode = &mut plan.root;
    root.actual_rows = Some(rows);
    root.actual_ms = Some(ms);
    root.loops = Some(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_per_dialect() {
        let e = request("postgresql", false, "select 1;", true).unwrap();
        assert_eq!(
            e.statement,
            "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) select 1"
        );
        let e = request("mysql", false, "select 1", false).unwrap();
        assert_eq!(e.format, Format::MySqlJson);
        let e = request("mysql", false, "select 1", true).unwrap();
        assert_eq!(e.format, Format::MySqlTree);
        let e = request("mariadb", false, "select 1", true).unwrap();
        assert_eq!(e.statement, "ANALYZE FORMAT=JSON select 1");
        let e = request("sqlite", false, "select 1", true).unwrap();
        assert!(e.measure.is_some());
        assert!(request("postgresql", false, "delete from t", true).is_err());
        assert!(request("postgresql", false, "delete from t", false).is_ok());
        let e = request("sqlpp", false, "SELECT * FROM b;", false).unwrap();
        assert_eq!(e.statement, "EXPLAIN SELECT * FROM b");
        assert_eq!(e.format, Format::Couchbase);
        let e = request("sqlpp", false, "SELECT 1", true).unwrap();
        assert!(matches!(e.request, Request::Native { .. }));
        assert!(request("partiql", false, "SELECT * FROM t", false).is_err());
        let e = request("tsql", false, "select 1;", true).unwrap();
        assert_eq!(e.format, Format::MssqlXml);
        assert_eq!(e.statement, "SET STATISTICS XML ON;\nselect 1");
        assert_eq!(e.request.arg("analyze"), Some("true"));
        assert_eq!(e.request.arg("statement"), Some("select 1"));
        let e = request("cockroachdb", false, "select 1", true).unwrap();
        assert_eq!(e.statement, "EXPLAIN ANALYZE (VERBOSE) select 1");
        assert!(request("redshift", false, "select 1", true).is_err());
        assert_eq!(
            request("redshift", false, "select 1", false)
                .unwrap()
                .format,
            Format::PgText
        );
        let e = request("oracle", false, "select * from dual;", false).unwrap();
        assert_eq!(e.format, Format::OraclePlan);
        assert_eq!(e.request.arg("sql"), Some("select * from dual"));
        let e = request("h2", false, "select 1", true).unwrap();
        assert_eq!(e.statement, "EXPLAIN ANALYZE select 1");
        assert_eq!(e.request.arg("analyze"), Some("true"));
        assert!(matches!(
            request("hiveql", false, "select 1", false).unwrap().format,
            Format::TextPlan("Hive plan")
        ));
        assert!(request("derby", false, "delete from t", true).is_err());
        assert_eq!(
            first_statement("select 1; select 2", "postgresql", false).as_deref(),
            Some("select 1")
        );
    }

    #[test]
    fn mongo_explain_commands() {
        let e = request(
            "",
            true,
            "db.orders.find({status: 'A'}).sort({total: -1}).limit(5)",
            true,
        )
        .unwrap();
        let Request::Mql(c) = &e.request else {
            panic!()
        };
        let Op::RunCommand { command, .. } = &c.op else {
            panic!()
        };
        assert_eq!(command.get_str("verbosity").unwrap(), "executionStats");
        let inner = command.get_document("explain").unwrap();
        assert_eq!(inner.get_str("find").unwrap(), "orders");
        assert_eq!(inner.get_i64("limit").unwrap(), 5);
        let e = request(
            "",
            true,
            "db.orders.aggregate([{$group: {_id: '$s'}}])",
            false,
        )
        .unwrap();
        assert!(
            e.statement.contains("\"aggregate\":\"orders\""),
            "{}",
            e.statement
        );
        assert!(request("", true, "db.orders.insertOne({a: 1})", false).is_err());
        assert!(request("", true, "db.o.aggregate([{$out: 'x'}])", false).is_err());
    }
}
