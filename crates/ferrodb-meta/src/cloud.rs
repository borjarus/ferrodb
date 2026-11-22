//! Snowflake and Google BigQuery introspection (plan, phase 8). Both are
//! reached through ADBC (`ferrodb-adbc`); the catalog comes from their
//! `INFORMATION_SCHEMA` views, which return flat rows the [`rows`] reader
//! understands, plus `SHOW PRIMARY KEYS` / `SHOW IMPORTED KEYS` on
//! Snowflake. BigQuery datasets are listed with the ADBC `GetObjects` call
//! (`Request::Native { method: "get_objects" }`, flattened by the driver),
//! which needs no region qualifier, with `INFORMATION_SCHEMA.SCHEMATA` as
//! the fallback.
//!
//! Containers: the schemas of the connection's database (Snowflake
//! `database → schema`), the datasets of the connection's project
//! (BigQuery `project → dataset`). The database / project itself is the
//! data source's `database` / `project` property.

use ferrodb_api::Result;
use ferrodb_drivers::{BoxFuture, CancellationToken, Connection, Request, sink::collect_request};

use crate::{
    Introspector,
    model::*,
    rows::{Row, query, rows},
};

pub struct SnowflakeIntrospector;
pub struct BigQueryIntrospector;

fn sf_literal(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
}

fn sf_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `NUMBER(10,2)`, `VARCHAR(20)`, `TIMESTAMP_NTZ(9)` from the
/// `INFORMATION_SCHEMA.COLUMNS` parts.
pub(crate) fn snowflake_type(r: &Row) -> String {
    let ty = r.string("type");
    match ty.as_str() {
        "NUMBER" => match (r.int("prec"), r.int("scale")) {
            (Some(p), Some(s)) => format!("NUMBER({p},{s})"),
            _ => ty,
        },
        "TEXT" => match r.int("len") {
            Some(16_777_216) | None => "VARCHAR".into(),
            Some(n) => format!("VARCHAR({n})"),
        },
        "BINARY" => match r.int("len") {
            Some(n) => format!("BINARY({n})"),
            None => ty,
        },
        t if t.starts_with("TIMESTAMP") || t == "TIME" => match r.int("dtp") {
            Some(p) => format!("{t}({p})"),
            None => ty,
        },
        _ => ty,
    }
}

