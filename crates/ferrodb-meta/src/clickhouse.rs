//! ClickHouse introspection from the `system` database (plan, phase 8):
//! `system.databases` (containers), `system.tables` (engine, partition
//! key, sorting / primary key, view queries, dictionaries, the server's
//! `CREATE` statement), `system.columns` (types, `DEFAULT` /
//! `MATERIALIZED` / `ALIAS` expressions, codecs) and
//! `system.data_skipping_indices`. DDL comes from `create_table_query`,
//! which is exactly what the server would recreate (engine settings, TTL,
//! projections included).
//!
//! The sorting key becomes the table's primary key in the model: it is
//! what ClickHouse looks rows up by (edits use it as the row key), though
//! it does not enforce uniqueness.

use ferrodb_api::Result;
use ferrodb_drivers::{BoxFuture, Connection};

use crate::{
    Introspector,
    model::*,
    rows::{Row, query},
};

pub struct ClickHouseIntrospector;

/// ClickHouse string literal (backslash escapes are always on).
pub(crate) fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

const CONTAINERS: &str = "select name, comment from system.databases \
     where name not in ('INFORMATION_SCHEMA', 'information_schema') \
     order by name = currentDatabase() desc, name";

fn tables(db: &str, only: &str) -> String {
    format!(
        "select name, engine, engine_full, comment, toString(total_rows) as total_rows, \
         create_table_query, as_select, partition_key, sorting_key, primary_key, \
         toString(is_temporary) as is_temporary \
         from system.tables where database = {db}{only} order by name"
    )
}

fn columns(db: &str, only: &str) -> String {
    format!(
        "select table as tbl, name, type, default_kind, default_expression, comment, \
         compression_codec from system.columns where database = {db}{only} \
         order by table, position"
    )
}

fn skipping_indices(db: &str, only: &str) -> String {
    format!(
        "select table as tbl, name, type_full, expr, toString(granularity) as granularity \
         from system.data_skipping_indices where database = {db}{only} order by table, name"
    )
}

/// `a, b` (a key expression list) → `["a", "b"]`, keeping commas inside
/// parentheses.
pub(crate) fn split_key(expr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in expr.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                cur.push(c);
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '`' | '"') => {
                quote = Some(c);
                cur.push(c);
            }
            (None, '(') => {
                depth += 1;
                cur.push(c);
            }
            (None, ')') => {
                depth -= 1;
                cur.push(c);
            }
            (None, ',') if depth == 0 => {
                out.push(cur.trim().to_owned());
                cur.clear();
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    // `tuple()` = no key.
    out.retain(|k| k != "tuple()");
    out
}

pub(crate) fn assemble(rels: Vec<Row>, cols: Vec<Row>, idx: Vec<Row>) -> Vec<Table> {
    let mut tables: Vec<Table> = rels
        .iter()
        .map(|r| {
            let engine = r.string("engine");
            let kind = match engine.as_str() {
                "View" | "LiveView" | "WindowView" => TableKind::View,
                "MaterializedView" => TableKind::MaterializedView,
                _ => TableKind::Table,
            };
            let view = kind != TableKind::Table;
            let key = r
                .text("sorting_key")
                .or_else(|| r.text("primary_key"))
                .map(|k| split_key(&k))
                .filter(|k| !k.is_empty());
            Table {
                name: r.string("name"),
                kind,
                comment: r.text("comment"),
                primary_key: key.map(|columns| Key {
                    name: None,
                    columns,
                }),
                view_definition: if view { r.text("as_select") } else { None },
                server_sql: r.text("create_table_query"),
                partition_by: r.text("partition_key"),
                engine: Some(engine).filter(|e| !e.is_empty()),
                rows: r.int("total_rows"),
                ..Default::default()
            }
        })
        .collect();
    for r in &cols {
        let Some(t) = tables
            .iter_mut()
            .find(|t| t.name == r.get("tbl").unwrap_or_default())
        else {
            continue;
        };
        let ty = r.string("type");
        let expr = r.text("default_expression");
        let kind = r.string("default_kind");
        let (default, generated, stored) = match kind.as_str() {
            "MATERIALIZED" => (None, expr, true),
            "ALIAS" => (None, expr, false),
            _ => (expr, None, false),
        };
        t.columns.push(Column {
            name: r.string("name"),
            nullable: ty.starts_with("Nullable(") || ty.starts_with("LowCardinality(Nullable("),
            data_type: ty,
            default,
            generated,
            generated_stored: stored,
            comment: r.text("comment"),
            ..Default::default()
        });
    }
    for r in &idx {
        let Some(t) = tables
            .iter_mut()
            .find(|t| t.name == r.get("tbl").unwrap_or_default())
        else {
            continue;
        };
        let method = match (r.text("type_full"), r.text("granularity")) {
            (Some(ty), Some(g)) => Some(format!("{ty} GRANULARITY {g}")),
            (ty, _) => ty,
        };
        t.indexes.push(Index {
            name: r.string("name"),
            columns: r.text("expr").into_iter().collect(),
            method,
            ..Default::default()
        });
    }
    tables
}

