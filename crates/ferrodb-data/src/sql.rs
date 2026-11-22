//! SQL generation per dialect: literals, row conditions, `INSERT` /
//! `UPDATE` / `DELETE`, `CREATE TABLE` from value types, and the
//! tab-separated text format read by PostgreSQL `COPY … FROM STDIN` and
//! MySQL `LOAD DATA LOCAL INFILE` (the same format with their defaults).

use ferrodb_lang::sql::Dialect;

use crate::value::{Value, ValueType, float_text, format_date, format_time, format_timestamp, hex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Postgres,
    MySql,
    Sqlite,
    /// SQL Server / Azure SQL (T-SQL).
    SqlServer,
    /// Oracle: no boolean literals before 23ai, typed date literals, one
    /// row per `VALUES`.
    Oracle,
    /// Sybase ASE through JDBC: `bit` booleans, `0x…` binaries.
    Sybase,
    /// Apache Hive: `STRING`, `unhex(…)` binaries.
    Hive,
    /// Vertica: one row per `VALUES`.
    Vertica,
    /// ClickHouse: backticks, backslash escapes, mutations (phase 8).
    ClickHouse,
    /// Snowflake: `"UPPER"` identifiers, backslash escapes.
    Snowflake,
    /// BigQuery (GoogleSQL): backticks, backslash escapes.
    BigQuery,
    Other,
}

/// SQL writer for one dialect.
#[derive(Clone, Copy, Debug)]
pub struct Sql {
    pub dialect: &'static Dialect,
    pub family: Family,
}

/// A column of a table to create.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ValueType,
    /// Precision and scale of exact decimals, when known.
    pub decimal: Option<(u8, i8)>,
}

impl Sql {
    pub fn new(dialect_id: &str) -> Self {
        let family = match dialect_id {
            "postgresql" | "greenplum" | "cockroachdb" | "redshift" | "duckdb" => Family::Postgres,
            "mysql" | "mariadb" => Family::MySql,
            "sqlite" => Family::Sqlite,
            "tsql" | "sqlserver" | "azuresql" => Family::SqlServer,
            "oracle" => Family::Oracle,
            "sybase" => Family::Sybase,
            "hiveql" | "hive" => Family::Hive,
            "vertica" => Family::Vertica,
            "clickhouse" => Family::ClickHouse,
            "snowflake" => Family::Snowflake,
            "googlesql" | "bigquery" => Family::BigQuery,
            _ => Family::Other,
        };
        Self {
            dialect: Dialect::get(dialect_id),
            family,
        }
    }

