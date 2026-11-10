//! SQLite introspection: `sqlite_master` (whose `sql` column is the exact
//! DDL, used verbatim) and the table-valued pragmas for columns, keys and
//! indexes. Containers are the attached databases (`main`, `temp`…).

use std::collections::HashMap;

use ferrodb_api::Result;
use ferrodb_drivers::{BoxFuture, Connection, quote_ident, quote_literal};

use crate::{
    Introspector,
    model::*,
    rows::{Row, query},
};

pub struct SqliteIntrospector;

const CONTAINERS: &str = "select name from pragma_database_list order by seq";

fn master(s: &str, only: &str) -> String {
    format!(
        "select type, name, tbl_name, sql from {s}.sqlite_master \
         where name not like 'sqlite\\_%' escape '\\'{only} order by type, name"
    )
}

fn columns(s: &str, lit: &str, only: &str) -> String {
    format!(
        "select m.name as tbl, p.cid as cid, p.name as name, p.type as type, \
         p.\"notnull\" as not_null, p.dflt_value as def, p.pk as pk, p.hidden as hidden \
         from {s}.sqlite_master m join pragma_table_xinfo(m.name, {lit}) p \
         where m.type in ('table', 'view') and m.name not like 'sqlite\\_%' escape '\\'{only} \
         order by m.name, p.cid"
    )
}

fn foreign_keys(s: &str, lit: &str, only: &str) -> String {
    format!(
        "select m.name as tbl, f.id as id, f.seq as seq, f.\"table\" as ref_table, \
         f.\"from\" as col, f.\"to\" as ref_col, f.on_update as on_update, f.on_delete as on_delete \
         from {s}.sqlite_master m join pragma_foreign_key_list(m.name, {lit}) f \
         where m.type = 'table'{only} order by m.name, f.id, f.seq"
    )
}

fn indexes(s: &str, lit: &str, only: &str) -> String {
    format!(
        "select m.name as tbl, il.name as name, il.\"unique\" as uniq, il.origin as origin, \
         il.partial as partial, \
         (select group_concat(coalesce(ii.name, '<expression>'), char(31)) \
            from pragma_index_info(il.name, {lit}) ii) as cols, \
         (select x.sql from {s}.sqlite_master x where x.type = 'index' and x.name = il.name) as sql \
         from {s}.sqlite_master m join pragma_index_list(m.name, {lit}) il \
         where m.type = 'table'{only} order by m.name, il.name"
    )
}

fn action(a: Option<String>) -> Option<String> {
    a.filter(|a| !a.eq_ignore_ascii_case("NO ACTION"))
}

pub(crate) fn assemble(
    master: Vec<Row>,
    cols: Vec<Row>,
    fks: Vec<Row>,
    idx: Vec<Row>,
) -> Vec<Table> {
    let mut tables: Vec<Table> = master
        .iter()
        .filter(|r| matches!(r.get("type"), Some("table" | "view")))
        .map(|r| {
            let view = r.get("type") == Some("view");
            let sql = r.text("sql");
            Table {
                name: r.string("name"),
                kind: if view {
                    TableKind::View
                } else {
                    TableKind::Table
                },
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
    let pos: HashMap<String, usize> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();
    let at = |name: Option<&str>| pos.get(name.unwrap_or_default()).copied();
    let mut pk: HashMap<usize, Vec<(i64, String)>> = HashMap::new();
    for r in &cols {
        let Some(ti) = at(r.get("tbl")) else { continue };
        let name = r.string("name");
        if let Some(n) = r.int("pk").filter(|n| *n > 0) {
            pk.entry(ti).or_default().push((n, name.clone()));
        }
        let hidden = r.int("hidden").unwrap_or(0);
        tables[ti].columns.push(Column {
            name,
            data_type: r.string("type"),
            nullable: !r.bool("not_null"),
            default: r.text("def"),
            generated: (hidden >= 2).then(|| "…".to_owned()),
            generated_stored: hidden == 3,
            ..Default::default()
        });
    }
    for (ti, mut cols) in pk {
        cols.sort();
        tables[ti].primary_key = Some(Key {
            name: None,
            columns: cols.into_iter().map(|(_, c)| c).collect(),
        });
    }
    for r in &fks {
        let Some(ti) = at(r.get("tbl")) else { continue };
        let id = r.string("id");
        let t = &mut tables[ti];
        let key = Some(format!("#{id}"));
        let col = r.string("col");
        let ref_col = r.text("ref_col");
        match t.foreign_keys.iter_mut().find(|f| f.name == key) {
            Some(f) => {
                f.columns.push(col);
                f.ref_columns.extend(ref_col);
            }
            None => t.foreign_keys.push(ForeignKey {
                name: key,
                columns: vec![col],
                ref_table: r.string("ref_table"),
                ref_columns: ref_col.into_iter().collect(),
                on_delete: action(r.text("on_delete")),
                on_update: action(r.text("on_update")),
                ..Default::default()
            }),
        }
    }
    // SQLite foreign keys are unnamed: `#<id>` was only a grouping key.
    for t in &mut tables {
        for f in &mut t.foreign_keys {
            f.name = None;
        }
    }
    for r in &idx {
        let Some(ti) = at(r.get("tbl")) else { continue };
        let columns = r.list("cols");
        match r.get("origin").unwrap_or_default() {
            "pk" => {}
            "u" => tables[ti].uniques.push(Key {
                name: None,
                columns,
            }),
            _ => tables[ti].indexes.push(Index {
                name: r.string("name"),
                columns,
                unique: r.bool("uniq"),
                predicate: r.bool("partial").then(|| "…".to_owned()),
                server_sql: r.text("sql"),
                ..Default::default()
            }),
        }
    }
    for r in master.iter().filter(|r| r.get("type") == Some("trigger")) {
        let Some(ti) = at(r.get("tbl_name")) else {
            continue;
        };
        let name = r.string("name");
        let def = r.text("sql");
        tables[ti].triggers.push(Trigger {
            event: def
                .as_deref()
                .and_then(|d| crate::pg::trigger_event(d, &name)),
            name,
            definition: def,
        });
    }
    tables
}

/// `SELECT …` of `CREATE VIEW v AS SELECT …`.
fn view_select(sql: &str) -> Option<String> {
    let upper = sql.to_ascii_uppercase();
    let i = upper.find(" AS ")?;
    Some(sql[i + 4..].trim().to_owned())
}

impl Introspector for SqliteIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
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
            let s = quote_ident(container, '"');
            let lit = quote_literal(container);
            let f = |col: &str| {
                only.map(|t| format!(" and {col} = {}", quote_literal(t)))
                    .unwrap_or_default()
            };
            let m = query(conn, master(&s, &f("tbl_name"))).await?;
            let cols = query(conn, columns(&s, &lit, &f("m.name"))).await?;
            let fks = query(conn, foreign_keys(&s, &lit, &f("m.name"))).await?;
            let idx = query(conn, indexes(&s, &lit, &f("m.name"))).await?;
            Ok(Content {
                tables: assemble(m, cols, fks, idx),
                ..Default::default()
            })
        })
    }
}
