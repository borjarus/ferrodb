//! SQL++ (N1QL) for Couchbase (plan 2.10, level B).
//!
//! JSON documents in keyspaces `bucket.scope.collection`; with a query
//! context (`bucket.scope`, the console's schema) a collection is named
//! alone. Backticks quote names, `"…"` and `'…'` are strings, `$name` /
//! `$1` / `?` are parameters. Keywords and the function catalog are in
//! `assets/dialects/sqlpp.toml` (shared with the SQL formatter); this
//! module adds keyspace-path completion (buckets, scopes, collections),
//! fields of the documents behind each alias (`UNNEST` aliases included),
//! `USE KEYS`, `NEST`/`UNNEST`, `INFER`, diagnostics and risks.

use std::ops::Range;

use ferrodb_api::{
    CompletionItem, CompletionKind, Diagnostic, LiveTemplate, Risk, RiskKind, Severity,
};

use crate::{
    sql::{
        self, Dialect, Flavor, Statement, StatementClass, TxCommand,
        lexer::{Token, TokenKind, tokenize},
        syntax::unquote,
        templates,
    },
    util::{Cand, lexical, prefix_at, rank},
};

mod tests;

/// Dialect id of the language (file `assets/dialects/sqlpp.toml`).
pub const DIALECT: &str = "sqlpp";

/// Built-in live templates (abbreviation, description, body).
pub const TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "sel",
        "SELECT … FROM … LIMIT",
        "SELECT c.* FROM $collection$ AS c LIMIT 100$END$",
    ),
    (
        "selw",
        "SELECT … WHERE",
        "SELECT c.* FROM $collection$ AS c WHERE c.$field$ = $END$",
    ),
    (
        "key",
        "SELECT … USE KEYS",
        "SELECT c.* FROM $collection$ AS c USE KEYS [\"$key$\"]$END$",
    ),
    ("meta", "META().id", "META($alias$).id$END$"),
    (
        "ins",
        "INSERT INTO … (KEY, VALUE)",
        "INSERT INTO $collection$ (KEY, VALUE) VALUES (\"$key$\", {$END$})",
    ),
    (
        "ups",
        "UPSERT INTO … (KEY, VALUE)",
        "UPSERT INTO $collection$ (KEY, VALUE) VALUES (\"$key$\", {$END$})",
    ),
    (
        "upd",
        "UPDATE … SET … WHERE",
        "UPDATE $collection$ AS c SET c.$field$ = $END$ WHERE ",
    ),
    (
        "unnest",
        "UNNEST an array field",
        "SELECT c.*, i FROM $collection$ AS c UNNEST c.$array$ AS i$END$",
    ),
    (
        "any",
        "ANY … IN … SATISFIES … END",
        "ANY $v$ IN $array$ SATISFIES $v$.$field$ = $END$ END",
    ),
    (
        "infer",
        "INFER collection",
        "INFER $collection$ WITH {\"sample_size\": 1000}$END$",
    ),
    (
        "idx",
        "CREATE INDEX",
        "CREATE INDEX $name$ ON $collection$($field$)$END$",
    ),
    (
        "pidx",
        "CREATE PRIMARY INDEX",
        "CREATE PRIMARY INDEX ON $collection$$END$",
    ),
];

const STATEMENT_START: &[&str] = &[
    "SELECT",
    "INSERT INTO",
    "UPSERT INTO",
    "UPDATE",
    "DELETE FROM",
    "MERGE INTO",
    "INFER",
    "EXPLAIN",
    "ADVISE",
    "PREPARE",
    "EXECUTE",
    "WITH",
    "CREATE INDEX",
    "CREATE PRIMARY INDEX",
    "BUILD INDEX ON",
    "DROP INDEX",
    "CREATE SCOPE",
    "CREATE COLLECTION",
    "DROP COLLECTION",
    "BEGIN WORK",
    "COMMIT",
    "ROLLBACK",
    "GRANT",
    "REVOKE",
    "UPDATE STATISTICS FOR",
];

const STATEMENT_WORDS: &[&str] = &[
    "select",
    "insert",
    "upsert",
    "update",
    "delete",
    "merge",
    "infer",
    "explain",
    "advise",
    "prepare",
    "execute",
    "with",
    "create",
    "drop",
    "alter",
    "build",
    "begin",
    "start",
    "commit",
    "rollback",
    "savepoint",
    "set",
    "grant",
    "revoke",
    "analyze",
    "values",
];