    pub fn ident(&self, name: &str) -> String {
        let plain = |upper: bool| {
            name.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| {
                    c == '_'
                        || c.is_ascii_digit()
                        || (c.is_ascii_alphabetic() && (!upper || c.is_ascii_uppercase()))
                })
                && !self.dialect.is_keyword(name)
        };
        match self.family {
            Family::ClickHouse | Family::BigQuery if !plain(false) => {
                format!("`{}`", name.replace('\\', "\\\\").replace('`', "\\`"))
            }
            // Unquoted Snowflake identifiers fold to upper case.
            Family::Snowflake if !plain(true) => format!("\"{}\"", name.replace('"', "\"\"")),
            Family::ClickHouse | Family::BigQuery | Family::Snowflake => name.to_owned(),
            _ => self.dialect.quote_if_needed(name),
        }
    }

    /// `schema.table`, each part quoted when needed.
    pub fn qualified(&self, container: Option<&str>, name: &str) -> String {
        match container {
            Some(c) if !c.is_empty() => format!("{}.{}", self.ident(c), self.ident(name)),
            _ => self.ident(name),
        }
    }

    fn string(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 3);
        // T-SQL: Unicode literals keep non-Latin text intact.
        if self.family == Family::SqlServer {
            out.push('N');
        }
        out.push('\'');
        for ch in s.chars() {
            match ch {
                // `''` is no escape in GoogleSQL.
                '\'' if matches!(self.family, Family::ClickHouse | Family::BigQuery) => {
                    out.push_str("\\'")
                }
                '\'' => out.push_str("''"),
                '\\' if matches!(
                    self.family,
                    Family::MySql | Family::ClickHouse | Family::BigQuery | Family::Snowflake
                ) =>
                {
                    out.push_str("\\\\")
                }
                c => out.push(c),
            }
        }
        out.push('\'');
        out
    }

    pub fn literal(&self, v: &Value) -> String {
        match v {
            Value::Null => "NULL".into(),
            Value::Bool(b) => match self.family {
                Family::Sqlite | Family::SqlServer | Family::Oracle | Family::Sybase => {
                    if *b { "1" } else { "0" }.into()
                }
                _ => if *b { "TRUE" } else { "FALSE" }.into(),
            },
            Value::Int(i) => i.to_string(),
            Value::UInt(u) => u.to_string(),
            Value::Float(f) if f.is_finite() => float_text(*f),
            Value::Float(f) => match self.family {
                Family::Postgres => format!("'{f}'::float8").replace("inf", "Infinity"),
                _ => "NULL".into(),
            },
            Value::Decimal(d)
                if !d.is_empty()
                    && d.trim_start_matches(['-', '+'])
                        .bytes()
                        .all(|b| b.is_ascii_digit() || b == b'.') =>
            {
                d.clone()
            }
            Value::Bytes(b) => match self.family {
                Family::Postgres => format!("'\\x{}'::bytea", hex(b)),
                Family::Oracle => format!("HEXTORAW('{}')", hex(b)),
                Family::SqlServer | Family::Sybase => format!("0x{}", hex(b)),
                Family::Hive => format!("unhex('{}')", hex(b)),
                Family::ClickHouse => format!("unhex('{}')", hex(b)),
                Family::BigQuery => format!("FROM_HEX('{}')", hex(b)),
                _ => format!("X'{}'", hex(b)),
            },
            // Oracle reads plain strings with the session's NLS format:
            // typed literals are unambiguous.
            Value::Date(d) if matches!(self.family, Family::Oracle | Family::Hive) => {
                format!("DATE {}", self.string(&format_date(*d)))
            }
            Value::Timestamp { micros, .. }
                if matches!(self.family, Family::Oracle | Family::Hive) =>
            {
                format!(
                    "TIMESTAMP {}",
                    self.string(&format_timestamp(*micros, false))
                )
            }
            // Dates and times are ASCII: no `N` prefix, no escapes.
            Value::Date(d) => format!("'{}'", format_date(*d)),
            Value::Time(t) => format!("'{}'", format_time(*t)),
            Value::Timestamp { micros, tz } => {
                let keep_tz = *tz && self.family == Family::Postgres;
                format!("'{}'", format_timestamp(*micros, keep_tz))
            }
            other => self.string(&other.to_text().unwrap_or_default()),
        }
    }

    /// `col = literal` or `col IS NULL`.
    pub fn condition(&self, column: &str, v: &Value) -> String {
        match v {
            Value::Null => format!("{} IS NULL", self.ident(column)),
            v => format!("{} = {}", self.ident(column), self.literal(v)),
        }
    }

    fn conditions(&self, key: &[(String, Value)]) -> String {
        key.iter()
            .map(|(c, v)| self.condition(c, v))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// `INSERT INTO t (a, b) VALUES (…), (…)`.
    pub fn insert(&self, table: &str, columns: &[String], rows: &[Vec<Value>]) -> String {
        let mut s = String::with_capacity(64 + rows.len() * columns.len() * 8);
        s.push_str("INSERT INTO ");
        s.push_str(table);
        s.push_str(" (");
        s.push_str(
            &columns
                .iter()
                .map(|c| self.ident(c))
                .collect::<Vec<_>>()
                .join(", "),
        );
        // Oracle (before 23ai), Sybase and Vertica take one row per
        // `VALUES`: several rows become `SELECT … UNION ALL SELECT …`.
        if rows.len() > 1
            && matches!(
                self.family,
                Family::Oracle | Family::Sybase | Family::Vertica
            )
        {
            s.push_str(") ");
            let from = if self.family == Family::Oracle {
                " FROM DUAL"
            } else {
                ""
            };
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    s.push_str(" UNION ALL ");
                }
                s.push_str("SELECT ");
                s.push_str(
                    &row.iter()
                        .map(|v| self.literal(v))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                s.push_str(from);
            }
            return s;
        }
        s.push_str(") VALUES ");
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push('(');
            for (j, v) in row.iter().enumerate() {
                if j > 0 {
                    s.push_str(", ");
                }
                s.push_str(&self.literal(v));
            }
            s.push(')');
        }
        s
    }

    /// `UPDATE t SET a = … WHERE k = …` (MySQL: `LIMIT 1` when `one`).
    pub fn update(
        &self,
        table: &str,
        set: &[(String, Value)],
        key: &[(String, Value)],
        one: bool,
    ) -> String {
        let assignments = set
            .iter()
            .map(|(c, v)| format!("{} = {}", self.ident(c), self.literal(v)))
            .collect::<Vec<_>>()
            .join(", ");
        // ClickHouse: a mutation (lightweight `UPDATE` needs a table
        // setting), synchronous so the refreshed result shows it.
        if self.family == Family::ClickHouse {
            return format!(
                "ALTER TABLE {table} UPDATE {assignments} WHERE {} SETTINGS mutations_sync = 1",
                self.conditions(key)
            );
        }
        // T-SQL limits with `TOP (1)` after the verb.
        let top = if one && self.family == Family::SqlServer {
            "TOP (1) "
        } else {
            ""
        };
        let mut s = format!(
            "UPDATE {top}{table} SET {assignments} WHERE {}",
            self.conditions(key)
        );
        if one && self.family == Family::MySql {
            s.push_str(" LIMIT 1");
        }
        s
    }

    pub fn delete(&self, table: &str, key: &[(String, Value)], one: bool) -> String {
        let top = if one && self.family == Family::SqlServer {
            "TOP (1) "
        } else {
            ""
        };
        let mut s = format!("DELETE {top}FROM {table} WHERE {}", self.conditions(key));
        if one && self.family == Family::MySql {
            s.push_str(" LIMIT 1");
        }
        s
    }

    /// A single-row `INSERT` of an edited result. SQL Server refuses
    /// explicit values for an `IDENTITY` column unless `IDENTITY_INSERT`
    /// is on: the statement turns it on (and off again) when one of
    /// `columns` is the table's identity column.
    pub fn insert_row(&self, table: &str, columns: &[String], values: Vec<Value>) -> String {
        let insert = self.insert(table, columns, &[values]);
        if self.family != Family::SqlServer || columns.is_empty() {
            return insert;
        }
        let names = columns
            .iter()
            .map(|c| self.string(c))
            .collect::<Vec<_>>()
            .join(", ");
        let obj = self.string(table);
        format!(
            "DECLARE @ii bit = CASE WHEN EXISTS (SELECT 1 FROM sys.identity_columns \
             WHERE object_id = OBJECT_ID({obj}) AND name IN ({names})) THEN 1 ELSE 0 END; \
             IF @ii = 1 SET IDENTITY_INSERT {table} ON; \
             {insert}; \
             IF @ii = 1 SET IDENTITY_INSERT {table} OFF"
        )
    }

    /// `INSERT … OUTPUT INSERTED.*` / `RETURNING *`: the inserted row as
    /// the database completed it (defaults, identity). `None` where the
    /// database cannot return it (Redshift, MySQL, SQL Server tables with
    /// triggers are not detected here).
    pub fn insert_returning(
        &self,
        table: &str,
        columns: &[String],
        values: Vec<Value>,
        dialect: &str,
    ) -> Option<String> {
        let insert = self.insert(table, columns, &[values]);
        match self.family {
            Family::SqlServer => {
                let at = insert.find(") VALUES ")? + 1;
                Some(format!(
                    "{} OUTPUT INSERTED.*{}",
                    &insert[..at],
                    &insert[at..]
                ))
            }
            Family::Postgres if dialect != "redshift" => Some(format!("{insert} RETURNING *")),
            Family::Sqlite => Some(format!("{insert} RETURNING *")),
            _ => None,
        }
    }

    pub fn select_all(&self, table: &str) -> String {
        format!("SELECT * FROM {table}")
    }

    /// `SELECT TOP (n) * FROM t` / `SELECT * FROM t LIMIT n`.
    pub fn select_top(&self, table: &str, n: u64) -> String {
        match self.family {
            Family::SqlServer => format!("SELECT TOP ({n}) * FROM {table}"),
            _ => format!("SELECT * FROM {table} LIMIT {n}"),
        }
    }

    pub fn truncate(&self, table: &str) -> String {
        match self.family {
            Family::Sqlite => format!("DELETE FROM {table}"),
            _ => format!("TRUNCATE TABLE {table}"),
        }
    }

    /// Column type for `CREATE TABLE`.
    pub fn type_name(&self, c: &ColumnDef) -> String {
        use ValueType as T;
        let dec = |dflt: &str| match c.decimal {
            Some((p, s)) if s >= 0 => format!("({p},{s})"),
            _ => dflt.to_owned(),
        };
        match self.family {
            Family::Postgres => match c.ty {
                T::Bool => "boolean".into(),
                T::Int => "bigint".into(),
                T::Float => "double precision".into(),
                T::Decimal => format!("numeric{}", dec("")),
                T::Text | T::Any => "text".into(),
                T::Bytes => "bytea".into(),
                T::Date => "date".into(),
                T::Time => "time".into(),
                T::Timestamp { tz: false } => "timestamp".into(),
                T::Timestamp { tz: true } => "timestamptz".into(),
                T::Json => "jsonb".into(),
            },
            Family::MySql => match c.ty {
                T::Bool => "boolean".into(),
                T::Int => "bigint".into(),
                T::Float => "double".into(),
                T::Decimal => format!("decimal{}", dec("(38,10)")),
                T::Text | T::Any => "longtext".into(),
                T::Bytes => "longblob".into(),
                T::Date => "date".into(),
                T::Time => "time(6)".into(),
                T::Timestamp { .. } => "datetime(6)".into(),
                T::Json => "json".into(),
            },
            Family::SqlServer => match c.ty {
                T::Bool => "bit".into(),
                T::Int => "bigint".into(),
                T::Float => "float".into(),
                T::Decimal => format!("decimal{}", dec("(38,10)")),
                T::Text | T::Any | T::Json => "nvarchar(max)".into(),
                T::Bytes => "varbinary(max)".into(),
                T::Date => "date".into(),
                T::Time => "time(6)".into(),
                T::Timestamp { tz: false } => "datetime2(6)".into(),
                T::Timestamp { tz: true } => "datetimeoffset(6)".into(),
            },
            Family::Sqlite => match c.ty {
                T::Bool => "BOOLEAN".into(),
                T::Int => "INTEGER".into(),
                T::Float => "REAL".into(),
                T::Decimal => format!("NUMERIC{}", dec("")),
                T::Text | T::Any => "TEXT".into(),
                T::Bytes => "BLOB".into(),
                T::Date => "DATE".into(),
                T::Time => "TIME".into(),
                T::Timestamp { .. } => "TIMESTAMP".into(),
                T::Json => "JSON".into(),
            },
            Family::Oracle => match c.ty {
                T::Bool => "NUMBER(1)".into(),
                T::Int => "NUMBER(19)".into(),
                T::Float => "BINARY_DOUBLE".into(),
                T::Decimal => format!("NUMBER{}", dec("")),
                T::Text | T::Any | T::Json => "VARCHAR2(4000)".into(),
                T::Bytes => "BLOB".into(),
                T::Date => "DATE".into(),
                T::Time | T::Timestamp { tz: false } => "TIMESTAMP".into(),
                T::Timestamp { tz: true } => "TIMESTAMP WITH TIME ZONE".into(),
            },
            Family::Sybase => match c.ty {
                T::Bool => "bit".into(),
                T::Int => "bigint".into(),
                T::Float => "float".into(),
                T::Decimal => format!("numeric{}", dec("(38,10)")),
                T::Text | T::Any | T::Json => "text".into(),
                T::Bytes => "image".into(),
                T::Date => "date".into(),
                T::Time => "time".into(),
                T::Timestamp { .. } => "datetime".into(),
            },
            Family::Hive => match c.ty {
                T::Bool => "BOOLEAN".into(),
                T::Int => "BIGINT".into(),
                T::Float => "DOUBLE".into(),
                T::Decimal => format!("DECIMAL{}", dec("(38,10)")),
                T::Text | T::Any | T::Json | T::Time => "STRING".into(),
                T::Bytes => "BINARY".into(),
                T::Date => "DATE".into(),
                T::Timestamp { .. } => "TIMESTAMP".into(),
            },
            Family::Vertica => match c.ty {
                T::Bool => "BOOLEAN".into(),
                T::Int => "INTEGER".into(),
                T::Float => "FLOAT".into(),
                T::Decimal => format!("NUMERIC{}", dec("(37,10)")),
                T::Text | T::Any | T::Json => "LONG VARCHAR".into(),
                T::Bytes => "LONG VARBINARY".into(),
                T::Date => "DATE".into(),
                T::Time => "TIME".into(),
                T::Timestamp { tz: false } => "TIMESTAMP".into(),
                T::Timestamp { tz: true } => "TIMESTAMPTZ".into(),
            },
            // Imported values may be missing: every column is Nullable.
            Family::ClickHouse => format!(
                "Nullable({})",
                match c.ty {
                    T::Bool => "Bool".into(),
                    T::Int => "Int64".into(),
                    T::Float => "Float64".into(),
                    T::Decimal => format!("Decimal{}", dec("(38,10)")),
                    T::Text | T::Any | T::Json | T::Bytes => "String".into(),
                    T::Date => "Date32".into(),
                    T::Time => "String".into(),
                    T::Timestamp { tz: false } => "DateTime64(6)".into(),
                    T::Timestamp { tz: true } => "DateTime64(6, 'UTC')".into(),
                }
            ),
            Family::Snowflake => match c.ty {
                T::Bool => "BOOLEAN".into(),
                T::Int => "NUMBER(38,0)".into(),
                T::Float => "FLOAT".into(),
                T::Decimal => format!("NUMBER{}", dec("(38,10)")),
                T::Text | T::Any => "VARCHAR".into(),
                T::Bytes => "BINARY".into(),
                T::Date => "DATE".into(),
                T::Time => "TIME".into(),
                T::Timestamp { tz: false } => "TIMESTAMP_NTZ".into(),
                T::Timestamp { tz: true } => "TIMESTAMP_TZ".into(),
                T::Json => "VARIANT".into(),
            },
            Family::BigQuery => match c.ty {
                T::Bool => "BOOL".into(),
                T::Int => "INT64".into(),
                T::Float => "FLOAT64".into(),
                T::Decimal => match c.decimal {
                    Some((p, s)) if s >= 0 && p <= 38 && s <= 9 && p - s as u8 <= 29 => {
                        format!("NUMERIC({p},{s})")
                    }
                    _ => "BIGNUMERIC".into(),
                },
                T::Text | T::Any => "STRING".into(),
                T::Bytes => "BYTES".into(),
                T::Date => "DATE".into(),
                T::Time => "TIME".into(),
                T::Timestamp { tz: false } => "DATETIME".into(),
                T::Timestamp { tz: true } => "TIMESTAMP".into(),
                T::Json => "JSON".into(),
            },
            Family::Other => match c.ty {
                T::Bool => "BOOLEAN".into(),
                T::Int => "BIGINT".into(),
                T::Float => "DOUBLE PRECISION".into(),
                T::Decimal => format!("DECIMAL{}", dec("(38,10)")),
                T::Text | T::Any | T::Json => "VARCHAR(4000)".into(),
                T::Bytes => "BLOB".into(),
                T::Date => "DATE".into(),
                T::Time => "TIME".into(),
                T::Timestamp { .. } => "TIMESTAMP".into(),
            },
        }
    }

    pub fn create_table(&self, table: &str, columns: &[ColumnDef]) -> String {
        let cols = columns
            .iter()
            .map(|c| format!("  {} {}", self.ident(&c.name), self.type_name(c)))
            .collect::<Vec<_>>()
            .join(",\n");
        match self.family {
            // A table engine is required; no sorting key is known.
            Family::ClickHouse => {
                format!("CREATE TABLE {table} (\n{cols}\n) ENGINE = MergeTree ORDER BY tuple()")
            }
            _ => format!("CREATE TABLE {table} (\n{cols}\n)"),
        }
    }

    /// Appends one row in the `COPY` / `LOAD DATA` text format.
    pub fn copy_row(&self, values: &[Value], out: &mut Vec<u8>) {
        for (i, v) in values.iter().enumerate() {
            if i > 0 {
                out.push(b'\t');
            }
            match v {
                Value::Null => out.extend_from_slice(b"\\N"),
                Value::Bool(b) => out.extend_from_slice(match (self.family, b) {
                    (Family::Postgres, true) => b"t",
                    (Family::Postgres, false) => b"f",
                    (_, true) => b"1",
                    (_, false) => b"0",
                }),
                Value::Bytes(b) if self.family == Family::Postgres => {
                    out.extend_from_slice(b"\\\\x");
                    out.extend_from_slice(hex(b).as_bytes());
                }
                Value::Bytes(b) => escape_copy(b, out),
                Value::Timestamp { micros, tz } => {
                    let keep_tz = *tz && self.family == Family::Postgres;
                    out.extend_from_slice(format_timestamp(*micros, keep_tz).as_bytes());
                }
                Value::Float(f) if !f.is_finite() => match self.family {
                    Family::Postgres => out.extend_from_slice(
                        if f.is_nan() {
                            "NaN".to_owned()
                        } else if *f > 0.0 {
                            "Infinity".to_owned()
                        } else {
                            "-Infinity".to_owned()
                        }
                        .as_bytes(),
                    ),
                    _ => out.extend_from_slice(b"\\N"),
                },
                other => escape_copy(other.to_text().unwrap_or_default().as_bytes(), out),
            }
        }
        out.push(b'\n');
    }
}

