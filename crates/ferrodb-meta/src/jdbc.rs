//! Introspection of the JDBC databases (Oracle, Db2, Sybase ASE, H2, Derby,
//! HyperSQL, Exasol, Vertica, Hive) through `DatabaseMetaData`, reached
//! with `Request::Native` calls of the JDBC bridge (`tables`, `columns`,
//! `primary_keys`, `imported_keys`, `index_info`, `procedures`,
//! `functions`…). Per-table calls go out once for the whole container
//! (`each_table`), so Oracle and Db2 answer in one round trip.
//!
//! Where `DatabaseMetaData` says too little, vendor catalogs fill in:
//! view definitions, sequences, Oracle packages, synonyms, triggers and
//! routine sources (`ALL_*` views), Hive partition columns (`DESCRIBE`).

use std::collections::{HashMap, HashSet};

use ferrodb_api::{Error, Result};
use ferrodb_drivers::{
    BoxFuture, CancellationToken, Connection, Request, quote_literal, sink::collect_request,
};

use crate::{
    Introspector,
    model::*,
    rows::{Row, query, rows},
};

pub struct JdbcIntrospector(pub Dialect);

/// Sybase: databases are JDBC catalogs; elsewhere containers are schemas.
fn catalog_container(d: Dialect) -> bool {
    d == Dialect::Sybase
}

async fn native(
    conn: &mut dyn Connection,
    method: &str,
    args: &[(&str, &str)],
) -> Result<Vec<Row>> {
    let out = collect_request(
        conn,
        Request::native(method, args),
        CancellationToken::new(),
    )
    .await
    .map_err(|e| match e {
        Error::Query(mut q) => {
            q.message = format!("{} (introspection: {method})", q.message);
            Error::Query(q)
        }
        other => other,
    })?;
    Ok(rows(&out.results))
}

/// Calls that some drivers do not support (Hive keys and indexes,
/// privileges on vendor catalogs): an error means "nothing to show".
async fn optional(conn: &mut dyn Connection, method: &str, args: &[(&str, &str)]) -> Vec<Row> {
    match native(conn, method, args).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(method, error = %e, "metadata call skipped");
            Vec::new()
        }
    }
}

async fn optional_query(conn: &mut dyn Connection, sql: String) -> Vec<Row> {
    match query(conn, sql).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "catalog query skipped");
            Vec::new()
        }
    }
}

/// Everything read for one container, before assembly.
#[derive(Default)]
pub(crate) struct Raw {
    pub tables: Vec<Row>,
    pub columns: Vec<Row>,
    pub primary_keys: Vec<Row>,
    pub foreign_keys: Vec<Row>,
    pub indexes: Vec<Row>,
    pub procedures: Vec<Row>,
    pub functions: Vec<Row>,
    /// `name`, `definition`.
    pub views: Vec<Row>,
    pub sequences: Vec<Row>,
    /// Oracle: `name`, `type`, `text` (one row per line).
    pub sources: Vec<Row>,
    /// Oracle: `synonym_name`, `table_owner`, `table_name`, `db_link`.
    pub synonyms: Vec<Row>,
    /// Oracle: `trigger_name`, `table_name`, `triggering_event`,
    /// `description`, `trigger_body`.
    pub triggers: Vec<Row>,
    /// Hive: table → partition columns (`name type`).
    pub partitions: HashMap<String, Vec<String>>,
}

fn views_sql(d: Dialect, s: &str) -> Option<String> {
    Some(match d {
        Dialect::Oracle => {
            format!("select view_name as name, text as definition from all_views where owner = {s}")
        }
        Dialect::H2 | Dialect::Hsqldb => format!(
            "select table_name as name, view_definition as definition from information_schema.views where table_schema = {s}"
        ),
        Dialect::Derby => format!(
            "select t.tablename as name, v.viewdefinition as definition from sys.sysviews v \
             join sys.systables t on v.tableid = t.tableid join sys.sysschemas c on t.schemaid = c.schemaid \
             where c.schemaname = {s}"
        ),
        Dialect::Db2 => format!(
            "select viewname as name, text as definition from syscat.views where viewschema = {s}"
        ),
        Dialect::Vertica => format!(
            "select table_name as name, view_definition as definition from v_catalog.views where table_schema = {s}"
        ),
        Dialect::Exasol => format!(
            "select view_name as name, view_text as definition from exa_all_views where view_schema = {s}"
        ),
        _ => return None,
    })
}