impl Introspector for ClickHouseIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::ClickHouse
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
            let db = literal(container);
            let name = |col: &str| {
                only.map(|t| format!(" and {col} = {}", literal(t)))
                    .unwrap_or_default()
            };
            let rels = query(conn, tables(&db, &name("name"))).await?;
            let cols = query(conn, columns(&db, &name("table"))).await?;
            let idx = query(conn, skipping_indices(&db, &name("table")))
                .await
                .unwrap_or_default();
            Ok(Content {
                tables: assemble(rels, cols, idx),
                ..Default::default()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::row;

    /// Live: `FERRODB_TCK_CLICKHOUSE=clickhouse://default:ferrodb@localhost:56323/tck`.
    #[tokio::test]
    async fn live_introspection_and_ddl() {
        use ferrodb_drivers::{CancellationToken, Registry, ResolvedConfig, sink::collect};
        let Ok(url) = std::env::var("FERRODB_TCK_CLICKHOUSE") else {
            return;
        };
        let cfg = ResolvedConfig::from_url("clickhouse", &url).unwrap();
        let reg = Registry::builtin();
        let mut c = reg
            .get(&"clickhouse".into())
            .unwrap()
            .connect(&cfg)
            .await
            .unwrap();
        let db = format!("meta_{}", std::process::id());
        for st in [
            format!("drop database if exists {db}"),
            format!("create database {db} comment 'introspection test'"),
            format!(
                "create table {db}.events (id UInt64, d Date, v LowCardinality(String), \
                 tags Array(String), attrs Map(String, UInt32), n Nested(k String, x Int8), \
                 amount Nullable(Decimal(18, 4)), v2 String MATERIALIZED upper(v), \
                 INDEX ix_v v TYPE bloom_filter GRANULARITY 4) \
                 engine = MergeTree partition by toYYYYMM(d) order by (id, d) \
                 ttl d + interval 1 year comment 'events'"
            ),
            format!(
                "create materialized view {db}.daily engine = SummingMergeTree order by d \
                 as select d, count() as c from {db}.events group by d"
            ),
            format!("create view {db}.v as select id from {db}.events"),
        ] {
            collect(c.as_mut(), st.clone(), CancellationToken::new())
                .await
                .unwrap_or_else(|e| panic!("{st}: {e}"));
        }
        let i = crate::introspector(Dialect::ClickHouse);
        let containers = i.containers(c.as_mut()).await.unwrap();
        let mine = containers.iter().find(|x| x.name == db).unwrap();
        assert_eq!(mine.comment.as_deref(), Some("introspection test"));
        let content = i.content(c.as_mut(), &db, None).await.unwrap();
        let ev = content.table("events").unwrap();
        assert_eq!(ev.engine.as_deref(), Some("MergeTree"));
        assert_eq!(ev.partition_by.as_deref(), Some("toYYYYMM(d)"));
        assert_eq!(ev.primary_key.as_ref().unwrap().columns, vec!["id", "d"]);
        assert_eq!(ev.comment.as_deref(), Some("events"));
        assert_eq!(ev.indexes[0].name, "ix_v");
        let col = |n: &str| ev.column(n).unwrap();
        assert_eq!(col("tags").data_type, "Array(String)");
        assert_eq!(col("n.k").data_type, "Array(String)");
        assert!(col("amount").nullable);
        assert_eq!(col("v2").generated.as_deref(), Some("upper(v)"));
        let ddl = crate::ddl::object_ddl(
            &crate::ddl::Target::new(Dialect::ClickHouse, &db),
            crate::ddl::Object::Table(ev),
        );
        assert!(ddl.contains("TTL d + toIntervalYear(1)"), "{ddl}");
        assert_eq!(
            content.table("daily").unwrap().kind,
            TableKind::MaterializedView
        );
        assert!(
            content
                .table("v")
                .unwrap()
                .view_definition
                .as_deref()
                .unwrap()
                .contains("SELECT id")
        );
        // Incremental refresh of one table.
        let one = i.content(c.as_mut(), &db, Some("events")).await.unwrap();
        assert_eq!(one.tables.len(), 1);
        collect(
            c.as_mut(),
            format!("drop database {db}"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    }

    #[test]
    fn keys_and_assembly() {
        assert_eq!(
            split_key("toYYYYMM(d), intHash32(id), tuple(a, b)"),
            vec!["toYYYYMM(d)", "intHash32(id)", "tuple(a, b)"]
        );
        assert!(split_key("tuple()").is_empty());
        let t = assemble(
            vec![
                row(&[
                    ("name", Some("events")),
                    ("engine", Some("MergeTree")),
                    ("comment", None),
                    ("total_rows", Some("12")),
                    (
                        "create_table_query",
                        Some(
                            "CREATE TABLE tck.events (`id` UInt64) ENGINE = MergeTree ORDER BY id",
                        ),
                    ),
                    ("as_select", None),
                    ("partition_key", Some("toYYYYMM(d)")),
                    ("sorting_key", Some("id, d")),
                    ("primary_key", Some("id")),
                ]),
                row(&[
                    ("name", Some("mv")),
                    ("engine", Some("MaterializedView")),
                    ("comment", None),
                    ("total_rows", None),
                    (
                        "create_table_query",
                        Some("CREATE MATERIALIZED VIEW tck.mv …"),
                    ),
                    ("as_select", Some("SELECT id FROM tck.events")),
                    ("partition_key", None),
                    ("sorting_key", None),
                    ("primary_key", None),
                ]),
            ],
            vec![
                row(&[
                    ("tbl", Some("events")),
                    ("name", Some("id")),
                    ("type", Some("UInt64")),
                    ("default_kind", None),
                    ("default_expression", None),
                    ("comment", None),
                ]),
                row(&[
                    ("tbl", Some("events")),
                    ("name", Some("d2")),
                    ("type", Some("Nullable(Date)")),
                    ("default_kind", Some("MATERIALIZED")),
                    ("default_expression", Some("today()")),
                    ("comment", Some("day")),
                ]),
            ],
            vec![row(&[
                ("tbl", Some("events")),
                ("name", Some("ix")),
                ("type_full", Some("minmax")),
                ("expr", Some("d2")),
                ("granularity", Some("4")),
            ])],
        );
        let e = &t[0];
        assert_eq!(e.engine.as_deref(), Some("MergeTree"));
        assert_eq!(e.partition_by.as_deref(), Some("toYYYYMM(d)"));
        assert_eq!(e.primary_key.as_ref().unwrap().columns, vec!["id", "d"]);
        assert_eq!(e.rows, Some(12));
        assert!(e.columns[1].nullable);
        assert_eq!(e.columns[1].generated.as_deref(), Some("today()"));
        assert!(e.columns[1].generated_stored);
        assert_eq!(e.indexes[0].method.as_deref(), Some("minmax GRANULARITY 4"));
        assert_eq!(t[1].kind, TableKind::MaterializedView);
        assert_eq!(
            t[1].view_definition.as_deref(),
            Some("SELECT id FROM tck.events")
        );
    }
}
