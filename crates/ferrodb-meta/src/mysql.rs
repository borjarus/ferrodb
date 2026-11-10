//! MySQL and MariaDB introspection from `information_schema`.
//!
//! Differences between the products handled here:
//! - `COLUMN_DEFAULT`: MySQL returns the bare value (`abc`, `0`) and marks
//!   expressions with `DEFAULT_GENERATED` in `EXTRA`; MariaDB returns an SQL
//!   expression (`'abc'`, `current_timestamp()`, `NULL`);
//! - `STATISTICS.EXPRESSION` (functional key parts) exists in MySQL only;
//! - `CHECK_CONSTRAINTS.TABLE_NAME` exists in MariaDB only;
//! - MariaDB also has system-versioned tables and sequences.

use std::collections::HashMap;

use ferrodb_api::Result;
use ferrodb_drivers::{BoxFuture, Connection, quote_literal};

use crate::{
    Introspector,
    model::*,
    rows::{Row, query},
};

pub struct MySqlIntrospector {
    pub mariadb: bool,
}

/// MySQL string literal (`'` doubled, `\` escaped).
pub fn literal(s: &str) -> String {
    quote_literal(&s.replace('\\', "\\\\"))
}

/// `_utf8mb4\'x\'` as printed by MySQL in generation / check expressions.
fn unescape(expr: &str) -> String {
    expr.replace("\\'", "'")
}

const CONTAINERS: &str = "select schema_name as name from information_schema.schemata \
     order by schema_name = database() desc, schema_name";

