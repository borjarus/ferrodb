//! DDL and script generation from the metadata model.
//!
//! - Tables are generated from the model (columns, keys, checks, indexes,
//!   comments); SQLite uses the statement kept in `sqlite_master`.
//! - Views, routines and triggers use the server-side definition.
//! - Several tables come in foreign-key order; tables in a reference cycle
//!   get their foreign keys as `ALTER TABLE … ADD CONSTRAINT` at the end.
//! - [`Target`] renames the container, so a schema can be recreated under
//!   another name (used by the DDL round-trip tests).

use std::collections::{BTreeSet, HashMap};

use ferrodb_api::{DdlOptions, ScriptKind};

use crate::model::*;

/// Where the generated objects go.
#[derive(Clone, Debug)]
pub struct Target<'a> {
    pub dialect: Dialect,
    /// Container the objects come from.
    pub container: &'a str,
    /// Container written into the DDL (default: `container`).
    pub rename: Option<&'a str>,
    pub opts: DdlOptions,
    /// Leave triggers out of table DDL (a whole container emits them after
    /// the functions they call).
    pub defer_triggers: bool,
}

impl<'a> Target<'a> {
    pub fn new(dialect: Dialect, container: &'a str) -> Self {
        Self {
            dialect,
            container,
            rename: None,
            opts: DdlOptions::default(),
            defer_triggers: false,
        }
    }

    fn schema(&self) -> &str {
        self.rename.unwrap_or(self.container)
    }

    pub fn q(&self, ident: &str) -> String {
        quote(self.dialect, ident)
    }

    /// `schema.name` with the target schema.
    pub fn qualified(&self, name: &str) -> String {
        format!("{}.{}", self.q(self.schema()), self.q(name))
    }

    /// A referenced table: same container → target container.
    fn referenced(&self, container: Option<&str>, name: &str) -> String {
        match container {
            Some(c) if c != self.container => format!("{}.{}", self.q(c), self.q(name)),
            _ => self.qualified(name),
        }
    }
}

const PG_RESERVED: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "authorization",
    "binary",
    "both",
    "case",
    "cast",
    "check",
    "collate",
    "collation",
    "column",
    "concurrently",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "deferrable",
    "desc",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "false",
    "fetch",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "grant",
    "group",
    "having",
    "ilike",
    "in",
    "initially",
    "inner",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "localtime",
    "localtimestamp",
    "natural",
    "not",
    "notnull",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "overlaps",
    "placing",
    "primary",
    "references",
    "returning",
    "right",
    "select",
    "session_user",
    "similar",
    "some",
    "symmetric",
    "system_user",
    "table",
    "tablesample",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "user",
    "using",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
];

/// Quotes an identifier when the dialect needs it (MySQL: always).
pub fn quote(dialect: Dialect, ident: &str) -> String {
    match dialect {
        Dialect::MySql | Dialect::MariaDb => format!("`{}`", ident.replace('`', "``")),
        Dialect::Postgres | Dialect::Sqlite => {
            let plain = ident
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                && ident
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && !PG_RESERVED.contains(&ident);
            if plain {
                ident.to_owned()
            } else {
                format!("\"{}\"", ident.replace('"', "\"\""))
            }
        }
        Dialect::Mongo => ident.to_owned(),
    }
}

/// SQL string literal (MySQL also escapes backslashes).
pub fn literal(dialect: Dialect, s: &str) -> String {
    match dialect {
        Dialect::MySql | Dialect::MariaDb => crate::mysql::literal(s),
        _ => format!("'{}'", s.replace('\'', "''")),
    }
}

fn idents(t: &Target<'_>, cols: &[String]) -> String {
    cols.iter().map(|c| t.q(c)).collect::<Vec<_>>().join(", ")
}

fn pg_serial(ty: &str) -> &'static str {
    match ty {
        "bigint" => "bigserial",
        "smallint" => "smallserial",
        _ => "serial",
    }
}

