//! Script parameters: `:name`, `$1` (PostgreSQL) and positional `?`
//! (MySQL, SQLite). Values are substituted as SQL literals before the text is
//! sent, so the same mechanism works with every driver; the server sees an
//! ordinary statement and coerces the literal (`'42'` to an integer column).

use std::ops::Range;

use ferrodb_api::Parameter;

use super::lexer::{Flavor, Token, TokenKind, tokenize};

/// All placeholders in `src` (one entry per occurrence).
///
/// T-SQL `@name` is a parameter unless the script declares it (`DECLARE
/// @x`, the parameters of a routine being defined). In Oracle triggers
/// `:new` / `:old` are row references, not parameters.
pub fn occurrences(src: &str, flavor: &Flavor) -> Vec<Parameter> {
    let toks = tokenize(src, flavor);
    let local = if flavor.at_params || flavor.plsql {
        local_names(src, &toks, flavor)
    } else {
        Vec::new()
    };
    let mut positional = 0;
    toks.into_iter()
        .filter(|t| t.kind == TokenKind::Param)
        .filter_map(|t| {
            let text = &src[t.range.clone()];
            let name = if text == "?" {
                positional += 1;
                format!("?{positional}")
            } else {
                text.trim_start_matches([':', '$', '@']).to_owned()
            };
            if local.iter().any(|l| l.eq_ignore_ascii_case(&name)) {
                return None;
            }
            Some(Parameter {
                name,
                range: t.range,
            })
        })
        .collect()
}

/// Words that start a T-SQL statement after a declaration.
const TSQL_STATEMENTS: &[&str] = &[
    "select", "set", "insert", "update", "delete", "merge", "exec", "execute", "if", "while",
    "print", "declare", "begin", "return", "with",
];

/// Names that are variables of the script, not parameters.
fn local_names(src: &str, toks: &[Token], flavor: &Flavor) -> Vec<String> {
    let sig: Vec<&Token> = toks.iter().filter(|t| !t.is_trivia()).collect();
    let mut out = Vec::new();
    if flavor.plsql {
        if sig.iter().any(|t| t.is_word(src, "trigger")) {
            out.extend(["new".to_owned(), "old".to_owned()]);
        }
        return out;
    }
    let word = |i: usize, w: &str| sig.get(i).is_some_and(|t| t.is_word(src, w));
    let param = |i: usize| {
        sig.get(i)
            .filter(|t| t.kind == TokenKind::Param)
            .map(|t| t.text(src).trim_start_matches('@').to_owned())
    };
    for i in 0..sig.len() {
        // `DECLARE @a int = 1, @b varchar(10)`: the `@x` after `DECLARE` and
        // after each top-level comma, until the next statement.
        if word(i, "declare") {
            let mut j = i + 1;
            let mut depth = 0;
            let mut expect = true;
            while j < sig.len() {
                match sig[j].text(src) {
                    "(" => depth += 1,
                    ")" => depth -= 1,
                    ";" => break,
                    "," if depth == 0 => expect = true,
                    _ if expect => {
                        let Some(n) = param(j) else { break };
                        out.push(n);
                        expect = false;
                    }
                    _ if depth == 0
                        && j > i + 2
                        && TSQL_STATEMENTS.iter().any(|k| sig[j].is_word(src, k)) =>
                    {
                        break;
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        // `CREATE PROCEDURE p @a int, @b int OUTPUT AS …`: every variable of
        // a routine definition is its own.
        let routine = ["procedure", "proc", "function", "trigger"]
            .iter()
            .any(|k| word(i, k));
        if routine && (1..=3).any(|k| i >= k && (word(i - k, "create") || word(i - k, "alter"))) {
            out.extend(
                sig[i..]
                    .iter()
                    .filter(|t| t.kind == TokenKind::Param)
                    .map(|t| t.text(src).trim_start_matches('@').to_owned()),
            );
            break;
        }
    }
    out
}

/// Distinct placeholders in order of first appearance.
pub fn parameters(src: &str, flavor: &Flavor) -> Vec<Parameter> {
    let mut out: Vec<Parameter> = Vec::new();
    for p in occurrences(src, flavor) {
        if !out.iter().any(|q| q.name == p.name) {
            out.push(p);
        }
    }
    out
}

/// Maps byte offsets in substituted text back to the original text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OffsetMap {
    /// (original range, substituted range) for each replaced placeholder.
    edits: Vec<(Range<usize>, Range<usize>)>,
}

impl OffsetMap {
    pub fn to_original(&self, pos: usize) -> usize {
        let mut delta: isize = 0;
        for (orig, new) in &self.edits {
            if pos < new.start {
                break;
            }
            if pos < new.end {
                return orig.start;
            }
            delta = orig.end as isize - new.end as isize;
        }
        (pos as isize + delta).max(0) as usize
    }

    pub fn range_to_original(&self, r: Range<usize>) -> Range<usize> {
        let s = self.to_original(r.start);
        s..self.to_original(r.end).max(s)
    }

    pub fn is_identity(&self) -> bool {
        self.edits.is_empty()
    }
}

/// Renders a value as a SQL literal: `NULL`, plain numbers and booleans as
/// is, everything else as a quoted string with escaped quotes.
pub fn literal(value: Option<&str>, flavor: &Flavor) -> String {
    let Some(v) = value else {
        return "NULL".into();
    };
    let t = v.trim();
    let numeric = !t.is_empty()
        && t.len() == v.len()
        && t.parse::<f64>().is_ok()
        && t.bytes()
            .all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b));
    if numeric || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
        return t.to_owned();
    }
    let mut s = v.replace('\'', "''");
    if flavor.backslash_escapes {
        s = s.replace('\\', "\\\\");
    }
    format!("'{s}'")
}