pub(crate) fn snowflake_assemble(
    rels: Vec<Row>,
    cols: Vec<Row>,
    views: Vec<Row>,
    pks: Vec<Row>,
    fks: Vec<Row>,
) -> Vec<Table> {
    let mut tables: Vec<Table> = rels
        .iter()
        .map(|r| {
            let kind = match r.string("kind").as_str() {
                "VIEW" => TableKind::View,
                "MATERIALIZED VIEW" => TableKind::MaterializedView,
                "EXTERNAL TABLE" => TableKind::Foreign,
                _ => TableKind::Table,
            };
            Table {
                name: r.string("name"),
                kind,
                comment: r.text("comment"),
                rows: r.int("row_count"),
                ..Default::default()
            }
        })
        .collect();
    let find = |tables: &mut [Table], name: &str| tables.iter().position(|t| t.name == name);
    for r in &cols {
        let Some(i) = find(&mut tables, r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        tables[i].columns.push(Column {
            name: r.string("name"),
            data_type: snowflake_type(r),
            nullable: r.bool("nullable"),
            default: r.text("def"),
            comment: r.text("comment"),
            identity: r.bool("is_identity").then(|| "BY DEFAULT".to_owned()),
            ..Default::default()
        });
    }
    for r in &views {
        if let Some(i) = find(&mut tables, r.get("name").unwrap_or_default()) {
            tables[i].view_definition = r.text("view_definition");
        }
    }
    // SHOW PRIMARY KEYS: one row per column, `key_sequence` orders them.
    let mut pks: Vec<&Row> = pks.iter().collect();
    pks.sort_by_key(|r| r.int("key_sequence").unwrap_or(0));
    for r in pks {
        let Some(i) = find(&mut tables, r.get("table_name").unwrap_or_default()) else {
            continue;
        };
        let pk = tables[i].primary_key.get_or_insert_with(|| Key {
            name: r.text("constraint_name"),
            columns: Vec::new(),
        });
        pk.columns.push(r.string("column_name"));
    }
    let mut fks: Vec<&Row> = fks.iter().collect();
    fks.sort_by_key(|r| r.int("key_sequence").unwrap_or(0));
    for r in fks {
        let Some(i) = find(&mut tables, r.get("fk_table_name").unwrap_or_default()) else {
            continue;
        };
        let name = r.text("fk_name");
        let t = &mut tables[i];
        let fk = match t.foreign_keys.iter_mut().position(|f| f.name == name) {
            Some(j) => &mut t.foreign_keys[j],
            None => {
                t.foreign_keys.push(ForeignKey {
                    name: name.clone(),
                    ref_table: r.string("pk_table_name"),
                    ref_container: r.text("pk_schema_name"),
                    on_delete: r.text("delete_rule").filter(|x| x != "NO ACTION"),
                    on_update: r.text("update_rule").filter(|x| x != "NO ACTION"),
                    ..Default::default()
                });
                t.foreign_keys.last_mut().expect("just pushed")
            }
        };
        fk.columns.push(r.string("fk_column_name"));
        fk.ref_columns.push(r.string("pk_column_name"));
    }
    tables
}

fn routines(rows: &[Row], kind: RoutineKind) -> Vec<Routine> {
    rows.iter()
        .map(|r| {
            let name = r.string("name");
            let args = r.string("args");
            Routine {
                signature: format!("{name}{args}"),
                name,
                kind,
                returns: r.text("returns"),
                language: r.text("lang"),
                comment: r.text("comment"),
                definition: r.text("definition"),
                ..Default::default()
            }
        })
        .collect()
}

impl Introspector for SnowflakeIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::Snowflake
    }

    fn containers<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
    ) -> BoxFuture<'a, Result<Vec<Container>>> {
        Box::pin(async move {
            let rows = query(
                conn,
                "select schema_name as name, comment from information_schema.schemata \
                 where schema_name <> 'INFORMATION_SCHEMA' order by schema_name",
            )
            .await?;
            Ok(rows
                .into_iter()
                .map(|r| Container {
                    name: r.string("name"),
                    comment: r.text("comment"),
                    content: None,
                })
                .collect())
        })
    }

    fn content<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
        container: &'a str,
        only: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Content>> {
        Box::pin(async move {
            let s = sf_literal(container);
            let f = only
                .map(|t| format!(" and table_name = {}", sf_literal(t)))
                .unwrap_or_default();
            let rels = query(
                conn,
                format!(
                    "select table_name as name, table_type as kind, comment, row_count \
                     from information_schema.tables where table_schema = {s}{f} order by table_name"
                ),
            )
            .await?;
            let cols = query(
                conn,
                format!(
                    "select table_name as tbl, column_name as name, data_type as type, \
                     character_maximum_length as len, numeric_precision as prec, \
                     numeric_scale as scale, datetime_precision as dtp, is_nullable as nullable, \
                     column_default as def, comment, is_identity \
                     from information_schema.columns where table_schema = {s}{f} \
                     order by table_name, ordinal_position"
                ),
            )
            .await?;
            let views = query(
                conn,
                format!(
                    "select table_name as name, view_definition from information_schema.views \
                     where table_schema = {s}{f}"
                ),
            )
            .await?;
            let schema = sf_ident(container);
            // Keys need privileges on the tables: missing ones are not an error.
            let pks = query(conn, format!("show primary keys in schema {schema}"))
                .await
                .unwrap_or_default();
            let fks = query(conn, format!("show imported keys in schema {schema}"))
                .await
                .unwrap_or_default();
            let mut content = Content {
                tables: snowflake_assemble(rels, cols, views, pks, fks),
                ..Default::default()
            };
            if only.is_none() {
                let funcs = query(
                    conn,
                    format!(
                        "select function_name as name, argument_signature as args, \
                         data_type as returns, function_language as lang, comment, \
                         function_definition as definition from information_schema.functions \
                         where function_schema = {s} order by function_name"
                    ),
                )
                .await
                .unwrap_or_default();
                let procs = query(
                    conn,
                    format!(
                        "select procedure_name as name, argument_signature as args, \
                         data_type as returns, procedure_language as lang, comment, \
                         procedure_definition as definition from information_schema.procedures \
                         where procedure_schema = {s} order by procedure_name"
                    ),
                )
                .await
                .unwrap_or_default();
                content.routines = routines(&funcs, RoutineKind::Function);
                content
                    .routines
                    .extend(routines(&procs, RoutineKind::Procedure));
                let seqs = query(
                    conn,
                    format!(
                        "select sequence_name as name, data_type, start_value as start, \
                         increment as inc, minimum_value as min, maximum_value as max, \
                         cycle_option as cycle from information_schema.sequences \
                         where sequence_schema = {s} order by sequence_name"
                    ),
                )
                .await
                .unwrap_or_default();
                content.sequences = seqs
                    .iter()
                    .map(|r| Sequence {
                        name: r.string("name"),
                        data_type: r.text("data_type"),
                        start: r.int("start"),
                        increment: r.int("inc"),
                        min: r.int("min"),
                        max: r.int("max"),
                        cache: None,
                        cycle: r.bool("cycle"),
                    })
                    .collect();
            }
            Ok(content)
        })
    }
}