fn column_def(t: &Target<'_>, c: &Column) -> String {
    let mut s = t.q(&c.name);
    s.push(' ');
    match t.dialect {
        Dialect::Postgres => {
            if c.serial {
                s.push_str(pg_serial(&c.data_type));
            } else {
                s.push_str(&c.data_type);
            }
            if let Some(coll) = &c.collation {
                s.push_str(&format!(" COLLATE {}", quote(Dialect::Postgres, coll)));
            }
            if let Some(id) = &c.identity {
                s.push_str(&format!(" GENERATED {id} AS IDENTITY"));
            }
            if let Some(g) = &c.generated {
                s.push_str(&format!(
                    " GENERATED ALWAYS AS ({g}){}",
                    if c.generated_stored { " STORED" } else { "" }
                ));
            }
            if let Some(d) = &c.default {
                s.push_str(&format!(" DEFAULT {d}"));
            }
            if !c.nullable {
                s.push_str(" NOT NULL");
            }
        }
        Dialect::MySql | Dialect::MariaDb => {
            s.push_str(&c.data_type);
            if let Some(coll) = &c.collation {
                s.push_str(&format!(" COLLATE {coll}"));
            }
            if let Some(g) = &c.generated {
                s.push_str(&format!(
                    " GENERATED ALWAYS AS ({g}) {}",
                    if c.generated_stored {
                        "STORED"
                    } else {
                        "VIRTUAL"
                    }
                ));
            }
            if !c.nullable {
                s.push_str(" NOT NULL");
            }
            if let Some(d) = &c.default {
                s.push_str(&format!(" DEFAULT {d}"));
            }
            if let Some(u) = &c.on_update {
                s.push_str(&format!(" ON UPDATE {u}"));
            }
            if c.auto_increment {
                s.push_str(" AUTO_INCREMENT");
            }
            if t.opts.include_comments
                && let Some(cm) = &c.comment
            {
                s.push_str(&format!(" COMMENT {}", literal(t.dialect, cm)));
            }
        }
        Dialect::Sqlite | Dialect::Mongo => {
            s.push_str(&c.data_type);
            if !c.nullable {
                s.push_str(" NOT NULL");
            }
            if let Some(d) = &c.default {
                s.push_str(&format!(" DEFAULT {d}"));
            }
        }
    }
    s.trim_end().to_owned()
}

fn constraint_name(t: &Target<'_>, name: &Option<String>) -> String {
    name.as_ref()
        .map(|n| format!("CONSTRAINT {} ", t.q(n)))
        .unwrap_or_default()
}

fn fk_clause(t: &Target<'_>, fk: &ForeignKey) -> String {
    let mut s = format!(
        "{}FOREIGN KEY ({}) REFERENCES {}",
        constraint_name(t, &fk.name),
        idents(t, &fk.columns),
        t.referenced(fk.ref_container.as_deref(), &fk.ref_table)
    );
    if !fk.ref_columns.is_empty() {
        s.push_str(&format!(" ({})", idents(t, &fk.ref_columns)));
    }
    if let Some(a) = &fk.on_update {
        s.push_str(&format!(" ON UPDATE {a}"));
    }
    if let Some(a) = &fk.on_delete {
        s.push_str(&format!(" ON DELETE {a}"));
    }
    if fk.deferrable {
        s.push_str(" DEFERRABLE");
        if fk.initially_deferred {
            s.push_str(" INITIALLY DEFERRED");
        }
    }
    s
}

