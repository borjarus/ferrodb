//! DuckDB introspection from its metadata functions (`duckdb_tables()`,
//! `duckdb_columns()`, `duckdb_constraints()`…), for file data sources and
//! DuckDB database files (plan, phase 7). Containers are the schemas of the
//! current database. Every value is cast to text in the query, so the rows
//! read the same whatever Arrow types DuckDB picks.

use ferrodb_api::Result;
use ferrodb_drivers::{BoxFuture, Connection, quote_literal};

use crate::{
    Introspector,
    model::*,
    rows::{Row, query},
};

pub struct DuckDbIntrospector;

// Temporary objects (the views of data files) live in the `temp` database
// under the same schema names: both are listed together. (`main` counts as
// `internal` in `duckdb_schemas()`, so catalog schemas are left out by name.)
const CONTAINERS: &str = "select schema_name::varchar as name, max(comment)::varchar as comment \
     from duckdb_schemas() where database_name in (current_database(), 'temp') \
     and schema_name not in ('information_schema', 'pg_catalog') group by schema_name \
     order by (schema_name = current_schema()) desc, schema_name";

const DB: &str = "database_name in (current_database(), 'temp')";

fn tables(s: &str, only: &str) -> String {
    format!(
        "select table_name::varchar as name, 'table' as kind, comment::varchar as comment, \
         estimated_size::varchar as row_estimate, sql::varchar as sql \
         from duckdb_tables() where {DB} and schema_name = {s}{only} \
         union all \
         select view_name::varchar, 'view', comment::varchar, null, sql::varchar \
         from duckdb_views() where {DB} and schema_name = {s} and not internal{only_v} \
         order by name",
        only_v = only.replace("table_name", "view_name")
    )
}

fn columns(s: &str, only: &str) -> String {
    format!(
        "select table_name::varchar as tbl, column_name::varchar as name, \
         data_type::varchar as type, is_nullable::varchar as nullable, \
         column_default::varchar as def, comment::varchar as comment \
         from duckdb_columns() where {DB} and schema_name = {s}{only} \
         order by table_name, column_index"
    )
}

fn constraints(s: &str, only: &str) -> String {
    format!(
        "select table_name::varchar as tbl, constraint_type::varchar as type, \
         constraint_name::varchar as name, \
         array_to_string(constraint_column_names, chr(31)) as cols, \
         referenced_table::varchar as ref_table, \
         array_to_string(referenced_column_names, chr(31)) as ref_cols, \
         expression::varchar as expression \
         from duckdb_constraints() where {DB} and schema_name = {s}{only} \
         order by table_name, constraint_index"
    )
}

fn indexes(s: &str, only: &str) -> String {
    format!(
        "select table_name::varchar as tbl, index_name::varchar as name, \
         is_unique::varchar as uniq, sql::varchar as sql \
         from duckdb_indexes() where {DB} and schema_name = {s}{only} \
         order by table_name, index_name"
    )
}

fn sequences(s: &str) -> String {
    format!(
        "select sequence_name::varchar as name, start_value::varchar as start, \
         increment_by::varchar as inc, min_value::varchar as min, max_value::varchar as max, \
         cycle::varchar as cycle from duckdb_sequences() where {DB} and schema_name = {s} \
         order by sequence_name"
    )
}

/// `SELECT …` of `CREATE VIEW v AS SELECT …`.
fn view_select(sql: &str) -> Option<String> {
    let upper = sql.to_ascii_uppercase();
    let i = upper.find(" AS ")?;
    Some(sql[i + 4..].trim().trim_end_matches(';').to_owned())
}

/// Columns of `CREATE INDEX i ON t(a, b)`.
fn index_columns(sql: &str) -> Vec<String> {
    let (Some(a), Some(b)) = (sql.find('('), sql.rfind(')')) else {
        return Vec::new();
    };
    if b <= a {
        return Vec::new();
    }
    sql[a + 1..b]
        .split(',')
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty())
        .collect()
}

