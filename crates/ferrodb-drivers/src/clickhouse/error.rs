//! ClickHouse exception text → [`QueryError`].
//!
//! The HTTP interface answers a failed statement with a non-200 status and
//! the exception as plain text, e.g.
//! `Code: 62. DB::Exception: Syntax error: failed at position 8 (frm) …
//! (SYNTAX_ERROR) (version 26.10.4.207 (official build))`. An exception
//! raised after the result started streaming is appended to the body
//! (after the rows already sent), framed by `__exception__` markers in
//! recent versions.

use ferrodb_api::{Error, QueryError};

/// Error code: query cancelled by `KILL QUERY` (`QUERY_WAS_CANCELLED`).
pub(crate) const QUERY_WAS_CANCELLED: &str = "QUERY_WAS_CANCELLED";
/// The session is still busy with a previous (killed) statement.
pub(crate) const SESSION_IS_LOCKED: &str = "SESSION_IS_LOCKED";

/// Parses an exception text. `sql` is the statement as sent, for the
/// position of syntax errors (`failed at position N`, 1-based bytes).
pub(crate) fn parse(text: &str, sql: &str) -> QueryError {
    let text = clean(text);
    let (number, rest) = match text.strip_prefix("Code: ") {
        Some(r) => {
            let end = r.find('.').unwrap_or(r.len());
            (
                Some(r[..end].trim().to_owned()),
                r[end..].trim_start_matches('.'),
            )
        }
        None => (None, text.as_str()),
    };
    let rest = rest.trim();
    let rest = rest.strip_prefix("DB::Exception:").unwrap_or(rest).trim();
    // Trailing ` (version …)` and ` (NAME)`.
    let mut message = rest.to_owned();
    if let Some(i) = message.rfind(" (version ") {
        message.truncate(i);
    }
    let mut name = None;
    if message.ends_with(')')
        && let Some(i) = message.rfind(" (")
    {
        let candidate = &message[i + 2..message.len() - 1];
        if !candidate.is_empty()
            && candidate
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            name = Some(candidate.to_owned());
            message.truncate(i);
        }
    }
    let range = position(&message, sql);
    // Syntax errors list every statement the parser could have expected
    // (several kilobytes): keep the beginning.
    if message.len() > 400
        && let Some(i) = message.find(" Expected one of: ")
    {
        let end = message[i..]
            .char_indices()
            .map(|(j, _)| i + j)
            .take_while(|j| *j < i + 200)
            .last()
            .unwrap_or(message.len());
        message.truncate(end);
        message.push('…');
    }
    QueryError {
        message: message.trim().to_owned(),
        code: name.or(number),
        range,
    }
}

/// The exception without the stream framing.
fn clean(text: &str) -> String {
    let t = text.trim();
    let t = match t.find("Code: ") {
        Some(i) => &t[i..],
        None => t,
    };
    let t = match t.find("\n__exception__") {
        Some(i) => &t[..i],
        None => t,
    };
    // Exception lines may end with a tag line (`<len> <tag>`).
    t.lines()
        .next()
        .map(str::trim)
        .unwrap_or_default()
        .to_owned()
}

/// `failed at position N (token)` → byte range of the token in `sql`.
fn position(message: &str, sql: &str) -> Option<std::ops::Range<usize>> {
    let i = message.find("at position ")?;
    let digits: String = message[i + 12..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let pos = digits.parse::<usize>().ok()?.checked_sub(1)?;
    if pos > sql.len() {
        return None;
    }
    // Token in parentheses or backticks right after the number.
    let after = &message[i + 12 + digits.len()..];
    let token = after
        .trim_start()
        .strip_prefix('(')
        .and_then(|r| r.split(')').next())
        .map(|t| t.trim_matches('\'').trim_matches('`'))
        .filter(|t| !t.is_empty() && !t.starts_with("line "));
    let len = token
        .filter(|t| sql[pos..].starts_with(t))
        .map_or(1, str::len);
    Some(pos..(pos + len).min(sql.len()).max(pos))
}

/// An exception text as an [`Error`]; cancellation becomes
/// [`Error::Cancelled`].
pub(crate) fn to_error(text: &str, sql: &str) -> Error {
    let q = parse(text, sql);
    if q.code.as_deref() == Some(QUERY_WAS_CANCELLED) || q.code.as_deref() == Some("394") {
        return Error::Cancelled;
    }
    Error::Query(q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_error_with_position() {
        let sql = "select * frm t";
        let q = parse(
            "Code: 62. DB::Exception: Syntax error: failed at position 10 (frm) (line 1, col 10): \
             frm t. Expected one of: token, Comma, FROM. (SYNTAX_ERROR) (version 26.10.4.207 (official build))\n",
            sql,
        );
        assert_eq!(q.code.as_deref(), Some("SYNTAX_ERROR"));
        assert!(q.message.starts_with("Syntax error: failed at position 10"));
        assert_eq!(q.range, Some(9..12));
        assert_eq!(&sql[9..12], "frm");
    }

    #[test]
    fn unknown_table() {
        let q = parse(
            "Code: 60. DB::Exception: Unknown table expression identifier 'nosuch_t'. In scope \
             SELECT * FROM nosuch_t. (UNKNOWN_TABLE) (version 26.10.4.207 (official build))",
            "select * from nosuch_t",
        );
        assert_eq!(q.code.as_deref(), Some("UNKNOWN_TABLE"));
        assert_eq!(
            q.message,
            "Unknown table expression identifier 'nosuch_t'. In scope SELECT * FROM nosuch_t."
        );
        assert_eq!(q.range, None);
    }

    #[test]
    fn exception_in_the_stream_and_cancellation() {
        let body = "\r\n__exception__\r\nabcd\r\nCode: 394. DB::Exception: Query was cancelled. \
                    (QUERY_WAS_CANCELLED) (version 26.10.4.207 (official build))\n140 abcd\r\n__exception__\r\n";
        assert_eq!(to_error(body, "select 1"), Error::Cancelled);
        let q = parse("plain failure", "x");
        assert_eq!((q.message.as_str(), q.code), ("plain failure", None));
    }
}