fn index_stmt(t: &Target<'_>, table: &Table, i: &Index) -> String {
    if let Some(sql) = &i.server_sql {
        return format!("{};", sql.trim_end_matches(';'));
    }
    match t.dialect {
        Dialect::MySql | Dialect::MariaDb => format!(
            "CREATE {}INDEX {} ON {} ({});",
            mysql_index_prefix(i),
            t.q(&i.name),
            t.qualified(&table.name),
            i.columns.join(", ")
        ),
        _ => {
            let mut s = format!(
                "CREATE {}INDEX {} ON {}",
                if i.unique { "UNIQUE " } else { "" },
                t.q(&i.name),
                t.qualified(&table.name)
            );
            if let Some(m) = &i.method {
                s.push_str(&format!(" USING {m}"));
            }
            s.push_str(&format!(" ({})", i.columns.join(", ")));
            if !i.include.is_empty() {
                s.push_str(&format!(" INCLUDE ({})", i.include.join(", ")));
            }
            if let Some(p) = &i.predicate {
                s.push_str(&format!(" WHERE {p}"));
            }
            s.push(';');
            s
        }
    }
}

fn mysql_index_prefix(i: &Index) -> &'static str {
    match i.method.as_deref().map(str::to_ascii_uppercase).as_deref() {
        Some("FULLTEXT") => "FULLTEXT ",
        Some("SPATIAL") => "SPATIAL ",
        _ if i.unique => "UNIQUE ",
        _ => "",
    }
}

fn mysql_key_clause(t: &Target<'_>, i: &Index) -> String {
    let kind = match i.method.as_deref().map(str::to_ascii_uppercase).as_deref() {
        Some("FULLTEXT") => "FULLTEXT KEY",
        Some("SPATIAL") => "SPATIAL KEY",
        _ if i.unique => "UNIQUE KEY",
        _ => "KEY",
    };
    let using = match i.method.as_deref().map(str::to_ascii_uppercase).as_deref() {
        Some("HASH") => " USING HASH",
        _ => "",
    };
    format!("{kind} {} ({}){using}", t.q(&i.name), i.columns.join(", "))
}

/// `CREATE TABLE` with its indexes, comments and triggers. Foreign keys in
/// `deferred` are left out (added later with `ALTER TABLE`).
fn table_ddl(t: &Target<'_>, table: &Table, deferred: &[&ForeignKey]) -> String {
    let mut out = String::new();
    if t.dialect == Dialect::Sqlite
        && let Some(sql) = &table.server_sql
    {
        out.push_str(&format!("{};\n", sql.trim_end_matches(';')));
        if t.opts.include_indexes {
            for i in &table.indexes {
                out.push_str(&index_stmt(t, table, i));
                out.push('\n');
            }
        }
        if !t.defer_triggers {
            for tr in &table.triggers {
                if let Some(d) = &tr.definition {
                    out.push_str(&format!("{};\n", d.trim_end_matches(';')));
                }
            }
        }
        return out;
    }
    let name = t.qualified(&table.name);
    if t.dialect == Dialect::Postgres
        && let Some((parent, bound)) = &table.partition_of
    {
        out.push_str(&format!(
            "CREATE TABLE {name} PARTITION OF {} {bound};\n",
            t.qualified(parent)
        ));
        comments(t, table, &mut out);
        return out;
    }
    let mysql = t.dialect.is_mysql();
    let mut items: Vec<String> = table.columns.iter().map(|c| column_def(t, c)).collect();
    if let Some(pk) = &table.primary_key {
        items.push(if mysql {
            format!("PRIMARY KEY ({})", idents(t, &pk.columns))
        } else {
            format!(
                "{}PRIMARY KEY ({})",
                constraint_name(t, &pk.name),
                idents(t, &pk.columns)
            )
        });
    }
    for u in &table.uniques {
        items.push(format!(
            "{}UNIQUE ({})",
            constraint_name(t, &u.name),
            idents(t, &u.columns)
        ));
    }
    if mysql && t.opts.include_indexes {
        for i in &table.indexes {
            items.push(mysql_key_clause(t, i));
        }
    }
    for fk in &table.foreign_keys {
        if !deferred.iter().any(|d| std::ptr::eq(*d, fk)) {
            items.push(fk_clause(t, fk));
        }
    }
    for c in &table.checks {
        items.push(format!(
            "{}CHECK {}",
            constraint_name(t, &c.name),
            c.expression
        ));
    }
    let foreign = table.kind == TableKind::Foreign;
    out.push_str(&format!(
        "CREATE {}TABLE {name} (\n    {}\n)",
        if foreign { "FOREIGN " } else { "" },
        items.join(",\n    ")
    ));
    if let Some(p) = &table.partition_by {
        out.push_str(&format!(" PARTITION BY {p}"));
    }
    if mysql {
        if let Some(e) = &table.engine {
            out.push_str(&format!(" ENGINE={e}"));
        }
        if let Some(c) = &table.collation {
            out.push_str(&format!(" COLLATE={c}"));
        }
        if t.opts.include_comments
            && let Some(c) = &table.comment
        {
            out.push_str(&format!(" COMMENT={}", literal(t.dialect, c)));
        }
    }
    out.push_str(";\n");
    if !mysql && t.opts.include_indexes {
        for i in &table.indexes {
            out.push_str(&index_stmt(t, table, i));
            out.push('\n');
        }
    }
    comments(t, table, &mut out);
    if !t.defer_triggers {
        for tr in &table.triggers {
            if let Some(d) = &tr.definition {
                out.push_str(&trigger_stmt(t, d));
            }
        }
    }
    out
}

