//! PartiQL for Amazon DynamoDB (plan 2.10, level B): the subset DynamoDB
//! runs — `SELECT`, `INSERT INTO … VALUE {…}`, `UPDATE … SET/REMOVE`,
//! `DELETE`, `EXISTS(…)` in transactions — with `"table"` and
//! `"table"."index"` names, `?` parameters, `MISSING`, and the functions
//! `begins_with`, `attribute_exists`… (`assets/dialects/partiql.toml`).
//!
//! The key schema matters more than anywhere else: a `SELECT` without an
//! equality (or `IN`) condition on the partition key reads the whole table
//! (a Scan), and `UPDATE`/`DELETE` must name a single item by its full key.
//! [`scan_reason`] tells the driver and the editor when that happens.

use ferrodb_api::{
    CompletionItem, CompletionKind, Diagnostic, LiveTemplate, Risk, RiskKind, Severity,
};

use crate::{
    sql::{
        self, Flavor, Statement, StatementClass, complete as sql_complete,
        document::Document,
        lexer::{Token, TokenKind, tokenize},
        schema::{ColumnInfo, SchemaInfo, SchemaModel, TableInfo},
        syntax::unquote,
    },
    util::{Cand, lexical, merge, prefix_at, rank},
};

mod tests;

/// Dialect id of the language (file `assets/dialects/partiql.toml`).
pub const DIALECT: &str = "partiql";

/// Built-in live templates (abbreviation, description, body).
pub const TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "sel",
        "SELECT by partition key",
        "SELECT * FROM \"$table$\" WHERE \"$key$\" = $END$",
    ),
    (
        "seli",
        "SELECT from an index",
        "SELECT * FROM \"$table$\".\"$index$\" WHERE \"$key$\" = $END$",
    ),
    (
        "ins",
        "INSERT INTO … VALUE {…}",
        "INSERT INTO \"$table$\" VALUE {'$key$': $END$}",
    ),
    (
        "upd",
        "UPDATE … SET … WHERE key",
        "UPDATE \"$table$\" SET $attribute$ = $END$ WHERE \"$key$\" = ",
    ),
    (
        "del",
        "DELETE FROM … WHERE key",
        "DELETE FROM \"$table$\" WHERE \"$key$\" = $END$",
    ),
    (
        "bw",
        "begins_with(attribute, prefix)",
        "begins_with($attribute$, '$END$')",
    ),
];

const STATEMENT_START: &[&str] = &[
    "SELECT * FROM",
    "INSERT INTO",
    "UPDATE",
    "DELETE FROM",
    "EXISTS",
];

/// A secondary index: name and key attributes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyIndex {
    pub name: String,
    pub partition_key: String,
    pub sort_key: Option<String>,
}

/// A DynamoDB table: key schema, indexes and the attributes seen in it
/// (inferred from a sample; DynamoDB has no column list).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyTable {
    pub name: String,
    pub partition_key: Option<String>,
    pub sort_key: Option<String>,
    pub indexes: Vec<KeyIndex>,
    pub attributes: Vec<String>,
}

/// What the editor knows about a DynamoDB account/region.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    pub tables: Vec<KeyTable>,
    /// The table list is complete (unknown tables are errors).
    pub loaded: bool,
}

impl Schema {
    pub fn table(&self, name: &str) -> Option<&KeyTable> {
        self.tables.iter().find(|t| t.name == name)
    }

