//! CQL, the Cassandra Query Language (plan 2.10, level B).
//!
//! CQL is close enough to SQL for the SQL engine to do the heavy lifting:
//! the `cql` dialect file (`assets/dialects/cql.toml`) gives keywords,
//! types (collections, `frozen`, UDTs by name) and native functions; the
//! schema model has keyspaces as schemas, so tables and columns resolve and
//! complete as in SQL. This module adds what is CQL's own: `BEGIN BATCH …
//! APPLY BATCH` as one statement (in the splitter), the cqlsh commands
//! (`USE`, `CONSISTENCY`, `SERIAL CONSISTENCY`, `TRACING`, `PAGING`…),
//! primary-key-first suggestions in `WHERE`, `ALLOW FILTERING` awareness
//! and statement classification for read-only sources and confirmations.

use ferrodb_api::{
    CompletionItem, CompletionKind, Diagnostic, LiveTemplate, Risk, RiskKind, Severity,
};

use crate::{
    sql::{
        self, Flavor, Statement, StatementClass, TxCommand, complete as sql_complete,
        document::Document,
        inspect,
        lexer::{Token, TokenKind, tokenize},
        resolve::TableTarget,
        schema::TableInfo,
        syntax::{Clause, StmtKind},
    },
    util::{Cand, has_top_level, merge, rank},
};

mod tests;

/// Dialect id of the language (file `assets/dialects/cql.toml`).
pub const DIALECT: &str = "cql";

/// Built-in live templates (abbreviation, description, body).
pub const TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "sel",
        "SELECT * FROM table LIMIT",
        "SELECT * FROM $table$ LIMIT 100$END$",
    ),
    (
        "selw",
        "SELECT … WHERE partition key",
        "SELECT * FROM $table$ WHERE $key$ = $END$",
    ),
    (
        "ins",
        "INSERT INTO … VALUES … USING TTL",
        "INSERT INTO $table$ ($columns$) VALUES ($END$) USING TTL $seconds$",
    ),
    (
        "upd",
        "UPDATE … SET … WHERE",
        "UPDATE $table$ SET $column$ = $END$ WHERE $key$ = ",
    ),
    (
        "del",
        "DELETE FROM … WHERE",
        "DELETE FROM $table$ WHERE $key$ = $END$",
    ),
    (
        "batch",
        "BEGIN BATCH … APPLY BATCH",
        "BEGIN BATCH\n    $SELECTION$$END$\nAPPLY BATCH;",
    ),
    (
        "ks",
        "CREATE KEYSPACE (SimpleStrategy)",
        "CREATE KEYSPACE IF NOT EXISTS $name$\n    WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1};$END$",
    ),
    (
        "crt",
        "CREATE TABLE with PRIMARY KEY",
        "CREATE TABLE $name$ (\n    $key$ $type$,\n    $clustering$ timeuuid,\n    $END$\n    PRIMARY KEY (($key$), $clustering$)\n) WITH CLUSTERING ORDER BY ($clustering$ DESC);",
    ),
    (
        "type",
        "CREATE TYPE (UDT)",
        "CREATE TYPE $name$ (\n    $field$ $type$$END$\n);",
    ),
    (
        "cons",
        "CONSISTENCY level",
        "CONSISTENCY $LOCAL_QUORUM$$END$",
    ),
];

/// Words that start a statement or a cqlsh command.
const STATEMENT_START: &[&str] = &[
    "SELECT",
    "INSERT INTO",
    "UPDATE",
    "DELETE FROM",
    "BEGIN BATCH",
    "BEGIN UNLOGGED BATCH",
    "APPLY BATCH",
    "USE",
    "TRUNCATE",
    "CREATE TABLE",
    "CREATE KEYSPACE",
    "CREATE INDEX",
    "CREATE MATERIALIZED VIEW",
    "CREATE TYPE",
    "CREATE FUNCTION",
    "CREATE ROLE",
    "ALTER TABLE",
    "ALTER KEYSPACE",
    "DROP TABLE",
    "DROP KEYSPACE",
    "DROP INDEX",
    "GRANT",
    "REVOKE",
    "LIST ROLES",
    "DESCRIBE KEYSPACES",
    "DESCRIBE TABLES",
    "DESCRIBE TABLE",
    "CONSISTENCY",
    "SERIAL CONSISTENCY",
    "TRACING ON",
    "TRACING OFF",
    "PAGING",
    "EXPAND ON",
];

