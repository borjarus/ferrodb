//! Schema comparison, migration scripts and `ALTER TABLE` generation
//! (plan, phase 7).
//!
//! [`compare`] lists what differs between two containers and builds the
//! script making the *target* like the *source*, in the target's dialect.
//! [`alter_table`] turns one table into another; the schema comparison and
//! the visual table editor share it.
//!
//! The script is ordered so it can run top to bottom: foreign keys, views,
//! triggers and indexes that change are dropped first, tables are created
//! and altered, then indexes, foreign keys, routines, views and triggers are
//! created. SQLite cannot alter most column properties: such a table is
//! rebuilt (create a copy, move the rows, drop, rename).

use ferrodb_api::{CompareOptions, DesignScript, DiffKind, ObjectKind, SchemaDiffItem};

use crate::{
    ddl::{self, Object, Target},
    model::*,
};

// Script phases, in execution order.
const P_DROP_FK: u8 = 0;
const P_DROP_VIEW: u8 = 1;
const P_DROP_TRIGGER: u8 = 2;
const P_DROP_INDEX: u8 = 3;
const P_DROP_ROUTINE: u8 = 4;
const P_SEQUENCE: u8 = 5;
const P_DROP_TABLE: u8 = 6;
const P_CREATE_TABLE: u8 = 7;
const P_ALTER: u8 = 8;
const P_INDEX: u8 = 9;
const P_ADD_FK: u8 = 10;
const P_ROUTINE: u8 = 11;
const P_VIEW: u8 = 12;
const P_TRIGGER: u8 = 13;

/// One side of a comparison.
#[derive(Clone, Copy, Debug)]
pub struct Side<'a> {
    pub dialect: Dialect,
    pub container: &'a str,
    pub content: &'a Content,
}

/// What [`compare`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Comparison {
    pub items: Vec<SchemaDiffItem>,
    pub script: String,
    pub warnings: Vec<String>,
}

/// Statements of one change with their phase.
#[derive(Clone, Debug, Default)]
pub struct Change {
    pub statements: Vec<(u8, String)>,
    /// Human-readable differences (`column total: type …`).
    pub details: Vec<String>,
    pub warnings: Vec<String>,
}

impl Change {
    fn push(&mut self, phase: u8, stmt: impl Into<String>) {
        let s = stmt.into();
        let s = s.trim();
        if !s.is_empty() {
            self.statements.push((phase, s.to_owned()));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.statements.is_empty() && self.details.is_empty()
    }

    /// Statements in execution order.
    pub fn ordered(&self) -> Vec<String> {
        let mut s = self.statements.clone();
        s.sort_by_key(|(p, _)| *p);
        s.into_iter().map(|(_, x)| x).collect()
    }
}

// ----------------------------------------------------------- normalizing

/// Type names compared the same way across spellings: `INT` = `integer`,
/// `int(11)` = `int`, `varchar(20)` = `character varying(20)`.
pub fn normalize_type(ty: &str) -> String {
    let lower = ty.trim().to_ascii_lowercase();
    let collapsed = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    let (base, args) = match collapsed.find('(') {
        Some(i) => (collapsed[..i].trim(), collapsed[i..].replace(' ', "")),
        None => (collapsed.as_str(), String::new()),
    };
    let (base, rest) = match base.split_once(' ') {
        Some((b, r))
            if !matches!(
                b,
                "character" | "double" | "timestamp" | "time" | "bit" | "long"
            ) =>
        {
            (b, format!(" {r}"))
        }
        _ => (base, String::new()),
    };
    let base = match base {
        "int" | "int4" | "integer" | "mediumint" => "integer",
        "int8" | "bigint" => "bigint",
        "int2" | "smallint" => "smallint",
        "bool" | "boolean" => "boolean",
        "float8" | "double" | "double precision" => "double precision",
        "float4" | "real" => "real",
        "varchar" | "character varying" => "character varying",
        "char" | "character" | "bpchar" => "character",
        "decimal" | "numeric" => "numeric",
        "timestamptz" | "timestamp with time zone" => "timestamp with time zone",
        "timestamp" | "timestamp without time zone" => "timestamp",
        "timetz" | "time with time zone" => "time with time zone",
        "time without time zone" => "time",
        other => other,
    };
    // Integer display widths (`int(11)`) mean nothing.
    let args = if matches!(base, "integer" | "bigint" | "smallint" | "tinyint") {
        String::new()
    } else {
        args
    };
    format!("{base}{args}{rest}")
}

/// Defaults compared without casts and outer parentheses: `'a'::text` =
/// `'a'`, `(0)` = `0`.
pub fn normalize_default(d: &str) -> String {
    let mut s = d.trim().to_owned();
    while s.starts_with('(') && s.ends_with(')') && balanced(&s[1..s.len() - 1]) {
        s = s[1..s.len() - 1].trim().to_owned();
    }
    // Strip trailing casts outside quotes: 'x'::character varying.
    if let Some(i) = s.rfind("::")
        && !s[i..].contains('\'')
    {
        s.truncate(i);
    }
    s.trim().to_ascii_lowercase()
}

fn balanced(s: &str) -> bool {
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

/// Definition text compared without case, spacing and a final `;`.
pub fn normalize_text(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(';')
        .trim()
        .to_ascii_lowercase()
}

fn same_type(a: &str, b: &str, opts: &CompareOptions) -> bool {
    if opts.normalize_types {
        normalize_type(a) == normalize_type(b)
    } else {
        a == b
    }
}

fn same_default(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => normalize_default(x) == normalize_default(y),
        // `DEFAULT NULL` is no default.
        (Some(x), None) | (None, Some(x)) => normalize_default(x) == "null",
    }
}

fn same_columns(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

fn same_fk(a: &ForeignKey, b: &ForeignKey) -> bool {
    same_columns(&a.columns, &b.columns)
        && a.ref_table == b.ref_table
        && same_columns(&a.ref_columns, &b.ref_columns)
        && norm_action(&a.on_delete) == norm_action(&b.on_delete)
        && norm_action(&a.on_update) == norm_action(&b.on_update)
}

fn norm_action(a: &Option<String>) -> String {
    match a.as_deref().map(str::to_ascii_uppercase).as_deref() {
        None | Some("NO ACTION") | Some("RESTRICT") => String::new(),
        Some(x) => x.to_owned(),
    }
}

fn same_index(a: &Index, b: &Index) -> bool {
    a.unique == b.unique
        && a.columns.len() == b.columns.len()
        && a.columns
            .iter()
            .zip(&b.columns)
            .all(|(x, y)| normalize_text(&strip_quotes(x)) == normalize_text(&strip_quotes(y)))
        && a.include == b.include
        && a.predicate.as_deref().map(normalize_text) == b.predicate.as_deref().map(normalize_text)
        && a.method
            .as_deref()
            .map(str::to_ascii_lowercase)
            .filter(|m| m != "btree")
            == b.method
                .as_deref()
                .map(str::to_ascii_lowercase)
                .filter(|m| m != "btree")
}

fn strip_quotes(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '"' | '`')).collect()
}

/// Foreign keys are matched by name when both have one, else by columns
/// and the referenced table.
fn fk_match<'t>(fk: &ForeignKey, among: &'t [ForeignKey]) -> Option<&'t ForeignKey> {
    if let Some(n) = &fk.name
        && let Some(f) = among.iter().find(|f| f.name.as_ref() == Some(n))
    {
        return Some(f);
    }
    among
        .iter()
        .find(|f| same_columns(&f.columns, &fk.columns) && f.ref_table == fk.ref_table)
}

