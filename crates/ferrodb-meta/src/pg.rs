//! PostgreSQL introspection from `pg_catalog` (PostgreSQL ≥ 12 and close
//! derivatives). One query per object kind for a whole schema; with
//! `only = Some(table)` the table queries are narrowed to one relation
//! (incremental refresh) and routines / sequences are skipped.

use std::collections::{HashMap, HashSet};

use ferrodb_api::Result;
use ferrodb_drivers::{BoxFuture, Connection, quote_literal};

use crate::{
    Introspector,
    model::*,
    rows::{Row, query},
};

pub struct PgIntrospector;

const CONTAINERS: &str = r"select n.nspname as name, obj_description(n.oid, 'pg_namespace') as comment
from pg_catalog.pg_namespace n
where n.nspname not like 'pg\_toast%' and n.nspname not like 'pg\_temp\_%'
order by (n.nspname = current_schema()) desc, n.nspname";

fn relations(s: &str, only: &str) -> String {
    format!(
        "select c.relname as name, c.relkind::text as kind,
  obj_description(c.oid, 'pg_class') as comment,
  c.reltuples::bigint as row_estimate,
  case when c.relkind in ('v', 'm') then pg_catalog.pg_get_viewdef(c.oid) end as view_def,
  case when c.relkind = 'p' then pg_catalog.pg_get_partkeydef(c.oid) end as part_key,
  case when c.relispartition then (select p.relname from pg_catalog.pg_inherits i
     join pg_catalog.pg_class p on p.oid = i.inhparent where i.inhrelid = c.oid limit 1) end as parent,
  case when c.relispartition then pg_catalog.pg_get_expr(c.relpartbound, c.oid) end as bound
from pg_catalog.pg_class c join pg_catalog.pg_namespace n on n.oid = c.relnamespace
where n.nspname = {s} and c.relkind in ('r', 'p', 'f', 'v', 'm'){only}
order by c.relname"
    )
}

fn columns(s: &str, only: &str) -> String {
    format!(
        "select c.relname as tbl, a.attname as name,
  pg_catalog.format_type(a.atttypid, a.atttypmod) as type,
  a.attnotnull as notnull, pg_catalog.pg_get_expr(d.adbin, d.adrelid) as def,
  pg_catalog.col_description(c.oid, a.attnum) as comment,
  a.attidentity::text as identity, a.attgenerated::text as generated,
  case when a.attcollation <> t.typcollation then co.collname end as collation,
  case when c.relkind in ('r', 'p') then pg_catalog.pg_get_serial_sequence(
    pg_catalog.quote_ident(n.nspname) || '.' || pg_catalog.quote_ident(c.relname), a.attname) end as serial_seq
from pg_catalog.pg_attribute a
join pg_catalog.pg_class c on c.oid = a.attrelid
join pg_catalog.pg_namespace n on n.oid = c.relnamespace
join pg_catalog.pg_type t on t.oid = a.atttypid
left join pg_catalog.pg_attrdef d on d.adrelid = a.attrelid and d.adnum = a.attnum
left join pg_catalog.pg_collation co on co.oid = a.attcollation
where n.nspname = {s} and c.relkind in ('r', 'p', 'f', 'v', 'm') and a.attnum > 0
  and not a.attisdropped{only}
order by c.relname, a.attnum"
    )
}

fn constraints(s: &str, only: &str) -> String {
    format!(
        r"select c.relname as tbl, con.conname as name, con.contype::text as type,
  pg_catalog.pg_get_constraintdef(con.oid) as def,
  (select string_agg(a.attname, E'\x1f' order by k.ord) from unnest(con.conkey) with ordinality k(num, ord)
     join pg_catalog.pg_attribute a on a.attrelid = con.conrelid and a.attnum = k.num) as cols,
  fn.nspname as ref_schema, fc.relname as ref_table,
  (select string_agg(a.attname, E'\x1f' order by k.ord) from unnest(con.confkey) with ordinality k(num, ord)
     join pg_catalog.pg_attribute a on a.attrelid = con.confrelid and a.attnum = k.num) as ref_cols,
  con.confupdtype::text as on_update, con.confdeltype::text as on_delete,
  con.condeferrable as deferrable, con.condeferred as deferred
from pg_catalog.pg_constraint con
join pg_catalog.pg_class c on c.oid = con.conrelid
join pg_catalog.pg_namespace n on n.oid = c.relnamespace
left join pg_catalog.pg_class fc on fc.oid = con.confrelid
left join pg_catalog.pg_namespace fn on fn.oid = fc.relnamespace
where n.nspname = {s} and con.contype in ('p', 'u', 'f', 'c') and con.conparentid = 0
  and con.conislocal{only}
order by c.relname, con.contype, con.conname"
    )
}

