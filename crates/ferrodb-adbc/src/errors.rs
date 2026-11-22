//! ADBC errors → [`ferrodb_api::Error`], with the position of syntax errors
//! where the database reports one in its message:
//!
//! - Snowflake: `syntax error line 1 at position 9 unexpected 'frm'.`
//!   (line 1-based, position 0-based);
//! - BigQuery: `… at [1:10]` (both 1-based).

use adbc_core::error::{Error as AdbcError, Status};
use ferrodb_api::{Error, QueryError};

/// Byte offset of (`line`, `col`), both 0-based, `col` in characters.
pub fn offset_of(sql: &str, line: usize, col: usize) -> Option<usize> {
    let mut offset = 0;
    for (i, l) in sql.split_inclusive('\n').enumerate() {
        if i == line {
            let byte = l.char_indices().nth(col).map(|(b, _)| b)?;
            return Some(offset + byte);
        }
        offset += l.len();
    }
    None
}

/// Snowflake `line L at position P` → byte range of the token.
pub fn snowflake_position(message: &str, sql: &str) -> Option<std::ops::Range<usize>> {
    let i = message.find("line ")?;
    let rest = &message[i + 5..];
    let line: usize = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    let j = rest.find("at position ")?;
    let col: usize = rest[j + 12..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    let start = offset_of(sql, line.checked_sub(1)?, col)?;
    // `unexpected 'token'`
    let len = rest
        .find("unexpected '")
        .and_then(|k| rest[k + 12..].split('\'').next())
        .filter(|t| sql[start..].starts_with(t))
        .map_or(1, str::len);
    Some(start..start + len)
}

/// SQLSTATE of an ADBC error, when the driver set one.
fn sqlstate(e: &AdbcError) -> Option<String> {
    let s: String = e
        .sqlstate
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8 as char)
        .collect();
    (!s.is_empty() && s != "00000").then_some(s)
}

/// Maps an ADBC error of statement `sql` (empty for non-statement calls).
pub fn map(e: AdbcError, sql: &str) -> Error {
    match e.status {
        Status::Cancelled => Error::Cancelled,
        Status::Timeout => Error::Timeout,
        Status::NotImplemented => Error::Unsupported(e.message),
        Status::Unauthenticated => Error::Connection(e.message),
        Status::IO if sql.is_empty() => Error::Connection(e.message),
        _ => {
            let range = snowflake_position(&e.message, sql)
                .or_else(|| crate::bigquery::error_position(&e.message, sql));
            let code = sqlstate(&e).or_else(|| {
                (e.vendor_code != 0 && e.vendor_code != i32::MIN).then(|| e.vendor_code.to_string())
            });
            Error::Query(QueryError {
                message: e.message,
                code,
                range,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions() {
        let sql = "select *\nfrm t";
        assert_eq!(offset_of(sql, 1, 0), Some(9));
        let r = snowflake_position(
            "001003 (42000): SQL compilation error:\nsyntax error line 2 at position 0 unexpected 'frm'.",
            sql,
        )
        .unwrap();
        assert_eq!(&sql[r], "frm");
        let e = map(
            AdbcError::with_message_and_status("[Snowflake] cancelled", Status::Cancelled),
            "x",
        );
        assert_eq!(e, Error::Cancelled);
        let mut e =
            AdbcError::with_message_and_status("no such table: t", Status::InvalidArguments);
        e.sqlstate = [b'4' as _, b'2' as _, b'S' as _, b'0' as _, b'2' as _];
        let Error::Query(q) = map(e, "select * from t") else {
            panic!()
        };
        assert_eq!(q.code.as_deref(), Some("42S02"));
    }
}