fn sequences_sql(d: Dialect, s: &str) -> Option<String> {
    Some(match d {
        Dialect::Oracle => format!(
            "select sequence_name as name, min_value as seq_min, max_value as seq_max, increment_by as seq_inc, \
             case cycle_flag when 'Y' then 1 else 0 end as seq_cycle, cache_size as seq_cache, last_number as seq_start \
             from all_sequences where sequence_owner = {s} order by sequence_name"
        ),
        Dialect::H2 => format!(
            "select sequence_name as name, cast(start_value as varchar(40)) as seq_start, \
             cast(minimum_value as varchar(40)) as seq_min, cast(maximum_value as varchar(40)) as seq_max, \
             cast(increment as varchar(40)) as seq_inc, cycle_option as seq_cycle, cast(cache as varchar(40)) as seq_cache, \
             data_type as seq_type from information_schema.sequences where sequence_schema = {s} order by sequence_name"
        ),
        Dialect::Hsqldb => format!(
            "select sequence_name as name, start_with as seq_start, minimum_value as seq_min, maximum_value as seq_max, \
             increment as seq_inc, cycle_option as seq_cycle, data_type as seq_type \
             from information_schema.sequences where sequence_schema = {s} order by sequence_name"
        ),
        Dialect::Derby => format!(
            "select q.sequencename as name, q.startvalue as seq_start, q.minimumvalue as seq_min, \
             q.maximumvalue as seq_max, q.increment as seq_inc, q.cycleoption as seq_cycle \
             from sys.syssequences q join sys.sysschemas c on q.schemaid = c.schemaid \
             where c.schemaname = {s} order by q.sequencename"
        ),
        Dialect::Db2 => format!(
            "select seqname as name, start as seq_start, minvalue as seq_min, maxvalue as seq_max, increment as seq_inc, \
             cycle as seq_cycle, cache as seq_cache from syscat.sequences where seqschema = {s} and seqtype = 'S' order by seqname"
        ),
        Dialect::Vertica => format!(
            "select sequence_name as name, minimum as seq_min, maximum as seq_max, increment_by as seq_inc, \
             allow_cycle as seq_cycle, session_cache_count as seq_cache from v_catalog.sequences \
             where sequence_schema = {s} and identity_table_name is null order by sequence_name"
        ),
        _ => return None,
    })
}

impl JdbcIntrospector {
    async fn read(
        &self,
        conn: &mut dyn Connection,
        container: &str,
        only: Option<&str>,
    ) -> Result<Raw> {
        let d = self.0;
        let (cat_key, sch_key) = if catalog_container(d) {
            ("catalog", "schema_ignored")
        } else {
            ("catalog_ignored", "schema")
        };
        let table = only.unwrap_or("%");
        let base = [(cat_key, container), (sch_key, container), ("table", table)];
        let each = [
            (cat_key, container),
            (sch_key, container),
            ("table", table),
            ("each_table", "true"),
        ];
        let mut raw = Raw {
            tables: native(conn, "tables", &base).await?,
            columns: native(conn, "columns", &base).await?,
            ..Default::default()
        };
        if d != Dialect::Hive {
            raw.primary_keys = optional(conn, "primary_keys", &each).await;
            raw.foreign_keys = optional(conn, "imported_keys", &each).await;
            if !matches!(d, Dialect::Vertica | Dialect::Exasol) {
                raw.indexes = optional(conn, "index_info", &each).await;
            }
        }
        let s = quote_literal(container);
        if let Some(sql) = views_sql(d, &s) {
            raw.views = optional_query(conn, sql).await;
        }
        if d == Dialect::Oracle {
            let only_t = only
                .map(|t| format!(" and table_name = {}", quote_literal(t)))
                .unwrap_or_default();
            raw.triggers = optional_query(
                conn,
                format!(
                    "select trigger_name, table_name, triggering_event, description, trigger_body \
                     from all_triggers where owner = {s}{only_t}"
                ),
            )
            .await;
        }
        if d == Dialect::Hive {
            for t in raw
                .tables
                .iter()
                .map(|r| r.string("table_name"))
                .collect::<Vec<_>>()
            {
                let q = |x: &str| format!("`{}`", x.replace('`', "``"));
                let rows =
                    optional_query(conn, format!("describe {}.{}", q(container), q(&t))).await;
                let parts = hive_partition_columns(&rows);
                if !parts.is_empty() {
                    raw.partitions.insert(t, parts);
                }
            }
        }
        if only.is_some() {
            return Ok(raw);
        }
        if d == Dialect::Oracle {
            raw.sources = optional_query(
                conn,
                format!(
                    "select name, type, text from all_source where owner = {s} \
                     and type in ('PROCEDURE', 'FUNCTION', 'PACKAGE', 'PACKAGE BODY') order by name, type, line"
                ),
            )
            .await;
            raw.synonyms = optional_query(
                conn,
                format!(
                    "select synonym_name, table_owner, table_name, db_link from all_synonyms \
                     where owner = {s} order by synonym_name"
                ),
            )
            .await;
        } else if d != Dialect::Hive {
            let sch = [(cat_key, container), (sch_key, container)];
            raw.procedures = optional(conn, "procedures", &sch).await;
            raw.functions = optional(conn, "functions", &sch).await;
        }
        if let Some(sql) = sequences_sql(d, &s) {
            raw.sequences = optional_query(conn, sql).await;
        }
        Ok(raw)
    }
}