/// Levels of the cqlsh `CONSISTENCY` command.
pub const CONSISTENCY_LEVELS: &[&str] = &[
    "ANY",
    "ONE",
    "TWO",
    "THREE",
    "QUORUM",
    "ALL",
    "LOCAL_QUORUM",
    "EACH_QUORUM",
    "SERIAL",
    "LOCAL_SERIAL",
    "LOCAL_ONE",
];

const COLLECTION_TYPES: &[&str] = &["frozen<", "list<", "set<", "map<", "tuple<", "vector<"];

fn flavor() -> Flavor {
    Flavor::for_dialect(DIALECT)
}

/// Statements of a console (`BEGIN BATCH … APPLY BATCH` is one).
pub fn split(src: &str) -> Vec<Statement> {
    sql::split(src, &flavor())
}

fn sig_tokens(stmt: &str) -> Vec<Token> {
    tokenize(stmt, &flavor())
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect()
}

/// `ALLOW FILTERING` at the end of a `SELECT`.
pub fn allows_filtering(stmt: &str) -> bool {
    let toks = sig_tokens(stmt);
    toks.windows(2)
        .any(|w| w[0].is_word(stmt, "allow") && w[1].is_word(stmt, "filtering"))
}

/// What a statement does, for read-only sources and transaction tracking.
/// cqlsh session commands (`USE`, `CONSISTENCY`, `TRACING`…) only change
/// the session.
pub fn classify(stmt: &str) -> StatementClass {
    let toks = sig_tokens(stmt);
    let first = toks
        .first()
        .filter(|t| t.kind == TokenKind::Word)
        .map(|t| t.text(stmt).to_ascii_lowercase())
        .unwrap_or_default();
    match first.as_str() {
        "select" | "describe" | "desc" | "list" | "show" | "help" => StatementClass::Read,
        "use" | "consistency" | "serial" | "tracing" | "paging" | "expand" | "timing"
        | "capture" | "source" | "clear" | "cls" | "login" | "" => StatementClass::Session,
        // CQL has no transactions; a batch is one atomic modification.
        "begin" if toks.get(1).is_some_and(|t| t.is_word(stmt, "transaction")) => {
            StatementClass::Transaction(TxCommand::Begin)
        }
        _ => StatementClass::Modification,
    }
}