/// A BigQuery dataset-qualified `INFORMATION_SCHEMA` view.
fn bq_view(dataset: &str, view: &str) -> String {
    format!("`{}`.INFORMATION_SCHEMA.{view}", dataset.replace('`', ""))
}

/// `"text"` (a GoogleSQL string literal in `TABLE_OPTIONS`) → `text`.
fn bq_option_text(v: &str) -> String {
    let v = v.trim();
    let inner = v
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(v);
    inner.replace("\\\"", "\"").replace("\\n", "\n")
}

pub(crate) fn bigquery_assemble(
    rels: Vec<Row>,
    cols: Vec<Row>,
    options: Vec<Row>,
    views: Vec<Row>,
    keys: Vec<Row>,
) -> Vec<Table> {
    let mut tables: Vec<Table> = rels
        .iter()
        .map(|r| {
            let kind = match r.string("kind").as_str() {
                "VIEW" => TableKind::View,
                "MATERIALIZED VIEW" => TableKind::MaterializedView,
                "EXTERNAL" => TableKind::Foreign,
                _ => TableKind::Table,
            };
            Table {
                name: r.string("name"),
                kind,
                server_sql: r.text("ddl"),
                ..Default::default()
            }
        })
        .collect();
    let find = |tables: &mut [Table], name: &str| tables.iter().position(|t| t.name == name);
    for r in &cols {
        let Some(i) = find(&mut tables, r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        let name = r.string("name");
        if r.bool("part") {
            tables[i].partition_by = Some(name.clone());
        }
        tables[i].columns.push(Column {
            name,
            data_type: r.string("type"),
            nullable: r.bool("nullable"),
            default: r.text("def").filter(|d| d != "NULL"),
            ..Default::default()
        });
    }
    for r in &options {
        if let (Some(i), Some(v)) = (
            find(&mut tables, r.get("tbl").unwrap_or_default()),
            r.text("option_value"),
        ) {
            tables[i].comment = Some(bq_option_text(&v));
        }
    }
    for r in &views {
        if let Some(i) = find(&mut tables, r.get("name").unwrap_or_default()) {
            tables[i].view_definition = r.text("view_definition");
        }
    }
    for r in &keys {
        if r.get("type") != Some("PRIMARY KEY") {
            continue;
        }
        let Some(i) = find(&mut tables, r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        let pk = tables[i].primary_key.get_or_insert_with(|| Key {
            name: r.text("name"),
            columns: Vec::new(),
        });
        pk.columns.push(r.string("col"));
    }
    tables
}

impl Introspector for BigQueryIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::BigQuery
    }

    fn containers<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
    ) -> BoxFuture<'a, Result<Vec<Container>>> {
        Box::pin(async move {
            let native = collect_request(
                conn,
                Request::native("get_objects", &[("depth", "schemas")]),
                CancellationToken::new(),
            )
            .await;
            let names: Vec<String> = match native {
                Ok(out) => rows(&out.results)
                    .iter()
                    .filter_map(|r| r.text("db_schema_name"))
                    .collect(),
                Err(_) => query(
                    conn,
                    "select schema_name as db_schema_name from INFORMATION_SCHEMA.SCHEMATA \
                     order by schema_name",
                )
                .await?
                .iter()
                .filter_map(|r| r.text("db_schema_name"))
                .collect(),
            };
            let mut names = names;
            names.sort();
            names.dedup();
            Ok(names
                .into_iter()
                .map(|name| Container {
                    name,
                    comment: None,
                    content: None,
                })
                .collect())
        })
    }

    fn content<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
        container: &'a str,
        only: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Content>> {
        Box::pin(async move {
            let ds = container;
            let w = |col: &str| {
                only.map(|t| {
                    format!(
                        " where {col} = '{}'",
                        t.replace('\\', "\\\\").replace('\'', "\\'")
                    )
                })
                .unwrap_or_default()
            };
            let and = |col: &str| w(col).replacen(" where ", " and ", 1);
            let rels = query(
                conn,
                format!(
                    "select table_name as name, table_type as kind, ddl from {}{} order by table_name",
                    bq_view(ds, "TABLES"),
                    w("table_name")
                ),
            )
            .await?;
            let cols = query(
                conn,
                format!(
                    "select table_name as tbl, column_name as name, data_type as type, \
                     is_nullable as nullable, column_default as def, \
                     is_partitioning_column as part from {}{} \
                     order by table_name, ordinal_position",
                    bq_view(ds, "COLUMNS"),
                    w("table_name")
                ),
            )
            .await?;
            let options = query(
                conn,
                format!(
                    "select table_name as tbl, option_value from {} \
                     where option_name = 'description'{}",
                    bq_view(ds, "TABLE_OPTIONS"),
                    and("table_name")
                ),
            )
            .await
            .unwrap_or_default();
            let views = query(
                conn,
                format!(
                    "select table_name as name, view_definition from {}{}",
                    bq_view(ds, "VIEWS"),
                    w("table_name")
                ),
            )
            .await
            .unwrap_or_default();
            let keys = query(
                conn,
                format!(
                    "select k.table_name as tbl, k.column_name as col, k.constraint_name as name, \
                     c.constraint_type as type from {} k join {} c using (constraint_name){} \
                     order by k.table_name, k.ordinal_position",
                    bq_view(ds, "KEY_COLUMN_USAGE"),
                    bq_view(ds, "TABLE_CONSTRAINTS"),
                    w("k.table_name")
                ),
            )
            .await
            .unwrap_or_default();
            let mut content = Content {
                tables: bigquery_assemble(rels, cols, options, views, keys),
                ..Default::default()
            };
            if only.is_none() {
                let rs = query(
                    conn,
                    format!(
                        "select routine_name as name, routine_type as kind, data_type as returns, \
                         coalesce(external_language, routine_body) as lang, ddl as definition \
                         from {} order by routine_name",
                        bq_view(ds, "ROUTINES")
                    ),
                )
                .await
                .unwrap_or_default();
                content.routines = rs
                    .iter()
                    .map(|r| {
                        let name = r.string("name");
                        Routine {
                            signature: name.clone(),
                            name,
                            kind: if r.get("kind") == Some("PROCEDURE") {
                                RoutineKind::Procedure
                            } else {
                                RoutineKind::Function
                            },
                            returns: r.text("returns"),
                            language: r.text("lang"),
                            definition: r.text("definition"),
                            ..Default::default()
                        }
                    })
                    .collect();
            }
            Ok(content)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::row;

    #[test]
    fn snowflake_catalog() {
        let t = snowflake_assemble(
            vec![
                row(&[
                    ("name", Some("ORDERS")),
                    ("kind", Some("BASE TABLE")),
                    ("comment", None),
                    ("row_count", Some("10")),
                ]),
                row(&[
                    ("name", Some("V")),
                    ("kind", Some("VIEW")),
                    ("comment", Some("a view")),
                    ("row_count", None),
                ]),
            ],
            vec![
                row(&[
                    ("tbl", Some("ORDERS")),
                    ("name", Some("ID")),
                    ("type", Some("NUMBER")),
                    ("len", None),
                    ("prec", Some("38")),
                    ("scale", Some("0")),
                    ("dtp", None),
                    ("nullable", Some("NO")),
                    ("def", None),
                    ("comment", None),
                    ("is_identity", Some("YES")),
                ]),
                row(&[
                    ("tbl", Some("ORDERS")),
                    ("name", Some("NOTE")),
                    ("type", Some("TEXT")),
                    ("len", Some("20")),
                    ("prec", None),
                    ("scale", None),
                    ("dtp", None),
                    ("nullable", Some("YES")),
                    ("def", None),
                    ("comment", None),
                    ("is_identity", Some("NO")),
                ]),
            ],
            vec![row(&[
                ("name", Some("V")),
                ("view_definition", Some("create view V as select 1")),
            ])],
            vec![row(&[
                ("table_name", Some("ORDERS")),
                ("column_name", Some("ID")),
                ("key_sequence", Some("1")),
                ("constraint_name", Some("PK_ORDERS")),
            ])],
            vec![],
        );
        let o = &t[0];
        assert_eq!(o.columns[0].data_type, "NUMBER(38,0)");
        assert!(!o.columns[0].nullable);
        assert_eq!(o.columns[0].identity.as_deref(), Some("BY DEFAULT"));
        assert_eq!(o.columns[1].data_type, "VARCHAR(20)");
        assert_eq!(o.primary_key.as_ref().unwrap().columns, vec!["ID"]);
        assert_eq!(o.rows, Some(10));
        assert_eq!(t[1].kind, TableKind::View);
        assert!(t[1].view_definition.is_some());
    }

    #[test]
    fn bigquery_catalog() {
        let t = bigquery_assemble(
            vec![row(&[
                ("name", Some("events")),
                ("kind", Some("BASE TABLE")),
                (
                    "ddl",
                    Some("CREATE TABLE `p.ds.events`\n(\n  id INT64 NOT NULL\n)\nPARTITION BY d;"),
                ),
            ])],
            vec![
                row(&[
                    ("tbl", Some("events")),
                    ("name", Some("id")),
                    ("type", Some("INT64")),
                    ("nullable", Some("NO")),
                    ("def", Some("NULL")),
                    ("part", Some("NO")),
                ]),
                row(&[
                    ("tbl", Some("events")),
                    ("name", Some("d")),
                    ("type", Some("DATE")),
                    ("nullable", Some("YES")),
                    ("def", Some("CURRENT_DATE()")),
                    ("part", Some("YES")),
                ]),
            ],
            vec![row(&[
                ("tbl", Some("events")),
                ("option_value", Some("\"All \\\"events\\\"\"")),
            ])],
            vec![],
            vec![row(&[
                ("tbl", Some("events")),
                ("col", Some("id")),
                ("name", Some("events.pk$")),
                ("type", Some("PRIMARY KEY")),
            ])],
        );
        let e = &t[0];
        assert_eq!(e.partition_by.as_deref(), Some("d"));
        assert_eq!(e.columns[0].default, None);
        assert_eq!(e.columns[1].default.as_deref(), Some("CURRENT_DATE()"));
        assert_eq!(e.comment.as_deref(), Some("All \"events\""));
        assert_eq!(e.primary_key.as_ref().unwrap().columns, vec!["id"]);
        assert!(e.server_sql.as_deref().unwrap().starts_with("CREATE TABLE"));
        assert_eq!(bq_view("ds", "TABLES"), "`ds`.INFORMATION_SCHEMA.TABLES");
    }
}