pub(crate) fn assemble(
    rels: Vec<Row>,
    cols: Vec<Row>,
    cons: Vec<Row>,
    idx: Vec<Row>,
) -> Vec<Table> {
    let mut tables: Vec<Table> = rels
        .iter()
        .map(|r| {
            let view = r.get("kind") == Some("view");
            let sql = r.text("sql");
            Table {
                name: r.string("name"),
                kind: if view {
                    TableKind::View
                } else {
                    TableKind::Table
                },
                comment: r.text("comment"),
                rows: r.int("row_estimate"),
                view_definition: if view {
                    sql.as_deref().and_then(view_select)
                } else {
                    None
                },
                server_sql: sql,
                ..Default::default()
            }
        })
        .collect();
    let find = |tables: &mut Vec<Table>, name: &str| tables.iter().position(|t| t.name == name);
    for r in &cols {
        let Some(ti) = find(&mut tables, r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        tables[ti].columns.push(Column {
            name: r.string("name"),
            data_type: r.string("type"),
            nullable: r.bool("nullable"),
            default: r.text("def"),
            comment: r.text("comment"),
            ..Default::default()
        });
    }
    for r in &cons {
        let Some(ti) = find(&mut tables, r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        let columns = r.list("cols");
        let t = &mut tables[ti];
        match r.get("type").unwrap_or_default() {
            "PRIMARY KEY" => {
                t.primary_key = Some(Key {
                    name: None,
                    columns,
                })
            }
            "UNIQUE" => t.uniques.push(Key {
                name: None,
                columns,
            }),
            "FOREIGN KEY" => t.foreign_keys.push(ForeignKey {
                name: None,
                columns,
                ref_table: r.string("ref_table"),
                ref_columns: r.list("ref_cols"),
                ..Default::default()
            }),
            "CHECK" => {
                if let Some(e) = r.text("expression") {
                    t.checks.push(Check {
                        name: None,
                        expression: e,
                    });
                }
            }
            _ => {}
        }
    }
    for r in &idx {
        let Some(ti) = find(&mut tables, r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        let sql = r.text("sql");
        tables[ti].indexes.push(Index {
            name: r.string("name"),
            columns: sql.as_deref().map(index_columns).unwrap_or_default(),
            unique: r.bool("uniq"),
            server_sql: sql,
            ..Default::default()
        });
    }
    tables
}

impl Introspector for DuckDbIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::DuckDb
    }

    fn containers<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
    ) -> BoxFuture<'a, Result<Vec<Container>>> {
        Box::pin(async move {
            Ok(query(conn, CONTAINERS)
                .await?
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
            let s = quote_literal(container);
            let f = only
                .map(|t| format!(" and table_name = {}", quote_literal(t)))
                .unwrap_or_default();
            let rels = query(conn, tables(&s, &f)).await?;
            let cols = query(conn, columns(&s, &f)).await?;
            let cons = query(conn, constraints(&s, &f)).await?;
            let idx = query(conn, indexes(&s, &f)).await?;
            let seqs = if only.is_none() {
                query(conn, sequences(&s)).await?
            } else {
                Vec::new()
            };
            Ok(Content {
                tables: assemble(rels, cols, cons, idx),
                sequences: seqs
                    .iter()
                    .map(|r| Sequence {
                        name: r.string("name"),
                        data_type: None,
                        start: r.int("start"),
                        increment: r.int("inc"),
                        min: r.int("min"),
                        max: r.int("max"),
                        cache: None,
                        cycle: r.bool("cycle"),
                    })
                    .collect(),
                ..Default::default()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::row;

    #[test]
    fn assembles_tables_keys_and_views() {
        let cols_pk = "id".to_owned();
        let fk_cols = "customer_id".to_owned();
        let t = assemble(
            vec![
                row(&[
                    ("name", Some("orders")),
                    ("kind", Some("table")),
                    ("sql", Some("CREATE TABLE orders(...);")),
                    ("comment", None),
                    ("row_estimate", Some("3")),
                ]),
                row(&[
                    ("name", Some("v")),
                    ("kind", Some("view")),
                    ("sql", Some("CREATE VIEW v AS SELECT 1;")),
                    ("comment", None),
                    ("row_estimate", None),
                ]),
            ],
            vec![
                row(&[
                    ("tbl", Some("orders")),
                    ("name", Some("id")),
                    ("type", Some("INTEGER")),
                    ("nullable", Some("false")),
                    ("def", None),
                    ("comment", None),
                ]),
                row(&[
                    ("tbl", Some("orders")),
                    ("name", Some("customer_id")),
                    ("type", Some("INTEGER")),
                    ("nullable", Some("true")),
                    ("def", None),
                    ("comment", None),
                ]),
            ],
            vec![
                row(&[
                    ("tbl", Some("orders")),
                    ("type", Some("PRIMARY KEY")),
                    ("name", None),
                    ("cols", Some(&cols_pk)),
                    ("ref_table", None),
                    ("ref_cols", None),
                    ("expression", None),
                ]),
                row(&[
                    ("tbl", Some("orders")),
                    ("type", Some("FOREIGN KEY")),
                    ("name", None),
                    ("cols", Some(&fk_cols)),
                    ("ref_table", Some("customers")),
                    ("ref_cols", Some("id")),
                    ("expression", None),
                ]),
            ],
            vec![row(&[
                ("tbl", Some("orders")),
                ("name", Some("ix")),
                ("uniq", Some("false")),
                ("sql", Some("CREATE INDEX ix ON orders(customer_id, id);")),
            ])],
        );
        assert_eq!(t.len(), 2);
        let o = &t[0];
        assert_eq!(o.columns.len(), 2);
        assert!(!o.columns[0].nullable);
        assert_eq!(o.primary_key.as_ref().unwrap().columns, vec!["id"]);
        assert_eq!(o.foreign_keys[0].ref_table, "customers");
        assert_eq!(o.indexes[0].columns, vec!["customer_id", "id"]);
        assert_eq!(o.rows, Some(3));
        assert_eq!(t[1].view_definition.as_deref(), Some("SELECT 1"));
    }
}
