//! MySQL column metadata → Arrow, and server error decoding.
//!
//! The text protocol (`COM_QUERY`) sends every value as a string (or raw
//! bytes for binary columns); the column definition (type, flags, character
//! set, length, decimals) decides the Arrow type. Text values are pushed as
//! [`Cell::Text`] and parsed by [`BatchBuilder`](crate::columns::BatchBuilder)
//! (exact decimals, dates, timestamps); values it cannot represent — zero
//! dates such as `0000-00-00` — become NULL and are counted as lossy.
//!
//! Decisions:
//! - `TINYINT(1)` is not treated as boolean: MySQL has no real boolean type
//!   and `TINYINT(1)` is only a convention, so it stays `Int64`.
//! - `TIME` stays text: MySQL `TIME` is an interval (`-838:59:59` to
//!   `838:59:59`), not a time of day.
//! - `BIT(n)` becomes an integer (`UInt64` for `BIT(64)`), as the bits are
//!   sent big-endian.
//! - Strings in the binary character set (63) are bytes (`BINARY`,
//!   `VARBINARY`, `BLOB`); `JSON` is reported with that character set too but
//!   is always text.

use std::ops::Range;

use mysql_async::consts::{ColumnFlags, ColumnType as MyType};

use crate::columns::{Cell, ColumnSpec, ColumnType};

/// Collation id of the `binary` character set.
pub const BINARY_CHARSET: u16 = 63;

/// Error code: query execution was interrupted (`KILL QUERY`).
pub const ER_QUERY_INTERRUPTED: u16 = 1317;
/// Error code: SQL syntax error.
pub const ER_PARSE_ERROR: u16 = 1064;

/// How a wire value is turned into a [`Cell`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conv {
    /// UTF-8 text, parsed by the batch builder for the column type.
    Text,
    /// Raw bytes.
    Bytes,
    /// Big-endian bit string → integer.
    Bit,
}

/// The parts of a column definition that decide its mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnMeta {
    pub ty: MyType,
    pub flags: ColumnFlags,
    pub charset: u16,
    pub length: u32,
    pub decimals: u8,
}

impl ColumnMeta {
    pub fn of(c: &mysql_async::Column) -> Self {
        Self {
            ty: c.column_type(),
            flags: c.flags(),
            charset: c.character_set(),
            length: c.column_length(),
            decimals: c.decimals(),
        }
    }
}

