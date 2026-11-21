//! Just enough PartiQL analysis for the driver: the statement kind, the
//! table (and index) it reads, and whether a `SELECT` scans the table.
//!
//! DynamoDB turns a `SELECT` into a `Query` only when its `WHERE` clause
//! pins the partition key with `=` or `IN` (and that condition is not
//! under an `OR`); everything else is a `Scan`, which reads and bills every
//! item of the table. Language support (completion, diagnostics) lives in
//! `ferrodb-lang`; this is only the runtime check behind the cost warning.

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Word(String),
    /// `"name"`: case-sensitive identifier.
    Quoted(String),
    /// `'text'`.
    Str,
    Punct(char),
    /// `<<` / `>>` (set literals) and other two-character operators.
    Op(String),
}

fn tokenize(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut it = s.char_indices().peekable();
    while let Some((_, c)) = it.next() {
        match c {
            c if c.is_whitespace() => {}
            '-' if it.peek().is_some_and(|(_, n)| *n == '-') => {
                for (_, n) in it.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
            }
            '"' | '\'' => {
                let mut text = String::new();
                while let Some((_, n)) = it.next() {
                    if n == c {
                        if it.peek().is_some_and(|(_, m)| *m == c) {
                            it.next();
                            text.push(c);
                            continue;
                        }
                        break;
                    }
                    text.push(n);
                }
                out.push(if c == '"' {
                    Tok::Quoted(text)
                } else {
                    Tok::Str
                });
            }
            '<' | '>' | '!' if it.peek().is_some_and(|(_, n)| matches!(n, '<' | '>' | '=')) => {
                let (_, n) = it.next().expect("peeked");
                out.push(Tok::Op(format!("{c}{n}")));
            }
            c if c.is_alphanumeric() || c == '_' || c == '$' => {
                let mut w = String::from(c);
                while let Some((_, n)) = it.peek() {
                    if n.is_alphanumeric() || *n == '_' || *n == '$' {
                        w.push(*n);
                        it.next();
                    } else {
                        break;
                    }
                }
                out.push(Tok::Word(w));
            }
            other => out.push(Tok::Punct(other)),
        }
    }
    out
}

fn is_kw(t: &Tok, kw: &str) -> bool {
    matches!(t, Tok::Word(w) if w.eq_ignore_ascii_case(kw))
}

fn name(t: &Tok) -> Option<&str> {
    match t {
        Tok::Word(w) | Tok::Quoted(w) => Some(w),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Select,
    Insert,
    Update,
    Delete,
    /// `EXISTS(SELECT …)` (condition checks in transactions).
    Exists,
    Other,
}

/// What the driver needs to know about one PartiQL statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    pub kind: Kind,
    pub table: Option<String>,
    /// `SELECT … FROM "table"."index"`.
    pub index: Option<String>,
    /// The `SELECT` projects whole items (`*`): results can be edited.
    pub star: bool,
    toks: Vec<Tok>,
    /// Token index of `WHERE`.
    where_at: Option<usize>,
}

pub fn analyze(text: &str) -> Statement {
    let toks = tokenize(text);
    let kind = match toks.first() {
        Some(t) if is_kw(t, "select") => Kind::Select,
        Some(t) if is_kw(t, "insert") => Kind::Insert,
        Some(t) if is_kw(t, "update") => Kind::Update,
        Some(t) if is_kw(t, "delete") => Kind::Delete,
        Some(t) if is_kw(t, "exists") => Kind::Exists,
        _ => Kind::Other,
    };
    // The table follows FROM (SELECT, DELETE), INTO (INSERT) or UPDATE.
    let after = match kind {
        Kind::Select | Kind::Delete | Kind::Exists => toks.iter().position(|t| is_kw(t, "from")),
        Kind::Insert => toks.iter().position(|t| is_kw(t, "into")),
        Kind::Update => Some(0),
        Kind::Other => None,
    };
    let (mut table, mut index) = (None, None);
    if let Some(i) = after {
        table = toks.get(i + 1).and_then(name).map(str::to_owned);
        if toks.get(i + 2) == Some(&Tok::Punct('.')) {
            index = toks.get(i + 3).and_then(name).map(str::to_owned);
        }
    }
    let star = kind == Kind::Select
        && toks.get(1) == Some(&Tok::Punct('*'))
        && toks.get(2).is_some_and(|t| is_kw(t, "from"));
    let where_at = toks.iter().position(|t| is_kw(t, "where"));
    Statement {
        kind,
        table,
        index,
        star,
        toks,
        where_at,
    }
}