/// Replaces every placeholder with its value. Fails listing the names that
/// have no value.
pub fn substitute(
    src: &str,
    flavor: &Flavor,
    values: &[(String, Option<String>)],
) -> Result<(String, OffsetMap), Vec<String>> {
    let occ = occurrences(src, flavor);
    let missing: Vec<String> = parameters(src, flavor)
        .into_iter()
        .filter(|p| !values.iter().any(|(n, _)| n == &p.name))
        .map(|p| p.name)
        .collect();
    if !missing.is_empty() {
        return Err(missing);
    }
    let mut out = String::with_capacity(src.len());
    let mut map = OffsetMap::default();
    let mut last = 0;
    for p in occ {
        out.push_str(&src[last..p.range.start]);
        let v = values
            .iter()
            .find(|(n, _)| n == &p.name)
            .and_then(|(_, v)| v.as_deref());
        let lit = literal(v, flavor);
        let start = out.len();
        out.push_str(&lit);
        map.edits.push((p.range.clone(), start..out.len()));
        last = p.range.end;
    }
    out.push_str(&src[last..]);
    Ok((out, map))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(src: &str, d: &str) -> Vec<String> {
        parameters(src, &Flavor::for_dialect(d))
            .into_iter()
            .map(|p| p.name)
            .collect()
    }

    #[test]
    fn finds_placeholders_per_dialect() {
        assert_eq!(
            names(
                "select * from t where a = :id and b = :id and c::text = $1 and d ? 'k' and ':x'",
                "postgresql"
            ),
            vec!["id", "1"]
        );
        assert_eq!(
            names("select ? , ?, :n, '?' -- ?", "mysql"),
            vec!["?1", "?2", "n"]
        );
        assert!(names("select a := 1", "mysql").is_empty());
    }

    #[test]
    fn substitutes_literals_and_maps_offsets() {
        let pg = Flavor::for_dialect("postgresql");
        let src = "select :name, :n, :x from t where :name <> 'a'";
        let vals = vec![
            ("name".to_owned(), Some("O'Brien".to_owned())),
            ("n".to_owned(), Some("42".to_owned())),
            ("x".to_owned(), None),
        ];
        let (out, map) = substitute(src, &pg, &vals).unwrap();
        assert_eq!(
            out,
            "select 'O''Brien', 42, NULL from t where 'O''Brien' <> 'a'"
        );
        // An error at "from" in the output maps back to "from" in the source.
        let from_out = out.find("from").unwrap();
        assert_eq!(map.to_original(from_out), src.find("from").unwrap());
        // Inside a substituted literal → start of the placeholder.
        assert_eq!(map.to_original(8), 7);
        assert_eq!(map.to_original(0), 0);
    }

    #[test]
    fn missing_values_are_reported() {
        let err = substitute("select :a, :b", &Flavor::GENERIC, &[("a".into(), None)]).unwrap_err();
        assert_eq!(err, vec!["b"]);
    }

    #[test]
    fn mysql_escapes_backslashes() {
        let my = Flavor::for_dialect("mysql");
        assert_eq!(literal(Some("a\\'b"), &my), "'a\\\\''b'");
        assert_eq!(literal(Some(" 1"), &my), "' 1'");
        assert_eq!(literal(Some("1e3"), &my), "1e3");
        assert_eq!(literal(Some("inf"), &my), "'inf'");
    }
}