/// Arrow type, database type name and value conversion for a column.
pub fn map_column(m: ColumnMeta) -> (ColumnType, String, Conv) {
    use MyType::*;
    let unsigned = m.flags.contains(ColumnFlags::UNSIGNED_FLAG);
    let binary = m.charset == BINARY_CHARSET;
    let int = |name: &str| {
        let ty = if unsigned && m.ty == MYSQL_TYPE_LONGLONG {
            ColumnType::UInt64
        } else {
            ColumnType::Int64
        };
        let name = if unsigned {
            format!("{name} unsigned")
        } else {
            name.to_owned()
        };
        (ty, name, Conv::Text)
    };
    let text = |name: &str| (ColumnType::Text, name.to_owned(), Conv::Text);
    let bytes = |name: &str| (ColumnType::Binary, name.to_owned(), Conv::Bytes);
    match m.ty {
        MYSQL_TYPE_TINY => int("tinyint"),
        MYSQL_TYPE_SHORT => int("smallint"),
        MYSQL_TYPE_INT24 => int("mediumint"),
        MYSQL_TYPE_LONG => int("int"),
        MYSQL_TYPE_LONGLONG => int("bigint"),
        MYSQL_TYPE_YEAR => (ColumnType::Int64, "year".into(), Conv::Text),
        MYSQL_TYPE_FLOAT => (ColumnType::Float32, "float".into(), Conv::Text),
        MYSQL_TYPE_DOUBLE => (ColumnType::Float64, "double".into(), Conv::Text),
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => {
            let (p, s) = decimal_precision(m.length, m.decimals, unsigned);
            let name = format!("decimal({p},{s})");
            let ty = if p <= 38 {
                ColumnType::Decimal {
                    precision: p as u8,
                    scale: s as i8,
                }
            } else {
                ColumnType::Text
            };
            (ty, name, Conv::Text)
        }
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => (ColumnType::Date, "date".into(), Conv::Text),
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => text("time"),
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => (
            ColumnType::Timestamp { tz: false },
            "datetime".into(),
            Conv::Text,
        ),
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => (
            ColumnType::Timestamp { tz: false },
            "timestamp".into(),
            Conv::Text,
        ),
        MYSQL_TYPE_JSON => text("json"),
        MYSQL_TYPE_ENUM => text("enum"),
        MYSQL_TYPE_SET => text("set"),
        MYSQL_TYPE_STRING if m.flags.contains(ColumnFlags::ENUM_FLAG) => text("enum"),
        MYSQL_TYPE_STRING if m.flags.contains(ColumnFlags::SET_FLAG) => text("set"),
        MYSQL_TYPE_STRING if binary => bytes("binary"),
        MYSQL_TYPE_STRING => text("char"),
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING if binary => bytes("varbinary"),
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => text("varchar"),
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            // The text protocol reports every BLOB/TEXT as MYSQL_TYPE_BLOB;
            // the size class comes from the length (bytes, so ×4 for
            // utf8mb4 text).
            let class = |unit: u64| {
                let len = u64::from(m.length);
                if len <= 255 * unit {
                    "tiny"
                } else if len <= 65_535 * unit {
                    ""
                } else if len <= 16_777_215 * unit {
                    "medium"
                } else {
                    "long"
                }
            };
            if binary {
                bytes(&format!("{}blob", class(1)))
            } else {
                text(&format!("{}text", class(4)))
            }
        }
        MYSQL_TYPE_BIT => {
            let ty = if m.length >= 64 {
                ColumnType::UInt64
            } else {
                ColumnType::Int64
            };
            (ty, format!("bit({})", m.length), Conv::Bit)
        }
        MYSQL_TYPE_GEOMETRY => bytes("geometry"),
        MYSQL_TYPE_VECTOR => bytes("vector"),
        MYSQL_TYPE_NULL => text("null"),
        _ if binary => bytes("unknown"),
        _ => text("unknown"),
    }
}

/// `(precision, scale)` of a DECIMAL column from its display length, which
/// counts the sign (unless unsigned) and the decimal point (if scale > 0).
pub fn decimal_precision(length: u32, decimals: u8, unsigned: bool) -> (u32, u32) {
    let scale = u32::from(decimals);
    let p = length
        .saturating_sub(u32::from(scale > 0))
        .saturating_sub(u32::from(!unsigned));
    (p.max(scale).max(1), scale)
}

pub fn column_spec(c: &mysql_async::Column) -> (ColumnSpec, Conv) {
    let (ty, db_type, conv) = map_column(ColumnMeta::of(c));
    (ColumnSpec::new(c.name_str(), ty, db_type), conv)
}

/// Big-endian bit string → integer.
pub fn bits(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |acc, x| (acc << 8) | u64::from(*x))
}

/// Wire value → cell. `Value::Bytes` is the only non-NULL value of the text
/// protocol; others (binary protocol) are not expected but handled.
pub fn cell(v: Option<&mysql_async::Value>, conv: Conv) -> Cell<'_> {
    use mysql_async::Value as V;
    match v {
        None | Some(V::NULL) => Cell::Null,
        Some(V::Bytes(b)) => match conv {
            Conv::Bytes => Cell::Bytes(b),
            Conv::Bit => Cell::UInt(bits(b)),
            Conv::Text => match std::str::from_utf8(b) {
                Ok(s) => Cell::Text(s),
                Err(_) => Cell::Bytes(b),
            },
        },
        Some(V::Int(i)) => Cell::Int(*i),
        Some(V::UInt(u)) => Cell::UInt(*u),
        Some(V::Float(f)) => Cell::Float(f64::from(*f)),
        Some(V::Double(f)) => Cell::Float(*f),
        // Date/Time values only come from the binary protocol.
        Some(_) => Cell::Null,
    }
}