fn indexes(s: &str, only: &str) -> String {
    format!(
        r"select t.relname as tbl, i.relname as name, ix.indisunique as uniq, am.amname as method,
  pg_catalog.pg_get_expr(ix.indpred, ix.indrelid) as pred,
  (select string_agg(pg_catalog.pg_get_indexdef(ix.indexrelid, k, true), E'\x1f' order by k)
     from generate_series(1, ix.indnkeyatts) k) as cols,
  (select string_agg(pg_catalog.pg_get_indexdef(ix.indexrelid, k, true), E'\x1f' order by k)
     from generate_series(ix.indnkeyatts + 1, ix.indnatts) k) as incl,
  ix.indoption::text as opts
from pg_catalog.pg_index ix
join pg_catalog.pg_class i on i.oid = ix.indexrelid
join pg_catalog.pg_class t on t.oid = ix.indrelid
join pg_catalog.pg_namespace n on n.oid = t.relnamespace
join pg_catalog.pg_am am on am.oid = i.relam
where n.nspname = {s}
  and not exists (select 1 from pg_catalog.pg_constraint con
                  where con.conindid = ix.indexrelid and con.contype in ('p', 'u', 'x'))
  and not exists (select 1 from pg_catalog.pg_inherits ih where ih.inhrelid = ix.indexrelid){only}
order by t.relname, i.relname"
    )
}

fn triggers(s: &str, only: &str) -> String {
    format!(
        "select c.relname as tbl, t.tgname as name, pg_catalog.pg_get_triggerdef(t.oid, true) as def
from pg_catalog.pg_trigger t
join pg_catalog.pg_class c on c.oid = t.tgrelid
join pg_catalog.pg_namespace n on n.oid = c.relnamespace
where n.nspname = {s} and not t.tgisinternal{only}
order by c.relname, t.tgname"
    )
}

fn routines(s: &str) -> String {
    format!(
        "select p.oid::bigint as id, p.proname as name, p.prokind::text as kind,
  pg_catalog.pg_get_function_identity_arguments(p.oid) as ident,
  case when p.prokind = 'f' then pg_catalog.pg_get_function_result(p.oid) end as returns,
  l.lanname as language, obj_description(p.oid, 'pg_proc') as comment,
  case when n.nspname not in ('pg_catalog', 'information_schema')
       then pg_catalog.pg_get_functiondef(p.oid) end as def
from pg_catalog.pg_proc p
join pg_catalog.pg_namespace n on n.oid = p.pronamespace
join pg_catalog.pg_language l on l.oid = p.prolang
where n.nspname = {s} and p.prokind in ('f', 'p')
  and not exists (select 1 from pg_catalog.pg_depend d where d.objid = p.oid and d.deptype = 'e')
order by p.proname, ident"
    )
}

fn arguments(s: &str) -> String {
    format!(
        "select p.oid::bigint as id, a.ord as ord, a.name as name,
  pg_catalog.format_type(a.typ, null) as type, coalesce(a.mode::text, 'i') as mode
from pg_catalog.pg_proc p
join pg_catalog.pg_namespace n on n.oid = p.pronamespace,
  unnest(coalesce(p.proallargtypes, p.proargtypes::oid[]), p.proargmodes, p.proargnames)
    with ordinality as a(typ, mode, name, ord)
where n.nspname = {s} and p.prokind in ('f', 'p') and a.typ is not null
order by p.oid, a.ord"
    )
}