fn trigger_stmt(t: &Target<'_>, def: &str) -> String {
    let def = def.trim().trim_end_matches(';');
    // A MySQL body may contain `;` (BEGIN … END): the console splitter
    // understands trigger bodies.
    format!("{};\n", retarget(t, def))
}

/// Server definitions name the source schema; point them at the target.
fn retarget(t: &Target<'_>, def: &str) -> String {
    match t.rename {
        Some(to) if to != t.container => {
            let from = format!("{}.", t.q(t.container));
            def.replace(&from, &format!("{}.", t.q(to)))
        }
        _ => def.to_owned(),
    }
}

fn comments(t: &Target<'_>, table: &Table, out: &mut String) {
    if t.dialect != Dialect::Postgres || !t.opts.include_comments {
        return;
    }
    let what = match table.kind {
        TableKind::View => "VIEW",
        TableKind::MaterializedView => "MATERIALIZED VIEW",
        TableKind::Foreign => "FOREIGN TABLE",
        TableKind::Table => "TABLE",
    };
    let name = t.qualified(&table.name);
    if let Some(c) = &table.comment {
        out.push_str(&format!(
            "COMMENT ON {what} {name} IS {};\n",
            literal(t.dialect, c)
        ));
    }
    for c in &table.columns {
        if let Some(cm) = &c.comment {
            out.push_str(&format!(
                "COMMENT ON COLUMN {name}.{} IS {};\n",
                t.q(&c.name),
                literal(t.dialect, cm)
            ));
        }
    }
}

fn view_ddl(t: &Target<'_>, v: &Table) -> String {
    if t.dialect == Dialect::Sqlite
        && let Some(sql) = &v.server_sql
    {
        return format!("{};\n", sql.trim_end_matches(';'));
    }
    let def = v
        .view_definition
        .clone()
        .unwrap_or_else(|| "-- definition not available (missing privilege)".into());
    let mut out = format!(
        "CREATE {}VIEW {} AS\n{};\n",
        if v.kind == TableKind::MaterializedView {
            "MATERIALIZED "
        } else {
            ""
        },
        t.qualified(&v.name),
        retarget(t, def.trim_end_matches(';'))
    );
    if t.opts.include_indexes {
        for i in &v.indexes {
            out.push_str(&index_stmt(t, v, i));
            out.push('\n');
        }
    }
    comments(t, v, &mut out);
    out
}