fn key_match<'k>(k: &Key, among: &'k [Key]) -> Option<&'k Key> {
    among.iter().find(|x| same_columns(&x.columns, &k.columns))
}

fn check_match<'c>(c: &Check, among: &'c [Check]) -> Option<&'c Check> {
    among
        .iter()
        .find(|x| normalize_text(&x.expression) == normalize_text(&c.expression))
}

// -------------------------------------------------------------- columns

fn column_changes(a: &Column, b: &Column, opts: &CompareOptions) -> Vec<String> {
    let mut out = Vec::new();
    if !same_type(&a.data_type, &b.data_type, opts) {
        out.push(format!("type {} → {}", a.data_type, b.data_type));
    }
    if a.nullable != b.nullable {
        out.push(if b.nullable {
            "NOT NULL dropped".into()
        } else {
            "NOT NULL added".into()
        });
    }
    if !same_default(&a.default, &b.default) {
        out.push(format!(
            "default {} → {}",
            a.default.as_deref().unwrap_or("none"),
            b.default.as_deref().unwrap_or("none")
        ));
    }
    if (a.identity.is_some() || a.auto_increment || a.serial)
        != (b.identity.is_some() || b.auto_increment || b.serial)
    {
        out.push(if b.identity.is_some() || b.auto_increment || b.serial {
            "auto-increment added".into()
        } else {
            "auto-increment dropped".into()
        });
    }
    if a.generated.as_deref().map(normalize_text) != b.generated.as_deref().map(normalize_text) {
        out.push("generated expression changed".into());
    }
    if !opts.ignore_comments && a.comment != b.comment {
        out.push("comment changed".into());
    }
    out
}

fn literal_comment(t: &Target<'_>, c: &Option<String>) -> String {
    match c {
        Some(c) => ddl::literal(t.dialect, c),
        None => "NULL".into(),
    }
}

/// `ALTER TABLE t …` statements turning column `a` into `b` (same name).
fn alter_column(
    t: &Target<'_>,
    table: &str,
    a: &Column,
    b: &Column,
    opts: &CompareOptions,
) -> Vec<String> {
    let name = t.qualified(table);
    let col = t.q(&b.name);
    match t.dialect {
        Dialect::MySql | Dialect::MariaDb => {
            vec![format!(
                "ALTER TABLE {name} MODIFY COLUMN {};",
                ddl::column_def(t, b)
            )]
        }
        Dialect::SqlServer => {
            let mut out = Vec::new();
            if (a.generated.is_some() || b.generated.is_some())
                && a.generated.as_deref().map(normalize_text)
                    != b.generated.as_deref().map(normalize_text)
            {
                // A computed column is dropped and added again.
                out.push(
                    ddl::drop_stmt(t, Object::Column(&named(table), a))
                        .trim_end()
                        .to_owned(),
                );
                out.push(format!("ALTER TABLE {name} ADD {};", ddl::column_def(t, b)));
                return out;
            }
            // Type and nullability are restated together.
            if !same_type(&a.data_type, &b.data_type, opts)
                || a.nullable != b.nullable
                || a.collation != b.collation
            {
                let mut def = format!("ALTER TABLE {name} ALTER COLUMN {col} {}", b.data_type);
                if let Some(c) = &b.collation {
                    def.push_str(&format!(" COLLATE {c}"));
                }
                def.push_str(if b.nullable { " NULL;" } else { " NOT NULL;" });
                out.push(def);
            }
            if !same_default(&a.default, &b.default) {
                out.push(ddl::mssql_drop_default(t, table, &b.name));
                if let Some(d) = &b.default {
                    out.push(format!("ALTER TABLE {name} ADD DEFAULT {d} FOR {col};"));
                }
            }
            if a.identity.is_some() != b.identity.is_some() {
                out.push(format!(
                    "-- {col}: SQL Server cannot add or remove IDENTITY on an existing column; \
                     recreate the table to change it"
                ));
            }
            if !opts.ignore_comments && a.comment != b.comment {
                out.push(ddl::mssql_comment(
                    t,
                    table,
                    "TABLE",
                    Some(&b.name),
                    b.comment.as_deref(),
                ));
            }
            out
        }
        _ => {
            let mut out = Vec::new();
            if !same_type(&a.data_type, &b.data_type, opts) {
                let using = if t.dialect == Dialect::Postgres {
                    format!(" USING {col}::{}", b.data_type)
                } else {
                    String::new()
                };
                out.push(format!(
                    "ALTER TABLE {name} ALTER COLUMN {col} {}{} {}{using};",
                    if t.dialect == Dialect::DuckDb {
                        "SET DATA "
                    } else {
                        ""
                    },
                    "TYPE",
                    b.data_type
                ));
            }
            if a.nullable != b.nullable {
                out.push(format!(
                    "ALTER TABLE {name} ALTER COLUMN {col} {} NOT NULL;",
                    if b.nullable { "DROP" } else { "SET" }
                ));
            }
            if !same_default(&a.default, &b.default) {
                out.push(match &b.default {
                    Some(d) => format!("ALTER TABLE {name} ALTER COLUMN {col} SET DEFAULT {d};"),
                    None => format!("ALTER TABLE {name} ALTER COLUMN {col} DROP DEFAULT;"),
                });
            }
            let had = a.identity.is_some();
            if t.dialect == Dialect::Postgres && had != b.identity.is_some() {
                out.push(match &b.identity {
                    Some(kind) => format!(
                        "ALTER TABLE {name} ALTER COLUMN {col} ADD GENERATED {kind} AS IDENTITY;"
                    ),
                    None => {
                        format!("ALTER TABLE {name} ALTER COLUMN {col} DROP IDENTITY IF EXISTS;")
                    }
                });
            }
            if !opts.ignore_comments && a.comment != b.comment && t.dialect == Dialect::Postgres {
                out.push(format!(
                    "COMMENT ON COLUMN {name}.{col} IS {};",
                    literal_comment(t, &b.comment)
                ));
            }
            out
        }
    }
}

fn drop_check(t: &Target<'_>, table: &str, c: &Check) -> String {
    let name = c.name.as_deref().unwrap_or("?");
    match t.dialect {
        Dialect::MySql => format!(
            "ALTER TABLE {} DROP CHECK {};",
            t.qualified(table),
            t.q(name)
        ),
        _ => format!(
            "ALTER TABLE {} DROP CONSTRAINT {};",
            t.qualified(table),
            t.q(name)
        ),
    }
}

fn add_check(t: &Target<'_>, table: &str, c: &Check) -> String {
    let expr = c.expression.trim();
    let expr = if expr.starts_with('(') {
        expr.to_owned()
    } else {
        format!("({expr})")
    };
    format!(
        "ALTER TABLE {} ADD {}CHECK {expr};",
        t.qualified(table),
        ddl::constraint_name(t, &c.name)
    )
}

/// A table known by name only (for statements that need just the name).
fn named(name: &str) -> Table {
    Table {
        name: name.to_owned(),
        ..Default::default()
    }
}

