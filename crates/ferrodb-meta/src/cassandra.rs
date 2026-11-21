//! Apache Cassandra introspection from `system_schema` and CQL DDL.
//!
//! Keyspaces (with their replication) are the containers. A keyspace holds
//! tables (partition key, clustering columns with their order, static and
//! regular columns, options), secondary indexes and SAI indexes, materialized
//! views, user-defined types, functions and aggregates.
//!
//! The model has no Cassandra-specific fields, so the introspector fills
//! the generic ones: the primary key lists the partition key columns then
//! the clustering columns, [`Table::partition_by`] names the partition key
//! columns, column comments say `partition key` / `clustering ASC|DESC` /
//! `static`, and [`Table::server_sql`] keeps the generated `CREATE TABLE …
//! PRIMARY KEY ((pk), ck) WITH CLUSTERING ORDER BY …` used by the DDL
//! generator verbatim. User-defined types have no node kind of their own:
//! they are listed with the functions (`returns` = `type`) and their
//! definition is the `CREATE TYPE` statement.

use std::collections::BTreeMap;

use ferrodb_api::Result;
use ferrodb_drivers::{BoxFuture, Connection, quote_literal};

use crate::{
    Introspector,
    ddl::quote,
    model::*,
    rows::{Row, query},
};

pub struct CassandraIntrospector;

/// A CQL identifier, quoted when needed.
pub fn q(name: &str) -> String {
    quote(Dialect::Cassandra, name)
}

/// One column of `system_schema.columns`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CqlColumn {
    pub name: String,
    pub data_type: String,
    /// `partition_key`, `clustering`, `regular`, `static`.
    pub kind: String,
    pub position: i64,
    /// `asc`, `desc`, `none`.
    pub clustering_order: String,
}

/// Partition key and clustering columns, in key order.
pub fn key_columns(cols: &[CqlColumn]) -> (Vec<&CqlColumn>, Vec<&CqlColumn>) {
    let mut pk: Vec<&CqlColumn> = cols.iter().filter(|c| c.kind == "partition_key").collect();
    let mut ck: Vec<&CqlColumn> = cols.iter().filter(|c| c.kind == "clustering").collect();
    pk.sort_by_key(|c| c.position);
    ck.sort_by_key(|c| c.position);
    (pk, ck)
}

/// `PRIMARY KEY ((a, b), c, d)` / `PRIMARY KEY (a, c)`.
pub fn primary_key_clause(cols: &[CqlColumn]) -> String {
    let (pk, ck) = key_columns(cols);
    let names = |v: &[&CqlColumn]| v.iter().map(|c| q(&c.name)).collect::<Vec<_>>().join(", ");
    let part = if pk.len() == 1 {
        names(&pk)
    } else {
        format!("({})", names(&pk))
    };
    if ck.is_empty() {
        format!("PRIMARY KEY ({part})")
    } else {
        format!("PRIMARY KEY ({part}, {})", names(&ck))
    }
}

