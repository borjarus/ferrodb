//! Error positions and messages that only exist as text in vendor messages.

/// 0-based character offset of the error in `sql`, read from the message:
///
/// - H2 marks the position with `[*]` in the echoed statement;
/// - Hive: `line 1:14 …` (1-based line, 0-based column);
/// - Exasol: `[line 1, column 15]` (1-based);
/// - otherwise the object named in the message (`Table/View 'X' does not
///   exist`, `object not found: X`), when it occurs once in the statement.
pub fn position(message: &str, sql: &str) -> Option<usize> {
    h2_marker(message, sql)
        .or_else(|| line_col(message, sql))
        .or_else(|| named_object(message, sql))
}

fn h2_marker(message: &str, sql: &str) -> Option<usize> {
    let (_, echoed) = message.split_once("SQL statement:")?;
    let echoed = echoed.trim_start_matches(['\r', '\n']);
    let at = echoed.find("[*]")?;
    let before = &echoed[..at];
    sql.starts_with(before).then(|| before.chars().count())
}

fn offset_of(sql: &str, line: usize, col: usize) -> Option<usize> {
    let mut chars = 0;
    for (i, l) in sql.split('\n').enumerate() {
        if i + 1 == line {
            return (col <= l.chars().count()).then_some(chars + col);
        }
        chars += l.chars().count() + 1;
    }
    None
}

fn number_after<'a>(s: &'a str, prefix: &str) -> Option<(usize, &'a str)> {
    let rest = &s[s.find(prefix)? + prefix.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let n = digits.parse().ok()?;
    Some((n, &rest[digits.len()..]))
}

fn line_col(message: &str, sql: &str) -> Option<usize> {
    // Exasol: "[line 1, column 15]".
    if let Some((line, rest)) = number_after(message, "[line ")
        && let Some((col, _)) = number_after(rest, ", column ")
    {
        return offset_of(sql, line, col.saturating_sub(1));
    }
    // Hive: "line 1:14 Table not found".
    let (line, rest) = number_after(message, "line ")?;
    let (col, _) = number_after(rest, ":")?;
    rest.starts_with(':').then_some(())?;
    offset_of(sql, line, col)
}

fn named_object(message: &str, sql: &str) -> Option<usize> {
    let mut names: Vec<&str> = Vec::new();
    for q in ['\'', '"'] {
        let mut parts = message.split(q);
        parts.next();
        while let (Some(inside), Some(_)) = (parts.next(), parts.next()) {
            names.push(inside);
        }
    }
    if let Some((_, rest)) = message.split_once("not found: ") {
        names.push(rest.split_whitespace().next().unwrap_or_default());
    }
    let lower = sql.to_lowercase();
    for name in names {
        let name = name.rsplit('.').next().unwrap_or(name).trim();
        if name.len() < 2 || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let n = name.to_lowercase();
        let hits: Vec<usize> = lower
            .match_indices(&n)
            .map(|(i, _)| i)
            .filter(|&i| {
                let before = lower[..i].chars().next_back();
                let after = lower[i + n.len()..].chars().next();
                let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                !word(before) && !word(after)
            })
            .collect();
        if let [one] = hits[..] {
            return Some(lower[..one].chars().count());
        }
    }
    None
}

/// Drops the statement H2 echoes after `; SQL statement:` (the editor
/// already shows it, with the position marked).
pub fn clean(message: &str) -> String {
    match message.split_once("; SQL statement:") {
        Some((head, tail)) => {
            // Keep H2's error code `[42104-224]` at the end.
            let code = tail
                .trim_end()
                .rsplit_once(" [")
                .map(|(_, c)| format!(" [{c}"))
                .unwrap_or_default();
            format!("{head}{code}")
        }
        None => message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQL: &str = "select * from tck_no_such_table";

    #[test]
    fn h2_marker_and_message() {
        let m = "Table \"TCK_NO_SUCH_TABLE\" not found (this database is empty); SQL statement:\nselect * from [*]tck_no_such_table [42104-232]";
        assert_eq!(position(m, SQL), Some(14));
        assert_eq!(
            clean(m),
            "Table \"TCK_NO_SUCH_TABLE\" not found (this database is empty) [42104-232]"
        );
    }

    #[test]
    fn derby_and_hsqldb_name_the_object() {
        assert_eq!(
            position("Table/View 'TCK_NO_SUCH_TABLE' does not exist.", SQL),
            Some(14)
        );
        assert_eq!(
            position(
                "user lacks privilege or object not found: TCK_NO_SUCH_TABLE",
                SQL
            ),
            Some(14)
        );
        // Ambiguous: named twice.
        assert_eq!(position("'T' x 'AB' y", "select ab from ab"), None);
    }

    #[test]
    fn hive_and_exasol_line_columns() {
        let sql = "select *\nfrom missing";
        assert_eq!(
            position(
                "Error while compiling statement: FAILED: SemanticException [Error 10001]: line 2:5 Table not found 'missing'",
                sql
            ),
            Some(14)
        );
        assert_eq!(
            position("object MISSING not found [line 2, column 6]", sql),
            Some(14)
        );
    }
}