fn routine_ddl(t: &Target<'_>, r: &Routine) -> String {
    match &r.definition {
        Some(d) => {
            let mut s = format!("{}\n", retarget(t, d.trim_end()));
            if t.dialect == Dialect::Postgres
                && t.opts.include_comments
                && let Some(c) = &r.comment
            {
                s.push_str(&format!(
                    "COMMENT ON {} {} IS {};\n",
                    routine_word(r),
                    routine_identity(t, r),
                    literal(t.dialect, c)
                ));
            }
            s
        }
        None => format!(
            "-- definition of {} is not available\n",
            t.qualified(&r.name)
        ),
    }
}

fn routine_word(r: &Routine) -> &'static str {
    match r.kind {
        RoutineKind::Function => "FUNCTION",
        RoutineKind::Procedure => "PROCEDURE",
    }
}

/// `schema.f(integer, text)` (PostgreSQL) / `db.f` (MySQL).
fn routine_identity(t: &Target<'_>, r: &Routine) -> String {
    if t.dialect == Dialect::Postgres {
        let args = r.signature.strip_prefix(&r.name).unwrap_or("()").to_owned();
        format!("{}{args}", t.qualified(&r.name))
    } else {
        t.qualified(&r.name)
    }
}

fn sequence_ddl(t: &Target<'_>, s: &Sequence) -> String {
    let mut out = format!("CREATE SEQUENCE {}", t.qualified(&s.name));
    if t.dialect == Dialect::Postgres {
        if let Some(ty) = &s.data_type
            && ty != "bigint"
        {
            out.push_str(&format!(" AS {ty}"));
        }
        if let Some(v) = s.increment {
            out.push_str(&format!(" INCREMENT BY {v}"));
        }
        if let Some(v) = s.min {
            out.push_str(&format!(" MINVALUE {v}"));
        }
        if let Some(v) = s.max {
            out.push_str(&format!(" MAXVALUE {v}"));
        }
        if let Some(v) = s.start {
            out.push_str(&format!(" START WITH {v}"));
        }
        if let Some(v) = s.cache {
            out.push_str(&format!(" CACHE {v}"));
        }
        if s.cycle {
            out.push_str(" CYCLE");
        }
    }
    out.push_str(";\n");
    out
}

/// Tables in foreign-key order and the foreign keys closing a cycle.
fn order_tables<'t>(tables: &[&'t Table]) -> (Vec<&'t Table>, Vec<(&'t Table, &'t ForeignKey)>) {
    let names: BTreeSet<&str> = tables.iter().map(|t| t.name.as_str()).collect();
    let deps: HashMap<&str, BTreeSet<&str>> = tables
        .iter()
        .map(|t| {
            let d = t
                .foreign_keys
                .iter()
                .filter(|f| f.ref_container.is_none() && f.ref_table != t.name)
                .map(|f| f.ref_table.as_str())
                // A partition comes after its parent.
                .chain(t.partition_of.as_ref().map(|(p, _)| p.as_str()))
                .filter(|r| names.contains(r))
                .collect();
            (t.name.as_str(), d)
        })
        .collect();
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let mut ordered = Vec::new();
    let mut deferred = Vec::new();
    let mut rest: Vec<&Table> = tables.to_vec();
    while !rest.is_empty() {
        let ready: Vec<usize> = rest
            .iter()
            .enumerate()
            .filter(|(_, t)| deps[t.name.as_str()].iter().all(|d| done.contains(d)))
            .map(|(i, _)| i)
            .collect();
        let pick = if ready.is_empty() {
            // A cycle: take the first table, its unmet references go last.
            let t = rest[0];
            for f in &t.foreign_keys {
                if f.ref_container.is_none()
                    && names.contains(f.ref_table.as_str())
                    && !done.contains(f.ref_table.as_str())
                    && f.ref_table != t.name
                {
                    deferred.push((t, f));
                }
            }
            vec![0]
        } else {
            ready
        };
        for i in pick.iter().rev() {
            let t = rest.remove(*i);
            done.insert(t.name.as_str());
            ordered.push(t);
        }
        let n = ordered.len();
        ordered[n - pick.len()..].reverse();
    }
    (ordered, deferred)
}