/// Partition columns from Hive's `DESCRIBE` (the rows after
/// `# Partition Information`).
fn hive_partition_columns(rows: &[Row]) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_part = false;
    for r in rows {
        let name = r.values().first().cloned().flatten().unwrap_or_default();
        let name = name.trim();
        let ty = r.values().get(1).cloned().flatten().unwrap_or_default();
        if name.starts_with("# Partition Information") {
            in_part = true;
            continue;
        }
        if !in_part || name.is_empty() || name.starts_with('#') {
            if in_part && name.starts_with("# Detailed") {
                break;
            }
            continue;
        }
        out.push(format!("{name} {}", ty.trim()));
    }
    out
}

/// JDBC rule codes (`DatabaseMetaData.importedKeyCascade`…).
fn rule(code: Option<i64>) -> Option<String> {
    match code? {
        0 => Some("CASCADE".into()),
        1 => Some("RESTRICT".into()),
        2 => Some("SET NULL".into()),
        4 => Some("SET DEFAULT".into()),
        _ => None, // 3 = NO ACTION
    }
}

/// `varchar(20)`, `decimal(10,2)`, `NUMBER` from `TYPE_NAME`, `COLUMN_SIZE`
/// and `DECIMAL_DIGITS`.
pub(crate) fn spelled_type(d: Dialect, r: &Row) -> String {
    let name = r.string("type_name");
    if name.contains('(') {
        return name;
    }
    let size = r.int("column_size").unwrap_or(0);
    let digits = r.int("decimal_digits");
    let lower = name.to_ascii_lowercase();
    let sized = [
        "char",
        "varchar",
        "varchar2",
        "nchar",
        "nvarchar",
        "nvarchar2",
        "character",
        "character varying",
        "binary",
        "varbinary",
        "raw",
        "binary varying",
        "char for bit data",
    ];
    if sized.contains(&lower.as_str()) && size > 0 {
        // Unbounded VARCHAR in H2 / HSQLDB reports Integer.MAX_VALUE.
        if size >= i32::MAX as i64 || (d == Dialect::Hive && lower == "string") {
            return name;
        }
        return format!("{name}({size})");
    }
    if matches!(lower.as_str(), "decimal" | "numeric" | "number" | "dec") && size > 0 {
        return match digits {
            Some(s) if s > 0 => format!("{name}({size},{s})"),
            Some(s) if d == Dialect::Oracle && s < 0 => name,
            _ => format!("{name}({size})"),
        };
    }
    name
}

/// Text after `AS` of a full `CREATE VIEW … AS …` (Derby, Db2, Exasol).
fn view_body(def: &str) -> String {
    let t = def.trim();
    let lower = t.to_ascii_lowercase();
    if !lower.starts_with("create") {
        return t.trim_end_matches(';').to_owned();
    }
    let bytes = lower.as_bytes();
    let mut i = 0;
    while let Some(p) = lower[i..].find("as") {
        let at = i + p;
        let before = at.checked_sub(1).map(|j| bytes[j]);
        let after = bytes.get(at + 2).copied();
        let ws = |c: Option<u8>| c.is_some_and(|c| c.is_ascii_whitespace() || c == b')');
        if ws(before) && after.is_some_and(|c| c.is_ascii_whitespace() || c == b'(') {
            return t[at + 2..].trim().trim_end_matches(';').to_owned();
        }
        i = at + 2;
    }
    t.to_owned()
}