/// Statements needing confirmation: `DROP`, `TRUNCATE`, `DELETE`/`UPDATE`
/// without `WHERE` (refused by the server, but never run unconfirmed) and,
/// on production sources, every modification.
pub fn risks(src: &str, all_modifications: bool) -> Vec<Risk> {
    split(src)
        .into_iter()
        .filter_map(|s| {
            let text = s.text(src);
            let toks = tokenize(text, &flavor());
            let first = crate::util::first_word(text, &toks).unwrap_or_default();
            let kind = match first.as_str() {
                "drop" => RiskKind::Drop,
                "truncate" => RiskKind::Truncate,
                "delete" if !has_top_level(text, &toks, "where") => RiskKind::DeleteAll,
                "update" if !has_top_level(text, &toks, "where") => RiskKind::UpdateAll,
                "alter" if toks.iter().any(|t| t.is_word(text, "drop")) => RiskKind::Drop,
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

/// The table of a statement's first source, when it is in the catalog.
fn first_table<'d>(doc: &'d Document<'_>, si: usize) -> Option<&'d TableInfo> {
    let s = &doc.stmts[si];
    match s.sources.get(&(0, 0)).copied().flatten() {
        Some(TableTarget::Db(id)) => Some(doc.env.model.table(id)),
        _ => None,
    }
}

/// SQL diagnostics plus CQL's own: filtering on columns outside the
/// primary key needs `ALLOW FILTERING` (or an index), which reads the whole
/// table.
pub fn diagnostics(doc: &Document<'_>) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = inspect::diagnostics(doc)
        .into_iter()
        .map(|x| Diagnostic {
            range: x.range,
            severity: x.severity,
            message: x.message,
        })
        .collect();
    for (si, s) in doc.stmts.iter().enumerate() {
        if s.parsed.kind != StmtKind::Select {
            continue;
        }
        let text = s.span.text(doc.text);
        let filtering = allows_filtering(text);
        if filtering {
            let at = s
                .toks
                .iter()
                .find(|t| t.is_word(doc.text, "allow"))
                .map_or(s.span.body.clone(), |t| t.range.clone());
            out.push(Diagnostic {
                range: at,
                severity: Severity::Info,
                message: "ALLOW FILTERING may read every partition of the table".into(),
            });
            continue;
        }
        let Some(table) = first_table(doc, si) else {
            continue;
        };
        for r in &s.parsed.refs {
            if r.role != sql::syntax::Role::Column {
                continue;
            }
            let range = r.range();
            if s.parsed.at(range.start).1 != Clause::Where {
                continue;
            }
            let Some(name) = r.parts.last() else { continue };
            let name = name.name(doc.dialect());
            if let Some(c) = table.columns.iter().find(|c| c.name == name)
                && !c.primary_key
            {
                out.push(Diagnostic {
                    range,
                    severity: Severity::Warning,
                    message: format!(
                        "'{}' is not part of the primary key of {}: the query needs ALLOW FILTERING or a secondary index",
                        c.name, table.name
                    ),
                });
            }
        }
    }
    out
}

/// Words (lower case) before `offset` in the statement at `offset`.
fn context<'d>(doc: &'d Document<'_>, offset: usize) -> (Option<usize>, Vec<&'d Token>) {
    let si = doc.statement_at(offset);
    let toks = si.map_or(&[][..], |s| doc.stmts[s].toks.as_slice());
    let n = toks.partition_point(|t| t.range.end < offset);
    let mut before: Vec<&Token> = toks[..n].iter().collect();
    // The word being typed is not context.
    if before
        .last()
        .is_some_and(|t| t.range.end == offset && t.kind == TokenKind::Word)
    {
        before.pop();
    }
    (si, before)
}

/// Completion: the SQL engine (tables, columns, functions, keywords) plus
/// cqlsh commands, keyspaces after `USE`, consistency levels, CQL clause
/// keywords and types; primary-key columns first in `WHERE`.
pub fn complete(
    doc: &Document<'_>,
    offset: usize,
    templates: &[LiveTemplate],
) -> Vec<CompletionItem> {
    let offset = offset.min(doc.text.len());
    let src = doc.text;
    let all = tokenize(src, &flavor());
    if crate::util::in_literal(src, &all, offset) {
        return Vec::new();
    }
    let (replace, prefix) = crate::util::prefix_at(src, &all, offset);
    let (si, before) = context(doc, replace.start);
    let word = |t: &&Token| t.text(src).to_ascii_lowercase();
    let prev = before.last().map(word).unwrap_or_default();
    let prev2 = before
        .len()
        .checked_sub(2)
        .map(|i| word(&before[i]))
        .unwrap_or_default();
    let mut extra = Vec::new();
    let mut only_extra = false;
    if before.is_empty() {
        for k in STATEMENT_START {
            extra.push(Cand::new(*k, CompletionKind::Keyword, 300));
        }
    } else if prev == "use"
        || (prev == "keyspace" && matches!(prev2.as_str(), "describe" | "desc" | "drop" | "alter"))
    {
        for s in &doc.env.model.schemas {
            extra.push(
                Cand::new(s.name.clone(), CompletionKind::Keyword, 300)
                    .insert(doc.dialect().quote_if_needed(&s.name))
                    .detail("keyspace"),
            );
        }
        only_extra = true;
    } else if prev == "consistency" {
        for l in CONSISTENCY_LEVELS {
            extra.push(Cand::new(*l, CompletionKind::Keyword, 300));
        }
        only_extra = true;
    } else if prev == "allow" {
        extra.push(Cand::new("FILTERING", CompletionKind::Keyword, 300));
        only_extra = true;
    } else if matches!(prev.as_str(), "tracing" | "expand" | "paging") {
        for k in ["ON", "OFF"] {
            extra.push(Cand::new(k, CompletionKind::Keyword, 300));
        }
        only_extra = true;
    } else if prev == "using" {
        for k in ["TTL", "TIMESTAMP"] {
            extra.push(Cand::new(k, CompletionKind::Keyword, 300));
        }
        only_extra = true;
    }
    if let (Some(si), false) = (si, only_extra) {
        let s = &doc.stmts[si];
        let last = before.last();
        let value_end = last.is_some_and(|t| {
            matches!(
                t.kind,
                TokenKind::Number | TokenKind::String | TokenKind::Param
            ) || t.text(src) == ")"
        });
        match s.parsed.kind {
            StmtKind::Select if value_end && s.parsed.has_where => {
                for k in [
                    "AND",
                    "ALLOW FILTERING",
                    "LIMIT",
                    "PER PARTITION LIMIT",
                    "ORDER BY",
                ] {
                    extra.push(Cand::new(k, CompletionKind::Keyword, 250));
                }
            }
            StmtKind::Insert
                if value_end && s.parsed.insert.as_ref().is_some_and(|i| !i.rows.is_empty()) =>
            {
                for k in ["USING TTL", "USING TIMESTAMP", "IF NOT EXISTS"] {
                    extra.push(Cand::new(k, CompletionKind::Keyword, 250));
                }
            }
            StmtKind::Update | StmtKind::Delete if value_end && s.parsed.has_where => {
                for k in ["AND", "IF EXISTS", "IF"] {
                    extra.push(Cand::new(k, CompletionKind::Keyword, 250));
                }
            }
            _ => {}
        }
        // Column types in CREATE TABLE / TYPE / ALTER … ADD.
        let ddl = before
            .first()
            .is_some_and(|t| t.is_word(src, "create") || t.is_word(src, "alter"));
        let after_name = last.is_some_and(|t| {
            matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent)
                && !doc.dialect().is_keyword(t.text(src))
                && before
                    .len()
                    .checked_sub(2)
                    .is_some_and(|i| matches!(before[i].text(src), "(" | ","))
        }) || last.is_some_and(|t| matches!(t.text(src), "<"))
            || (last.is_some_and(|t| t.text(src) == ",")
                && before.iter().rev().any(|t| t.text(src) == "<"));
        if ddl && after_name {
            for t in &doc.dialect().types {
                extra.push(Cand::new(t.clone(), CompletionKind::Keyword, 280).detail("type"));
            }
            for t in COLLECTION_TYPES {
                extra.push(Cand::new(*t, CompletionKind::Keyword, 270).detail("type"));
            }
            only_extra = true;
        }
    }
    let ranked = rank(extra, &prefix, replace);
    if only_extra {
        return ranked;
    }
    let mut items = sql_complete::complete(doc, offset, templates);
    // Primary-key columns first after WHERE / AND.
    if matches!(prev.as_str(), "where" | "and")
        && let Some(si) = si
        && let Some(t) = first_table(doc, si)
    {
        let pk = |i: &CompletionItem| {
            i.kind == CompletionKind::Column
                && t.columns.iter().any(|c| c.primary_key && c.name == i.label)
        };
        let (mut keys, rest): (Vec<_>, Vec<_>) = items.into_iter().partition(pk);
        keys.extend(rest);
        items = keys;
    }
    merge(ranked, items)
}
