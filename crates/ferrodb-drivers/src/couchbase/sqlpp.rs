//! What the driver reads from a SQL++ statement before sending it:
//! transaction control (`BEGIN WORK`, `COMMIT`, `ROLLBACK`), whether it
//! returns rows, and the one rewrite it does.
//!
//! `SELECT * FROM <keyspace> …` (one keyspace, no joins, no grouping)
//! becomes `SELECT META(k).id AS _id, k.* FROM <keyspace> …`: each row is
//! the document itself with its key as `_id` instead of `{ "k": {…} }`
//! without a key. That makes the result a keyspace's documents, editable
//! like a MongoDB collection (edits address documents by key). Everything
//! else is sent as typed.

/// A keyspace path as written: one to three parts (`coll`, `scope.coll`,
/// `bucket.scope.coll` or a bucket), without the namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyspace {
    pub parts: Vec<String>,
}

impl Keyspace {
    /// Bucket, scope and collection, resolving a partial path against the
    /// query context (`bucket` or `bucket.scope`).
    pub fn resolve(&self, bucket: Option<&str>, scope: Option<&str>) -> Option<[String; 3]> {
        let p = &self.parts;
        Some(match p.len() {
            3 => [p[0].clone(), p[1].clone(), p[2].clone()],
            2 => [bucket?.to_owned(), p[0].clone(), p[1].clone()],
            1 => match (bucket, scope) {
                (Some(b), Some(s)) => [b.to_owned(), s.to_owned(), p[0].clone()],
                // Without a scope in the context, one name is a bucket.
                _ => [p[0].clone(), "_default".into(), "_default".into()],
            },
            _ => return None,
        })
    }
}

/// `name` or `` `name` `` when it is not a plain identifier.
pub fn ident(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain && !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        name.to_owned()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

/// `bucket.scope.collection`, quoted where needed.
pub fn path(parts: &[String]) -> String {
    parts.iter().map(|p| ident(p)).collect::<Vec<_>>().join(".")
}

/// Splits a path written by [`path`] back into its parts.
pub fn split_path(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' if quoted && chars.peek() == Some(&'`') => {
                chars.next();
                cur.push('`');
            }
            '`' => quoted = !quoted,
            '.' if !quoted => out.push(std::mem::take(&mut cur)),
            other => cur.push(other),
        }
    }
    out.push(cur);
    out
}

/// Words that end a keyspace path / alias, or rule the rewrite out.
const RESERVED: &[&str] = &[
    "select",
    "from",
    "where",
    "order",
    "limit",
    "offset",
    "group",
    "having",
    "let",
    "letting",
    "use",
    "join",
    "inner",
    "left",
    "right",
    "outer",
    "nest",
    "unnest",
    "union",
    "intersect",
    "except",
    "as",
    "on",
    "keys",
    "index",
    "by",
    "window",
    "value",
    "values",
    "key",
    "set",
    "all",
    "distinct",
    "raw",
    "element",
];

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Quoted(String),
    Str,
    Punct(char),
}

/// Tokens with their byte ranges.
fn tokenize(s: &str) -> Vec<(Tok, std::ops::Range<usize>)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        match c {
            c if c.is_ascii_whitespace() => i += 1,
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b'`' | b'"' | b'\'' => {
                i += 1;
                let mut text = String::new();
                while i < b.len() {
                    if b[i] == b'\\' && c != b'`' {
                        i += 2;
                        continue;
                    }
                    if b[i] == c {
                        if b.get(i + 1) == Some(&c) {
                            text.push(c as char);
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    let ch = s[i..].chars().next().unwrap_or(' ');
                    text.push(ch);
                    i += ch.len_utf8();
                }
                i = (i + 1).min(b.len());
                out.push((
                    if c == b'`' {
                        Tok::Quoted(text)
                    } else {
                        Tok::Str
                    },
                    start..i,
                ));
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80 => {
                while i < b.len()
                    && (b[i].is_ascii_alphanumeric()
                        || b[i] == b'_'
                        || b[i] == b'$'
                        || b[i] >= 0x80)
                {
                    i += 1;
                }
                out.push((Tok::Word(s[start..i].to_owned()), start..i));
            }
            _ => {
                let ch = s[i..].chars().next().unwrap_or(' ');
                i += ch.len_utf8();
                out.push((Tok::Punct(ch), start..i));
            }
        }
    }
    out
}

fn kw(t: &Tok, w: &str) -> bool {
    matches!(t, Tok::Word(x) if x.eq_ignore_ascii_case(w))
}

/// Transaction control typed in the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxControl {
    Begin,
    Commit,
    Rollback,
}