    /// The schema as an SQL model (one schema of tables whose columns are
    /// the key and the inferred attributes), for the SQL completion engine.
    pub fn to_model(&self) -> SchemaModel {
        let tables = self
            .tables
            .iter()
            .map(|t| {
                let mut names: Vec<&String> =
                    t.partition_key.iter().chain(t.sort_key.iter()).collect();
                for a in &t.attributes {
                    if !names.contains(&a) {
                        names.push(a);
                    }
                }
                TableInfo {
                    name: t.name.clone(),
                    view: false,
                    columns: names
                        .into_iter()
                        .map(|n| ColumnInfo {
                            name: n.clone(),
                            data_type: String::new(),
                            nullable: true,
                            primary_key: Some(n) == t.partition_key.as_ref()
                                || Some(n) == t.sort_key.as_ref(),
                            generated: false,
                            comment: None,
                        })
                        .collect(),
                    foreign_keys: Vec::new(),
                    comment: None,
                }
            })
            .collect();
        SchemaModel::new(
            vec![SchemaInfo {
                name: "dynamodb".into(),
                loaded: self.loaded,
                tables,
                routines: Vec::new(),
            }],
            vec!["dynamodb".into()],
        )
    }
}

fn flavor() -> Flavor {
    Flavor::for_dialect(DIALECT)
}

fn sig(stmt: &str) -> Vec<Token> {
    tokenize(stmt, &flavor())
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect()
}

pub fn split(src: &str) -> Vec<Statement> {
    sql::split(src, &flavor())
}

/// `SELECT` and `EXISTS` read; everything else writes.
pub fn classify(stmt: &str) -> StatementClass {
    match sig(stmt)
        .first()
        .map(|t| t.text(stmt).to_ascii_lowercase())
        .as_deref()
    {
        Some("select" | "exists") => StatementClass::Read,
        None => StatementClass::Session,
        _ => StatementClass::Modification,
    }
}

/// Target of a statement: (table, index, range of the table name).
pub fn target(stmt: &str) -> Option<(String, Option<String>, std::ops::Range<usize>)> {
    let toks = sig(stmt);
    let at = toks.iter().position(|t| {
        t.is_word(stmt, "from") || t.is_word(stmt, "into") || t.is_word(stmt, "update")
    })?;
    let i = at + 1;
    let name = |t: &Token| match t.kind {
        TokenKind::QuotedIdent => Some(unquote(t.text(stmt))),
        TokenKind::Word => Some(t.text(stmt).to_owned()),
        _ => None,
    };
    let t = toks.get(i)?;
    let table = name(t)?;
    let index = (toks.get(i + 1).is_some_and(|d| d.text(stmt) == "."))
        .then(|| toks.get(i + 2).and_then(name))
        .flatten();
    Some((table, index, t.range.clone()))
}

/// `name = value` / `name IN (…)` conditions joined by `AND` at the top
/// level of `WHERE`; `None` when an `OR` makes them unusable as a key.
fn equality_attributes(stmt: &str, toks: &[Token]) -> Option<Vec<String>> {
    let w = toks.iter().position(|t| t.is_word(stmt, "where"))?;
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut i = w + 1;
    while i < toks.len() {
        let t = &toks[i];
        match t.text(stmt) {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            if t.is_word(stmt, "or") {
                return None;
            }
            if t.is_word(stmt, "returning") {
                break;
            }
            let is_name = matches!(t.kind, TokenKind::QuotedIdent | TokenKind::Word)
                && !t.is_word(stmt, "and")
                && !t.is_word(stmt, "not");
            let next = toks.get(i + 1);
            let eq = next.is_some_and(|n| n.text(stmt) == "=")
                && toks.get(i + 2).is_none_or(|n| n.text(stmt) != "=");
            let inn = next.is_some_and(|n| n.is_word(stmt, "in"));
            let negated = i > 0 && toks[i - 1].is_word(stmt, "not");
            if is_name && (eq || inn) && !negated {
                let n = if t.kind == TokenKind::QuotedIdent {
                    unquote(t.text(stmt))
                } else {
                    t.text(stmt).to_owned()
                };
                out.push(n);
            }
        }
        i += 1;
    }
    Some(out)
}

