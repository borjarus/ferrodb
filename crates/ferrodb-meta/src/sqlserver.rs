//! SQL Server and Azure SQL introspection from the `sys` catalog views of
//! the current database (SQL Server 2017+: `STRING_AGG`). One query per
//! object kind for a whole schema; with `only = Some(table)` the table
//! queries are narrowed to one object and routines, sequences and synonyms
//! are skipped.
//!
//! Column identity is kept as `IDENTITY(seed,increment)`, computed columns
//! as the generated expression (`PERSISTED` = stored), clustered and
//! columnstore indexes in the index method, comments from the
//! `MS_Description` extended property. Views, routines and triggers keep
//! the server's `CREATE` text (`sys.sql_modules`).

use std::collections::HashMap;

use ferrodb_api::Result;
use ferrodb_drivers::{BoxFuture, Connection};

use crate::{
    Introspector,
    model::*,
    rows::{Row, query},
};

pub struct SqlServerIntrospector;

/// `N'…'` literal.
pub(crate) fn nlit(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

const CONTAINERS: &str = "select s.name as name, cast(ep.value as nvarchar(max)) as comment
from sys.schemas s
left join sys.extended_properties ep on ep.class = 3 and ep.major_id = s.schema_id
  and ep.minor_id = 0 and ep.name = N'MS_Description'
where s.schema_id < 16384 or s.schema_id > 16399
order by case when s.name = schema_name() then 0
  when s.name in (N'sys', N'INFORMATION_SCHEMA', N'guest') then 2 else 1 end, s.name";

/// `MS_Description` of an object (`minor` = column id, 0 = the object).
fn description(object: &str, minor: &str) -> String {
    format!(
        "(select cast(ep.value as nvarchar(max)) from sys.extended_properties ep \
         where ep.class = 1 and ep.major_id = {object} and ep.minor_id = {minor} \
         and ep.name = N'MS_Description')"
    )
}

fn relations(s: &str, only: &str) -> String {
    format!(
        "select o.name as name, rtrim(o.type) as kind, {comment} as comment,
  (select sum(p.rows) from sys.partitions p where p.object_id = o.object_id and p.index_id in (0, 1)) as row_estimate,
  m.definition as def
from sys.objects o
left join sys.sql_modules m on m.object_id = o.object_id
where o.schema_id = schema_id({s}) and o.type in ('U', 'V') and o.is_ms_shipped = 0{only}
order by o.name",
        comment = description("o.object_id", "0")
    )
}

fn columns(s: &str, only: &str) -> String {
    format!(
        "select o.name as tbl, c.name as name, type_name(c.user_type_id) as type_name,
  t.is_user_defined as udt, c.max_length as max_length, c.precision as prec, c.scale as scale,
  c.is_nullable as nullable, dc.definition as def, ic.seed_value as seed, ic.increment_value as inc,
  cc.definition as computed, cc.is_persisted as persisted,
  case when c.collation_name <> cast(databasepropertyex(db_name(), 'Collation') as sysname)
    then c.collation_name end as collation,
  {comment} as comment
from sys.columns c
join sys.objects o on o.object_id = c.object_id
join sys.types t on t.user_type_id = c.user_type_id
left join sys.default_constraints dc on dc.object_id = c.default_object_id
left join sys.identity_columns ic on ic.object_id = c.object_id and ic.column_id = c.column_id
left join sys.computed_columns cc on cc.object_id = c.object_id and cc.column_id = c.column_id
where o.schema_id = schema_id({s}) and o.type in ('U', 'V') and o.is_ms_shipped = 0{only}
order by o.name, c.column_id",
        comment = description("c.object_id", "c.column_id")
    )
}

fn keys(s: &str, only: &str) -> String {
    format!(
        "select o.name as tbl, k.name as name, rtrim(k.type) as type, i.type_desc as index_type,
  (select string_agg(col_name(ic.object_id, ic.column_id), nchar(31)) within group (order by ic.key_ordinal)
     from sys.index_columns ic where ic.object_id = i.object_id and ic.index_id = i.index_id
     and ic.is_included_column = 0) as cols
from sys.key_constraints k
join sys.objects o on o.object_id = k.parent_object_id
join sys.indexes i on i.object_id = k.parent_object_id and i.index_id = k.unique_index_id
where o.schema_id = schema_id({s}){only}
order by o.name, k.type, k.name"
    )
}

fn foreign_keys(s: &str, only: &str) -> String {
    format!(
        "select o.name as tbl, fk.name as name, schema_name(r.schema_id) as ref_schema,
  r.name as ref_table,
  (select string_agg(col_name(fc.parent_object_id, fc.parent_column_id), nchar(31))
     within group (order by fc.constraint_column_id)
     from sys.foreign_key_columns fc where fc.constraint_object_id = fk.object_id) as cols,
  (select string_agg(col_name(fc.referenced_object_id, fc.referenced_column_id), nchar(31))
     within group (order by fc.constraint_column_id)
     from sys.foreign_key_columns fc where fc.constraint_object_id = fk.object_id) as ref_cols,
  fk.delete_referential_action_desc as on_delete, fk.update_referential_action_desc as on_update
from sys.foreign_keys fk
join sys.objects o on o.object_id = fk.parent_object_id
join sys.objects r on r.object_id = fk.referenced_object_id
where o.schema_id = schema_id({s}){only}
order by o.name, fk.name"
    )
}

fn checks(s: &str, only: &str) -> String {
    format!(
        "select o.name as tbl, ck.name as name, ck.definition as def
from sys.check_constraints ck
join sys.objects o on o.object_id = ck.parent_object_id
where o.schema_id = schema_id({s}){only}
order by o.name, ck.name"
    )
}

fn indexes(s: &str, only: &str) -> String {
    format!(
        "select o.name as tbl, i.name as name, i.is_unique as uniq, i.type_desc as type_desc,
  i.filter_definition as pred,
  (select string_agg(quotename(col_name(ic.object_id, ic.column_id))
       + case when ic.is_descending_key = 1 then N' DESC' else N'' end, nchar(31))
     within group (order by ic.key_ordinal)
     from sys.index_columns ic where ic.object_id = i.object_id and ic.index_id = i.index_id
     and ic.is_included_column = 0 and ic.key_ordinal > 0) as cols,
  (select string_agg(quotename(col_name(ic.object_id, ic.column_id)), nchar(31))
     within group (order by ic.index_column_id)
     from sys.index_columns ic where ic.object_id = i.object_id and ic.index_id = i.index_id
     and (ic.is_included_column = 1 or (i.type = 6 and ic.key_ordinal = 0))) as incl
from sys.indexes i
join sys.objects o on o.object_id = i.object_id
where o.schema_id = schema_id({s}) and o.type in ('U', 'V') and o.is_ms_shipped = 0
  and i.type in (1, 2, 5, 6) and i.is_primary_key = 0 and i.is_unique_constraint = 0
  and i.is_hypothetical = 0{only}
order by o.name, i.name"
    )
}

fn triggers(s: &str, only: &str) -> String {
    format!(
        "select o.name as tbl, tr.name as name, m.definition as def, tr.is_instead_of_trigger as instead,
  (select string_agg(te.type_desc, N', ') from sys.trigger_events te where te.object_id = tr.object_id) as events
from sys.triggers tr
join sys.objects o on o.object_id = tr.parent_id
left join sys.sql_modules m on m.object_id = tr.object_id
where o.schema_id = schema_id({s}){only}
order by o.name, tr.name"
    )
}

fn routines(s: &str) -> String {
    format!(
        "select o.object_id as id, o.name as name, rtrim(o.type) as kind, m.definition as def,
  {comment} as comment,
  (select type_name(p.user_type_id) from sys.parameters p
     where p.object_id = o.object_id and p.parameter_id = 0) as returns
from sys.objects o
left join sys.sql_modules m on m.object_id = o.object_id
where o.schema_id = schema_id({s}) and o.type in ('P', 'PC', 'FN', 'IF', 'TF', 'FS', 'FT')
  and o.is_ms_shipped = 0
order by o.name",
        comment = description("o.object_id", "0")
    )
}

fn arguments(s: &str) -> String {
    format!(
        "select p.object_id as id, p.name as name, type_name(p.user_type_id) as type_name,
  p.max_length as max_length, p.precision as prec, p.scale as scale, p.is_output as is_output,
  t.is_user_defined as udt
from sys.parameters p
join sys.objects o on o.object_id = p.object_id
join sys.types t on t.user_type_id = p.user_type_id
where o.schema_id = schema_id({s}) and p.parameter_id > 0
order by p.object_id, p.parameter_id"
    )
}

fn sequences(s: &str) -> String {
    format!(
        "select sq.name as name, type_name(sq.user_type_id) as type,
  cast(sq.start_value as bigint) as start, cast(sq.increment as bigint) as inc,
  cast(sq.minimum_value as bigint) as min, cast(sq.maximum_value as bigint) as max,
  sq.is_cycling as cycle, case when sq.is_cached = 1 then sq.cache_size end as cache
from sys.sequences sq
where sq.schema_id = schema_id({s})
order by sq.name"
    )
}

fn synonyms(s: &str) -> String {
    format!(
        "select sy.name as name, sy.base_object_name as target from sys.synonyms sy
where sy.schema_id = schema_id({s}) order by sy.name"
    )
}

/// `nvarchar(20)`, `decimal(10,2)`, `varbinary(max)`, `datetime2(3)`.
pub(crate) fn type_text(
    name: &str,
    max_length: i64,
    precision: i64,
    scale: i64,
    udt: bool,
) -> String {
    if udt {
        return name.to_owned();
    }
    let len = |unicode: bool| match max_length {
        -1 => "max".to_owned(),
        n if unicode => (n / 2).to_string(),
        n => n.to_string(),
    };
    match name {
        "varchar" | "char" | "varbinary" | "binary" => format!("{name}({})", len(false)),
        "nvarchar" | "nchar" => format!("{name}({})", len(true)),
        "decimal" | "numeric" => format!("{name}({precision},{scale})"),
        "datetime2" | "time" | "datetimeoffset" => format!("{name}({scale})"),
        "float" if precision != 53 => format!("float({precision})"),
        _ => name.to_owned(),
    }
}

fn fk_action(desc: &str) -> Option<String> {
    match desc {
        "CASCADE" => Some("CASCADE".into()),
        "SET_NULL" => Some("SET NULL".into()),
        "SET_DEFAULT" => Some("SET DEFAULT".into()),
        _ => None,
    }
}

/// Index method kept in the model: clustered and columnstore indexes;
/// `None` = an ordinary nonclustered index.
fn index_method(type_desc: &str) -> Option<String> {
    match type_desc {
        "NONCLUSTERED" => None,
        other => Some(other.to_owned()),
    }
}

/// The query of a view: the text after the `AS` that follows the name.
pub(crate) fn view_query(def: &str) -> Option<String> {
    let upper = def.to_ascii_uppercase();
    let view = upper.find("VIEW")?;
    let mut from = view + 4;
    // `AS` as a word after the name (and optional column list / WITH).
    while let Some(i) = upper[from..].find("AS") {
        let at = from + i;
        let before = upper[..at].chars().next_back();
        let after = upper[at + 2..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ']');
        if !word(before) && !word(after) {
            return Some(
                def[at + 2..]
                    .trim()
                    .trim_end_matches(';')
                    .trim_end()
                    .to_owned(),
            );
        }
        from = at + 2;
    }
    None
}

#[allow(clippy::too_many_arguments)] // one list per catalog query
pub(crate) fn assemble(
    rels: Vec<Row>,
    cols: Vec<Row>,
    keys: Vec<Row>,
    fks: Vec<Row>,
    cks: Vec<Row>,
    idx: Vec<Row>,
    trg: Vec<Row>,
    container: &str,
) -> Vec<Table> {
    let mut tables: Vec<Table> = rels
        .iter()
        .map(|r| {
            let def = r.text("def").map(|d| d.trim().to_owned());
            let view = r.get("kind") == Some("V");
            Table {
                name: r.string("name"),
                kind: if view {
                    TableKind::View
                } else {
                    TableKind::Table
                },
                comment: r.text("comment"),
                rows: r.int("row_estimate").filter(|n| *n >= 0),
                view_definition: def.as_deref().and_then(view_query),
                server_sql: def.filter(|_| view),
                ..Default::default()
            }
        })
        .collect();
    let pos: HashMap<String, usize> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();
    let at = |r: &Row| pos.get(r.get("tbl").unwrap_or_default()).copied();
    for r in &cols {
        let Some(ti) = at(r) else { continue };
        let computed = r.text("computed");
        let identity = r
            .int("seed")
            .zip(r.int("inc"))
            .map(|(s, i)| format!("IDENTITY({s},{i})"));
        tables[ti].columns.push(Column {
            name: r.string("name"),
            data_type: type_text(
                r.get("type_name").unwrap_or_default(),
                r.int("max_length").unwrap_or(0),
                r.int("prec").unwrap_or(0),
                r.int("scale").unwrap_or(0),
                r.bool("udt"),
            ),
            nullable: r.bool("nullable"),
            default: r.text("def"),
            comment: r.text("comment"),
            identity,
            generated_stored: computed.is_some() && r.bool("persisted"),
            generated: computed,
            collation: r.text("collation"),
            ..Default::default()
        });
    }
    for r in &keys {
        let Some(ti) = at(r) else { continue };
        let key = Key {
            name: r.text("name"),
            columns: r.list("cols"),
        };
        if r.get("type") == Some("PK") {
            tables[ti].primary_key = Some(key);
        } else {
            tables[ti].uniques.push(key);
        }
    }
    for r in &fks {
        let Some(ti) = at(r) else { continue };
        let ref_schema = r.text("ref_schema").filter(|s| s != container);
        tables[ti].foreign_keys.push(ForeignKey {
            name: r.text("name"),
            columns: r.list("cols"),
            ref_container: ref_schema,
            ref_table: r.string("ref_table"),
            ref_columns: r.list("ref_cols"),
            on_delete: fk_action(r.get("on_delete").unwrap_or_default()),
            on_update: fk_action(r.get("on_update").unwrap_or_default()),
            deferrable: false,
            initially_deferred: false,
        });
    }
    for r in &cks {
        let Some(ti) = at(r) else { continue };
        tables[ti].checks.push(Check {
            name: r.text("name"),
            expression: r.string("def"),
        });
    }
    for r in &idx {
        let Some(ti) = at(r) else { continue };
        let ty = r.string("type_desc");
        let columnstore = ty.contains("COLUMNSTORE");
        let (columns, include) = if columnstore {
            // Columnstore indexes list their columns without key order.
            let mut all = r.list("cols");
            all.extend(r.list("incl"));
            (all, Vec::new())
        } else {
            (r.list("cols"), r.list("incl"))
        };
        tables[ti].indexes.push(Index {
            name: r.string("name"),
            columns,
            include,
            unique: r.bool("uniq"),
            method: index_method(&ty),
            predicate: r.text("pred"),
            server_sql: None,
        });
    }
    for r in &trg {
        let Some(ti) = at(r) else { continue };
        let events = r.text("events");
        tables[ti].triggers.push(Trigger {
            name: r.string("name"),
            event: events.map(|e| {
                format!(
                    "{} {e}",
                    if r.bool("instead") {
                        "INSTEAD OF"
                    } else {
                        "AFTER"
                    }
                )
            }),
            definition: r.text("def").map(|d| d.trim().to_owned()),
        });
    }
    tables
}

fn routine_kind(code: &str) -> RoutineKind {
    match code {
        "P" | "PC" => RoutineKind::Procedure,
        _ => RoutineKind::Function,
    }
}

impl Introspector for SqlServerIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::SqlServer
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
            let s = nlit(container);
            let narrow = only
                .map(|t| format!(" and o.name = {}", nlit(t)))
                .unwrap_or_default();
            let rels = query(conn, relations(&s, &narrow)).await?;
            let cols = query(conn, columns(&s, &narrow)).await?;
            let ks = query(conn, keys(&s, &narrow)).await?;
            let fks = query(conn, foreign_keys(&s, &narrow)).await?;
            let cks = query(conn, checks(&s, &narrow)).await?;
            let idx = query(conn, indexes(&s, &narrow)).await?;
            let trg = query(conn, triggers(&s, &narrow)).await?;
            let mut content = Content {
                tables: assemble(rels, cols, ks, fks, cks, idx, trg, container),
                ..Default::default()
            };
            if only.is_some() {
                return Ok(content);
            }
            let mut args: HashMap<String, Vec<Argument>> = HashMap::new();
            for a in query(conn, arguments(&s)).await? {
                args.entry(a.string("id")).or_default().push(Argument {
                    name: a.text("name"),
                    data_type: type_text(
                        a.get("type_name").unwrap_or_default(),
                        a.int("max_length").unwrap_or(0),
                        a.int("prec").unwrap_or(0),
                        a.int("scale").unwrap_or(0),
                        a.bool("udt"),
                    ),
                    // OUTPUT parameters are passed in and out.
                    mode: if a.bool("is_output") { "INOUT" } else { "IN" }.into(),
                });
            }
            content.routines = query(conn, routines(&s))
                .await?
                .into_iter()
                .map(|r| {
                    let name = r.string("name");
                    let kind = r.string("kind");
                    let arguments = args.remove(&r.string("id")).unwrap_or_default();
                    let sig: Vec<&str> = arguments.iter().map(|a| a.data_type.as_str()).collect();
                    Routine {
                        signature: format!("{name}({})", sig.join(", ")),
                        name,
                        kind: routine_kind(&kind),
                        returns: match kind.as_str() {
                            "IF" | "TF" | "FT" => Some("TABLE".into()),
                            _ => r.text("returns"),
                        },
                        arguments,
                        language: Some(
                            if matches!(kind.as_str(), "PC" | "FS" | "FT") {
                                "CLR"
                            } else {
                                "T-SQL"
                            }
                            .into(),
                        ),
                        comment: r.text("comment"),
                        definition: r.text("def").map(|d| d.trim().to_owned()),
                    }
                })
                .collect();
            content.sequences = query(conn, sequences(&s))
                .await?
                .into_iter()
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
            content.synonyms = query(conn, synonyms(&s))
                .await?
                .into_iter()
                .map(|r| Synonym {
                    name: r.string("name"),
                    target: r.string("target"),
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
    fn types_and_view_queries() {
        assert_eq!(type_text("nvarchar", 40, 0, 0, false), "nvarchar(20)");
        assert_eq!(type_text("varbinary", -1, 0, 0, false), "varbinary(max)");
        assert_eq!(type_text("decimal", 9, 10, 2, false), "decimal(10,2)");
        assert_eq!(type_text("datetime2", 8, 27, 7, false), "datetime2(7)");
        assert_eq!(type_text("float", 8, 53, 0, false), "float");
        assert_eq!(type_text("Phone", 20, 0, 0, true), "Phone");
        assert_eq!(
            view_query("CREATE VIEW [dbo].[v_class] AS\nSELECT a AS b FROM t;").as_deref(),
            Some("SELECT a AS b FROM t")
        );
        assert_eq!(
            view_query("create view dbo.v (x) with schemabinding as select 1 as x").as_deref(),
            Some("select 1 as x")
        );
    }

    #[test]
    fn assembles_recorded_catalog_rows() {
        let rels = vec![
            row(&[
                ("name", Some("orders")),
                ("kind", Some("U")),
                ("comment", Some("All orders")),
                ("row_estimate", Some("12")),
            ]),
            row(&[
                ("name", Some("v_orders")),
                ("kind", Some("V")),
                (
                    "def",
                    Some("CREATE VIEW dbo.v_orders AS SELECT id FROM dbo.orders"),
                ),
            ]),
        ];
        let cols = vec![
            row(&[
                ("tbl", Some("orders")),
                ("name", Some("id")),
                ("type_name", Some("int")),
                ("udt", Some("false")),
                ("max_length", Some("4")),
                ("prec", Some("10")),
                ("scale", Some("0")),
                ("nullable", Some("false")),
                ("seed", Some("1")),
                ("inc", Some("1")),
            ]),
            row(&[
                ("tbl", Some("orders")),
                ("name", Some("total")),
                ("type_name", Some("decimal")),
                ("max_length", Some("9")),
                ("prec", Some("10")),
                ("scale", Some("2")),
                ("nullable", Some("true")),
                ("def", Some("((0))")),
            ]),
            row(&[
                ("tbl", Some("orders")),
                ("name", Some("doubled")),
                ("type_name", Some("decimal")),
                ("max_length", Some("9")),
                ("prec", Some("11")),
                ("scale", Some("2")),
                ("nullable", Some("true")),
                ("computed", Some("([total]*(2))")),
                ("persisted", Some("true")),
            ]),
        ];
        let keys = vec![row(&[
            ("tbl", Some("orders")),
            ("name", Some("PK_orders")),
            ("type", Some("PK")),
            ("cols", Some("id")),
        ])];
        let fks = vec![row(&[
            ("tbl", Some("orders")),
            ("name", Some("FK_orders_customers")),
            ("ref_schema", Some("sales")),
            ("ref_table", Some("customers")),
            ("cols", Some("customer_id")),
            ("ref_cols", Some("id")),
            ("on_delete", Some("CASCADE")),
            ("on_update", Some("NO_ACTION")),
        ])];
        let idx = vec![row(&[
            ("tbl", Some("orders")),
            ("name", Some("ix_total")),
            ("uniq", Some("false")),
            ("type_desc", Some("NONCLUSTERED")),
            ("cols", Some("[total] DESC")),
            ("incl", Some("[id]")),
            ("pred", Some("([total]>(0))")),
        ])];
        let trg = vec![row(&[
            ("tbl", Some("orders")),
            ("name", Some("tr_audit")),
            ("instead", Some("false")),
            ("events", Some("INSERT, UPDATE")),
            (
                "def",
                Some("CREATE TRIGGER tr_audit ON dbo.orders AFTER INSERT, UPDATE AS SELECT 1"),
            ),
        ])];
        let t = assemble(rels, cols, keys, fks, vec![], idx, trg, "dbo");
        assert_eq!(t.len(), 2);
        let o = &t[0];
        assert_eq!(o.columns[0].identity.as_deref(), Some("IDENTITY(1,1)"));
        assert_eq!(o.columns[1].default.as_deref(), Some("((0))"));
        assert!(o.columns[2].generated_stored);
        assert_eq!(o.primary_key.as_ref().unwrap().columns, ["id"]);
        assert_eq!(o.foreign_keys[0].ref_container.as_deref(), Some("sales"));
        assert_eq!(o.foreign_keys[0].on_delete.as_deref(), Some("CASCADE"));
        assert_eq!(o.foreign_keys[0].on_update, None);
        assert_eq!(o.indexes[0].method, None);
        assert_eq!(o.indexes[0].include, ["[id]"]);
        assert_eq!(o.triggers[0].event.as_deref(), Some("AFTER INSERT, UPDATE"));
        assert_eq!(t[1].kind, TableKind::View);
        assert_eq!(
            t[1].view_definition.as_deref(),
            Some("SELECT id FROM dbo.orders")
        );
    }
}