/// Index names the database generates for constraints: their DDL is the
/// constraint's.
fn system_index(d: Dialect, name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    match d {
        // `PRIMARY_KEY_5`, `CONSTRAINT_INDEX_8`, `FK_CUST_INDEX_8` (H2 2.x).
        Dialect::H2 => {
            n.starts_with("PRIMARY_KEY_")
                || n.rsplit_once("_INDEX_").is_some_and(|(_, tail)| {
                    !tail.is_empty() && tail.chars().all(|c| c.is_ascii_hexdigit())
                })
        }
        Dialect::Derby => {
            n.starts_with("SQL") && n[3..].chars().next().is_some_and(|c| c.is_ascii_digit())
        }
        // `SYS_IDX_…`, `SYS_PK_…`, `SYS_FK_…`, `SYS_CT_…`.
        Dialect::Hsqldb => n.starts_with("SYS_"),
        Dialect::Oracle => n.starts_with("SYS_C") || n.starts_with("SYS_IL"),
        Dialect::Db2 => {
            n.starts_with("SQL") && n[3..].chars().next().is_some_and(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

fn kind_of(table_type: &str) -> Option<TableKind> {
    let t = table_type.to_ascii_uppercase();
    if t.contains("SYSTEM")
        || t.contains("SYNONYM")
        || t == "ALIAS"
        || t == "SEQUENCE"
        || t.contains("INDEX")
    {
        return None;
    }
    Some(if t.contains("MATERIALIZED") {
        TableKind::MaterializedView
    } else if t.contains("VIEW") {
        TableKind::View
    } else {
        TableKind::Table
    })
}

/// Names with their position (`KEY_SEQ`, `ORDINAL_POSITION`).
type Ordered = Vec<(i64, String)>;
/// Foreign key column pairs (position, column, referenced column).
type FkColumns = Vec<(i64, String, String)>;

pub(crate) fn assemble(d: Dialect, container: &str, raw: Raw) -> Content {
    let mut tables: Vec<Table> = raw
        .tables
        .iter()
        .filter_map(|r| {
            let kind = kind_of(&r.string("table_type"))?;
            Some(Table {
                name: r.string("table_name"),
                kind,
                comment: r.text("remarks"),
                ..Default::default()
            })
        })
        .collect();
    let pos: HashMap<String, usize> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();
    let at = |name: Option<&str>| pos.get(name.unwrap_or_default()).copied();
    let mut cols: Vec<(usize, i64, Column)> = Vec::new();
    for r in &raw.columns {
        let Some(ti) = at(r.get("table_name")) else {
            continue;
        };
        let auto = r.get("is_autoincrement") == Some("YES");
        let generated = r.get("is_generatedcolumn") == Some("YES") && !auto;
        let default = r
            .text("column_def")
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("NULL"))
            // Identity defaults are sequence calls or Derby's `AUTOINCREMENT: …`.
            .filter(|_| !auto);
        cols.push((
            ti,
            r.int("ordinal_position").unwrap_or(0),
            Column {
                name: r.string("column_name"),
                data_type: spelled_type(d, r),
                nullable: r.get("is_nullable") != Some("NO") && r.int("nullable") != Some(0),
                comment: r.text("remarks"),
                identity: auto.then(|| "BY DEFAULT".to_owned()),
                generated: if generated { default.clone() } else { None },
                default: if generated { None } else { default },
                ..Default::default()
            },
        ));
    }
    cols.sort_by_key(|(t, p, _)| (*t, *p));
    for (ti, _, c) in cols {
        tables[ti].columns.push(c);
    }
    // Primary keys.
    let mut pks: HashMap<usize, (Option<String>, Ordered)> = HashMap::new();
    for r in &raw.primary_keys {
        let Some(ti) = at(r.get("table_name")) else {
            continue;
        };
        let e = pks.entry(ti).or_default();
        e.0 = r.text("pk_name");
        e.1.push((r.int("key_seq").unwrap_or(0), r.string("column_name")));
    }
    for (ti, (name, mut cols)) in pks {
        cols.sort();
        tables[ti].primary_key = Some(Key {
            name: name.filter(|n| !system_index(d, n)),
            columns: cols.into_iter().map(|(_, c)| c).collect(),
        });
    }
    // Foreign keys, grouped by name (or by referenced table and sequence).
    let mut fks: Vec<(usize, String, ForeignKey, FkColumns)> = Vec::new();
    for r in &raw.foreign_keys {
        let Some(ti) = at(r.get("fktable_name")) else {
            continue;
        };
        let name = r.text("fk_name");
        let ref_table = r.string("pktable_name");
        let key = format!("{}\u{1f}{}", name.clone().unwrap_or_default(), ref_table);
        let seq = r.int("key_seq").unwrap_or(0);
        let ref_schema = if catalog_container(d) {
            r.text("pktable_cat")
        } else {
            r.text("pktable_schem")
        };
        let idx = fks
            .iter()
            .position(|(t, k, _, _)| *t == ti && *k == key && name.is_some());
        let idx = match idx {
            Some(i) if seq > 1 => i,
            _ => {
                let deferrability = r.int("deferrability");
                fks.push((
                    ti,
                    key,
                    ForeignKey {
                        name: name.clone().filter(|n| !system_index(d, n)),
                        ref_container: ref_schema.filter(|s| s != container),
                        ref_table,
                        on_delete: rule(r.int("delete_rule")),
                        on_update: rule(r.int("update_rule")),
                        deferrable: matches!(deferrability, Some(5 | 6)),
                        initially_deferred: deferrability == Some(5),
                        ..Default::default()
                    },
                    Vec::new(),
                ));
                fks.len() - 1
            }
        };
        fks[idx]
            .3
            .push((seq, r.string("fkcolumn_name"), r.string("pkcolumn_name")));
    }
    for (ti, _, mut fk, mut cols) in fks {
        cols.sort();
        fk.columns = cols.iter().map(|(_, c, _)| c.clone()).collect();
        fk.ref_columns = cols.into_iter().map(|(_, _, c)| c).collect();
        tables[ti].foreign_keys.push(fk);
    }
    // Indexes (statistics rows and constraint-backing indexes left out).
    let mut idx: Vec<(usize, Index, Ordered)> = Vec::new();
    for r in &raw.indexes {
        let Some(ti) = at(r.get("table_name")) else {
            continue;
        };
        let Some(name) = r.text("index_name") else {
            continue;
        };
        if r.int("type") == Some(0) || system_index(d, &name) {
            continue;
        }
        let mut col = r.string("column_name");
        if col.is_empty() {
            continue;
        }
        if r.get("asc_or_desc") == Some("D") {
            col = format!("{} DESC", crate::ddl::quote(d, &col));
        } else {
            col = crate::ddl::quote(d, &col);
        }
        let ord = r.int("ordinal_position").unwrap_or(0);
        match idx.iter_mut().find(|(t, i, _)| *t == ti && i.name == name) {
            Some((_, _, cols)) => cols.push((ord, col)),
            None => idx.push((
                ti,
                Index {
                    name,
                    unique: !r.bool("non_unique"),
                    predicate: r.text("filter_condition"),
                    ..Default::default()
                },
                vec![(ord, col)],
            )),
        }
    }
    for (ti, mut i, mut cols) in idx {
        cols.sort();
        i.columns = cols.into_iter().map(|(_, c)| c).collect();
        let t = &mut tables[ti];
        // The unique index of the primary key (H2 names it differently).
        let pk_cols: Option<Vec<String>> = t
            .primary_key
            .as_ref()
            .map(|k| k.columns.iter().map(|c| crate::ddl::quote(d, c)).collect());
        if i.unique && pk_cols.as_ref() == Some(&i.columns) {
            continue;
        }
        if t.primary_key.as_ref().and_then(|k| k.name.as_ref()) == Some(&i.name) {
            continue;
        }
        // HyperSQL names the index backing a foreign key after the key.
        if t.foreign_keys
            .iter()
            .any(|f| f.name.as_ref() == Some(&i.name))
        {
            continue;
        }
        t.indexes.push(i);
    }
    // View definitions.
    for r in &raw.views {
        if let Some(ti) = at(r.get("name"))
            && let Some(def) = r.text("definition")
        {
            tables[ti].view_definition = Some(view_body(&def));
        }
    }
    // Hive partitions: partition columns leave the column list.
    for (t, parts) in &raw.partitions {
        if let Some(ti) = at(Some(t)) {
            let names: HashSet<&str> = parts
                .iter()
                .filter_map(|p| p.split_whitespace().next())
                .collect();
            tables[ti]
                .columns
                .retain(|c| !names.contains(c.name.as_str()));
            tables[ti].partition_by = Some(parts.join(", "));
        }
    }
    // Oracle triggers.
    for r in &raw.triggers {
        if let Some(ti) = at(r.get("table_name")) {
            let desc = r.string("description");
            let body = r.string("trigger_body");
            tables[ti].triggers.push(Trigger {
                name: r.string("trigger_name"),
                event: r.text("triggering_event"),
                definition: Some(format!(
                    "CREATE OR REPLACE TRIGGER {}{}",
                    desc.trim_end(),
                    body.trim_end()
                )),
            });
        }
    }
    // Oracle synonyms: shown with the views, their DDL is `CREATE SYNONYM`.
    for r in &raw.synonyms {
        let name = r.string("synonym_name");
        let target = format!(
            "{}.{}{}",
            crate::ddl::quote(d, &r.string("table_owner")),
            crate::ddl::quote(d, &r.string("table_name")),
            r.text("db_link")
                .map(|l| format!("@{l}"))
                .unwrap_or_default()
        );
        tables.push(Table {
            name: name.clone(),
            kind: TableKind::View,
            comment: Some(format!("synonym for {target}")),
            server_sql: Some(format!(
                "CREATE OR REPLACE SYNONYM {}.{} FOR {target}",
                crate::ddl::quote(d, container),
                crate::ddl::quote(d, &name)
            )),
            ..Default::default()
        });
    }
    tables.sort_by(|a, b| a.name.cmp(&b.name));
    Content {
        tables,
        routines: routines(d, &raw),
        sequences: raw
            .sequences
            .iter()
            .filter(|r| !r.string("name").starts_with("SYSTEM_SEQUENCE"))
            .map(|r| Sequence {
                name: r.string("name"),
                data_type: r.text("seq_type"),
                start: r.int("seq_start"),
                increment: r.int("seq_inc"),
                min: r.int("seq_min"),
                max: r.int("seq_max"),
                cache: r.int("seq_cache"),
                cycle: r.bool("seq_cycle"),
            })
            .collect(),
        ..Default::default()
    }
}

fn routines(d: Dialect, raw: &Raw) -> Vec<Routine> {
    let mut out: Vec<Routine> = Vec::new();
    if d == Dialect::Oracle {
        // ALL_SOURCE lines → one definition per object (a package: spec and body).
        let mut objs: Vec<(String, String, String)> = Vec::new();
        for r in &raw.sources {
            let (name, ty) = (r.string("name"), r.string("type"));
            match objs.last_mut() {
                Some((n, t, text)) if *n == name && *t == ty => text.push_str(&r.string("text")),
                _ => objs.push((name, ty, r.string("text"))),
            }
        }
        for (name, ty, text) in objs {
            let def = format!("CREATE OR REPLACE {}\n/\n", text.trim_end());
            if ty == "PACKAGE BODY" {
                if let Some(p) = out.iter_mut().find(|r| r.name == name) {
                    let d = p.definition.get_or_insert_with(String::new);
                    d.push_str(&def);
                }
                continue;
            }
            out.push(Routine {
                signature: name.clone(),
                name,
                kind: if ty == "FUNCTION" {
                    RoutineKind::Function
                } else {
                    RoutineKind::Procedure
                },
                language: Some(
                    if ty == "PACKAGE" {
                        "PL/SQL package"
                    } else {
                        "PL/SQL"
                    }
                    .into(),
                ),
                definition: Some(def),
                ..Default::default()
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        return out;
    }
    let mut seen = HashSet::new();
    for r in &raw.functions {
        let name = r.string("function_name");
        let sig = r.text("specific_name").unwrap_or_else(|| name.clone());
        if !seen.insert(sig.clone()) {
            continue;
        }
        out.push(Routine {
            name: name.clone(),
            kind: RoutineKind::Function,
            comment: r.text("remarks"),
            signature: name,
            ..Default::default()
        });
    }
    for r in &raw.procedures {
        let name = r.string("procedure_name");
        // Some drivers list functions as procedures returning a result.
        if out.iter().any(|f| f.name == name) {
            continue;
        }
        let sig = r.text("specific_name").unwrap_or_else(|| name.clone());
        if !seen.insert(sig) {
            continue;
        }
        out.push(Routine {
            name: name.clone(),
            kind: if r.int("procedure_type") == Some(2) {
                RoutineKind::Function
            } else {
                RoutineKind::Procedure
            },
            comment: r.text("remarks"),
            signature: name,
            ..Default::default()
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

impl Introspector for JdbcIntrospector {
    fn dialect(&self) -> Dialect {
        self.0
    }

    fn containers<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
    ) -> BoxFuture<'a, Result<Vec<Container>>> {
        Box::pin(async move {
            let (method, col) = if catalog_container(self.0) {
                ("catalogs", "table_cat")
            } else {
                ("schemas", "table_schem")
            };
            let mut names: Vec<String> = native(conn, method, &[])
                .await?
                .iter()
                .map(|r| r.string(col))
                .filter(|n| !n.is_empty())
                .collect();
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
            let raw = self.read(conn, container, only).await?;
            Ok(assemble(self.0, container, raw))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::row;

    fn table(name: &str, ty: &str) -> Row {
        row(&[
            ("table_name", Some(name)),
            ("table_type", Some(ty)),
            ("remarks", None),
        ])
    }

    #[allow(clippy::too_many_arguments)]
    fn column(
        t: &str,
        c: &str,
        ty: &str,
        size: &str,
        digits: Option<&str>,
        nullable: &str,
        def: Option<&str>,
        auto: &str,
        pos: &str,
    ) -> Row {
        row(&[
            ("table_name", Some(t)),
            ("column_name", Some(c)),
            ("type_name", Some(ty)),
            ("column_size", Some(size)),
            ("decimal_digits", digits),
            ("is_nullable", Some(nullable)),
            ("column_def", def),
            ("is_autoincrement", Some(auto)),
            ("is_generatedcolumn", Some("NO")),
            ("ordinal_position", Some(pos)),
        ])
    }

    /// Rows as H2 2.3 returns them for two related tables.
    fn h2_raw() -> Raw {
        Raw {
            tables: vec![
                table("CUSTOMERS", "BASE TABLE"),
                table("ORDERS", "BASE TABLE"),
                table("BIG_ORDERS", "VIEW"),
                table("INFORMATION_SCHEMA_X", "SYSTEM TABLE"),
            ],
            columns: vec![
                column(
                    "CUSTOMERS",
                    "ID",
                    "INTEGER",
                    "32",
                    Some("0"),
                    "NO",
                    None,
                    "YES",
                    "1",
                ),
                column(
                    "CUSTOMERS",
                    "NAME",
                    "CHARACTER VARYING",
                    "100",
                    None,
                    "NO",
                    Some("'anon'"),
                    "NO",
                    "2",
                ),
                column(
                    "ORDERS",
                    "TOTAL",
                    "NUMERIC",
                    "10",
                    Some("2"),
                    "YES",
                    None,
                    "NO",
                    "2",
                ),
                column(
                    "ORDERS",
                    "ID",
                    "BIGINT",
                    "64",
                    Some("0"),
                    "NO",
                    None,
                    "NO",
                    "1",
                ),
                column(
                    "ORDERS",
                    "CUSTOMER_ID",
                    "INTEGER",
                    "32",
                    Some("0"),
                    "YES",
                    None,
                    "NO",
                    "3",
                ),
            ],
            primary_keys: vec![
                row(&[
                    ("table_name", Some("CUSTOMERS")),
                    ("column_name", Some("ID")),
                    ("key_seq", Some("1")),
                    ("pk_name", Some("CONSTRAINT_5")),
                ]),
                row(&[
                    ("table_name", Some("ORDERS")),
                    ("column_name", Some("ID")),
                    ("key_seq", Some("1")),
                    ("pk_name", Some("PK_ORDERS")),
                ]),
            ],
            foreign_keys: vec![row(&[
                ("pktable_schem", Some("PUBLIC")),
                ("pktable_name", Some("CUSTOMERS")),
                ("pkcolumn_name", Some("ID")),
                ("fktable_name", Some("ORDERS")),
                ("fkcolumn_name", Some("CUSTOMER_ID")),
                ("key_seq", Some("1")),
                ("update_rule", Some("3")),
                ("delete_rule", Some("0")),
                ("fk_name", Some("FK_CUST")),
                ("deferrability", Some("7")),
            ])],
            indexes: vec![
                row(&[
                    ("table_name", Some("CUSTOMERS")),
                    ("non_unique", Some("false")),
                    ("index_name", Some("PRIMARY_KEY_5")),
                    ("type", Some("3")),
                    ("ordinal_position", Some("1")),
                    ("column_name", Some("ID")),
                    ("asc_or_desc", Some("A")),
                ]),
                row(&[
                    ("table_name", Some("ORDERS")),
                    ("non_unique", Some("true")),
                    ("index_name", Some("IDX_TOTAL")),
                    ("type", Some("3")),
                    ("ordinal_position", Some("1")),
                    ("column_name", Some("TOTAL")),
                    ("asc_or_desc", Some("D")),
                ]),
                row(&[
                    ("table_name", Some("ORDERS")),
                    ("non_unique", Some("true")),
                    ("index_name", Some("FK_CUST_INDEX_8")),
                    ("type", Some("3")),
                    ("ordinal_position", Some("1")),
                    ("column_name", Some("CUSTOMER_ID")),
                    ("asc_or_desc", Some("A")),
                ]),
            ],
            views: vec![row(&[
                ("name", Some("BIG_ORDERS")),
                (
                    "definition",
                    Some("SELECT * FROM PUBLIC.ORDERS WHERE TOTAL > 100"),
                ),
            ])],
            sequences: vec![row(&[
                ("name", Some("SEQ_A")),
                ("seq_start", Some("1")),
                ("seq_inc", Some("5")),
                ("seq_min", Some("1")),
                ("seq_max", Some("1000")),
                ("seq_cycle", Some("NO")),
                ("seq_cache", Some("32")),
                ("seq_type", Some("BIGINT")),
            ])],
            functions: vec![row(&[
                ("function_name", Some("F1")),
                ("specific_name", Some("F1_1")),
                ("remarks", None),
            ])],
            ..Default::default()
        }
    }

    #[test]
    fn h2_metadata_is_assembled() {
        let c = assemble(Dialect::H2, "PUBLIC", h2_raw());
        let names: Vec<&str> = c.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["BIG_ORDERS", "CUSTOMERS", "ORDERS"]);
        let cust = c.table("CUSTOMERS").unwrap();
        assert_eq!(cust.columns[0].identity.as_deref(), Some("BY DEFAULT"));
        assert_eq!(cust.columns[1].data_type, "CHARACTER VARYING(100)");
        assert_eq!(cust.columns[1].default.as_deref(), Some("'anon'"));
        assert!(cust.indexes.is_empty(), "the PK index is the constraint");
        let orders = c.table("ORDERS").unwrap();
        assert_eq!(
            orders
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["ID", "TOTAL", "CUSTOMER_ID"]
        );
        assert_eq!(orders.columns[1].data_type, "NUMERIC(10,2)");
        assert_eq!(
            orders.primary_key.as_ref().unwrap().name.as_deref(),
            Some("PK_ORDERS")
        );
        let fk = &orders.foreign_keys[0];
        assert_eq!(
            (
                fk.ref_table.as_str(),
                fk.on_delete.as_deref(),
                fk.on_update.as_deref()
            ),
            ("CUSTOMERS", Some("CASCADE"), None)
        );
        assert_eq!(fk.ref_container, None);
        assert_eq!(orders.indexes.len(), 1, "the FK index is the constraint's");
        assert_eq!(orders.indexes[0].columns, ["TOTAL DESC"]);
        let v = c.table("BIG_ORDERS").unwrap();
        assert_eq!(v.kind, TableKind::View);
        assert!(v.view_definition.as_deref().unwrap().starts_with("SELECT"));
        assert_eq!(c.sequences[0].increment, Some(5));
        assert_eq!(c.routines[0].kind, RoutineKind::Function);
    }

    #[test]
    fn oracle_sources_synonyms_and_types() {
        let raw = Raw {
            tables: vec![table("EMP", "TABLE")],
            columns: vec![
                column(
                    "EMP",
                    "ID",
                    "NUMBER",
                    "0",
                    Some("-127"),
                    "NO",
                    Some("\"HR\".\"ISEQ$$_7\".nextval "),
                    "YES",
                    "1",
                ),
                column(
                    "EMP",
                    "SAL",
                    "NUMBER",
                    "8",
                    Some("2"),
                    "YES",
                    Some("0\n"),
                    "NO",
                    "2",
                ),
                column(
                    "EMP",
                    "HIRED",
                    "TIMESTAMP(6)",
                    "11",
                    Some("6"),
                    "YES",
                    None,
                    "NO",
                    "3",
                ),
            ],
            sources: vec![
                row(&[
                    ("name", Some("PKG")),
                    ("type", Some("PACKAGE")),
                    ("text", Some("PACKAGE pkg AS\n")),
                ]),
                row(&[
                    ("name", Some("PKG")),
                    ("type", Some("PACKAGE")),
                    ("text", Some("  PROCEDURE p;\nEND;\n")),
                ]),
                row(&[
                    ("name", Some("PKG")),
                    ("type", Some("PACKAGE BODY")),
                    (
                        "text",
                        Some("PACKAGE BODY pkg AS\n PROCEDURE p IS BEGIN NULL; END;\nEND;\n"),
                    ),
                ]),
                row(&[
                    ("name", Some("RAISE_SAL")),
                    ("type", Some("PROCEDURE")),
                    ("text", Some("PROCEDURE raise_sal IS BEGIN NULL; END;\n")),
                ]),
            ],
            synonyms: vec![row(&[
                ("synonym_name", Some("EMPLOYEES")),
                ("table_owner", Some("HR")),
                ("table_name", Some("EMP")),
                ("db_link", None),
            ])],
            ..Default::default()
        };
        let c = assemble(Dialect::Oracle, "HR", raw);
        let emp = c.table("EMP").unwrap();
        assert_eq!(emp.columns[0].data_type, "NUMBER");
        assert_eq!(
            emp.columns[0].default, None,
            "identity sequence default dropped"
        );
        assert_eq!(emp.columns[1].data_type, "NUMBER(8,2)");
        assert_eq!(emp.columns[1].default.as_deref(), Some("0"));
        assert_eq!(emp.columns[2].data_type, "TIMESTAMP(6)");
        let pkg = c.routines.iter().find(|r| r.name == "PKG").unwrap();
        let def = pkg.definition.as_deref().unwrap();
        assert!(def.starts_with("CREATE OR REPLACE PACKAGE pkg AS"), "{def}");
        assert!(def.contains("CREATE OR REPLACE PACKAGE BODY pkg"), "{def}");
        assert_eq!(c.routines.len(), 2);
        let syn = c.table("EMPLOYEES").unwrap();
        assert_eq!(
            syn.server_sql.as_deref(),
            Some("CREATE OR REPLACE SYNONYM HR.EMPLOYEES FOR HR.EMP")
        );
    }

    #[test]
    fn hive_partitions_and_view_bodies() {
        let describe = vec![
            row(&[("col_name", Some("id")), ("data_type", Some("int"))]),
            row(&[("col_name", Some("dt")), ("data_type", Some("string"))]),
            row(&[("col_name", Some("")), ("data_type", None)]),
            row(&[
                ("col_name", Some("# Partition Information")),
                ("data_type", None),
            ]),
            row(&[
                ("col_name", Some("# col_name            ")),
                ("data_type", Some("data_type")),
            ]),
            row(&[("col_name", Some("dt")), ("data_type", Some("string"))]),
        ];
        assert_eq!(hive_partition_columns(&describe), ["dt string"]);
        let raw = Raw {
            tables: vec![table("events", "TABLE")],
            columns: vec![
                column("events", "id", "int", "10", None, "YES", None, "NO", "1"),
                column(
                    "events",
                    "dt",
                    "string",
                    "2147483647",
                    None,
                    "YES",
                    None,
                    "NO",
                    "2",
                ),
            ],
            partitions: HashMap::from([("events".to_owned(), vec!["dt string".to_owned()])]),
            ..Default::default()
        };
        let c = assemble(Dialect::Hive, "default", raw);
        let t = c.table("events").unwrap();
        assert_eq!(t.columns.len(), 1);
        assert_eq!(t.partition_by.as_deref(), Some("dt string"));
        assert_eq!(
            view_body("CREATE VIEW app.v (a) AS select a from t"),
            "select a from t"
        );
        assert_eq!(
            view_body("create view \"V\" as\nSELECT 1 FROM SYSIBM.SYSDUMMY1;"),
            "SELECT 1 FROM SYSIBM.SYSDUMMY1"
        );
        assert_eq!(view_body("SELECT 1"), "SELECT 1");
    }
}