/// First keyword of `text`, lowercase, skipping whitespace and comments.
pub fn first_keyword(text: &str) -> String {
    let mut s = text;
    loop {
        s = s.trim_start();
        if let Some(r) = s.strip_prefix("--").or_else(|| s.strip_prefix('#')) {
            s = r.split_once('\n').map(|(_, r)| r).unwrap_or("");
        } else if let Some(r) = s.strip_prefix("/*") {
            s = r.split_once("*/").map(|(_, r)| r).unwrap_or("");
        } else {
            break;
        }
    }
    s.chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Statements whose affected-row count is reported.
pub fn is_dml(text: &str) -> bool {
    matches!(
        first_keyword(text).as_str(),
        "insert" | "update" | "delete" | "replace"
    )
}

/// Byte range of the `near '<snippet>' at line N` part of a syntax error
/// message in the statement text.
pub fn syntax_error_range(message: &str, text: &str) -> Option<Range<usize>> {
    let start = message.find("near '")? + "near '".len();
    let rest = &message[start..];
    let end = rest.rfind("' at line ")?;
    let snippet = &rest[..end];
    let line: usize = rest[end + "' at line ".len()..]
        .trim()
        .trim_end_matches('.')
        .parse()
        .ok()?;
    if snippet.is_empty() {
        return None;
    }
    // Byte offset of line N (1-based).
    let line_start = if line <= 1 {
        0
    } else {
        text.match_indices('\n').nth(line - 2).map(|(i, _)| i + 1)?
    };
    let pos = text[line_start..]
        .find(snippet)
        .map(|p| p + line_start)
        .or_else(|| text.find(snippet))?;
    Some(pos..pos + snippet.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(ty: MyType, flags: ColumnFlags, charset: u16, length: u32, decimals: u8) -> ColumnMeta {
        ColumnMeta {
            ty,
            flags,
            charset,
            length,
            decimals,
        }
    }

    #[test]
    fn maps_column_types() {
        use MyType::*;
        let none = ColumnFlags::empty();
        let uns = ColumnFlags::UNSIGNED_FLAG;
        let m = |ty, flags, cs, len, dec| {
            let (t, n, _) = map_column(meta(ty, flags, cs, len, dec));
            (t, n)
        };
        assert_eq!(
            m(MYSQL_TYPE_LONG, none, 63, 11, 0),
            (ColumnType::Int64, "int".into())
        );
        assert_eq!(
            m(MYSQL_TYPE_TINY, none, 63, 1, 0),
            (ColumnType::Int64, "tinyint".into())
        );
        assert_eq!(
            m(MYSQL_TYPE_LONGLONG, uns, 63, 20, 0),
            (ColumnType::UInt64, "bigint unsigned".into())
        );
        assert_eq!(
            m(MYSQL_TYPE_LONG, uns, 63, 10, 0),
            (ColumnType::Int64, "int unsigned".into())
        );
        assert_eq!(m(MYSQL_TYPE_YEAR, none, 63, 4, 0).0, ColumnType::Int64);
        assert_eq!(m(MYSQL_TYPE_FLOAT, none, 63, 12, 31).0, ColumnType::Float32);
        assert_eq!(
            m(MYSQL_TYPE_DOUBLE, none, 63, 22, 31).0,
            ColumnType::Float64
        );
        assert_eq!(
            m(MYSQL_TYPE_NEWDECIMAL, none, 63, 12, 2),
            (
                ColumnType::Decimal {
                    precision: 10,
                    scale: 2
                },
                "decimal(10,2)".into()
            )
        );
        assert_eq!(
            m(MYSQL_TYPE_NEWDECIMAL, uns, 63, 11, 2),
            (
                ColumnType::Decimal {
                    precision: 10,
                    scale: 2
                },
                "decimal(10,2)".into()
            )
        );
        assert_eq!(
            m(MYSQL_TYPE_NEWDECIMAL, none, 63, 67, 30),
            (ColumnType::Text, "decimal(65,30)".into())
        );
        assert_eq!(m(MYSQL_TYPE_DATE, none, 63, 10, 0).0, ColumnType::Date);
        assert_eq!(m(MYSQL_TYPE_TIME, none, 63, 10, 0).0, ColumnType::Text);
        assert_eq!(
            m(MYSQL_TYPE_DATETIME, none, 63, 19, 0),
            (ColumnType::Timestamp { tz: false }, "datetime".into())
        );
        assert_eq!(m(MYSQL_TYPE_TIMESTAMP, none, 63, 19, 0).1, "timestamp");
        assert_eq!(
            m(MYSQL_TYPE_VAR_STRING, none, 255, 400, 0),
            (ColumnType::Text, "varchar".into())
        );
        assert_eq!(
            m(MYSQL_TYPE_VAR_STRING, none, 63, 100, 0),
            (ColumnType::Binary, "varbinary".into())
        );
        assert_eq!(
            m(MYSQL_TYPE_STRING, ColumnFlags::ENUM_FLAG, 255, 4, 0).1,
            "enum"
        );
        assert_eq!(
            m(MYSQL_TYPE_STRING, ColumnFlags::SET_FLAG, 255, 4, 0).1,
            "set"
        );
        assert_eq!(
            m(MYSQL_TYPE_STRING, none, 63, 4, 0),
            (ColumnType::Binary, "binary".into())
        );
        assert_eq!(
            m(MYSQL_TYPE_STRING, none, 255, 4, 0),
            (ColumnType::Text, "char".into())
        );
        assert_eq!(
            m(MYSQL_TYPE_JSON, none, 63, 0, 0),
            (ColumnType::Text, "json".into())
        );
        assert_eq!(
            m(MYSQL_TYPE_BLOB, none, 63, 65_535, 0),
            (ColumnType::Binary, "blob".into())
        );
        assert_eq!(
            m(MYSQL_TYPE_BLOB, none, 255, 262_140, 0),
            (ColumnType::Text, "text".into())
        );
        assert_eq!(m(MYSQL_TYPE_BLOB, none, 255, u32::MAX, 0).1, "longtext");
        assert_eq!(
            m(MYSQL_TYPE_BIT, none, 63, 8, 0),
            (ColumnType::Int64, "bit(8)".into())
        );
        assert_eq!(m(MYSQL_TYPE_BIT, none, 63, 64, 0).0, ColumnType::UInt64);
        assert_eq!(m(MYSQL_TYPE_GEOMETRY, none, 63, 0, 0).0, ColumnType::Binary);
    }

    #[test]
    fn converts_cells() {
        use mysql_async::Value;
        assert_eq!(bits(&[0x01, 0x02]), 258);
        let v = Value::Bytes(b"12".to_vec());
        assert_eq!(cell(Some(&v), Conv::Text), Cell::Text("12"));
        assert_eq!(cell(Some(&v), Conv::Bytes), Cell::Bytes(b"12"));
        assert_eq!(cell(Some(&Value::NULL), Conv::Text), Cell::Null);
        assert_eq!(
            cell(Some(&Value::Bytes(vec![0x80])), Conv::Bit),
            Cell::UInt(128)
        );
    }

    #[test]
    fn finds_dml_keyword() {
        assert!(is_dml("  insert into t values (1)"));
        assert!(is_dml("/* x */ -- y\n UPDATE t set a = 1"));
        assert!(is_dml("# c\nreplace into t values (1)"));
        assert!(!is_dml("create table t (a int)"));
        assert!(!is_dml("select 1"));
    }

    #[test]
    fn locates_syntax_error() {
        let text = "select 1;\nselect * frm t where a = 1";
        let msg = "You have an error in your SQL syntax; check the manual that corresponds to \
                   your MySQL server version for the right syntax to use near 'frm t where a = 1' at line 2";
        let r = syntax_error_range(msg, text).unwrap();
        assert_eq!(&text[r], "frm t where a = 1");
        // A snippet that also appears earlier is searched from line N.
        let text = "select 'frm';\nselect frm";
        let msg = "... to use near 'frm' at line 2";
        let r = syntax_error_range(msg, text).unwrap();
        assert_eq!(r.start, text.rfind("frm").unwrap());
        // Error at the end of the statement: nothing to point at.
        assert_eq!(syntax_error_range("... near '' at line 1", "select"), None);
        assert_eq!(syntax_error_range("Unknown column 'x'", "select x"), None);
    }

    #[test]
    fn decimal_precision_from_length() {
        assert_eq!(decimal_precision(12, 2, false), (10, 2));
        assert_eq!(decimal_precision(11, 2, true), (10, 2));
        assert_eq!(decimal_precision(4, 0, false), (3, 0));
        assert_eq!(decimal_precision(0, 0, false), (1, 0));
    }
}