const AFTER_SOURCE: &[&str] = &[
    "AS",
    "USE KEYS",
    "USE INDEX",
    "WHERE",
    "JOIN",
    "LEFT JOIN",
    "INNER JOIN",
    "NEST",
    "LEFT NEST",
    "UNNEST",
    "LEFT UNNEST",
    "LET",
    "GROUP BY",
    "ORDER BY",
    "LIMIT",
    "OFFSET",
    "ON KEYS",
    "ON",
];

const IN_EXPRESSION: &[&str] = &[
    "AND",
    "OR",
    "NOT",
    "IN",
    "WITHIN",
    "IS MISSING",
    "IS NOT MISSING",
    "IS NULL",
    "IS NOT NULL",
    "IS VALUED",
    "LIKE",
    "BETWEEN",
    "ANY",
    "EVERY",
    "SATISFIES",
    "END",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "ARRAY",
    "FIRST",
    "FOR",
    "EXISTS",
    "MISSING",
    "NULL",
    "TRUE",
    "FALSE",
    "META()",
    "DISTINCT",
    "RAW",
    "ELEMENT",
    "VALUE",
    "FROM",
    "WHERE",
    "GROUP BY",
    "ORDER BY",
    "LIMIT",
    "RETURNING",
];

/// A keyspace (collection) with the fields seen in its documents (paths
/// like `address.city`, from schema inference).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keyspace {
    pub bucket: String,
    pub scope: String,
    pub collection: String,
    pub fields: Vec<String>,
}

/// What the editor knows about a Couchbase cluster.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    pub keyspaces: Vec<Keyspace>,
    /// The keyspace list is complete (unknown keyspaces are errors).
    pub loaded: bool,
}

impl Schema {
    pub fn buckets(&self) -> Vec<&str> {
        let mut v: Vec<&str> = Vec::new();
        for k in &self.keyspaces {
            if !v.contains(&k.bucket.as_str()) {
                v.push(&k.bucket);
            }
        }
        v
    }

    pub fn scopes(&self, bucket: &str) -> Vec<&str> {
        let mut v: Vec<&str> = Vec::new();
        for k in self.keyspaces.iter().filter(|k| k.bucket == bucket) {
            if !v.contains(&k.scope.as_str()) {
                v.push(&k.scope);
            }
        }
        v
    }

    pub fn collections(&self, bucket: &str, scope: &str) -> Vec<&Keyspace> {
        self.keyspaces
            .iter()
            .filter(|k| k.bucket == bucket && k.scope == scope)
            .collect()
    }

    /// A keyspace path: `bucket.scope.collection`, `collection` in the query
    /// context, `bucket` (its default collection).
    pub fn resolve(&self, parts: &[String], ctx: Option<&QueryContext>) -> Option<&Keyspace> {
        let find = |b: &str, s: &str, c: &str| {
            self.keyspaces
                .iter()
                .find(|k| k.bucket == b && k.scope == s && k.collection == c)
        };
        match parts {
            [b, s, c] => find(b, s, c),
            [s, c] => ctx.and_then(|x| find(&x.bucket, s, c)),
            [c] => ctx
                .and_then(|x| x.scope.as_deref().and_then(|s| find(&x.bucket, s, c)))
                .or_else(|| find(c, "_default", "_default")),
            _ => None,
        }
    }
}

/// `bucket.scope` the console runs in (`query_context`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryContext {
    pub bucket: String,
    pub scope: Option<String>,
}

impl QueryContext {
    /// From `default:bucket.scope`, `bucket.scope` or `bucket`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches("default:");
        let mut it = s.split('.').map(|p| p.trim_matches('`'));
        let bucket = it.next().filter(|b| !b.is_empty())?.to_owned();
        Some(Self {
            bucket,
            scope: it.next().map(str::to_owned),
        })
    }
}

fn flavor() -> Flavor {
    Flavor::for_dialect(DIALECT)
}

fn dialect() -> &'static Dialect {
    Dialect::get(DIALECT)
}