/// `CLUSTERING ORDER BY (c DESC, d ASC)`, when there are clustering columns.
pub fn clustering_order(cols: &[CqlColumn]) -> Option<String> {
    let (_, ck) = key_columns(cols);
    if ck.is_empty() {
        return None;
    }
    Some(format!(
        "CLUSTERING ORDER BY ({})",
        ck.iter()
            .map(|c| format!(
                "{} {}",
                q(&c.name),
                if c.clustering_order.eq_ignore_ascii_case("desc") {
                    "DESC"
                } else {
                    "ASC"
                }
            ))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// A `system_schema` map (JSON text) as CQL map literal `{'k': 'v'}`.
pub fn map_literal(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_object()?;
    Some(format!(
        "{{{}}}",
        obj.iter()
            .map(|(k, v)| {
                let v = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{}: {}", quote_literal(k), quote_literal(&v))
            })
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// `CREATE TABLE ks.t (…) WITH CLUSTERING ORDER BY … AND …`.
pub fn create_table(
    keyspace: &str,
    table: &str,
    cols: &[CqlColumn],
    options: &[(String, String)],
) -> String {
    let mut ordered: Vec<&CqlColumn> = Vec::new();
    let (pk, ck) = key_columns(cols);
    ordered.extend(pk);
    ordered.extend(ck);
    let mut rest: Vec<&CqlColumn> = cols
        .iter()
        .filter(|c| c.kind != "partition_key" && c.kind != "clustering")
        .collect();
    rest.sort_by(|a, b| a.name.cmp(&b.name));
    ordered.extend(rest);
    let mut lines: Vec<String> = ordered
        .iter()
        .map(|c| {
            format!(
                "    {} {}{}",
                q(&c.name),
                c.data_type,
                if c.kind == "static" { " STATIC" } else { "" }
            )
        })
        .collect();
    lines.push(format!("    {}", primary_key_clause(cols)));
    let mut with: Vec<String> = Vec::new();
    if let Some(o) = clustering_order(cols) {
        with.push(o);
    }
    for (k, v) in options {
        with.push(format!("{k} = {v}"));
    }
    let mut s = format!(
        "CREATE TABLE {}.{} (\n{}\n)",
        q(keyspace),
        q(table),
        lines.join(",\n")
    );
    if !with.is_empty() {
        s.push_str(&format!(" WITH {}", with.join("\n    AND ")));
    }
    s.push(';');
    s
}

/// Table options worth keeping in the DDL, as CQL literals.
fn table_options(r: &Row) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(c) = r.text("comment") {
        out.push(("comment".to_owned(), quote_literal(&c)));
    }
    for k in [
        "default_time_to_live",
        "gc_grace_seconds",
        "bloom_filter_fp_chance",
    ] {
        if let Some(v) = r.text(k) {
            out.push((k.to_owned(), v));
        }
    }
    for k in ["compaction", "compression", "caching"] {
        if let Some(m) = r.text(k).and_then(|j| map_literal(&j)) {
            out.push((k.to_owned(), m));
        }
    }
    out
}

/// JSON list text (`["a","b"]`) as strings.
fn json_list(s: Option<String>) -> Vec<String> {
    s.and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|v| match v {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        })
        .collect()
}

/// `SimpleStrategy, replication_factor 1` from the replication map.
pub fn replication_text(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_object()?;
    let class = obj
        .get("class")
        .and_then(|c| c.as_str())
        .map(|c| c.rsplit('.').next().unwrap_or(c).to_owned())?;
    let rest: Vec<String> = obj
        .iter()
        .filter(|(k, _)| *k != "class")
        .map(|(k, v)| {
            format!(
                "{k} {}",
                v.as_str().map(str::to_owned).unwrap_or(v.to_string())
            )
        })
        .collect();
    Some(if rest.is_empty() {
        class
    } else {
        format!("{class}, {}", rest.join(", "))
    })
}

fn index(keyspace: &str, table: &str, r: &Row) -> Option<Index> {
    let name = r.text("index_name")?;
    let options: BTreeMap<String, String> = r
        .text("options")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    let target = options.get("target").cloned().unwrap_or_default();
    let class = options.get("class_name").cloned();
    let kind = r.string("kind").to_ascii_uppercase();
    let method = match (&class, kind.as_str()) {
        (Some(c), _) if c.ends_with("StorageAttachedIndex") || c.eq_ignore_ascii_case("sai") => {
            Some("SAI".to_owned())
        }
        (Some(c), _) => Some(c.clone()),
        (None, "COMPOSITES" | "KEYS") => None,
        (None, k) => Some(k.to_owned()),
    };
    let server_sql = match &class {
        Some(c) => Some(format!(
            "CREATE CUSTOM INDEX {} ON {}.{} ({}) USING {}",
            q(&name),
            q(keyspace),
            q(table),
            target,
            quote_literal(if method.as_deref() == Some("SAI") {
                "StorageAttachedIndex"
            } else {
                c
            })
        )),
        None => Some(format!(
            "CREATE INDEX {} ON {}.{} ({})",
            q(&name),
            q(keyspace),
            q(table),
            target
        )),
    };
    Some(Index {
        name,
        columns: vec![target],
        method,
        server_sql,
        ..Default::default()
    })
}

fn signature(name: &str, types: &[String]) -> String {
    format!("{name}({})", types.join(", "))
}

impl Introspector for CassandraIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::Cassandra
    }

    fn containers<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
    ) -> BoxFuture<'a, Result<Vec<Container>>> {
        Box::pin(async move {
            let rows = query(
                conn,
                "SELECT keyspace_name, replication FROM system_schema.keyspaces",
            )
            .await?;
            let mut out: Vec<Container> = rows
                .iter()
                .filter_map(|r| {
                    Some(Container {
                        name: r.text("keyspace_name")?,
                        comment: r.text("replication").and_then(|j| replication_text(&j)),
                        content: None,
                    })
                })
                .collect();
            // User keyspaces first, then the system ones.
            out.sort_by_key(|c| (c.name.starts_with("system"), c.name.clone()));
            Ok(out)
        })
    }

    fn content<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
        container: &'a str,
        only: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Content>> {
        Box::pin(async move {
            let ks = quote_literal(container);
            let only_filter = only
                .map(|t| format!(" AND table_name = {}", quote_literal(t)))
                .unwrap_or_default();
            let tables = query(
                conn,
                format!(
                    "SELECT * FROM system_schema.tables WHERE keyspace_name = {ks}{only_filter}"
                ),
            )
            .await?;
            let columns = query(
                conn,
                format!(
                    "SELECT table_name, column_name, clustering_order, kind, position, type \
                     FROM system_schema.columns WHERE keyspace_name = {ks}{only_filter}"
                ),
            )
            .await?;
            let indexes = query(
                conn,
                format!(
                    "SELECT table_name, index_name, kind, options FROM system_schema.indexes \
                     WHERE keyspace_name = {ks}{only_filter}"
                ),
            )
            .await?;
            let views = query(
                conn,
                format!(
                    "SELECT view_name, base_table_name, where_clause, include_all_columns \
                     FROM system_schema.views WHERE keyspace_name = {ks}"
                ),
            )
            .await?;
            let mut by_table: BTreeMap<String, Vec<CqlColumn>> = BTreeMap::new();
            for r in &columns {
                let (Some(t), Some(name)) = (r.text("table_name"), r.text("column_name")) else {
                    continue;
                };
                by_table.entry(t).or_default().push(CqlColumn {
                    name,
                    data_type: r.string("type"),
                    kind: r.string("kind"),
                    position: r.int("position").unwrap_or(0),
                    clustering_order: r.string("clustering_order"),
                });
            }
            let mut content = Content::default();
            let table_of = |name: &str, cols: &[CqlColumn], kind: TableKind| -> Table {
                let (pk, ck) = key_columns(cols);
                let mut key: Vec<String> = pk.iter().map(|c| c.name.clone()).collect();
                let partition = key.join(", ");
                key.extend(ck.iter().map(|c| c.name.clone()));
                let mut ordered: Vec<&CqlColumn> = pk.clone();
                ordered.extend(ck.iter().copied());
                let mut rest: Vec<&CqlColumn> = cols
                    .iter()
                    .filter(|c| c.kind != "partition_key" && c.kind != "clustering")
                    .collect();
                rest.sort_by(|a, b| a.name.cmp(&b.name));
                ordered.extend(rest);
                Table {
                    name: name.to_owned(),
                    kind,
                    columns: ordered
                        .iter()
                        .map(|c| Column {
                            name: c.name.clone(),
                            data_type: c.data_type.clone(),
                            nullable: !matches!(c.kind.as_str(), "partition_key" | "clustering"),
                            comment: match c.kind.as_str() {
                                "partition_key" => Some("partition key".to_owned()),
                                "clustering" => Some(format!(
                                    "clustering {}",
                                    c.clustering_order.to_ascii_uppercase()
                                )),
                                "static" => Some("static".to_owned()),
                                _ => None,
                            },
                            ..Default::default()
                        })
                        .collect(),
                    primary_key: (!key.is_empty()).then_some(Key {
                        name: None,
                        columns: key,
                    }),
                    partition_by: (!partition.is_empty()).then_some(partition),
                    ..Default::default()
                }
            };
            for r in &tables {
                let Some(name) = r.text("table_name") else {
                    continue;
                };
                let cols = by_table.get(&name).cloned().unwrap_or_default();
                let mut t = table_of(&name, &cols, TableKind::Table);
                t.comment = r.text("comment");
                t.server_sql = Some(create_table(container, &name, &cols, &table_options(r)));
                t.indexes = indexes
                    .iter()
                    .filter(|i| i.text("table_name").as_deref() == Some(name.as_str()))
                    .filter_map(|i| index(container, &name, i))
                    .collect();
                content.tables.push(t);
            }
            for v in &views {
                let Some(name) = v.text("view_name") else {
                    continue;
                };
                if only.is_some_and(|o| o != name) {
                    continue;
                }
                let cols = match by_table.get(&name) {
                    Some(c) => c.clone(),
                    None if only.is_some() => {
                        // Columns of an incrementally refreshed view.
                        let rows = query(
                            conn,
                            format!(
                                "SELECT column_name, clustering_order, kind, position, type \
                                 FROM system_schema.columns WHERE keyspace_name = {ks} AND table_name = {}",
                                quote_literal(&name)
                            ),
                        )
                        .await?;
                        rows.iter()
                            .filter_map(|r| {
                                Some(CqlColumn {
                                    name: r.text("column_name")?,
                                    data_type: r.string("type"),
                                    kind: r.string("kind"),
                                    position: r.int("position").unwrap_or(0),
                                    clustering_order: r.string("clustering_order"),
                                })
                            })
                            .collect()
                    }
                    None => Vec::new(),
                };
                let mut t = table_of(&name, &cols, TableKind::MaterializedView);
                let base = v.string("base_table_name");
                let selected = if v.bool("include_all_columns") {
                    "*".to_owned()
                } else {
                    cols.iter()
                        .map(|c| q(&c.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let filter = v.text("where_clause").unwrap_or_default();
                t.view_definition = Some(format!(
                    "SELECT {selected} FROM {}.{} WHERE {filter}",
                    q(container),
                    q(&base)
                ));
                let mut sql = format!(
                    "CREATE MATERIALIZED VIEW {}.{} AS\n    SELECT {selected} FROM {}.{}\n    WHERE {filter}\n    {}",
                    q(container),
                    q(&name),
                    q(container),
                    q(&base),
                    primary_key_clause(&cols)
                );
                if let Some(o) = clustering_order(&cols) {
                    sql.push_str(&format!("\n    WITH {o}"));
                }
                sql.push(';');
                t.server_sql = Some(sql);
                t.comment = Some(format!("view of {base}"));
                content.tables.push(t);
            }
            content.tables.sort_by(|a, b| a.name.cmp(&b.name));
            if only.is_none() {
                content.routines = routines(conn, container).await?;
            }
            Ok(content)
        })
    }
}

/// Functions, aggregates and user-defined types of a keyspace.
async fn routines(conn: &mut dyn Connection, keyspace: &str) -> Result<Vec<Routine>> {
    let ks = quote_literal(keyspace);
    let mut out = Vec::new();
    for r in query(
        conn,
        format!(
            "SELECT function_name, argument_names, argument_types, return_type, language, body, \
             called_on_null_input FROM system_schema.functions WHERE keyspace_name = {ks}"
        ),
    )
    .await?
    {
        let Some(name) = r.text("function_name") else {
            continue;
        };
        let names = json_list(r.text("argument_names"));
        let types = json_list(r.text("argument_types"));
        let returns = r.string("return_type");
        let language = r.string("language");
        let args: Vec<String> = names
            .iter()
            .zip(&types)
            .map(|(n, t)| format!("{} {t}", q(n)))
            .collect();
        let definition = format!(
            "CREATE FUNCTION {}.{}({})\n    {} ON NULL INPUT\n    RETURNS {returns}\n    LANGUAGE {language}\n    AS $${}$$;",
            q(keyspace),
            q(&name),
            args.join(", "),
            if r.bool("called_on_null_input") {
                "CALLED"
            } else {
                "RETURNS NULL"
            },
            r.string("body")
        );
        out.push(Routine {
            signature: signature(&name, &types),
            name,
            kind: RoutineKind::Function,
            arguments: names
                .iter()
                .zip(&types)
                .map(|(n, t)| Argument {
                    name: Some(n.clone()),
                    data_type: t.clone(),
                    mode: "IN".into(),
                })
                .collect(),
            returns: Some(returns),
            language: Some(language),
            definition: Some(definition),
            ..Default::default()
        });
    }
    for r in query(
        conn,
        format!(
            "SELECT aggregate_name, argument_types, state_func, state_type, final_func, initcond, \
             return_type FROM system_schema.aggregates WHERE keyspace_name = {ks}"
        ),
    )
    .await?
    {
        let Some(name) = r.text("aggregate_name") else {
            continue;
        };
        let types = json_list(r.text("argument_types"));
        let mut def = format!(
            "CREATE AGGREGATE {}.{}({})\n    SFUNC {}\n    STYPE {}",
            q(keyspace),
            q(&name),
            types.join(", "),
            q(&r.string("state_func")),
            r.string("state_type")
        );
        if let Some(f) = r.text("final_func") {
            def.push_str(&format!("\n    FINALFUNC {}", q(&f)));
        }
        if let Some(i) = r.text("initcond") {
            def.push_str(&format!("\n    INITCOND {i}"));
        }
        def.push(';');
        out.push(Routine {
            signature: signature(&name, &types),
            name,
            kind: RoutineKind::Function,
            arguments: types
                .iter()
                .map(|t| Argument {
                    name: None,
                    data_type: t.clone(),
                    mode: "IN".into(),
                })
                .collect(),
            returns: r.text("return_type"),
            language: Some("aggregate".into()),
            comment: Some("aggregate".into()),
            definition: Some(def),
        });
    }
    for r in query(
        conn,
        format!(
            "SELECT type_name, field_names, field_types FROM system_schema.types \
             WHERE keyspace_name = {ks}"
        ),
    )
    .await?
    {
        let Some(name) = r.text("type_name") else {
            continue;
        };
        let fields: Vec<String> = json_list(r.text("field_names"))
            .iter()
            .zip(json_list(r.text("field_types")))
            .map(|(n, t)| format!("    {} {t}", q(n)))
            .collect();
        out.push(Routine {
            signature: name.clone(),
            definition: Some(format!(
                "CREATE TYPE {}.{} (\n{}\n);",
                q(keyspace),
                q(&name),
                fields.join(",\n")
            )),
            name,
            kind: RoutineKind::Function,
            returns: Some("type".into()),
            language: Some("type".into()),
            comment: Some("user-defined type".into()),
            ..Default::default()
        });
    }
    out.sort_by(|a, b| a.signature.cmp(&b.signature));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str, kind: &str, pos: i64, order: &str) -> CqlColumn {
        CqlColumn {
            name: name.into(),
            data_type: ty.into(),
            kind: kind.into(),
            position: pos,
            clustering_order: order.into(),
        }
    }

    #[test]
    fn ddl_with_compound_keys() {
        let cols = vec![
            col("v", "text", "regular", -1, "none"),
            col("day", "date", "partition_key", 1, "none"),
            col("sensor", "uuid", "partition_key", 0, "none"),
            col("ts", "timestamp", "clustering", 0, "desc"),
            col("seq", "int", "clustering", 1, "asc"),
            col("Owner", "text", "static", -1, "none"),
        ];
        let s = create_table(
            "iot",
            "readings",
            &cols,
            &[("comment".into(), "'raw data'".into())],
        );
        assert_eq!(
            s,
            "CREATE TABLE iot.readings (\n    sensor uuid,\n    day date,\n    ts timestamp,\n    seq int,\n    \"Owner\" text STATIC,\n    v text,\n    PRIMARY KEY ((sensor, day), ts, seq)\n) WITH CLUSTERING ORDER BY (ts DESC, seq ASC)\n    AND comment = 'raw data';"
        );
        let simple = vec![col("id", "int", "partition_key", 0, "none")];
        assert_eq!(primary_key_clause(&simple), "PRIMARY KEY (id)");
        assert_eq!(clustering_order(&simple), None);
    }

    #[test]
    fn replication_and_maps() {
        assert_eq!(
            replication_text(
                r#"{"class":"org.apache.cassandra.locator.SimpleStrategy","replication_factor":"1"}"#
            )
            .as_deref(),
            Some("SimpleStrategy, replication_factor 1")
        );
        assert_eq!(
            map_literal(r#"{"class":"LZ4Compressor","chunk_length_in_kb":"16"}"#).as_deref(),
            Some("{'chunk_length_in_kb': '16', 'class': 'LZ4Compressor'}")
        );
        assert_eq!(json_list(Some(r#"["a","b"]"#.into())), vec!["a", "b"]);
    }
}