impl Statement {
    /// A `SELECT` that DynamoDB runs as a `Scan` of the table, given the
    /// partition key of the table (or of the index it reads).
    pub fn scans(&self, partition_key: &str) -> bool {
        if self.kind != Kind::Select {
            return false;
        }
        let Some(w) = self.where_at else {
            return true;
        };
        // The WHERE clause up to ORDER BY.
        let cond: Vec<&Tok> = self.toks[w + 1..]
            .iter()
            .take_while(|t| !is_kw(t, "order"))
            .collect();
        // A top-level OR may bypass the key condition.
        let mut depth = 0i32;
        for t in &cond {
            match t {
                Tok::Punct('(' | '[' | '{') => depth += 1,
                Tok::Punct(')' | ']' | '}') => depth -= 1,
                t if depth == 0 && is_kw(t, "or") => return true,
                _ => {}
            }
        }
        // No OR above: parentheses only group ANDed conditions.
        for (i, t) in cond.iter().enumerate() {
            if name(t) == Some(partition_key) {
                // Not a path into a map (`pk.x`), compared with = or IN.
                let prev_dot = i > 0 && cond[i - 1] == &Tok::Punct('.');
                match cond.get(i + 1) {
                    Some(Tok::Punct('=')) if !prev_dot => return false,
                    Some(n) if is_kw(n, "in") && !prev_dot => return false,
                    _ => {}
                }
            }
            // `'x' = pk`
            if *t == &Tok::Punct('=')
                && i + 1 < cond.len()
                && name(cond[i + 1]) == Some(partition_key)
                && cond.get(i + 2) != Some(&&Tok::Punct('.'))
                && i > 0
                && !matches!(cond[i - 1], Tok::Word(_) | Tok::Quoted(_))
            {
                return false;
            }
        }
        true
    }
}

/// API operations accepted as console text: `Scan {"TableName": "t"}`.
pub const OPERATIONS: &[&str] = &[
    "ListTables",
    "DescribeTable",
    "Query",
    "Scan",
    "GetItem",
    "PutItem",
    "UpdateItem",
    "DeleteItem",
    "TransactWriteItems",
    "ExecuteStatement",
    "ExecuteTransaction",
    "CreateTable",
    "DeleteTable",
];

/// `Operation {json}` typed in the console: (operation, JSON request).
pub fn api_call(text: &str) -> Option<(&'static str, &str)> {
    let t = text.trim().trim_end_matches(';').trim();
    let end = t
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(t.len());
    let (word, rest) = t.split_at(end);
    let op = OPERATIONS.iter().find(|o| o.eq_ignore_ascii_case(word))?;
    let rest = rest.trim();
    (rest.is_empty() || rest.starts_with('{')).then_some((*op, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_tables_and_indexes() {
        let s = analyze(r#"SELECT * FROM "Orders"."byCustomer" WHERE customer = 'c1'"#);
        assert_eq!(s.kind, Kind::Select);
        assert_eq!(s.table.as_deref(), Some("Orders"));
        assert_eq!(s.index.as_deref(), Some("byCustomer"));
        assert!(s.star);
        let s = analyze(r#"insert into "T" value {'pk': 'a'}"#);
        assert_eq!((s.kind, s.table.as_deref()), (Kind::Insert, Some("T")));
        let s = analyze(r#"UPDATE T SET a = 1 WHERE pk = 'x'"#);
        assert_eq!((s.kind, s.table.as_deref()), (Kind::Update, Some("T")));
        let s = analyze(r#"DELETE FROM "T" WHERE pk = 'x'"#);
        assert_eq!((s.kind, s.table.as_deref()), (Kind::Delete, Some("T")));
        assert!(!analyze("SELECT a FROM t").star);
    }

    #[test]
    fn scans_without_a_partition_key_condition() {
        let scan = |q: &str| analyze(q).scans("pk");
        assert!(scan(r#"SELECT * FROM "T""#));
        assert!(scan(r#"SELECT * FROM "T" WHERE sk = 1"#));
        assert!(scan(r#"SELECT * FROM "T" WHERE pk = 'a' OR sk = 1"#));
        assert!(scan(r#"SELECT * FROM "T" WHERE pk > 'a'"#));
        assert!(scan(r#"SELECT * FROM "T" WHERE m.pk = 'a'"#));
        assert!(!scan(r#"SELECT * FROM "T" WHERE pk = 'a'"#));
        assert!(!scan(
            r#"SELECT * FROM "T" WHERE "pk" = 'a' AND sk BETWEEN 1 AND 5"#
        ));
        assert!(!scan(r#"SELECT * FROM "T" WHERE pk IN ['a', 'b']"#));
        assert!(!scan(r#"SELECT * FROM "T" WHERE (sk > 1) AND 'a' = pk"#));
        assert!(!scan(r#"SELECT * FROM "T" WHERE pk = ? ORDER BY sk DESC"#));
        assert!(!analyze(r#"UPDATE "T" SET a = 1"#).scans("pk"));
    }

    #[test]
    fn api_calls_in_the_console() {
        assert_eq!(
            api_call(r#"Scan {"TableName": "t"}"#),
            Some(("Scan", r#"{"TableName": "t"}"#))
        );
        assert_eq!(api_call("listtables"), Some(("ListTables", "")));
        assert_eq!(api_call("SELECT * FROM t"), None);
        assert_eq!(api_call("Scanner {}"), None);
    }
}