fn alter_add_fk(t: &Target<'_>, table: &Table, fk: &ForeignKey) -> String {
    format!(
        "ALTER TABLE {} ADD {};\n",
        t.qualified(&table.name),
        fk_clause(t, fk)
    )
}

/// DDL of several tables of one container, in dependency order.
pub fn tables_ddl(t: &Target<'_>, tables: &[&Table]) -> String {
    let (ordered, deferred) = order_tables(tables);
    let mut out = String::new();
    for table in ordered {
        let mine: Vec<&ForeignKey> = deferred
            .iter()
            .filter(|(d, _)| std::ptr::eq(*d, table))
            .map(|(_, f)| *f)
            .collect();
        if table.is_view() {
            out.push_str(&view_ddl(t, table));
        } else {
            out.push_str(&table_ddl(t, table, &mine));
        }
        out.push('\n');
    }
    for (table, fk) in deferred {
        out.push_str(&alter_add_fk(t, table, fk));
    }
    out.trim_end().to_owned() + "\n"
}

/// Views in an order where a view comes after the views it mentions.
fn order_views<'t>(views: &[&'t Table]) -> Vec<&'t Table> {
    let mut rest: Vec<&Table> = views.to_vec();
    let mut out: Vec<&Table> = Vec::new();
    while !rest.is_empty() {
        let pos = rest
            .iter()
            .position(|v| {
                let def = v.view_definition.as_deref().unwrap_or_default();
                !rest
                    .iter()
                    .any(|o| o.name != v.name && mentions(def, &o.name))
            })
            .unwrap_or(0);
        out.push(rest.remove(pos));
    }
    out
}

fn mentions(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + name.len()..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        !word(before) && !word(after)
    })
}

/// Everything in a container: sequences, tables, views, routines, then
/// triggers (they call routines).
pub fn container_ddl(t: &Target<'_>, content: &Content) -> String {
    let deferred = Target {
        defer_triggers: true,
        ..t.clone()
    };
    let t = &deferred;
    let mut out = String::new();
    if t.dialect == Dialect::Postgres {
        out.push_str(&format!(
            "CREATE SCHEMA IF NOT EXISTS {};\n\n",
            t.q(t.schema())
        ));
    }
    for s in &content.sequences {
        out.push_str(&sequence_ddl(t, s));
    }
    if !content.sequences.is_empty() {
        out.push('\n');
    }
    let tables: Vec<&Table> = content.tables.iter().filter(|x| !x.is_view()).collect();
    if !tables.is_empty() {
        out.push_str(&tables_ddl(t, &tables));
        out.push('\n');
    }
    let views: Vec<&Table> = content.tables.iter().filter(|x| x.is_view()).collect();
    for v in order_views(&views) {
        out.push_str(&view_ddl(t, v));
        out.push('\n');
    }
    for r in &content.routines {
        out.push_str(&routine_ddl(t, r));
        out.push('\n');
    }
    for table in &content.tables {
        for tr in &table.triggers {
            if let Some(d) = &tr.definition {
                out.push_str(&if t.dialect == Dialect::Sqlite {
                    format!("{};\n", d.trim_end_matches(';'))
                } else {
                    trigger_stmt(t, d)
                });
            }
        }
    }
    out.trim_end().to_owned() + "\n"
}