/// Why a `SELECT` reads the whole table or index (`None`: it is a Query or
/// a key lookup). Without the key schema, only a missing `WHERE` counts.
pub fn scan_reason(stmt: &str, schema: &Schema) -> Option<String> {
    let toks = sig(stmt);
    if !toks.first().is_some_and(|t| t.is_word(stmt, "select")) {
        return None;
    }
    let (table, index, _) = target(stmt)?;
    let Some(eqs) = equality_attributes(stmt, &toks) else {
        let what = if toks.iter().any(|t| t.is_word(stmt, "where")) {
            "the WHERE clause combines conditions with OR"
        } else {
            "there is no WHERE clause"
        };
        return Some(format!("Full table scan of \"{table}\": {what}"));
    };
    let t = schema.table(&table)?;
    let key = match &index {
        Some(ix) => t
            .indexes
            .iter()
            .find(|i| &i.name == ix)
            .map(|i| i.partition_key.clone()),
        None => t.partition_key.clone(),
    }?;
    (!eqs.iter().any(|e| e == &key)).then(|| {
        format!("Full table scan of \"{table}\": no equality or IN condition on the partition key \"{key}\"")
    })
}

/// `true` when the statement will Scan (see [`scan_reason`]).
pub fn will_scan(stmt: &str, schema: &Schema) -> bool {
    scan_reason(stmt, schema).is_some()
}