fn rename_table(t: &Target<'_>, from: &str, to: &str) -> String {
    if t.dialect == Dialect::SqlServer {
        return format!(
            "EXEC sp_rename {}, {};",
            ddl::nliteral(&t.qualified(from)),
            ddl::nliteral(to)
        );
    }
    if t.dialect.is_mysql() {
        format!("RENAME TABLE {} TO {};", t.qualified(from), t.qualified(to))
    } else {
        // The new name is never qualified (it stays in the schema).
        format!("ALTER TABLE {} RENAME TO {};", t.qualified(from), t.q(to))
    }
}

// --------------------------------------------------------------- tables

/// Does turning `old` into `new` need a SQLite table rebuild?
fn sqlite_needs_rebuild(
    old: &Table,
    new: &Table,
    renames: &[(String, String)],
    opts: &CompareOptions,
) -> bool {
    let mapped = |c: &str| {
        renames
            .iter()
            .find(|(n, _)| n == c)
            .map(|(_, o)| o.as_str())
            .unwrap_or(c)
            .to_owned()
    };
    let pk = |t: &Table| {
        t.primary_key
            .as_ref()
            .map(|k| k.columns.clone())
            .unwrap_or_default()
    };
    let new_pk: Vec<String> = pk(new).iter().map(|c| mapped(c)).collect();
    if !same_columns(&pk(old), &new_pk) {
        return true;
    }
    for c in &new.columns {
        let src = mapped(&c.name);
        match old.column(&src) {
            Some(o) => {
                if !column_changes(
                    o,
                    c,
                    &CompareOptions {
                        ignore_comments: true,
                        ..opts.clone()
                    },
                )
                .is_empty()
                {
                    return true;
                }
            }
            // ADD COLUMN cannot add NOT NULL without a default.
            None => {
                if !c.nullable && c.default.is_none() {
                    return true;
                }
            }
        }
    }
    let used: Vec<&String> = old
        .uniques
        .iter()
        .flat_map(|k| &k.columns)
        .chain(old.foreign_keys.iter().flat_map(|f| &f.columns))
        .chain(old.primary_key.iter().flat_map(|k| &k.columns))
        .collect();
    for c in &old.columns {
        let kept = new.columns.iter().any(|n| mapped(&n.name) == c.name);
        if !kept && used.contains(&&c.name) {
            return true;
        }
    }
    let fks_same = old.foreign_keys.len() == new.foreign_keys.len()
        && new
            .foreign_keys
            .iter()
            .all(|f| fk_match(f, &old.foreign_keys).is_some_and(|o| same_fk(o, f)));
    let uniques_same = old.uniques.len() == new.uniques.len()
        && new
            .uniques
            .iter()
            .all(|k| key_match(k, &old.uniques).is_some());
    let checks_same = old.checks.len() == new.checks.len()
        && new
            .checks
            .iter()
            .all(|c| check_match(c, &old.checks).is_some());
    !(fks_same && uniques_same && checks_same)
}

/// SQLite: create the new table under a temporary name, copy the rows,
/// replace the old table, recreate indexes and triggers.
fn sqlite_rebuild(
    t: &Target<'_>,
    old: &Table,
    new: &Table,
    renames: &[(String, String)],
) -> Change {
    let mut ch = Change::default();
    let tmp_name = format!("_ferrodb_new_{}", new.name);
    let mut tmp = new.clone();
    tmp.name = tmp_name.clone();
    tmp.server_sql = None;
    tmp.indexes.clear();
    tmp.triggers.clear();
    let bare = Target {
        opts: ferrodb_api::DdlOptions {
            include_indexes: false,
            include_comments: false,
        },
        defer_triggers: true,
        ..t.clone()
    };
    // Keep `AUTOINCREMENT` of the old table.
    if let Some(sql) = &old.server_sql
        && sql.to_ascii_uppercase().contains("AUTOINCREMENT")
        && let Some(pk) = &tmp.primary_key
        && pk.columns.len() == 1
    {
        let pkc = pk.columns[0].clone();
        if let Some(c) = tmp.columns.iter_mut().find(|c| c.name == pkc) {
            c.auto_increment = true;
        }
    }
    ch.push(P_ALTER, "PRAGMA defer_foreign_keys = ON;");
    ch.push(P_ALTER, ddl::table_ddl(&bare, &tmp, &[]));
    let pairs: Vec<(String, String)> = new
        .columns
        .iter()
        .filter_map(|c| {
            let src = renames
                .iter()
                .find(|(n, _)| *n == c.name)
                .map(|(_, o)| o.clone())
                .unwrap_or_else(|| c.name.clone());
            old.column(&src).map(|_| (c.name.clone(), src))
        })
        .collect();
    if !pairs.is_empty() {
        let into: Vec<String> = pairs.iter().map(|(n, _)| t.q(n)).collect();
        let from: Vec<String> = pairs.iter().map(|(_, o)| t.q(o)).collect();
        ch.push(
            P_ALTER,
            format!(
                "INSERT INTO {} ({}) SELECT {} FROM {};",
                t.qualified(&tmp_name),
                into.join(", "),
                from.join(", "),
                t.qualified(&old.name)
            ),
        );
    }
    ch.push(P_ALTER, format!("DROP TABLE {};", t.qualified(&old.name)));
    // Views and triggers naming the table would make the rename fail while
    // the table is missing; the legacy rename does not check them.
    ch.push(P_ALTER, "PRAGMA legacy_alter_table = ON;");
    ch.push(P_ALTER, rename_table(t, &tmp_name, &new.name));
    ch.push(P_ALTER, "PRAGMA legacy_alter_table = OFF;");
    for i in &new.indexes {
        let mut i = i.clone();
        // A stored statement may name the old table or dropped columns.
        if old.name != new.name || !renames.is_empty() {
            i.server_sql = None;
        }
        ch.push(P_INDEX, ddl::index_stmt(t, new, &i));
    }
    for tr in &old.triggers {
        if let Some(d) = &tr.definition {
            ch.push(P_TRIGGER, format!("{};", d.trim().trim_end_matches(';')));
        }
    }
    ch.warnings.push(format!(
        "SQLite cannot alter table {} in place: it is rebuilt (rows copied into a new table)",
        new.name
    ));
    if old
        .server_sql
        .as_deref()
        .is_some_and(|s| s.to_ascii_uppercase().contains("CHECK"))
        && new.checks.is_empty()
    {
        ch.warnings.push(format!(
            "CHECK constraints of {} are not read from SQLite and are not carried over by the rebuild",
            old.name
        ));
    }
    ch
}