pub fn split(src: &str) -> Vec<Statement> {
    sql::split(src, &flavor())
}

fn name_of(src: &str, t: &Token) -> Option<String> {
    match t.kind {
        TokenKind::QuotedIdent => Some(unquote(t.text(src))),
        TokenKind::Word => Some(t.text(src).to_owned()),
        _ => None,
    }
}

/// A source of a statement: `path [AS] alias`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Source {
    path: Vec<String>,
    range: Range<usize>,
    alias: Option<String>,
}

const SOURCE_WORDS: &[&str] = &[
    "from", "join", "nest", "unnest", "into", "update", "using", "infer", "keyspace",
];

/// Sources of the statement whose significant tokens are `toks`.
fn sources(src: &str, toks: &[&Token]) -> Vec<Source> {
    let d = dialect();
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let w = t.text(src).to_ascii_lowercase();
        if t.kind != TokenKind::Word || !SOURCE_WORDS.contains(&w.as_str()) {
            continue;
        }
        // `CREATE INDEX … ON ks(…)` is handled by `ON` below; `DELETE FROM`.
        let mut j = i + 1;
        let mut path = Vec::new();
        let start = toks.get(j).map_or(0, |t| t.range.start);
        let mut end = start;
        while let Some(n) = toks.get(j).and_then(|t| name_of(src, t)) {
            if toks[j].kind == TokenKind::Word && d.is_keyword(&n) && path.is_empty() {
                break;
            }
            path.push(n);
            end = toks[j].range.end;
            j += 1;
            if toks.get(j).is_some_and(|t| t.text(src) == ".")
                && toks.get(j + 1).is_some_and(|t| name_of(src, t).is_some())
            {
                j += 1;
            } else {
                break;
            }
        }
        if path.is_empty() {
            continue;
        }
        if toks.get(j).is_some_and(|t| t.is_word(src, "as")) {
            j += 1;
        }
        let alias = toks
            .get(j)
            .filter(|t| {
                t.kind == TokenKind::QuotedIdent
                    || (t.kind == TokenKind::Word && !d.is_keyword(t.text(src)))
            })
            .and_then(|t| name_of(src, t));
        out.push(Source {
            path,
            range: start..end,
            alias,
        });
    }
    out
}

/// (keyspace, field prefix) an alias or collection name stands for.
fn alias_target<'s>(
    name: &str,
    srcs: &[Source],
    schema: &'s Schema,
    ctx: Option<&QueryContext>,
    depth: usize,
) -> Option<(&'s Keyspace, String)> {
    if depth > 4 {
        return None;
    }
    for s in srcs {
        let exposed = s.alias.as_deref().or(s.path.last().map(String::as_str));
        if exposed != Some(name) {
            continue;
        }
        if let Some(k) = schema.resolve(&s.path, ctx) {
            return Some((k, String::new()));
        }
        // `UNNEST o.items AS i`: a path inside another source.
        if s.path.len() >= 2
            && s.path[0] != name
            && let Some((k, pre)) = alias_target(&s.path[0], srcs, schema, ctx, depth + 1)
        {
            return Some((k, format!("{pre}{}.", s.path[1..].join("."))));
        }
    }
    None
}

/// Field names one level below `prefix` (`""` = top level).
fn next_segments<'k>(k: &'k Keyspace, prefix: &str) -> Vec<&'k str> {
    let mut out: Vec<&str> = Vec::new();
    for f in &k.fields {
        // `schedule[].day`: array elements are reached through the alias.
        let Some(rest) = f.replace("[]", "").strip_prefix(prefix).map(str::to_owned) else {
            continue;
        };
        let seg = rest.split('.').next().unwrap_or_default();
        // Borrow the segment from the stored path.
        let Some(at) = f.find(seg).filter(|_| !seg.is_empty()) else {
            continue;
        };
        let seg = &f[at..at + seg.len()];
        if !out.contains(&seg) {
            out.push(seg);
        }
    }
    out
}

/// `META()`-style key functions and the catalog functions.
fn functions(cands: &mut Vec<Cand>) {
    for f in &dialect().functions {
        let no_args = f.signatures.iter().all(|s| s.contains("()"));
        cands.push(
            Cand::new(f.name.clone(), CompletionKind::Function, 0)
                .insert(if no_args {
                    format!("{}()", f.name)
                } else {
                    format!("{}(", f.name)
                })
                .detail(f.signatures.first().cloned().unwrap_or_default()),
        );
    }
}