fn escape_copy(bytes: &[u8], out: &mut Vec<u8>) {
    for &b in bytes {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0 => out.extend_from_slice(b"\\0"),
            b => out.push(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_per_dialect() {
        let pg = Sql::new("postgresql");
        let my = Sql::new("mysql");
        let lite = Sql::new("sqlite");
        assert_eq!(pg.literal(&Value::Text("it's".into())), "'it''s'");
        assert_eq!(my.literal(&Value::Text("a\\b".into())), "'a\\\\b'");
        assert_eq!(pg.literal(&Value::Text("a\\b".into())), "'a\\b'");
        assert_eq!(lite.literal(&Value::Bool(true)), "1");
        assert_eq!(pg.literal(&Value::Bytes(vec![1, 255])), "'\\x01ff'::bytea");
        assert_eq!(my.literal(&Value::Bytes(vec![1, 255])), "X'01ff'");
        assert_eq!(pg.literal(&Value::Decimal("-1.50".into())), "-1.50");
        assert_eq!(
            pg.literal(&Value::Timestamp {
                micros: 0,
                tz: true
            }),
            "'1970-01-01 00:00:00+00:00'"
        );
        assert_eq!(
            my.literal(&Value::Timestamp {
                micros: 0,
                tz: true
            }),
            "'1970-01-01 00:00:00'"
        );
    }

    #[test]
    fn statements() {
        let pg = Sql::new("postgresql");
        let t = pg.qualified(Some("public"), "Order");
        assert_eq!(t, "public.\"Order\"");
        assert_eq!(
            pg.update(
                &t,
                &[("status".into(), Value::Text("paid".into()))],
                &[("id".into(), Value::Int(7)), ("note".into(), Value::Null)],
                true
            ),
            "UPDATE public.\"Order\" SET status = 'paid' WHERE id = 7 AND note IS NULL"
        );
        let my = Sql::new("mysql");
        assert_eq!(
            my.delete("t", &[("id".into(), Value::Int(1))], true),
            "DELETE FROM t WHERE id = 1 LIMIT 1"
        );
        assert_eq!(
            pg.insert(
                "t",
                &["a".into(), "user".into()],
                &[
                    vec![Value::Int(1), Value::Null],
                    vec![Value::Int(2), Value::Text("x".into())]
                ]
            ),
            "INSERT INTO t (a, \"user\") VALUES (1, NULL), (2, 'x')"
        );
        let ddl = pg.create_table(
            "t",
            &[
                ColumnDef {
                    name: "id".into(),
                    ty: ValueType::Int,
                    decimal: None,
                },
                ColumnDef {
                    name: "amount".into(),
                    ty: ValueType::Decimal,
                    decimal: Some((10, 2)),
                },
            ],
        );
        assert_eq!(
            ddl,
            "CREATE TABLE t (\n  id bigint,\n  amount numeric(10,2)\n)"
        );
    }

    #[test]
    fn sql_server_statements() {
        let ms = Sql::new("tsql");
        assert_eq!(ms.family, Family::SqlServer);
        assert_eq!(
            ms.literal(&Value::Text("zażółć it's".into())),
            "N'zażółć it''s'"
        );
        assert_eq!(ms.literal(&Value::Bool(true)), "1");
        assert_eq!(ms.literal(&Value::Bytes(vec![1, 255])), "0x01ff");
        assert_eq!(ms.literal(&Value::Date(0)), "'1970-01-01'");
        let t = ms.qualified(Some("dbo"), "order");
        assert_eq!(t, "dbo.[order]");
        assert_eq!(
            ms.update(
                &t,
                &[("v".into(), Value::Int(1))],
                &[("id".into(), Value::Int(7))],
                true
            ),
            "UPDATE TOP (1) dbo.[order] SET v = 1 WHERE id = 7"
        );
        assert_eq!(
            ms.delete(&t, &[("id".into(), Value::Int(7))], true),
            "DELETE TOP (1) FROM dbo.[order] WHERE id = 7"
        );
        let ins = ms.insert_row(&t, &["id".into()], vec![Value::Int(5)]);
        assert!(ins.contains("SET IDENTITY_INSERT dbo.[order] ON"), "{ins}");
        assert!(ins.contains("name IN (N'id')"), "{ins}");
        assert!(
            ins.contains("INSERT INTO dbo.[order] (id) VALUES (5);"),
            "{ins}"
        );
        assert_eq!(
            ms.insert_returning(&t, &["v".into()], vec![Value::Int(1)], "tsql")
                .as_deref(),
            Some("INSERT INTO dbo.[order] (v) OUTPUT INSERTED.* VALUES (1)")
        );
        assert_eq!(
            Sql::new("redshift").insert_returning(
                "t",
                &["a".into()],
                vec![Value::Int(1)],
                "redshift"
            ),
            None
        );
        assert_eq!(ms.select_top("t", 10), "SELECT TOP (10) * FROM t");
        let ddl = ms.create_table(
            "t",
            &[ColumnDef {
                name: "s".into(),
                ty: ValueType::Text,
                decimal: None,
            }],
        );
        assert_eq!(ddl, "CREATE TABLE t (\n  s nvarchar(max)\n)");
    }

    #[test]
    fn jdbc_dialects() {
        let ora = Sql::new("oracle");
        assert_eq!(ora.literal(&Value::Bool(true)), "1");
        assert_eq!(ora.literal(&Value::Bytes(vec![1, 255])), "HEXTORAW('01ff')");
        assert_eq!(ora.literal(&Value::Date(0)), "DATE '1970-01-01'");
        assert_eq!(
            ora.literal(&Value::Timestamp {
                micros: 0,
                tz: false
            }),
            "TIMESTAMP '1970-01-01 00:00:00'"
        );
        let rows = [vec![Value::Int(1)], vec![Value::Int(2)]];
        assert_eq!(
            ora.insert("T", &["A".into()], &rows),
            "INSERT INTO T (A) SELECT 1 FROM DUAL UNION ALL SELECT 2 FROM DUAL"
        );
        assert_eq!(
            Sql::new("vertica").insert("t", &["a".into()], &rows),
            "INSERT INTO t (a) SELECT 1 UNION ALL SELECT 2"
        );
        assert_eq!(
            ora.insert("T", &["A".into()], &rows[..1]),
            "INSERT INTO T (A) VALUES (1)"
        );
        // No LIMIT outside MySQL.
        assert_eq!(
            ora.update(
                "T",
                &[("A".into(), Value::Int(2))],
                &[("ID".into(), Value::Int(1))],
                true
            ),
            "UPDATE T SET A = 2 WHERE ID = 1"
        );
        assert_eq!(
            Sql::new("h2").delete("T", &[("ID".into(), Value::Int(1))], true),
            "DELETE FROM T WHERE ID = 1"
        );
        let syb = Sql::new("sybase");
        assert_eq!(syb.literal(&Value::Bytes(vec![10])), "0x0a");
        assert_eq!(syb.literal(&Value::Bool(false)), "0");
        let hive = Sql::new("hiveql");
        assert_eq!(hive.literal(&Value::Bytes(vec![10])), "unhex('0a')");
        let col = ColumnDef {
            name: "x".into(),
            ty: ValueType::Text,
            decimal: None,
        };
        assert_eq!(hive.type_name(&col), "STRING");
        assert_eq!(ora.type_name(&col), "VARCHAR2(4000)");
        assert_eq!(Sql::new("derby").literal(&Value::Bool(true)), "TRUE");
    }

    #[test]
    fn copy_text() {
        let pg = Sql::new("postgresql");
        let mut out = Vec::new();
        pg.copy_row(
            &[
                Value::Int(1),
                Value::Null,
                Value::Text("a\tb\\c\nd".into()),
                Value::Bool(true),
                Value::Bytes(vec![10]),
            ],
            &mut out,
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "1\t\\N\ta\\tb\\\\c\\nd\tt\t\\\\x0a\n"
        );
        let my = Sql::new("mariadb");
        let mut out = Vec::new();
        my.copy_row(
            &[Value::Bool(false), Value::Bytes(vec![0, b'\t'])],
            &mut out,
        );
        assert_eq!(out, b"0\t\\0\\t\n");
    }

    #[test]
    fn analytical_cloud_dialects() {
        let ch = Sql::new("clickhouse");
        let t = ch.qualified(Some("tck"), "my table");
        assert_eq!(t, "tck.`my table`");
        assert_eq!(
            ch.literal(&Value::Text("it's a\\b".into())),
            "'it\\'s a\\\\b'"
        );
        assert_eq!(ch.literal(&Value::Bytes(vec![1, 255])), "unhex('01ff')");
        assert_eq!(
            ch.update(
                &t,
                &[("v".into(), Value::Text("x".into()))],
                &[("id".into(), Value::Int(7))],
                true
            ),
            "ALTER TABLE tck.`my table` UPDATE v = 'x' WHERE id = 7 SETTINGS mutations_sync = 1"
        );
        assert_eq!(
            ch.delete(&t, &[("id".into(), Value::Int(7))], true),
            "DELETE FROM tck.`my table` WHERE id = 7"
        );
        let ddl = ch.create_table(
            "t",
            &[ColumnDef {
                name: "id".into(),
                ty: ValueType::Int,
                decimal: None,
            }],
        );
        assert!(
            ddl.ends_with("ENGINE = MergeTree ORDER BY tuple()"),
            "{ddl}"
        );
        assert!(ddl.contains("id Nullable(Int64)"), "{ddl}");

        let bq = Sql::new("googlesql");
        assert_eq!(
            bq.qualified(Some("my-project.ds"), "t"),
            "`my-project.ds`.t"
        );
        assert_eq!(bq.literal(&Value::Text("it's".into())), "'it\\'s'");
        assert_eq!(bq.literal(&Value::Bytes(vec![10])), "FROM_HEX('0a')");
        assert_eq!(
            bq.update(
                "ds.t",
                &[("a".into(), Value::Int(1))],
                &[("k".into(), Value::Int(2))],
                true
            ),
            "UPDATE ds.t SET a = 1 WHERE k = 2"
        );

        let sf = Sql::new("snowflake");
        assert_eq!(sf.ident("ORDERS"), "ORDERS");
        assert_eq!(sf.ident("orders"), "\"orders\"");
        assert_eq!(
            sf.literal(&Value::Text("it's a\\b".into())),
            "'it''s a\\\\b'"
        );
        assert_eq!(
            sf.type_name(&ColumnDef {
                name: "x".into(),
                ty: ValueType::Timestamp { tz: true },
                decimal: None
            }),
            "TIMESTAMP_TZ"
        );
    }
}