pub fn risks(src: &str, all_modifications: bool) -> Vec<Risk> {
    split(src)
        .into_iter()
        .filter_map(|s| {
            let text = s.text(src);
            let toks = sig(text);
            let first = toks.first()?.text(text).to_ascii_lowercase();
            let has_where = toks.iter().any(|t| t.is_word(text, "where"));
            let kind = match first.as_str() {
                "delete" if !has_where => RiskKind::DeleteAll,
                "update" if !has_where => RiskKind::UpdateAll,
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

fn diag(
    range: std::ops::Range<usize>,
    severity: Severity,
    message: impl Into<String>,
) -> Diagnostic {
    Diagnostic {
        range,
        severity,
        message: message.into(),
    }
}

/// Lexical errors, statements DynamoDB does not run, unknown tables and
/// indexes, `INSERT` without `VALUE`, key-less `UPDATE`/`DELETE`, and a
/// warning on every `SELECT` that scans.
pub fn diagnostics(src: &str, schema: &Schema) -> Vec<Diagnostic> {
    let all = tokenize(src, &flavor());
    let mut out = lexical(src, &all, &[('(', ')'), ('[', ']'), ('{', '}')]);
    for s in split(src) {
        let text = s.text(src);
        let base = s.body.start;
        let toks = sig(text);
        let Some(first) = toks.first() else { continue };
        let fw = first.text(text).to_ascii_lowercase();
        let at = |r: &std::ops::Range<usize>| r.start + base..r.end + base;
        if !matches!(
            fw.as_str(),
            "select" | "insert" | "update" | "delete" | "exists"
        ) {
            let hint = crate::util::closest(&fw, ["select", "insert", "update", "delete"])
                .map(|w| format!("; did you mean {}?", w.to_ascii_uppercase()))
                .unwrap_or_default();
            out.push(diag(
                at(&first.range),
                Severity::Error,
                format!("DynamoDB PartiQL runs SELECT, INSERT, UPDATE and DELETE only{hint}"),
            ));
            continue;
        }
        if let Some((table, index, r)) = target(text)
            && schema.loaded
        {
            match schema.table(&table) {
                None => out.push(diag(
                    at(&r),
                    Severity::Error,
                    format!("Unknown table \"{table}\""),
                )),
                Some(t) => {
                    if let Some(ix) = &index
                        && !t.indexes.iter().any(|i| &i.name == ix)
                    {
                        out.push(diag(
                            at(&r),
                            Severity::Error,
                            format!("Table \"{table}\" has no index \"{ix}\""),
                        ));
                    }
                }
            }
        }
        match fw.as_str() {
            "insert" if !toks.iter().any(|t| t.is_word(text, "value")) => out.push(diag(
                at(&first.range),
                Severity::Error,
                "INSERT needs VALUE {…} with the item (DynamoDB inserts one item per statement)",
            )),
            "update" | "delete" if !toks.iter().any(|t| t.is_word(text, "where")) => {
                out.push(diag(
                    at(&first.range),
                    Severity::Error,
                    format!(
                        "{} needs a WHERE clause naming the item's full primary key",
                        fw.to_ascii_uppercase()
                    ),
                ))
            }
            "select" => {
                if let Some(reason) = scan_reason(text, schema) {
                    out.push(diag(at(&first.range), Severity::Warning, reason));
                }
            }
            _ => {}
        }
    }
    out
}

/// SQL completion over the key schema, plus statement starts, index names
/// after `"table".`, the item skeleton after `VALUE`, and key attributes
/// first in `WHERE`.
pub fn complete(
    src: &str,
    offset: usize,
    schema: &Schema,
    model: &SchemaModel,
    templates: &[LiveTemplate],
) -> Vec<CompletionItem> {
    let offset = offset.min(src.len());
    let all = tokenize(src, &flavor());
    if crate::util::in_literal(src, &all, offset) {
        return Vec::new();
    }
    let (replace, prefix) = prefix_at(src, &all, offset);
    let before: Vec<&Token> = all
        .iter()
        .filter(|t| !t.is_trivia() && t.range.end <= replace.start)
        .collect();
    let stmt_start = before
        .iter()
        .rposition(|t| t.text(src) == ";")
        .map_or(0, |i| i + 1);
    let before = &before[stmt_start..];
    let mut extra = Vec::new();
    let last = before
        .last()
        .map(|t| t.text(src).to_ascii_lowercase())
        .unwrap_or_default();
    if before.is_empty() {
        for k in STATEMENT_START {
            extra.push(Cand::new(*k, CompletionKind::Keyword, 300));
        }
    } else if last == "." && before.len() >= 2 {
        let q = before[before.len() - 2];
        let name = if q.kind == TokenKind::QuotedIdent {
            unquote(q.text(src))
        } else {
            q.text(src).to_owned()
        };
        if let Some(t) = schema.table(&name) {
            for ix in &t.indexes {
                let detail = match &ix.sort_key {
                    Some(sk) => format!("index ({}, {sk})", ix.partition_key),
                    None => format!("index ({})", ix.partition_key),
                };
                extra.push(
                    Cand::new(ix.name.clone(), CompletionKind::Table, 300)
                        .insert(format!("\"{}\"", ix.name))
                        .detail(detail),
                );
            }
            return rank(extra, &prefix, replace);
        }
    } else if last == "value" {
        let table = target(&src[before[0].range.start..offset]).map(|x| x.0);
        let t = table.as_deref().and_then(|n| schema.table(n));
        let keys: Vec<String> = t
            .map(|t| {
                t.partition_key
                    .iter()
                    .chain(t.sort_key.iter())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let body = if keys.is_empty() {
            "{'': ?}".to_owned()
        } else {
            format!(
                "{{{}}}",
                keys.iter()
                    .map(|k| format!("'{k}': ?"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        extra.push(Cand::new(body, CompletionKind::Snippet, 300).detail("item with its key"));
        return rank(extra, &prefix, replace);
    } else if matches!(last.as_str(), "where" | "and") {
        let table = target(&src[before[0].range.start..offset]).map(|x| x.0);
        if let Some(t) = table.as_deref().and_then(|n| schema.table(n)) {
            for (k, what) in [
                (&t.partition_key, "partition key"),
                (&t.sort_key, "sort key"),
            ] {
                if let Some(k) = k {
                    extra.push(
                        Cand::new(k.clone(), CompletionKind::Column, 400)
                            .insert(crate::sql::Dialect::get(DIALECT).quote_if_needed(k))
                            .detail(what),
                    );
                }
            }
        }
    }
    let doc = Document::analyze(src, DIALECT, model, None);
    merge(
        rank(extra, &prefix, replace),
        sql_complete::complete(&doc, offset, templates),
    )
}
