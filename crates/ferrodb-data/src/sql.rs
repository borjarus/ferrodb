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
            "postgresql" | "greenplum" | "cockroachdb" | "redshift" => Family::Postgres,
            "mysql" | "mariadb" => Family::MySql,
            "sqlite" => Family::Sqlite,
            _ => Family::Other,
        };
        Self {
            dialect: Dialect::get(dialect_id),
            family,
        }
    }

    pub fn ident(&self, name: &str) -> String {
        self.dialect.quote_if_needed(name)
    }

    /// `schema.table`, each part quoted when needed.
    pub fn qualified(&self, container: Option<&str>, name: &str) -> String {
        match container {
            Some(c) if !c.is_empty() => format!("{}.{}", self.ident(c), self.ident(name)),
            _ => self.ident(name),
        }
    }

    fn string(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('\'');
        for ch in s.chars() {
            match ch {
                '\'' => out.push_str("''"),
                '\\' if self.family == Family::MySql => out.push_str("\\\\"),
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
                Family::Sqlite => if *b { "1" } else { "0" }.into(),
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
                _ => format!("X'{}'", hex(b)),
            },
            Value::Date(d) => self.string(&format_date(*d)),
            Value::Time(t) => self.string(&format_time(*t)),
            Value::Timestamp { micros, tz } => {
                let keep_tz = *tz && self.family == Family::Postgres;
                self.string(&format_timestamp(*micros, keep_tz))
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
        let mut s = format!(
            "UPDATE {table} SET {assignments} WHERE {}",
            self.conditions(key)
        );
        if one && self.family == Family::MySql {
            s.push_str(" LIMIT 1");
        }
        s
    }

    pub fn delete(&self, table: &str, key: &[(String, Value)], one: bool) -> String {
        let mut s = format!("DELETE FROM {table} WHERE {}", self.conditions(key));
        if one && self.family == Family::MySql {
            s.push_str(" LIMIT 1");
        }
        s
    }

    pub fn select_all(&self, table: &str) -> String {
        format!("SELECT * FROM {table}")
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
        format!("CREATE TABLE {table} (\n{cols}\n)")
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
}