/// Statements turning table `old` into `new` in target `t`. `renames`
/// pairs (new column name, old column name) of renamed columns.
pub fn alter_table(
    t: &Target<'_>,
    old: &Table,
    new: &Table,
    renames: &[(String, String)],
    opts: &CompareOptions,
) -> Change {
    let mut ch = Change::default();
    // Drops of columns lose data: say so whatever the dialect.
    for c in &old.columns {
        let kept = new.columns.iter().any(|n| {
            n.name == c.name || renames.iter().any(|(nn, o)| *nn == n.name && *o == c.name)
        });
        if !kept {
            ch.details.push(format!("column {} dropped", c.name));
            ch.warnings.push(format!(
                "dropping column {}.{} deletes its data",
                old.name, c.name
            ));
        }
    }
    for (n, o) in renames {
        if n != o {
            ch.details.push(format!("column {o} renamed to {n}"));
        }
    }
    let old_of = |c: &str| -> String {
        renames
            .iter()
            .find(|(n, _)| n == c)
            .map(|(_, o)| o.clone())
            .unwrap_or_else(|| c.to_owned())
    };
    for c in &new.columns {
        match old.column(&old_of(&c.name)) {
            None => ch.details.push(format!("column {} added", c.name)),
            Some(o) => {
                for d in column_changes(o, c, opts) {
                    ch.details.push(format!("column {}: {d}", c.name));
                }
            }
        }
    }
    if t.dialect == Dialect::Sqlite && sqlite_needs_rebuild(old, new, renames, opts) {
        let r = sqlite_rebuild(t, old, new, renames);
        ch.statements = r.statements;
        ch.warnings.extend(r.warnings);
        return ch;
    }

    let mut name = old.name.clone();
    if old.name != new.name {
        ch.details
            .push(format!("renamed from {} to {}", old.name, new.name));
        ch.push(P_ALTER, rename_table(t, &old.name, &new.name));
        name = new.name.clone();
    }
    let qname = t.qualified(&name);
    for (n, o) in renames {
        if n != o && old.column(o).is_some() {
            ch.push(
                P_ALTER,
                if t.dialect == Dialect::SqlServer {
                    format!(
                        "EXEC sp_rename {}, {}, N'COLUMN';",
                        ddl::nliteral(&format!("{qname}.{}", t.q(o))),
                        ddl::nliteral(n)
                    )
                } else {
                    format!(
                        "ALTER TABLE {qname} RENAME COLUMN {} TO {};",
                        t.q(o),
                        t.q(n)
                    )
                },
            );
        }
    }

    // Foreign keys: dropped first (before a rename, so by the old name),
    // added last.
    for f in &old.foreign_keys {
        let kept = fk_match(f, &new.foreign_keys).filter(|n| same_fk(f, n));
        if kept.is_none() {
            if f.name.is_some() {
                ch.push(P_DROP_FK, ddl::drop_stmt(t, Object::ForeignKey(old, f)));
            } else {
                ch.warnings.push(format!(
                    "foreign key of {name} ({}) has no name and cannot be dropped",
                    f.columns.join(", ")
                ));
            }
            ch.details.push(format!(
                "foreign key ({}) → {} dropped",
                f.columns.join(", "),
                f.ref_table
            ));
        }
    }
    for f in &new.foreign_keys {
        let existing = fk_match(f, &old.foreign_keys).filter(|o| same_fk(o, f));
        if existing.is_none() {
            ch.push(
                P_ADD_FK,
                format!("ALTER TABLE {qname} ADD {};", ddl::fk_clause(t, f)),
            );
            ch.details.push(format!(
                "foreign key ({}) → {} added",
                f.columns.join(", "),
                f.ref_table
            ));
        }
    }

    // Primary key.
    let old_pk = old
        .primary_key
        .as_ref()
        .map(|k| k.columns.clone())
        .unwrap_or_default();
    let new_pk = new
        .primary_key
        .as_ref()
        .map(|k| k.columns.clone())
        .unwrap_or_default();
    let old_pk_mapped: Vec<String> = old_pk
        .iter()
        .map(|c| {
            renames
                .iter()
                .find(|(_, o)| o == c)
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| c.clone())
        })
        .collect();
    if !same_columns(&old_pk_mapped, &new_pk) {
        if let Some(k) = &old.primary_key {
            let mut k = k.clone();
            if k.name.is_none() && t.dialect == Dialect::Postgres {
                k.name = Some(format!("{}_pkey", old.name));
            }
            if k.name.is_none() && t.dialect == Dialect::SqlServer {
                // Look the system-generated name up at run time.
                ch.push(
                    P_ALTER,
                    format!(
                        "DECLARE @pk sysname = (SELECT name FROM sys.key_constraints \
                         WHERE parent_object_id = OBJECT_ID({}) AND type = 'PK'); \
                         IF @pk IS NOT NULL BEGIN DECLARE @pksql nvarchar(max) = {} + QUOTENAME(@pk); EXEC(@pksql); END",
                        ddl::nliteral(&qname),
                        ddl::nliteral(&format!("ALTER TABLE {qname} DROP CONSTRAINT "))
                    ),
                );
            } else {
                ch.push(
                    P_ALTER,
                    ddl::drop_stmt(t, Object::PrimaryKey(&with_name(old, &name), &k)),
                );
            }
        }
        if !new_pk.is_empty() {
            ch.push(
                P_ALTER,
                format!(
                    "ALTER TABLE {qname} ADD PRIMARY KEY ({});",
                    ddl::idents(t, &new_pk)
                ),
            );
        }
        ch.details.push(format!(
            "primary key ({}) → ({})",
            old_pk.join(", "),
            new_pk.join(", ")
        ));
    }

    // Columns: drop, add, alter.
    for c in &old.columns {
        let kept = new.columns.iter().any(|n| old_of(&n.name) == c.name);
        if !kept {
            ch.push(
                P_ALTER,
                ddl::drop_stmt(t, Object::Column(&with_name(old, &name), c)),
            );
        }
    }
    for c in &new.columns {
        match old.column(&old_of(&c.name)) {
            None => {
                if !c.nullable && c.default.is_none() {
                    ch.warnings.push(format!(
                        "column {}.{} is NOT NULL without a default: adding it fails if the table has rows",
                        new.name, c.name
                    ));
                }
                ch.push(
                    P_ALTER,
                    format!(
                        "ALTER TABLE {qname} ADD {}{};",
                        if t.dialect == Dialect::SqlServer {
                            ""
                        } else {
                            "COLUMN "
                        },
                        ddl::column_def(t, c)
                    ),
                );
                if t.dialect == Dialect::SqlServer
                    && !opts.ignore_comments
                    && let Some(cm) = &c.comment
                {
                    ch.push(
                        P_ALTER,
                        ddl::mssql_comment(t, &name, "TABLE", Some(&c.name), Some(cm)),
                    );
                }
                if t.dialect == Dialect::Postgres
                    && !opts.ignore_comments
                    && let Some(cm) = &c.comment
                {
                    ch.push(
                        P_ALTER,
                        format!(
                            "COMMENT ON COLUMN {qname}.{} IS {};",
                            t.q(&c.name),
                            ddl::literal(t.dialect, cm)
                        ),
                    );
                }
            }
            Some(o) => {
                if !column_changes(o, c, opts).is_empty() {
                    if !same_type(&o.data_type, &c.data_type, opts) {
                        ch.warnings.push(format!(
                            "changing the type of {}.{} converts existing values and may fail",
                            new.name, c.name
                        ));
                    }
                    let mut renamed_old = o.clone();
                    renamed_old.name = c.name.clone();
                    for s in alter_column(t, &name, &renamed_old, c, opts) {
                        ch.push(P_ALTER, s);
                    }
                }
            }
        }
    }

    // Unique constraints.
    for k in &old.uniques {
        if key_match(k, &new.uniques).is_none() {
            match &k.name {
                Some(_) if t.dialect.is_mysql() => ch.push(
                    P_DROP_INDEX,
                    format!(
                        "ALTER TABLE {} DROP INDEX {};",
                        t.qualified(&old.name),
                        t.q(k.name.as_deref().unwrap_or_default())
                    ),
                ),
                Some(_) => ch.push(P_DROP_INDEX, ddl::drop_stmt(t, Object::Unique(old, k))),
                None => ch.warnings.push(format!(
                    "unique constraint ({}) has no name and cannot be dropped",
                    k.columns.join(", ")
                )),
            }
            ch.details
                .push(format!("unique ({}) dropped", k.columns.join(", ")));
        }
    }
    for k in &new.uniques {
        if key_match(k, &old.uniques).is_none() {
            ch.push(
                P_INDEX,
                format!(
                    "ALTER TABLE {qname} ADD {}UNIQUE ({});",
                    ddl::constraint_name(t, &k.name),
                    ddl::idents(t, &k.columns)
                ),
            );
            ch.details
                .push(format!("unique ({}) added", k.columns.join(", ")));
        }
    }

    // Checks.
    for c in &old.checks {
        if check_match(c, &new.checks).is_none() {
            if c.name.is_some() {
                ch.push(P_ALTER, drop_check(t, &name, c));
            } else {
                ch.warnings.push(format!(
                    "check ({}) has no name and cannot be dropped",
                    c.expression
                ));
            }
            ch.details.push(format!("check {} dropped", c.expression));
        }
    }
    for c in &new.checks {
        if check_match(c, &old.checks).is_none() {
            ch.push(P_ALTER, add_check(t, &name, c));
            ch.details.push(format!("check {} added", c.expression));
        }
    }

    // Indexes (by name).
    let target_table = with_name(new, &name);
    for i in &old.indexes {
        match new.indexes.iter().find(|n| n.name == i.name) {
            Some(n) if same_index(i, n) => {}
            other => {
                ch.push(P_DROP_INDEX, ddl::drop_stmt(t, Object::Index(old, i)));
                ch.details.push(format!(
                    "index {} {}",
                    i.name,
                    if other.is_some() {
                        "changed"
                    } else {
                        "dropped"
                    }
                ));
            }
        }
    }
    for i in &new.indexes {
        match old.indexes.iter().find(|o| o.name == i.name) {
            Some(o) if same_index(o, i) => {}
            other => {
                let mut i = i.clone();
                if old.name != new.name {
                    i.server_sql = None;
                }
                ch.push(P_INDEX, ddl::index_stmt(t, &target_table, &i));
                if other.is_none() {
                    ch.details.push(format!("index {} added", i.name));
                }
            }
        }
    }

    // Table comment.
    if !opts.ignore_comments && old.comment != new.comment {
        ch.details.push("table comment changed".into());
        match t.dialect {
            Dialect::Postgres => ch.push(
                P_ALTER,
                format!(
                    "COMMENT ON TABLE {qname} IS {};",
                    literal_comment(t, &new.comment)
                ),
            ),
            Dialect::MySql | Dialect::MariaDb => ch.push(
                P_ALTER,
                format!(
                    "ALTER TABLE {qname} COMMENT = {};",
                    ddl::literal(t.dialect, new.comment.as_deref().unwrap_or(""))
                ),
            ),
            Dialect::SqlServer => ch.push(
                P_ALTER,
                ddl::mssql_comment(t, &name, "TABLE", None, new.comment.as_deref()),
            ),
            _ => {}
        }
    }

    // Triggers (by name, definition text).
    for tr in &old.triggers {
        match new.triggers.iter().find(|n| n.name == tr.name) {
            Some(n) if same_definition(&tr.definition, &n.definition) => {}
            other => {
                ch.push(P_DROP_TRIGGER, ddl::drop_stmt(t, Object::Trigger(old, tr)));
                if other.is_none() {
                    ch.details.push(format!("trigger {} dropped", tr.name));
                }
            }
        }
    }
    for tr in &new.triggers {
        match old.triggers.iter().find(|o| o.name == tr.name) {
            Some(o) if same_definition(&o.definition, &tr.definition) => {}
            other => {
                if let Some(d) = &tr.definition {
                    ch.push(P_TRIGGER, format!("{};", d.trim().trim_end_matches(';')));
                }
                ch.details.push(format!(
                    "trigger {} {}",
                    tr.name,
                    if other.is_some() { "changed" } else { "added" }
                ));
            }
        }
    }
    ch
}