fn sequences(s: &str) -> String {
    format!(
        "select c.relname as name, pg_catalog.format_type(q.seqtypid, null) as type,
  q.seqstart as start, q.seqincrement as inc, q.seqmin as min, q.seqmax as max,
  q.seqcache as cache, q.seqcycle as cycle
from pg_catalog.pg_sequence q
join pg_catalog.pg_class c on c.oid = q.seqrelid
join pg_catalog.pg_namespace n on n.oid = c.relnamespace
where n.nspname = {s}
  and not exists (select 1 from pg_catalog.pg_depend d where d.classid = 'pg_catalog.pg_class'::regclass
                  and d.objid = c.oid and d.deptype = 'i')
order by c.relname"
    )
}

fn fk_action(code: &str) -> Option<String> {
    Some(
        match code {
            "r" => "RESTRICT",
            "c" => "CASCADE",
            "n" => "SET NULL",
            "d" => "SET DEFAULT",
            _ => return None,
        }
        .into(),
    )
}

/// Default of a `serial` column: `nextval('t_id_seq'::regclass)` on the
/// sequence PostgreSQL would create for it.
fn is_serial(table: &str, column: &str, default: Option<&str>, seq: Option<&str>) -> bool {
    let (Some(def), Some(seq)) = (default, seq) else {
        return false;
    };
    let seq_name = seq.rsplit('.').next().unwrap_or(seq).trim_matches('"');
    let implicit = format!("{table}_{column}_seq");
    // PostgreSQL truncates identifiers to 63 bytes; then the name differs.
    seq_name == implicit && def.starts_with("nextval(") && def.contains(seq_name)
}

/// Index key columns with `DESC` / `NULLS FIRST|LAST` from `indoption`.
fn index_columns(cols: Vec<String>, opts: &str) -> Vec<String> {
    let opts: Vec<i64> = opts
        .split_whitespace()
        .map(|o| o.parse().unwrap_or(0))
        .collect();
    cols.into_iter()
        .enumerate()
        .map(|(i, c)| {
            let o = opts.get(i).copied().unwrap_or(0);
            let desc = o & 1 != 0;
            let nulls_first = o & 2 != 0;
            match (desc, nulls_first) {
                (false, false) => c,
                (false, true) => format!("{c} NULLS FIRST"),
                (true, true) => format!("{c} DESC"),
                (true, false) => format!("{c} DESC NULLS LAST"),
            }
        })
        .collect()
}

fn arg_mode(code: &str) -> &'static str {
    match code {
        "o" => "OUT",
        "b" => "INOUT",
        "v" => "VARIADIC",
        "t" => "TABLE",
        _ => "IN",
    }
}

/// `BEFORE INSERT OR UPDATE` from `CREATE TRIGGER name BEFORE … ON …`.
pub(crate) fn trigger_event(def: &str, name: &str) -> Option<String> {
    let upper = def.to_ascii_uppercase();
    let start = upper.find(&name.to_ascii_uppercase())? + name.len();
    let end = upper[start..].find(" ON ")? + start;
    Some(def[start..end].trim().to_owned())
}