/// Completion at `offset`. `context` is the console's query context.
pub fn complete(
    src: &str,
    offset: usize,
    schema: &Schema,
    context: Option<&str>,
    templates_list: &[LiveTemplate],
) -> Vec<CompletionItem> {
    let offset = offset.min(src.len());
    let ctx = context.and_then(QueryContext::parse);
    let all = tokenize(src, &flavor());
    if crate::util::in_literal(src, &all, offset) {
        return Vec::new();
    }
    let (replace, prefix) = prefix_at(src, &all, offset);
    // The statement around the cursor: between the `;` before and after it.
    let semis: Vec<usize> = all
        .iter()
        .filter(|t| t.kind == TokenKind::Punct && t.text(src) == ";")
        .map(|t| t.range.start)
        .collect();
    let stmt_start = semis
        .iter()
        .rev()
        .find(|&&p| p < replace.start)
        .map_or(0, |p| p + 1);
    let stmt_end = semis
        .iter()
        .find(|&&p| p >= replace.start)
        .copied()
        .unwrap_or(src.len());
    let toks: Vec<&Token> = all
        .iter()
        .filter(|t| !t.is_trivia() && t.range.start >= stmt_start && t.range.end <= stmt_end)
        .collect();
    let before: Vec<&Token> = toks
        .iter()
        .copied()
        .filter(|t| t.range.end <= replace.start)
        .collect();
    let srcs = sources(src, &toks);
    let d = dialect();
    let q = |n: &str| quote(n);
    let mut c = Vec::new();
    let Some(last) = before.last() else {
        for k in STATEMENT_START {
            c.push(Cand::new(*k, CompletionKind::Keyword, 300));
        }
        for t in templates_list {
            let (text, _) = templates::expand_body(&t.body, None);
            c.push(
                Cand::new(t.abbreviation.clone(), CompletionKind::Snippet, 250)
                    .insert(text)
                    .detail(t.description.clone()),
            );
        }
        return rank(c, &prefix, replace);
    };
    let lt = last.text(src).to_ascii_lowercase();
    if lt == "." {
        // The qualifier chain before the dot.
        let mut chain = Vec::new();
        let mut k = before.len() - 1;
        while k > 0 {
            let Some(n) = name_of(src, before[k - 1]) else {
                break;
            };
            chain.insert(0, n);
            if k >= 2 && before[k - 2].text(src) == "." {
                k -= 2;
            } else {
                break;
            }
        }
        // A source whose path is being typed is not a qualifier.
        let others: Vec<Source> = srcs
            .iter()
            .filter(|s| !(s.range.start <= last.range.start && last.range.start <= s.range.end))
            .cloned()
            .collect();
        if let Some(first) = chain.first()
            && let Some((ks, pre)) = alias_target(first, &others, schema, ctx.as_ref(), 0)
        {
            let prefix_path = format!(
                "{pre}{}",
                chain[1..]
                    .iter()
                    .map(|p| format!("{p}."))
                    .collect::<String>()
            );
            for f in next_segments(ks, &prefix_path) {
                c.push(
                    Cand::new(f, CompletionKind::Field, 100)
                        .insert(q(f))
                        .detail(ks.collection.clone()),
                );
            }
            return rank(c, &prefix, replace);
        }
        match chain.as_slice() {
            [b] => {
                for s in schema.scopes(b) {
                    c.push(
                        Cand::new(s, CompletionKind::Collection, 100)
                            .insert(q(s))
                            .detail("scope"),
                    );
                }
            }
            [b, s] => {
                for k in schema.collections(b, s) {
                    c.push(
                        Cand::new(k.collection.clone(), CompletionKind::Collection, 100)
                            .insert(q(&k.collection))
                            .detail("collection"),
                    );
                }
            }
            _ => {}
        }
        return rank(c, &prefix, replace);
    }
    let in_index_on = lt == "on"
        && toks
            .first()
            .is_some_and(|t| t.is_word(src, "create") || t.is_word(src, "build"));
    if SOURCE_WORDS.contains(&lt.as_str())
        || in_index_on
        || (lt == "," && before.iter().rev().any(|t| t.is_word(src, "from")))
    {
        if let Some(x) = &ctx
            && let Some(sc) = &x.scope
        {
            for k in schema.collections(&x.bucket, sc) {
                c.push(
                    Cand::new(k.collection.clone(), CompletionKind::Collection, 300)
                        .insert(q(&k.collection))
                        .detail(format!("{}.{}", k.bucket, k.scope)),
                );
            }
        }
        for k in &schema.keyspaces {
            let label = format!("{}.{}.{}", k.bucket, k.scope, k.collection);
            c.push(
                Cand::new(label, CompletionKind::Collection, 100).insert(format!(
                    "{}.{}.{}",
                    q(&k.bucket),
                    q(&k.scope),
                    q(&k.collection)
                )),
            );
        }
        for b in schema.buckets() {
            c.push(
                Cand::new(b, CompletionKind::Collection, 50)
                    .insert(q(b))
                    .detail("bucket"),
            );
        }
        return rank(c, &prefix, replace);
    }
    if lt == "use" {
        for k in ["KEYS", "INDEX"] {
            c.push(Cand::new(k, CompletionKind::Keyword, 300));
        }
        return rank(c, &prefix, replace);
    }
    // Right after a source name: clauses.
    let after_source = srcs.iter().any(|s| {
        s.range.end == last.range.end
            || (last.range.start > s.range.end
                && s.alias.is_some()
                && name_of(src, last) == s.alias)
    });
    if after_source {
        for k in AFTER_SOURCE {
            c.push(Cand::new(*k, CompletionKind::Keyword, 300));
        }
        return rank(c, &prefix, replace);
    }
    // Expressions: fields of the sources, functions, keywords.
    let qualify = srcs.len() > 1;
    for s in &srcs {
        let exposed = s
            .alias
            .clone()
            .or_else(|| s.path.last().cloned())
            .unwrap_or_default();
        if let Some((ks, pre)) = alias_target(&exposed, &srcs, schema, ctx.as_ref(), 0) {
            for f in next_segments(ks, &pre) {
                let insert = if qualify {
                    format!("{}.{}", q(&exposed), q(f))
                } else {
                    q(f)
                };
                c.push(
                    Cand::new(f, CompletionKind::Field, 80)
                        .insert(insert)
                        .detail(exposed.clone()),
                );
            }
        }
        c.push(
            Cand::new(exposed.clone(), CompletionKind::Collection, 60)
                .insert(q(&exposed))
                .detail("alias"),
        );
    }
    functions(&mut c);
    for k in IN_EXPRESSION {
        c.push(Cand::new(*k, CompletionKind::Keyword, 0));
    }
    let mut ks: Vec<&String> = d.keywords.iter().collect();
    ks.sort();
    for k in ks {
        c.push(Cand::new(
            k.to_ascii_uppercase(),
            CompletionKind::Keyword,
            -50,
        ));
    }
    rank(c, &prefix, replace)
}