fn numeric(ty: &str) -> bool {
    let t = ty.trim_start().to_ascii_lowercase();
    [
        "tinyint",
        "smallint",
        "mediumint",
        "int",
        "integer",
        "bigint",
        "decimal",
        "numeric",
        "float",
        "double",
        "real",
        "bit",
        "year",
        "bool",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}

/// Default clause value for a column (already SQL).
fn default_expr(mariadb: bool, ty: &str, raw: Option<String>, extra: &str) -> Option<String> {
    let raw = raw?;
    if mariadb {
        return (raw != "NULL").then_some(raw);
    }
    if extra.to_ascii_uppercase().contains("DEFAULT_GENERATED") {
        let u = raw.to_ascii_uppercase();
        let plain = ["CURRENT_TIMESTAMP", "NOW()", "LOCALTIME", "LOCALTIMESTAMP"]
            .iter()
            .any(|p| u.starts_with(p));
        let raw = unescape(&raw);
        return Some(if plain { raw } else { format!("({raw})") });
    }
    if numeric(ty) && !raw.is_empty() {
        Some(raw)
    } else {
        Some(literal(&raw))
    }
}

/// `on update CURRENT_TIMESTAMP(3)` from `EXTRA`.
fn on_update(extra: &str) -> Option<String> {
    let lower = extra.to_ascii_lowercase();
    let i = lower.find("on update ")?;
    let rest = &extra[i + "on update ".len()..];
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    Some(rest[..end].to_owned())
}

fn tables(d: &str, only: &str) -> String {
    format!(
        "select table_name as name, table_type as type, engine as engine, \
         table_comment as comment, table_rows as row_estimate, table_collation as collation \
         from information_schema.tables where table_schema = {d}{only} order by table_name"
    )
}

fn columns(d: &str, only: &str) -> String {
    format!(
        "select table_name as tbl, column_name as name, column_type as type, \
         is_nullable as nullable, column_default as def, extra as extra, \
         column_comment as comment, generation_expression as gen, collation_name as collation \
         from information_schema.columns where table_schema = {d}{only} \
         order by table_name, ordinal_position"
    )
}

fn keys(d: &str, only: &str) -> String {
    format!(
        "select k.table_name as tbl, k.constraint_name as name, t.constraint_type as type, \
         k.column_name as col, k.ordinal_position as pos, \
         k.referenced_table_schema as ref_schema, k.referenced_table_name as ref_table, \
         k.referenced_column_name as ref_col, r.update_rule as on_update, r.delete_rule as on_delete \
         from information_schema.key_column_usage k \
         join information_schema.table_constraints t on t.constraint_schema = k.constraint_schema \
           and t.constraint_name = k.constraint_name and t.table_name = k.table_name \
         left join information_schema.referential_constraints r \
           on r.constraint_schema = k.constraint_schema and r.constraint_name = k.constraint_name \
           and r.table_name = k.table_name \
         where k.table_schema = {d} and t.constraint_type in ('PRIMARY KEY', 'FOREIGN KEY'){only} \
         order by k.table_name, k.constraint_name, k.ordinal_position"
    )
}

fn checks(mariadb: bool, d: &str, only: &str) -> String {
    if mariadb {
        format!(
            "select table_name as tbl, constraint_name as name, check_clause as expr \
             from information_schema.check_constraints where constraint_schema = {d}{only} \
             order by table_name, constraint_name"
        )
    } else {
        format!(
            "select t.table_name as tbl, c.constraint_name as name, c.check_clause as expr \
             from information_schema.check_constraints c \
             join information_schema.table_constraints t on t.constraint_schema = c.constraint_schema \
               and t.constraint_name = c.constraint_name and t.constraint_type = 'CHECK' \
             where c.constraint_schema = {d}{only} order by t.table_name, c.constraint_name"
        )
    }
}

fn statistics(mariadb: bool, d: &str, only: &str) -> String {
    let expr = if mariadb { "null" } else { "expression" };
    format!(
        "select table_name as tbl, index_name as name, non_unique as non_unique, \
         seq_in_index as seq, column_name as col, sub_part as sub_part, index_type as method, \
         collation as collation, {expr} as expr \
         from information_schema.statistics where table_schema = {d} \
         and index_name <> 'PRIMARY'{only} order by table_name, index_name, seq_in_index"
    )
}

fn views(d: &str, only: &str) -> String {
    format!(
        "select table_name as name, view_definition as def, check_option as check_option, \
         security_type as security from information_schema.views where table_schema = {d}{only}"
    )
}

fn triggers(d: &str, only: &str) -> String {
    format!(
        "select trigger_name as name, event_manipulation as event, event_object_table as tbl, \
         action_timing as timing, action_statement as body \
         from information_schema.triggers where trigger_schema = {d}{only} \
         order by event_object_table, action_order"
    )
}

fn routines(d: &str) -> String {
    format!(
        "select routine_name as name, routine_type as type, dtd_identifier as returns, \
         routine_definition as body, is_deterministic as det, sql_data_access as access, \
         security_type as security, routine_comment as comment, specific_name as spec \
         from information_schema.routines where routine_schema = {d} order by routine_name"
    )
}

fn parameters(d: &str) -> String {
    format!(
        "select specific_name as spec, ordinal_position as pos, parameter_mode as mode, \
         parameter_name as name, dtd_identifier as type \
         from information_schema.parameters where specific_schema = {d} and ordinal_position > 0 \
         order by specific_name, ordinal_position"
    )
}

fn fk_action(rule: Option<String>) -> Option<String> {
    rule.filter(|r| !r.eq_ignore_ascii_case("NO ACTION") && !r.eq_ignore_ascii_case("RESTRICT"))
}

pub(crate) struct Raw {
    pub tables: Vec<Row>,
    pub columns: Vec<Row>,
    pub keys: Vec<Row>,
    pub checks: Vec<Row>,
    pub stats: Vec<Row>,
    pub views: Vec<Row>,
    pub triggers: Vec<Row>,
}

pub(crate) fn assemble(mariadb: bool, container: &str, raw: Raw) -> (Vec<Table>, Vec<Sequence>) {
    let mut sequences = Vec::new();
    let mut out: Vec<Table> = Vec::new();
    for r in &raw.tables {
        let ty = r.string("type").to_ascii_uppercase();
        let name = r.string("name");
        if ty == "SEQUENCE" {
            sequences.push(Sequence {
                name,
                ..Default::default()
            });
            continue;
        }
        let view = ty.contains("VIEW");
        out.push(Table {
            name,
            kind: if view {
                TableKind::View
            } else {
                TableKind::Table
            },
            comment: if view { None } else { r.text("comment") },
            engine: r.text("engine"),
            collation: r.text("collation"),
            rows: r.int("row_estimate"),
            ..Default::default()
        });
    }
    let pos: HashMap<String, usize> = out
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();
    let table = |name: Option<&str>| pos.get(name.unwrap_or_default()).copied();
    for r in &raw.columns {
        let Some(ti) = table(r.get("tbl")) else {
            continue;
        };
        let t = &mut out[ti];
        let ty = r.string("type");
        let extra = r.string("extra");
        let upper = extra.to_ascii_uppercase();
        let generated = r.text("gen").map(|g| unescape(&g));
        let collation = r
            .text("collation")
            .filter(|c| Some(c) != t.collation.as_ref());
        t.columns.push(Column {
            name: r.string("name"),
            nullable: r
                .get("nullable")
                .is_some_and(|n| n.eq_ignore_ascii_case("YES")),
            default: if generated.is_some() {
                None
            } else {
                default_expr(
                    mariadb,
                    &ty,
                    r.text("def").or(r.get("def").map(str::to_owned)),
                    &extra,
                )
            },
            comment: r.text("comment"),
            auto_increment: upper.contains("AUTO_INCREMENT"),
            generated_stored: upper.contains("STORED"),
            generated,
            collation,
            on_update: on_update(&extra),
            data_type: ty,
            ..Default::default()
        });
    }
    for r in &raw.keys {
        let Some(ti) = table(r.get("tbl")) else {
            continue;
        };
        let t = &mut out[ti];
        let name = r.string("name");
        let col = r.string("col");
        if r.get("type") == Some("PRIMARY KEY") {
            t.primary_key
                .get_or_insert_with(|| Key {
                    name: None,
                    columns: Vec::new(),
                })
                .columns
                .push(col);
            continue;
        }
        match t
            .foreign_keys
            .iter_mut()
            .find(|f| f.name.as_deref() == Some(name.as_str()))
        {
            Some(fk) => {
                fk.columns.push(col);
                fk.ref_columns.push(r.string("ref_col"));
            }
            None => t.foreign_keys.push(ForeignKey {
                name: Some(name),
                columns: vec![col],
                ref_container: r.text("ref_schema").filter(|s| s != container),
                ref_table: r.string("ref_table"),
                ref_columns: vec![r.string("ref_col")],
                on_delete: fk_action(r.text("on_delete")),
                on_update: fk_action(r.text("on_update")),
                ..Default::default()
            }),
        }
    }
    for r in &raw.checks {
        let Some(ti) = table(r.get("tbl")) else {
            continue;
        };
        let expr = unescape(&r.string("expr"));
        let expression = if expr.starts_with('(') {
            expr
        } else {
            format!("({expr})")
        };
        out[ti].checks.push(Check {
            name: r.text("name"),
            expression,
        });
    }
    for r in &raw.stats {
        let Some(ti) = table(r.get("tbl")) else {
            continue;
        };
        let t = &mut out[ti];
        let name = r.string("name");
        let mut col = match r.text("col") {
            Some(c) => format!("`{}`", c.replace('`', "``")),
            None => format!("({})", unescape(&r.string("expr"))),
        };
        if let Some(n) = r.int("sub_part") {
            col.push_str(&format!("({n})"));
        }
        if r.get("collation") == Some("D") {
            col.push_str(" DESC");
        }
        match t.indexes.iter_mut().find(|i| i.name == name) {
            Some(i) => i.columns.push(col),
            None => {
                let method = r
                    .text("method")
                    .filter(|m| !m.eq_ignore_ascii_case("BTREE"));
                t.indexes.push(Index {
                    name,
                    columns: vec![col],
                    unique: r.int("non_unique") == Some(0),
                    method,
                    ..Default::default()
                });
            }
        }
    }
    for r in &raw.views {
        let Some(ti) = table(r.get("name")) else {
            continue;
        };
        out[ti].view_definition = r.text("def");
    }
    for r in &raw.triggers {
        let Some(ti) = table(r.get("tbl")) else {
            continue;
        };
        let name = r.string("name");
        let event = format!("{} {}", r.string("timing"), r.string("event"));
        let q = |x: &str| format!("`{}`", x.replace('`', "``"));
        let def = format!(
            "CREATE TRIGGER {}.{} {event} ON {}.{} FOR EACH ROW {}",
            q(container),
            q(&name),
            q(container),
            q(&out[ti].name),
            r.string("body")
        );
        out[ti].triggers.push(Trigger {
            name,
            event: Some(event),
            definition: Some(def),
        });
    }
    (out, sequences)
}

fn routine_definition(container: &str, r: &Row, args: &[Argument]) -> Option<String> {
    let body = r.text("body")?;
    let name = format!(
        "{}`.`{}",
        container.replace('`', "``"),
        r.string("name").replace('`', "``")
    );
    let function = r.get("type") == Some("FUNCTION");
    let params: Vec<String> = args
        .iter()
        .map(|a| {
            let n = a
                .name
                .as_deref()
                .map(|n| format!("`{}` ", n.replace('`', "``")))
                .unwrap_or_default();
            if function {
                format!("{n}{}", a.data_type)
            } else {
                format!("{} {n}{}", a.mode, a.data_type)
            }
        })
        .collect();
    let mut s = format!(
        "CREATE {} `{name}`({})",
        if function { "FUNCTION" } else { "PROCEDURE" },
        params.join(", ")
    );
    if function && let Some(ret) = r.text("returns") {
        s.push_str(&format!(" RETURNS {ret}"));
    }
    if let Some(c) = r.text("comment") {
        s.push_str(&format!("\n    COMMENT {}", literal(&c)));
    }
    if r.get("det") == Some("YES") {
        s.push_str("\n    DETERMINISTIC");
    }
    match r.get("access").unwrap_or_default() {
        "READS SQL DATA" | "MODIFIES SQL DATA" | "NO SQL" => {
            s.push_str(&format!("\n    {}", r.string("access")))
        }
        _ => {}
    }
    if r.get("security") == Some("INVOKER") {
        s.push_str("\n    SQL SECURITY INVOKER");
    }
    s.push('\n');
    s.push_str(&body);
    s.push(';');
    Some(s)
}

impl Introspector for MySqlIntrospector {
    fn dialect(&self) -> Dialect {
        if self.mariadb {
            Dialect::MariaDb
        } else {
            Dialect::MySql
        }
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
            let d = literal(container);
            let f = |col: &str| {
                only.map(|t| format!(" and {col} = {}", literal(t)))
                    .unwrap_or_default()
            };
            let raw = Raw {
                tables: query(conn, tables(&d, &f("table_name"))).await?,
                columns: query(conn, columns(&d, &f("table_name"))).await?,
                keys: query(conn, keys(&d, &f("k.table_name"))).await?,
                // CHECK_CONSTRAINTS: MySQL ≥ 8.0.16, MariaDB ≥ 10.2.
                checks: query(
                    conn,
                    checks(
                        self.mariadb,
                        &d,
                        &f(if self.mariadb {
                            "table_name"
                        } else {
                            "t.table_name"
                        }),
                    ),
                )
                .await
                .unwrap_or_default(),
                stats: query(conn, statistics(self.mariadb, &d, &f("table_name"))).await?,
                views: query(conn, views(&d, &f("table_name"))).await?,
                triggers: query(conn, triggers(&d, &f("event_object_table"))).await?,
            };
            let (tables, sequences) = assemble(self.mariadb, container, raw);
            let mut content = Content {
                tables,
                sequences,
                ..Default::default()
            };
            if only.is_some() {
                return Ok(content);
            }
            let mut params: HashMap<String, Vec<Argument>> = HashMap::new();
            for p in query(conn, parameters(&d)).await? {
                params.entry(p.string("spec")).or_default().push(Argument {
                    name: p.text("name"),
                    data_type: p.string("type"),
                    mode: p.text("mode").unwrap_or_else(|| "IN".into()),
                });
            }
            content.routines = query(conn, routines(&d))
                .await?
                .iter()
                .map(|r| {
                    let arguments = params.remove(&r.string("spec")).unwrap_or_default();
                    Routine {
                        name: r.string("name"),
                        signature: r.string("name"),
                        kind: if r.get("type") == Some("FUNCTION") {
                            RoutineKind::Function
                        } else {
                            RoutineKind::Procedure
                        },
                        definition: routine_definition(container, r, &arguments),
                        returns: r.text("returns"),
                        language: Some("SQL".into()),
                        comment: r.text("comment"),
                        arguments,
                    }
                })
                .collect();
            Ok(content)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_per_product() {
        assert_eq!(
            default_expr(false, "varchar(10)", Some("it's".into()), "").as_deref(),
            Some("'it''s'")
        );
        assert_eq!(
            default_expr(false, "int", Some("0".into()), "").as_deref(),
            Some("0")
        );
        assert_eq!(
            default_expr(
                false,
                "datetime",
                Some("CURRENT_TIMESTAMP".into()),
                "DEFAULT_GENERATED"
            )
            .as_deref(),
            Some("CURRENT_TIMESTAMP")
        );
        assert_eq!(
            default_expr(
                false,
                "json",
                Some("json_array()".into()),
                "DEFAULT_GENERATED"
            )
            .as_deref(),
            Some("(json_array())")
        );
        assert_eq!(default_expr(true, "int", Some("NULL".into()), ""), None);
        assert_eq!(
            default_expr(true, "varchar(5)", Some("'x'".into()), "").as_deref(),
            Some("'x'")
        );
        assert_eq!(
            on_update("DEFAULT_GENERATED on update CURRENT_TIMESTAMP(3)").as_deref(),
            Some("CURRENT_TIMESTAMP(3)")
        );
        assert_eq!(unescape("(_utf8mb4\\'a\\')"), "(_utf8mb4'a')");
    }
}