pub(crate) fn assemble(
    rels: Vec<Row>,
    cols: Vec<Row>,
    cons: Vec<Row>,
    idx: Vec<Row>,
    trg: Vec<Row>,
) -> Vec<Table> {
    let mut tables: Vec<Table> = rels
        .iter()
        .map(|r| Table {
            name: r.string("name"),
            kind: match r.get("kind").unwrap_or("r") {
                "v" => TableKind::View,
                "m" => TableKind::MaterializedView,
                "f" => TableKind::Foreign,
                _ => TableKind::Table,
            },
            comment: r.text("comment"),
            rows: r.int("row_estimate").filter(|n| *n >= 0),
            view_definition: r
                .text("view_def")
                .map(|d| d.trim().trim_end_matches(';').trim_end().to_owned()),
            partition_by: r.text("part_key"),
            partition_of: r.text("parent").zip(r.text("bound")),
            ..Default::default()
        })
        .collect();
    let pos: HashMap<String, usize> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();
    for r in &cols {
        let Some(&ti) = pos.get(r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        let t = &mut tables[ti];
        let name = r.string("name");
        let default = r.text("def");
        let identity = match r.get("identity").unwrap_or_default() {
            "a" => Some("ALWAYS".to_owned()),
            "d" => Some("BY DEFAULT".to_owned()),
            _ => None,
        };
        let generated = r.get("generated").unwrap_or_default();
        let serial = identity.is_none()
            && generated.is_empty()
            && is_serial(&t.name, &name, default.as_deref(), r.get("serial_seq"));
        let (default, generated_expr) = if !generated.is_empty() {
            (None, default)
        } else if serial || identity.is_some() {
            (None, None)
        } else {
            (default, None)
        };
        t.columns.push(Column {
            name,
            data_type: r.string("type"),
            nullable: !r.bool("notnull"),
            default,
            comment: r.text("comment"),
            identity,
            serial,
            generated: generated_expr,
            generated_stored: generated == "s",
            collation: r.text("collation"),
            ..Default::default()
        });
    }
    for r in &cons {
        let Some(&ti) = pos.get(r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        let t = &mut tables[ti];
        let name = r.text("name");
        let columns = r.list("cols");
        match r.get("type").unwrap_or_default() {
            "p" => t.primary_key = Some(Key { name, columns }),
            "u" => t.uniques.push(Key { name, columns }),
            "c" => {
                let def = r.string("def");
                let expr = def.strip_prefix("CHECK ").unwrap_or(&def).to_owned();
                t.checks.push(Check {
                    name,
                    expression: expr,
                });
            }
            "f" => {
                let ref_schema = r.text("ref_schema");
                t.foreign_keys.push(ForeignKey {
                    name,
                    columns,
                    ref_container: ref_schema,
                    ref_table: r.string("ref_table"),
                    ref_columns: r.list("ref_cols"),
                    on_delete: fk_action(r.get("on_delete").unwrap_or_default()),
                    on_update: fk_action(r.get("on_update").unwrap_or_default()),
                    deferrable: r.bool("deferrable"),
                    initially_deferred: r.bool("deferred"),
                });
            }
            _ => {}
        }
    }
    for r in &idx {
        let Some(&ti) = pos.get(r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        let method = r.text("method").filter(|m| m != "btree");
        tables[ti].indexes.push(Index {
            name: r.string("name"),
            columns: index_columns(r.list("cols"), r.get("opts").unwrap_or_default()),
            include: r.list("incl"),
            unique: r.bool("uniq"),
            method,
            predicate: r.text("pred"),
            server_sql: None,
        });
    }
    for r in &trg {
        let Some(&ti) = pos.get(r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        let name = r.string("name");
        let def = r.text("def");
        tables[ti].triggers.push(Trigger {
            event: def.as_deref().and_then(|d| trigger_event(d, &name)),
            name,
            definition: def,
        });
    }
    tables
}

fn owned_serials(tables: &[Table]) -> HashSet<String> {
    tables
        .iter()
        .flat_map(|t| {
            t.columns
                .iter()
                .filter(|c| c.serial)
                .map(move |c| format!("{}_{}_seq", t.name, c.name))
        })
        .collect()
}

impl Introspector for PgIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
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
            let rel = |alias: &str| {
                only.map(|t| format!(" and {alias}.relname = {}", quote_literal(t)))
                    .unwrap_or_default()
            };
            let rels = query(conn, relations(&s, &rel("c"))).await?;
            let cols = query(conn, columns(&s, &rel("c"))).await?;
            let cons = query(conn, constraints(&s, &rel("c"))).await?;
            let idx = query(conn, indexes(&s, &rel("t"))).await?;
            let trg = query(conn, triggers(&s, &rel("c"))).await?;
            let mut tables = assemble(rels, cols, cons, idx, trg);
            for t in &mut tables {
                for fk in &mut t.foreign_keys {
                    if fk.ref_container.as_deref() == Some(container) {
                        fk.ref_container = None;
                    }
                }
            }
            let mut content = Content {
                tables,
                ..Default::default()
            };
            if only.is_some() {
                return Ok(content);
            }
            let args = query(conn, arguments(&s)).await?;
            let mut by_id: HashMap<String, Vec<Argument>> = HashMap::new();
            for a in args {
                by_id.entry(a.string("id")).or_default().push(Argument {
                    name: a.text("name"),
                    data_type: a.string("type"),
                    mode: arg_mode(a.get("mode").unwrap_or("i")).to_owned(),
                });
            }
            content.routines = query(conn, routines(&s))
                .await?
                .into_iter()
                .map(|r| {
                    let name = r.string("name");
                    let ident = r.string("ident");
                    Routine {
                        signature: format!("{name}({ident})"),
                        name,
                        kind: if r.get("kind") == Some("p") {
                            RoutineKind::Procedure
                        } else {
                            RoutineKind::Function
                        },
                        arguments: by_id.remove(&r.string("id")).unwrap_or_default(),
                        returns: r.text("returns"),
                        language: r.text("language"),
                        comment: r.text("comment"),
                        definition: r.text("def").map(|d| {
                            let d = d.trim_end();
                            format!("{d};")
                        }),
                    }
                })
                .collect();
            let owned = owned_serials(&content.tables);
            content.sequences = query(conn, sequences(&s))
                .await?
                .into_iter()
                .filter(|r| !owned.contains(r.get("name").unwrap_or_default()))
                .map(|r| Sequence {
                    name: r.string("name"),
                    data_type: r.text("type"),
                    start: r.int("start"),
                    increment: r.int("inc"),
                    min: r.int("min"),
                    max: r.int("max"),
                    cache: r.int("cache"),
                    cycle: r.bool("cycle"),
                })
                .collect();
            Ok(content)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::row;

    #[test]
    fn serial_detection_and_index_options() {
        assert!(is_serial(
            "orders",
            "id",
            Some("nextval('orders_id_seq'::regclass)"),
            Some("public.orders_id_seq")
        ));
        assert!(!is_serial(
            "orders",
            "id",
            Some("nextval('custom_seq'::regclass)"),
            Some("public.custom_seq")
        ));
        assert_eq!(
            index_columns(vec!["a".into(), "b".into(), "c".into()], "0 3 1"),
            vec!["a", "b DESC", "c DESC NULLS LAST"]
        );
        assert_eq!(
            trigger_event(
                "CREATE TRIGGER audit BEFORE INSERT OR UPDATE ON public.t FOR EACH ROW EXECUTE FUNCTION f()",
                "audit"
            )
            .as_deref(),
            Some("BEFORE INSERT OR UPDATE")
        );
    }

    #[test]
    fn assembles_tables_from_catalog_rows() {
        let rels = vec![row(&[
            ("name", Some("orders")),
            ("kind", Some("r")),
            ("comment", Some("All orders")),
            ("row_estimate", Some("-1")),
        ])];
        let cols = vec![
            row(&[
                ("tbl", Some("orders")),
                ("name", Some("id")),
                ("type", Some("integer")),
                ("notnull", Some("true")),
                ("def", Some("nextval('orders_id_seq'::regclass)")),
                ("identity", Some("")),
                ("generated", Some("")),
                ("serial_seq", Some("public.orders_id_seq")),
            ]),
            row(&[
                ("tbl", Some("orders")),
                ("name", Some("total")),
                ("type", Some("numeric(10,2)")),
                ("notnull", Some("false")),
                ("def", Some("0")),
                ("identity", Some("")),
                ("generated", Some("")),
            ]),
        ];
        let cons = vec![row(&[
            ("tbl", Some("orders")),
            ("name", Some("orders_total_check")),
            ("type", Some("c")),
            ("def", Some("CHECK ((total >= (0)::numeric))")),
        ])];
        let t = assemble(rels, cols, cons, vec![], vec![]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].rows, None);
        assert!(t[0].columns[0].serial && t[0].columns[0].default.is_none());
        assert_eq!(t[0].columns[1].default.as_deref(), Some("0"));
        assert_eq!(t[0].checks[0].expression, "((total >= (0)::numeric))");
    }
}