fn same_definition(a: &Option<String>, b: &Option<String>) -> bool {
    a.as_deref().map(normalize_text) == b.as_deref().map(normalize_text)
}

fn with_name(t: &Table, name: &str) -> Table {
    if t.name == name {
        return t.clone();
    }
    Table {
        name: name.to_owned(),
        ..t.clone()
    }
}

/// `CREATE TABLE` (with indexes and comments) of a new table.
pub fn create_table(t: &Target<'_>, table: &Table) -> Change {
    let mut ch = Change::default();
    let full = Target {
        defer_triggers: false,
        ..t.clone()
    };
    for s in split_statements(&ddl::table_ddl(&full, table, &[])) {
        ch.push(P_CREATE_TABLE, s);
    }
    ch.details.push(format!("table {} created", table.name));
    ch
}

/// Splits generated DDL into statements (one per line group ending in `;`;
/// trigger bodies with `BEGIN … END` stay whole).
fn split_statements(ddl: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for line in ddl.lines() {
        let upper = line.trim().to_ascii_uppercase();
        if upper.ends_with("BEGIN") || upper.contains(" BEGIN ") || upper == "BEGIN" {
            depth += 1;
        }
        if upper.starts_with("END") && depth > 0 {
            depth -= 1;
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
        if depth == 0 && line.trim_end().ends_with(';') {
            out.push(std::mem::take(&mut cur).trim().to_owned());
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    out.retain(|s| !s.is_empty());
    out
}

// ------------------------------------------------------------- schemas

fn item(
    kind: DiffKind,
    object_kind: ObjectKind,
    name: impl Into<String>,
    detail: impl Into<String>,
    source_ddl: Option<String>,
    target_ddl: Option<String>,
) -> SchemaDiffItem {
    SchemaDiffItem {
        kind,
        object_kind,
        name: name.into(),
        detail: detail.into(),
        source_ddl,
        target_ddl,
        statements: Vec::new(),
    }
}

fn table_object_kind(t: &Table) -> ObjectKind {
    match t.kind {
        TableKind::View => ObjectKind::View,
        TableKind::MaterializedView => ObjectKind::MaterializedView,
        _ => ObjectKind::Table,
    }
}

fn routine_object_kind(r: &Routine) -> ObjectKind {
    match r.kind {
        RoutineKind::Function => ObjectKind::Function,
        RoutineKind::Procedure => ObjectKind::Procedure,
    }
}

/// Compares `source` with `target`; the script makes `target` like
/// `source`.
pub fn compare(source: Side<'_>, target: Side<'_>, opts: &CompareOptions) -> Comparison {
    let mut out = Comparison::default();
    if source.dialect != target.dialect {
        out.warnings.push(format!(
            "comparing {:?} with {:?}: types and definitions are copied as written and may need changes",
            source.dialect, target.dialect
        ));
    }
    // Source objects written into the target container, target's dialect.
    let src_t = Target {
        rename: Some(target.container),
        ..Target::new(target.dialect, source.container)
    };
    let src_view = Target::new(source.dialect, source.container);
    let tgt_t = Target::new(target.dialect, target.container);
    let mut all: Vec<(u8, String)> = Vec::new();
    let mut add = |out: &mut Comparison, mut it: SchemaDiffItem, ch: &Change| {
        it.statements = ch.ordered();
        all.extend(ch.statements.iter().cloned());
        out.warnings.extend(ch.warnings.iter().cloned());
        out.items.push(it);
    };

    // Sequences first: column defaults may use them.
    for s in &source.content.sequences {
        let mut ch = Change::default();
        match target.content.sequences.iter().find(|x| x.name == s.name) {
            None => {
                ch.push(P_SEQUENCE, ddl::object_ddl(&src_t, Object::Sequence(s)));
                let it = item(
                    DiffKind::Added,
                    ObjectKind::Sequence,
                    &s.name,
                    "sequence created",
                    Some(ddl::object_ddl(&src_view, Object::Sequence(s))),
                    None,
                );
                add(&mut out, it, &ch);
            }
            Some(x) if !same_sequence(s, x) => {
                if target.dialect == Dialect::Postgres {
                    ch.push(P_SEQUENCE, alter_sequence(&tgt_t, s));
                } else {
                    ch.push(P_SEQUENCE, ddl::drop_stmt(&tgt_t, Object::Sequence(x)));
                    ch.push(P_SEQUENCE, ddl::object_ddl(&src_t, Object::Sequence(s)));
                }
                let it = item(
                    DiffKind::Changed,
                    ObjectKind::Sequence,
                    &s.name,
                    "sequence options changed",
                    Some(ddl::object_ddl(&src_view, Object::Sequence(s))),
                    Some(ddl::object_ddl(&tgt_t, Object::Sequence(x))),
                );
                add(&mut out, it, &ch);
            }
            _ => {}
        }
    }

    // Tables (not views).
    let src_tables: Vec<&Table> = source
        .content
        .tables
        .iter()
        .filter(|t| !t.is_view())
        .collect();
    let tgt_tables: Vec<&Table> = target
        .content
        .tables
        .iter()
        .filter(|t| !t.is_view())
        .collect();
    let added: Vec<&Table> = src_tables
        .iter()
        .copied()
        .filter(|s| !tgt_tables.iter().any(|t| t.name == s.name))
        .collect();
    if !added.is_empty() {
        // Foreign keys are added once every table exists (SQLite cannot
        // add them later, but does not check the referenced table early).
        let no_triggers = Target {
            defer_triggers: true,
            ..src_t.clone()
        };
        for s in &added {
            let mut ch = Change::default();
            let mut s_in = (*s).clone();
            for f in &mut s_in.foreign_keys {
                if f.ref_container.as_deref() == Some(source.container) {
                    f.ref_container = None;
                }
            }
            if source.dialect != target.dialect {
                s_in.server_sql = None;
                for i in &mut s_in.indexes {
                    i.server_sql = None;
                }
            }
            let inline = target.dialect == Dialect::Sqlite;
            let deferred: Vec<&ForeignKey> = if inline {
                Vec::new()
            } else {
                s_in.foreign_keys.iter().collect()
            };
            for st in split_statements(&ddl::table_ddl(&no_triggers, &s_in, &deferred)) {
                let phase =
                    if st.starts_with("CREATE INDEX") || st.starts_with("CREATE UNIQUE INDEX") {
                        P_INDEX
                    } else {
                        P_CREATE_TABLE
                    };
                ch.push(phase, st);
            }
            for f in &deferred {
                ch.push(
                    P_ADD_FK,
                    format!(
                        "ALTER TABLE {} ADD {};",
                        src_t.qualified(&s_in.name),
                        ddl::fk_clause(&src_t, f)
                    ),
                );
            }
            if opts.include_routines {
                for tr in &s_in.triggers {
                    if let Some(d) = &tr.definition {
                        ch.push(P_TRIGGER, format!("{};", d.trim().trim_end_matches(';')));
                    }
                }
            }
            let it = item(
                DiffKind::Added,
                ObjectKind::Table,
                &s.name,
                format!("table with {} columns", s.columns.len()),
                Some(ddl::object_ddl(&src_view, Object::Table(s))),
                None,
            );
            add(&mut out, it, &ch);
        }
    }
    for t in &tgt_tables {
        if src_tables.iter().any(|s| s.name == t.name) {
            continue;
        }
        let mut ch = Change::default();
        if opts.drop_removed {
            for f in &t.foreign_keys {
                if f.name.is_some() && target.dialect != Dialect::Sqlite {
                    ch.push(P_DROP_FK, ddl::drop_stmt(&tgt_t, Object::ForeignKey(t, f)));
                }
            }
            ch.push(P_DROP_TABLE, ddl::drop_stmt(&tgt_t, Object::Table(t)));
            ch.warnings
                .push(format!("dropping table {} deletes its data", t.name));
        }
        let it = item(
            DiffKind::Removed,
            ObjectKind::Table,
            &t.name,
            if opts.drop_removed {
                "table dropped"
            } else {
                "only in the target (kept; enable dropping to remove it)"
            },
            None,
            Some(ddl::object_ddl(&tgt_t, Object::Table(t))),
        );
        add(&mut out, it, &ch);
    }
    for s in &src_tables {
        let Some(t) = tgt_tables.iter().find(|t| t.name == s.name) else {
            continue;
        };
        // The source table as it would be in the target container.
        let mut want = (*s).clone();
        for f in &mut want.foreign_keys {
            if f.ref_container.as_deref() == Some(source.container) {
                f.ref_container = None;
            }
        }
        if source.dialect != target.dialect {
            want.server_sql = None;
            for i in &mut want.indexes {
                i.server_sql = None;
            }
        }
        let mut ch = alter_table(&tgt_t, t, &want, &[], opts);
        if !opts.include_routines {
            ch.statements
                .retain(|(p, _)| *p != P_TRIGGER && *p != P_DROP_TRIGGER);
        }
        if ch.is_empty() {
            continue;
        }
        let it = item(
            DiffKind::Changed,
            ObjectKind::Table,
            &s.name,
            ch.details.join("; "),
            Some(ddl::object_ddl(&src_view, Object::Table(s))),
            Some(ddl::object_ddl(&tgt_t, Object::Table(t))),
        );
        add(&mut out, it, &ch);
    }

    // Views: drop changed ones before the tables change, create after.
    let src_views: Vec<&Table> = source
        .content
        .tables
        .iter()
        .filter(|t| t.is_view())
        .collect();
    let tgt_views: Vec<&Table> = target
        .content
        .tables
        .iter()
        .filter(|t| t.is_view())
        .collect();
    for v in &src_views {
        let existing = tgt_views.iter().find(|t| t.name == v.name);
        let same = existing.is_some_and(|t| {
            same_definition(&t.view_definition, &v.view_definition)
                && (t.view_definition.is_some() || same_definition(&t.server_sql, &v.server_sql))
        });
        if same {
            continue;
        }
        let mut ch = Change::default();
        if let Some(t) = existing {
            ch.push(P_DROP_VIEW, ddl::drop_stmt(&tgt_t, Object::Table(t)));
        }
        for st in split_statements(&ddl::object_ddl(&src_t, Object::Table(v))) {
            ch.push(P_VIEW, st);
        }
        let it = item(
            if existing.is_some() {
                DiffKind::Changed
            } else {
                DiffKind::Added
            },
            table_object_kind(v),
            &v.name,
            if existing.is_some() {
                "definition changed"
            } else {
                "view created"
            },
            Some(ddl::object_ddl(&src_view, Object::Table(v))),
            existing.map(|t| ddl::object_ddl(&tgt_t, Object::Table(t))),
        );
        add(&mut out, it, &ch);
    }
    for t in &tgt_views {
        if src_views.iter().any(|v| v.name == t.name) {
            continue;
        }
        let mut ch = Change::default();
        if opts.drop_removed {
            ch.push(P_DROP_VIEW, ddl::drop_stmt(&tgt_t, Object::Table(t)));
        }
        let it = item(
            DiffKind::Removed,
            table_object_kind(t),
            &t.name,
            if opts.drop_removed {
                "view dropped"
            } else {
                "only in the target (kept)"
            },
            None,
            Some(ddl::object_ddl(&tgt_t, Object::Table(t))),
        );
        add(&mut out, it, &ch);
    }

    // Routines (by signature, definition text).
    if opts.include_routines {
        for r in &source.content.routines {
            let existing = target
                .content
                .routines
                .iter()
                .find(|x| x.signature == r.signature);
            if existing.is_some_and(|x| same_definition(&x.definition, &r.definition)) {
                continue;
            }
            let mut ch = Change::default();
            if let Some(x) = existing {
                ch.push(P_DROP_ROUTINE, ddl::drop_stmt(&tgt_t, Object::Routine(x)));
            }
            ch.push(P_ROUTINE, ddl::object_ddl(&src_t, Object::Routine(r)));
            let it = item(
                if existing.is_some() {
                    DiffKind::Changed
                } else {
                    DiffKind::Added
                },
                routine_object_kind(r),
                &r.signature,
                if existing.is_some() {
                    "definition changed"
                } else {
                    "created"
                },
                Some(ddl::object_ddl(&src_view, Object::Routine(r))),
                existing.map(|x| ddl::object_ddl(&tgt_t, Object::Routine(x))),
            );
            add(&mut out, it, &ch);
        }
        for x in &target.content.routines {
            if source
                .content
                .routines
                .iter()
                .any(|r| r.signature == x.signature)
            {
                continue;
            }
            let mut ch = Change::default();
            if opts.drop_removed {
                ch.push(P_DROP_ROUTINE, ddl::drop_stmt(&tgt_t, Object::Routine(x)));
            }
            let it = item(
                DiffKind::Removed,
                routine_object_kind(x),
                &x.signature,
                if opts.drop_removed {
                    "dropped"
                } else {
                    "only in the target (kept)"
                },
                None,
                Some(ddl::object_ddl(&tgt_t, Object::Routine(x))),
            );
            add(&mut out, it, &ch);
        }
    }
    for x in &target.content.sequences {
        if source.content.sequences.iter().any(|s| s.name == x.name) {
            continue;
        }
        let mut ch = Change::default();
        if opts.drop_removed {
            ch.push(P_DROP_TABLE, ddl::drop_stmt(&tgt_t, Object::Sequence(x)));
        }
        let it = item(
            DiffKind::Removed,
            ObjectKind::Sequence,
            &x.name,
            if opts.drop_removed {
                "dropped"
            } else {
                "only in the target (kept)"
            },
            None,
            Some(ddl::object_ddl(&tgt_t, Object::Sequence(x))),
        );
        add(&mut out, it, &ch);
    }

    all.sort_by_key(|(p, _)| *p);
    out.script = all
        .into_iter()
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join("\n");
    if !out.script.is_empty() {
        out.script.push('\n');
    }
    out.warnings.dedup();
    out
}

fn same_sequence(a: &Sequence, b: &Sequence) -> bool {
    a.increment == b.increment
        && a.min == b.min
        && a.max == b.max
        && a.cache == b.cache
        && a.cycle == b.cycle
}

fn alter_sequence(t: &Target<'_>, s: &Sequence) -> String {
    let mut out = format!("ALTER SEQUENCE {}", t.qualified(&s.name));
    if let Some(v) = s.increment {
        out.push_str(&format!(" INCREMENT BY {v}"));
    }
    match s.min {
        Some(v) => out.push_str(&format!(" MINVALUE {v}")),
        None => out.push_str(" NO MINVALUE"),
    }
    match s.max {
        Some(v) => out.push_str(&format!(" MAXVALUE {v}")),
        None => out.push_str(" NO MAXVALUE"),
    }
    if let Some(v) = s.cache {
        out.push_str(&format!(" CACHE {v}"));
    }
    out.push_str(if s.cycle { " CYCLE;" } else { " NO CYCLE;" });
    out
}

// ---------------------------------------------------------- table editor

/// A design (API) as a model table, keeping what the editor does not show
/// (stored SQL, triggers, index methods…) from `base`.
pub fn design_to_table(
    d: Dialect,
    design: &ferrodb_api::TableDesign,
    base: Option<&Table>,
) -> Table {
    let mut t = base.cloned().unwrap_or_default();
    t.name = design.name.trim().to_owned();
    t.kind = TableKind::Table;
    t.comment = design.comment.clone().filter(|c| !c.is_empty());
    t.server_sql = None;
    t.columns = design
        .columns
        .iter()
        .filter(|c| !c.name.trim().is_empty())
        .map(|c| {
            let old = c
                .original
                .as_deref()
                .and_then(|o| base.and_then(|b| b.column(o)));
            let mut col = old.cloned().unwrap_or_default();
            col.name = c.name.trim().to_owned();
            col.data_type = c.data_type.trim().to_owned();
            col.nullable = c.nullable;
            col.default = c.default.clone().filter(|x| !x.trim().is_empty());
            col.comment = c.comment.clone().filter(|x| !x.is_empty());
            let was_auto =
                old.is_some_and(|o| o.auto_increment || o.identity.is_some() || o.serial);
            if c.auto_increment != was_auto || old.is_none() {
                col.auto_increment = false;
                col.identity = None;
                col.serial = false;
                if c.auto_increment {
                    match d {
                        Dialect::Postgres => col.identity = Some("BY DEFAULT".into()),
                        Dialect::SqlServer => col.identity = Some("IDENTITY(1,1)".into()),
                        _ => col.auto_increment = true,
                    }
                }
            }
            col
        })
        .collect();
    t.primary_key = (!design.primary_key.is_empty()).then(|| Key {
        name: base
            .and_then(|b| b.primary_key.as_ref())
            .filter(|k| same_columns(&k.columns, &design.primary_key))
            .and_then(|k| k.name.clone()),
        columns: design.primary_key.clone(),
    });
    t.uniques = design
        .uniques
        .iter()
        .filter(|u| !u.is_empty())
        .map(|u| Key {
            name: base
                .and_then(|b| {
                    key_match(
                        &Key {
                            name: None,
                            columns: u.clone(),
                        },
                        &b.uniques,
                    )
                })
                .and_then(|k| k.name.clone()),
            columns: u.clone(),
        })
        .collect();
    t.foreign_keys = design
        .foreign_keys
        .iter()
        .filter(|f| !f.columns.is_empty() && !f.ref_table.is_empty())
        .map(|f| {
            let mut fk = ForeignKey {
                name: f.name.clone().filter(|n| !n.is_empty()),
                columns: f.columns.clone(),
                ref_container: None,
                ref_table: f.ref_table.clone(),
                ref_columns: f.ref_columns.clone(),
                on_delete: f.on_delete.clone().filter(|a| !a.is_empty()),
                on_update: f.on_update.clone().filter(|a| !a.is_empty()),
                ..Default::default()
            };
            if let Some(old) = base.and_then(|b| fk_match(&fk, &b.foreign_keys)) {
                fk.ref_container = old.ref_container.clone();
                fk.deferrable = old.deferrable;
                fk.initially_deferred = old.initially_deferred;
            }
            fk
        })
        .collect();
    t.indexes = design
        .indexes
        .iter()
        .filter(|i| !i.name.is_empty() && !i.columns.is_empty())
        .map(|i| {
            let old = base.and_then(|b| b.indexes.iter().find(|x| x.name == i.name));
            let mut idx = old.cloned().unwrap_or_default();
            let changed = old.is_none_or(|o| o.unique != i.unique || o.columns != i.columns);
            idx.name = i.name.clone();
            idx.columns = i.columns.clone();
            idx.unique = i.unique;
            if changed {
                idx.server_sql = None;
            }
            idx
        })
        .collect();
    t.checks = design
        .checks
        .iter()
        .filter(|c| !c.trim().is_empty())
        .map(|c| {
            base.and_then(|b| {
                check_match(
                    &Check {
                        name: None,
                        expression: c.clone(),
                    },
                    &b.checks,
                )
            })
            .cloned()
            .unwrap_or(Check {
                name: None,
                expression: c.clone(),
            })
        })
        .collect();
    t
}

/// A model table as the table editor shows it.
pub fn table_to_design(t: &Table) -> ferrodb_api::TableDesign {
    ferrodb_api::TableDesign {
        original: Some(t.name.clone()),
        name: t.name.clone(),
        comment: t.comment.clone(),
        columns: t
            .columns
            .iter()
            .map(|c| ferrodb_api::ColumnDesign {
                original: Some(c.name.clone()),
                name: c.name.clone(),
                data_type: if c.serial {
                    match c.data_type.as_str() {
                        "bigint" => "bigserial".into(),
                        "smallint" => "smallserial".into(),
                        _ => "serial".into(),
                    }
                } else {
                    c.data_type.clone()
                },
                nullable: c.nullable,
                default: if c.serial { None } else { c.default.clone() },
                comment: c.comment.clone(),
                auto_increment: c.auto_increment || c.identity.is_some() || c.serial,
            })
            .collect(),
        primary_key: t
            .primary_key
            .as_ref()
            .map(|k| k.columns.clone())
            .unwrap_or_default(),
        uniques: t.uniques.iter().map(|k| k.columns.clone()).collect(),
        foreign_keys: t
            .foreign_keys
            .iter()
            .map(|f| ferrodb_api::ForeignKeyDesign {
                name: f.name.clone(),
                columns: f.columns.clone(),
                ref_table: f.ref_table.clone(),
                ref_columns: f.ref_columns.clone(),
                on_delete: f.on_delete.clone(),
                on_update: f.on_update.clone(),
            })
            .collect(),
        indexes: t
            .indexes
            .iter()
            .map(|i| ferrodb_api::IndexDesign {
                name: i.name.clone(),
                columns: i.columns.clone(),
                unique: i.unique,
            })
            .collect(),
        checks: t.checks.iter().map(|c| c.expression.clone()).collect(),
    }
}

/// Statements of a table design in `container` of `content`.
pub fn design_script(
    d: Dialect,
    container: &str,
    content: &Content,
    design: &ferrodb_api::TableDesign,
) -> std::result::Result<DesignScript, String> {
    if design.name.trim().is_empty() {
        return Err("the table has no name".into());
    }
    let named: Vec<&ferrodb_api::ColumnDesign> = design
        .columns
        .iter()
        .filter(|c| !c.name.trim().is_empty())
        .collect();
    if named.is_empty() {
        return Err("the table has no columns".into());
    }
    for (i, c) in named.iter().enumerate() {
        if c.data_type.trim().is_empty() {
            return Err(format!("column {} has no type", c.name));
        }
        if named[..i]
            .iter()
            .any(|o| o.name.eq_ignore_ascii_case(&c.name))
        {
            return Err(format!("column {} appears twice", c.name));
        }
    }
    for k in design.primary_key.iter() {
        if !named.iter().any(|c| c.name == *k) {
            return Err(format!("primary key column {k} does not exist"));
        }
    }
    let t = Target::new(d, container);
    let base = design.original.as_deref().and_then(|o| content.table(o));
    if design.original.is_some() && base.is_none() {
        return Err(format!(
            "table {} no longer exists; refresh the explorer",
            design.original.as_deref().unwrap_or_default()
        ));
    }
    if let Some(existing) = content.table(design.name.trim())
        && base.is_none_or(|b| b.name != existing.name)
    {
        return Err(format!("a table named {} already exists", existing.name));
    }
    let new = design_to_table(d, design, base);
    let ch = match base {
        None => create_table(&t, &new),
        Some(old) => {
            let renames: Vec<(String, String)> = design
                .columns
                .iter()
                .filter_map(|c| {
                    let o = c.original.as_ref()?;
                    (o != c.name.trim()).then(|| (c.name.trim().to_owned(), o.clone()))
                })
                .collect();
            alter_table(&t, old, &new, &renames, &CompareOptions::default())
        }
    };
    let mut warnings = ch.warnings.clone();
    warnings.dedup();
    Ok(DesignScript {
        statements: ch.ordered(),
        warnings,
    })
}

// ------------------------------------------------------------ ER model

/// Tables and foreign keys of a container.
pub fn er_model(container: &str, content: &Content) -> ferrodb_api::ErModel {
    let entities = content
        .tables
        .iter()
        .filter(|t| t.partition_of.is_none())
        .map(|t| {
            let pk = t
                .primary_key
                .as_ref()
                .map(|k| k.columns.as_slice())
                .unwrap_or(&[]);
            ferrodb_api::ErEntity {
                name: t.name.clone(),
                kind: Some(table_object_kind(t)),
                comment: t.comment.clone(),
                columns: t
                    .columns
                    .iter()
                    .map(|c| ferrodb_api::ErColumn {
                        name: c.name.clone(),
                        data_type: c.data_type.clone(),
                        nullable: c.nullable,
                        primary: pk.contains(&c.name),
                        foreign: t.foreign_keys.iter().any(|f| f.columns.contains(&c.name)),
                    })
                    .collect(),
            }
        })
        .collect();
    let mut relations = Vec::new();
    for t in &content.tables {
        for f in &t.foreign_keys {
            if f.ref_container.as_deref().is_some_and(|c| c != container) {
                continue;
            }
            let optional = f
                .columns
                .iter()
                .any(|c| t.column(c).is_none_or(|c| c.nullable));
            let unique_cols = |cols: &[String]| same_columns(cols, &f.columns);
            let one_to_one = t
                .primary_key
                .as_ref()
                .is_some_and(|k| unique_cols(&k.columns))
                || t.uniques.iter().any(|k| unique_cols(&k.columns))
                || t.indexes
                    .iter()
                    .any(|i| i.unique && unique_cols(&i.columns));
            relations.push(ferrodb_api::ErRelation {
                name: f.name.clone(),
                from: t.name.clone(),
                from_columns: f.columns.clone(),
                to: f.ref_table.clone(),
                to_columns: f.ref_columns.clone(),
                optional,
                one_to_one,
            });
        }
    }
    ferrodb_api::ErModel {
        container: container.to_owned(),
        entities,
        relations,
    }
}

#[cfg(test)]
mod tests;