/// A name as SQL++ wants it: backticks unless a plain non-reserved word.
pub fn quote(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain && !dialect().is_keyword(name) {
        name.to_owned()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

/// What a statement does.
pub fn classify(stmt: &str) -> StatementClass {
    let toks: Vec<Token> = tokenize(stmt, &flavor())
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect();
    let w = |i: usize| {
        toks.get(i)
            .map(|t| t.text(stmt).to_ascii_lowercase())
            .unwrap_or_default()
    };
    match w(0).as_str() {
        "select" | "explain" | "infer" | "advise" | "values" => StatementClass::Read,
        "with"
            if !toks.iter().any(|t| {
                ["insert", "upsert", "update", "delete", "merge"]
                    .iter()
                    .any(|k| t.is_word(stmt, k))
            }) =>
        {
            StatementClass::Read
        }
        "begin" | "start" => StatementClass::Transaction(TxCommand::Begin),
        "commit" => StatementClass::Transaction(TxCommand::Commit),
        "rollback" if toks.iter().any(|t| t.is_word(stmt, "savepoint")) => {
            StatementClass::Transaction(TxCommand::Savepoint)
        }
        "rollback" => StatementClass::Transaction(TxCommand::Rollback),
        "savepoint" => StatementClass::Transaction(TxCommand::Savepoint),
        "set" if w(1) == "transaction" => StatementClass::Session,
        "" => StatementClass::Session,
        _ => StatementClass::Modification,
    }
}

fn narrows(src: &str, toks: &[Token]) -> bool {
    crate::util::has_top_level(src, toks, "where")
        || toks
            .windows(2)
            .any(|w| w[0].is_word(src, "use") && w[1].is_word(src, "keys"))
}

pub fn risks(src: &str, all_modifications: bool) -> Vec<Risk> {
    split(src)
        .into_iter()
        .filter_map(|s| {
            let text = s.text(src);
            let toks: Vec<Token> = tokenize(text, &flavor())
                .into_iter()
                .filter(|t| !t.is_trivia())
                .collect();
            let first = toks.first()?.text(text).to_ascii_lowercase();
            let kind = match first.as_str() {
                "delete" if !narrows(text, &toks) => RiskKind::DeleteAll,
                "update"
                    if !narrows(text, &toks)
                        && !toks.get(1).is_some_and(|t| t.is_word(text, "statistics")) =>
                {
                    RiskKind::UpdateAll
                }
                "drop" => RiskKind::Drop,
                _ if all_modifications && classify(text) == StatementClass::Modification => {
                    RiskKind::Modification
                }
                _ => return None,
            };
            Some(Risk {
                kind,
                range: s.body,
            })
        })
        .collect()
}

/// Lexical errors, unknown statements and keyspaces (when the keyspace
/// list is loaded), `UPDATE`/`DELETE` that touch every document.
pub fn diagnostics(src: &str, schema: &Schema, context: Option<&str>) -> Vec<Diagnostic> {
    let ctx = context.and_then(QueryContext::parse);
    let all = tokenize(src, &flavor());
    let mut out = lexical(src, &all, &[('(', ')'), ('[', ']'), ('{', '}')]);
    for s in split(src) {
        let toks: Vec<&Token> = all
            .iter()
            .filter(|t| {
                !t.is_trivia() && t.range.start >= s.body.start && t.range.end <= s.body.end
            })
            .collect();
        let Some(first) = toks.first() else { continue };
        let fw = first.text(src).to_ascii_lowercase();
        if first.kind != TokenKind::Word || !STATEMENT_WORDS.contains(&fw.as_str()) {
            let hint = crate::util::closest(&fw, STATEMENT_WORDS.iter().copied())
                .map(|w| format!("; did you mean {}?", w.to_ascii_uppercase()))
                .unwrap_or_default();
            out.push(Diagnostic {
                range: first.range.clone(),
                severity: Severity::Error,
                message: format!("Unknown statement '{}'{hint}", first.text(src)),
            });
            continue;
        }
        let srcs = sources(src, &toks);
        if schema.loaded {
            for x in &srcs {
                // Paths inside another source (`UNNEST o.items`) are fields.
                let inner = srcs.iter().any(|o| {
                    o.range != x.range
                        && o.alias.as_deref().or(o.path.last().map(String::as_str))
                            == Some(x.path[0].as_str())
                });
                if inner || schema.resolve(&x.path, ctx.as_ref()).is_some() {
                    continue;
                }
                // A bucket alone is fine when it exists (default collection
                // may be absent from the list).
                if x.path.len() == 1 && schema.buckets().contains(&x.path[0].as_str()) {
                    continue;
                }
                out.push(Diagnostic {
                    range: x.range.clone(),
                    severity: Severity::Error,
                    message: format!("Unknown keyspace '{}'", x.path.join(".")),
                });
            }
        }
        let owned: Vec<Token> = toks.iter().map(|t| (*t).clone()).collect();
        if matches!(fw.as_str(), "update" | "delete")
            && !narrows(src, &owned)
            && !toks.get(1).is_some_and(|t| t.is_word(src, "statistics"))
        {
            out.push(Diagnostic {
                range: first.range.clone(),
                severity: Severity::Warning,
                message: format!(
                    "{} without WHERE or USE KEYS changes every document of the collection",
                    fw.to_ascii_uppercase()
                ),
            });
        }
    }
    out
}