/// One object of a container, as addressed by the explorer.
#[derive(Clone, Copy, Debug)]
pub enum Object<'a> {
    Table(&'a Table),
    Column(&'a Table, &'a Column),
    PrimaryKey(&'a Table, &'a Key),
    Unique(&'a Table, &'a Key),
    ForeignKey(&'a Table, &'a ForeignKey),
    Index(&'a Table, &'a Index),
    Trigger(&'a Table, &'a Trigger),
    Routine(&'a Routine),
    Sequence(&'a Sequence),
}

/// DDL of one object.
pub fn object_ddl(t: &Target<'_>, o: Object<'_>) -> String {
    match o {
        Object::Table(x) if x.is_view() => view_ddl(t, x),
        Object::Table(x) => table_ddl(t, x, &[]),
        Object::Column(x, c) => format!(
            "ALTER TABLE {} ADD COLUMN {};\n",
            t.qualified(&x.name),
            column_def(t, c)
        ),
        Object::PrimaryKey(x, k) => format!(
            "ALTER TABLE {} ADD {}PRIMARY KEY ({});\n",
            t.qualified(&x.name),
            if t.dialect.is_mysql() {
                String::new()
            } else {
                constraint_name(t, &k.name)
            },
            idents(t, &k.columns)
        ),
        Object::Unique(x, k) => format!(
            "ALTER TABLE {} ADD {}UNIQUE ({});\n",
            t.qualified(&x.name),
            constraint_name(t, &k.name),
            idents(t, &k.columns)
        ),
        Object::ForeignKey(x, f) => alter_add_fk(t, x, f),
        Object::Index(x, i) => format!("{}\n", index_stmt(t, x, i)),
        Object::Trigger(_, tr) => tr
            .definition
            .as_deref()
            .map(|d| trigger_stmt(t, d))
            .unwrap_or_default(),
        Object::Routine(r) => routine_ddl(t, r),
        Object::Sequence(s) => sequence_ddl(t, s),
    }
}

fn param(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    format!(":{clean}")
}

/// Snippets and calls for an object.
pub fn script(t: &Target<'_>, o: Object<'_>, kind: ScriptKind) -> Option<String> {
    match (o, kind) {
        (Object::Table(x), ScriptKind::Select) => {
            let cols: Vec<String> = x.columns.iter().map(|c| t.q(&c.name)).collect();
            Some(format!(
                "SELECT {}\nFROM {};\n",
                if cols.is_empty() {
                    "*".to_owned()
                } else {
                    cols.join(",\n       ")
                },
                t.qualified(&x.name)
            ))
        }
        (Object::Table(x), ScriptKind::Insert) => {
            let cols: Vec<&Column> = x
                .columns
                .iter()
                .filter(|c| {
                    c.generated.is_none()
                        && c.identity.as_deref() != Some("ALWAYS")
                        && !c.serial
                        && !c.auto_increment
                })
                .collect();
            Some(format!(
                "INSERT INTO {} ({})\nVALUES ({});\n",
                t.qualified(&x.name),
                cols.iter()
                    .map(|c| t.q(&c.name))
                    .collect::<Vec<_>>()
                    .join(", "),
                cols.iter()
                    .map(|c| param(&c.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
        (Object::Table(x), ScriptKind::Update) => {
            let key = key_columns(x);
            let set: Vec<String> = x
                .columns
                .iter()
                .filter(|c| !key.contains(&c.name) && c.generated.is_none())
                .map(|c| format!("{} = {}", t.q(&c.name), param(&c.name)))
                .collect();
            Some(format!(
                "UPDATE {}\nSET {}\n{};\n",
                t.qualified(&x.name),
                set.join(",\n    "),
                where_clause(t, x, &key)
            ))
        }
        (Object::Table(x), ScriptKind::Delete) => Some(format!(
            "DELETE FROM {}\n{};\n",
            t.qualified(&x.name),
            where_clause(t, x, &key_columns(x))
        )),
        (Object::Routine(r), ScriptKind::Call) => {
            let args: Vec<String> = r
                .arguments
                .iter()
                .enumerate()
                .filter(|(_, a)| a.is_input() || (t.dialect.is_mysql() && a.mode != "TABLE"))
                .map(|(i, a)| {
                    let n = a.name.clone().unwrap_or_else(|| format!("arg{}", i + 1));
                    if t.dialect.is_mysql() && a.mode == "OUT" {
                        format!("@{n}")
                    } else {
                        param(&n)
                    }
                })
                .collect();
            let call = format!("{}({})", t.qualified(&r.name), args.join(", "));
            Some(match r.kind {
                RoutineKind::Procedure => format!("CALL {call};\n"),
                RoutineKind::Function
                    if t.dialect == Dialect::Postgres
                        && r.returns
                            .as_deref()
                            .is_some_and(|x| x.starts_with("SETOF") || x.starts_with("TABLE")) =>
                {
                    format!("SELECT *\nFROM {call};\n")
                }
                RoutineKind::Function => format!("SELECT {call};\n"),
            })
        }
        (o, ScriptKind::Drop) => Some(drop_stmt(t, o)),
        _ => None,
    }
}

fn key_columns(x: &Table) -> Vec<String> {
    x.row_key().map(<[String]>::to_vec).unwrap_or_default()
}

fn where_clause(t: &Target<'_>, x: &Table, key: &[String]) -> String {
    if key.is_empty() {
        let all: Vec<String> = x
            .columns
            .iter()
            .map(|c| format!("{} = {}", t.q(&c.name), param(&c.name)))
            .collect();
        format!(
            "-- {} has no primary or unique key: every column is compared\nWHERE {}",
            x.name,
            all.join("\n  AND ")
        )
    } else {
        format!(
            "WHERE {}",
            key.iter()
                .map(|c| format!("{} = {}", t.q(c), param(c)))
                .collect::<Vec<_>>()
                .join("\n  AND ")
        )
    }
}

fn drop_stmt(t: &Target<'_>, o: Object<'_>) -> String {
    match o {
        Object::Table(x) => format!(
            "DROP {} {};\n",
            match x.kind {
                TableKind::View => "VIEW",
                TableKind::MaterializedView => "MATERIALIZED VIEW",
                TableKind::Foreign => "FOREIGN TABLE",
                TableKind::Table => "TABLE",
            },
            t.qualified(&x.name)
        ),
        Object::Column(x, c) => format!(
            "ALTER TABLE {} DROP COLUMN {};\n",
            t.qualified(&x.name),
            t.q(&c.name)
        ),
        Object::PrimaryKey(x, k) => match (t.dialect.is_mysql(), &k.name) {
            (false, Some(n)) => format!(
                "ALTER TABLE {} DROP CONSTRAINT {};\n",
                t.qualified(&x.name),
                t.q(n)
            ),
            _ => format!("ALTER TABLE {} DROP PRIMARY KEY;\n", t.qualified(&x.name)),
        },
        Object::Unique(x, k) => {
            format!(
                "ALTER TABLE {} DROP CONSTRAINT {};\n",
                t.qualified(&x.name),
                t.q(k.name.as_deref().unwrap_or("?"))
            )
        }
        Object::ForeignKey(x, f) => format!(
            "ALTER TABLE {} DROP {} {};\n",
            t.qualified(&x.name),
            if t.dialect.is_mysql() {
                "FOREIGN KEY"
            } else {
                "CONSTRAINT"
            },
            t.q(f.name.as_deref().unwrap_or("?"))
        ),
        Object::Index(x, i) => {
            if t.dialect.is_mysql() {
                format!("DROP INDEX {} ON {};\n", t.q(&i.name), t.qualified(&x.name))
            } else {
                format!("DROP INDEX {};\n", t.qualified(&i.name))
            }
        }
        Object::Trigger(x, tr) => match t.dialect {
            Dialect::Postgres => format!(
                "DROP TRIGGER {} ON {};\n",
                t.q(&tr.name),
                t.qualified(&x.name)
            ),
            _ => format!("DROP TRIGGER {};\n", t.qualified(&tr.name)),
        },
        Object::Routine(r) => format!("DROP {} {};\n", routine_word(r), routine_identity(t, r)),
        Object::Sequence(s) => format!("DROP SEQUENCE {};\n", t.qualified(&s.name)),
    }
}

#[cfg(test)]
mod tests;