pub fn tx_control(stmt: &str) -> Option<TxControl> {
    let t: Vec<Tok> = tokenize(stmt)
        .into_iter()
        .map(|(t, _)| t)
        .filter(|t| *t != Tok::Punct(';'))
        .collect();
    let tail_ok = |rest: &[Tok]| {
        rest.is_empty()
            || (rest.len() == 1 && (kw(&rest[0], "work") || kw(&rest[0], "transaction")))
    };
    match t.as_slice() {
        [first, rest @ ..] if kw(first, "begin") && tail_ok(rest) => Some(TxControl::Begin),
        [first, second] if kw(first, "start") && kw(second, "transaction") => {
            Some(TxControl::Begin)
        }
        [first, rest @ ..] if kw(first, "commit") && tail_ok(rest) => Some(TxControl::Commit),
        // ROLLBACK TO SAVEPOINT stays a statement of the transaction.
        [first, rest @ ..] if kw(first, "rollback") && tail_ok(rest) => Some(TxControl::Rollback),
        _ => None,
    }
}

/// Statements whose `results` are rows even when there are none.
pub fn returns_rows(stmt: &str) -> bool {
    let toks = tokenize(stmt);
    let first = toks.first().map(|(t, _)| t);
    first.is_some_and(|t| {
        [
            "select", "with", "explain", "infer", "advise", "execute", "values",
        ]
        .iter()
        .any(|w| kw(t, w))
    }) || toks.iter().any(|(t, _)| kw(t, "returning"))
        || first == Some(&Tok::Punct('('))
}

/// The editable form of `SELECT * FROM <keyspace> …`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewrite {
    pub text: String,
    pub keyspace: Keyspace,
    /// Where the rewritten part starts (the `*`) and how much longer the
    /// text became: server error positions after it are moved back.
    pub at: usize,
    pub grew: usize,
}

pub fn rewrite_select_star(stmt: &str) -> Option<Rewrite> {
    let toks = tokenize(stmt);
    let t: Vec<&Tok> = toks.iter().map(|(t, _)| t).collect();
    if t.len() < 4 || !kw(t[0], "select") || t[1] != &Tok::Punct('*') || !kw(t[2], "from") {
        return None;
    }
    // keyspace path: [namespace:]part(.part){0,2}
    let mut i = 3;
    if matches!(t.get(i + 1), Some(Tok::Punct(':'))) {
        i += 2;
    }
    let mut parts = Vec::new();
    loop {
        match t.get(i) {
            Some(Tok::Word(w)) if !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(w)) => {
                parts.push(w.clone())
            }
            Some(Tok::Quoted(q)) => parts.push(q.clone()),
            _ => return None,
        }
        i += 1;
        if t.get(i) == Some(&&Tok::Punct('.')) {
            i += 1;
            continue;
        }
        break;
    }
    if parts.len() > 3 {
        return None;
    }
    // Optional alias.
    let mut alias = parts.last().cloned().unwrap_or_default();
    if t.get(i).is_some_and(|x| kw(x, "as")) {
        i += 1;
    }
    match t.get(i) {
        Some(Tok::Word(w)) if !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(w)) => {
            alias = w.clone();
            i += 1;
        }
        Some(Tok::Quoted(q)) => {
            alias = q.clone();
            i += 1;
        }
        _ => {}
    }
    // Joins, comma joins, grouping and set operations change the row shape.
    let rest = &t[i..];
    if rest.iter().any(|x| {
        [
            "join",
            "nest",
            "unnest",
            "group",
            "union",
            "intersect",
            "except",
            "letting",
        ]
        .iter()
        .any(|w| kw(x, w))
    }) || rest.first() == Some(&&Tok::Punct(','))
    {
        return None;
    }
    let star = toks[1].1.clone();
    let a = ident(&alias);
    let projection = format!("META({a}).id AS _id, {a}.*");
    let text = format!("{}{projection}{}", &stmt[..star.start], &stmt[star.end..]);
    Some(Rewrite {
        text,
        keyspace: Keyspace { parts },
        at: star.start,
        grew: projection.len() - 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_star_is_rewritten_with_the_key() {
        let r = rewrite_select_star("SELECT * FROM tck.app.docs WHERE a = 1 LIMIT 5").unwrap();
        assert_eq!(
            r.text,
            "SELECT META(docs).id AS _id, docs.* FROM tck.app.docs WHERE a = 1 LIMIT 5"
        );
        assert_eq!(r.keyspace.parts, vec!["tck", "app", "docs"]);
        assert_eq!((r.at, r.grew), (7, 27));
        let r = rewrite_select_star("select * from `travel-sample`.inventory.airline a").unwrap();
        assert!(r.text.starts_with("select META(a).id AS _id, a.* from"));
        let r =
            rewrite_select_star("SELECT * FROM default:`b-1` AS `my doc` USE KEYS 'k'").unwrap();
        assert!(r.text.contains("META(`my doc`).id"), "{}", r.text);
        assert_eq!(r.keyspace.parts, vec!["b-1"]);
        for q in [
            "SELECT * FROM a JOIN b ON a.x = b.y",
            "SELECT * FROM a, b",
            "SELECT * FROM a UNNEST a.items i",
            "SELECT a FROM t",
            "SELECT * FROM t GROUP BY x",
            "SELECT * FROM (SELECT 1) s",
            "UPDATE t SET a = 1",
        ] {
            assert_eq!(rewrite_select_star(q), None, "{q}");
        }
    }

    #[test]
    fn keyspaces_resolve_against_the_query_context() {
        let k = |p: &[&str]| Keyspace {
            parts: p.iter().map(|s| (*s).to_owned()).collect(),
        };
        assert_eq!(
            k(&["s", "c"]).resolve(Some("b"), None),
            Some(["b".into(), "s".into(), "c".into()])
        );
        assert_eq!(
            k(&["c"]).resolve(Some("b"), Some("s")),
            Some(["b".into(), "s".into(), "c".into()])
        );
        assert_eq!(
            k(&["b"]).resolve(None, None),
            Some(["b".into(), "_default".into(), "_default".into()])
        );
        assert_eq!(k(&["s", "c"]).resolve(None, None), None);
        let p = path(&[
            "travel-sample".into(),
            "inventory".into(),
            "air`line".into(),
        ]);
        assert_eq!(p, "`travel-sample`.inventory.`air``line`");
        assert_eq!(
            split_path(&p),
            vec!["travel-sample", "inventory", "air`line"]
        );
        assert_eq!(ident("value"), "`value`");
    }

    #[test]
    fn transaction_control_and_row_statements() {
        assert_eq!(tx_control("BEGIN WORK"), Some(TxControl::Begin));
        assert_eq!(tx_control("start transaction;"), Some(TxControl::Begin));
        assert_eq!(tx_control("COMMIT"), Some(TxControl::Commit));
        assert_eq!(
            tx_control("ROLLBACK TRANSACTION"),
            Some(TxControl::Rollback)
        );
        assert_eq!(tx_control("ROLLBACK TO SAVEPOINT s1"), None);
        assert_eq!(tx_control("SELECT 1"), None);
        assert!(returns_rows("select 1"));
        assert!(returns_rows("INFER t"));
        assert!(returns_rows("UPDATE t SET a = 1 RETURNING *"));
        assert!(!returns_rows("UPSERT INTO t (KEY, VALUE) VALUES ('k', {})"));
    }
}
