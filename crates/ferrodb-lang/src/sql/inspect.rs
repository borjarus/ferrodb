//! Diagnostics, inspections and quick fixes for SQL.
//!
//! Three layers, cheapest first:
//! - lexical: unterminated strings, quoted names and comments, unbalanced
//!   parentheses (every dialect);
//! - syntax: `sqlparser` with the dialect's parser, for the statement kinds
//!   it understands (DML and table/view/index DDL; routine bodies and
//!   client commands are skipped);
//! - semantic, from name resolution: unknown tables, columns, qualifiers
//!   and schemas (only where the catalog is loaded), ambiguous columns,
//!   and inspections: `= NULL`, `UPDATE`/`DELETE` without `WHERE`, unused
//!   CTEs, duplicate aliases, `INSERT` value counts.
//!
//! Every diagnostic may carry quick fixes ("Alt+Enter").

use std::{
    collections::HashMap,
    ops::Range,
    sync::{Mutex, OnceLock},
};

use ferrodb_api::{CodeAction, Severity, TextEdit};
use sqlparser::{
    dialect::{
        Dialect as SpDialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect,
        SQLiteDialect,
    },
    parser::Parser,
};

use super::{
    document::{Document, StmtAnalysis},
    lexer::{Token, TokenKind},
    resolve::{Status, Symbol, What},
    syntax::{Role, SourceKind, StmtKind},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    pub fixes: Vec<CodeAction>,
}

fn fix(title: impl Into<String>, edits: Vec<TextEdit>) -> CodeAction {
    CodeAction {
        title: title.into(),
        edits,
        preferred: true,
    }
}

fn edit(range: Range<usize>, text: impl Into<String>) -> TextEdit {
    TextEdit {
        range,
        new_text: text.into(),
    }
}

/// All diagnostics of a document.
pub fn diagnostics(doc: &Document<'_>) -> Vec<Diag> {
    let mut out = Vec::new();
    lexical(doc, &mut out);
    for (si, s) in doc.stmts.iter().enumerate() {
        let lexical_error = out
            .iter()
            .any(|d| d.severity == Severity::Error && overlaps(&d.range, &s.span.body));
        if !lexical_error {
            syntax(doc, s, &mut out);
        }
        semantic(doc, si, &mut out);
    }
    out.sort_by_key(|d| (d.range.start, d.range.end));
    out
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end || a.start == b.start
}

// ------------------------------------------------------------------ lexical

fn lexical(doc: &Document<'_>, out: &mut Vec<Diag>) {
    let src = doc.text;
    for t in &doc.tokens {
        let text = t.text(src);
        let msg = match t.kind {
            TokenKind::String => {
                let closed = if let Some(stripped) = text.strip_prefix('$') {
                    let tag_end = stripped.find('$').map_or(0, |i| i + 2);
                    text.len() >= tag_end * 2 && text.ends_with(&text[..tag_end])
                } else {
                    // `'…'`, `E'…'`, `q'[…]'`, and `"…"` where it is a string.
                    let open = text.find(['\'', '"']).unwrap_or(0);
                    let qc = if text[open..].starts_with('"') {
                        '"'
                    } else {
                        '\''
                    };
                    let q = text.rfind(qc).unwrap_or(0);
                    text.len() >= 2 && text.ends_with(qc) && q > open
                };
                (!closed).then_some("Unterminated string literal")
            }
            TokenKind::QuotedIdent => {
                let close = match text.chars().next() {
                    Some('[') => ']',
                    Some(c) => c,
                    None => continue,
                };
                (text.chars().count() < 2 || !text.ends_with(close))
                    .then_some("Unterminated quoted identifier")
            }
            TokenKind::BlockComment => {
                (!text.ends_with("*/") || text.len() < 4).then_some("Unterminated comment")
            }
            _ => None,
        };
        if let Some(m) = msg {
            out.push(Diag {
                range: t.range.start
                    ..(t.range.start + text.chars().next().map_or(1, char::len_utf8)),
                severity: Severity::Error,
                message: m.into(),
                fixes: Vec::new(),
            });
        }
    }
    for s in &doc.stmts {
        let mut open: Vec<&Token> = Vec::new();
        for t in &s.toks {
            match (t.kind, t.text(src)) {
                (TokenKind::Punct, "(") => open.push(t),
                (TokenKind::Punct, ")") if open.pop().is_none() => {
                    out.push(Diag {
                        range: t.range.clone(),
                        severity: Severity::Error,
                        message: "Unmatched ')'".into(),
                        fixes: vec![fix("Remove ')'", vec![edit(t.range.clone(), "")])],
                    });
                }
                _ => {}
            }
        }
        for t in open {
            out.push(Diag {
                range: t.range.clone(),
                severity: Severity::Error,
                message: "Unclosed '('".into(),
                fixes: vec![fix(
                    "Add ')' at the end of the statement",
                    vec![edit(s.span.body.end..s.span.body.end, ")")],
                )],
            });
        }
    }
}

// ------------------------------------------------------------------- syntax

fn sp_dialect(name: &str) -> Option<Box<dyn SpDialect>> {
    Some(match name {
        "postgresql" => Box::new(PostgreSqlDialect {}),
        "mysql" => Box::new(MySqlDialect {}),
        "sqlite" => Box::new(SQLiteDialect {}),
        "duckdb" => Box::new(sqlparser::dialect::DuckDbDialect {}),
        "mssql" => Box::new(MsSqlDialect {}),
        "generic" => Box::new(GenericDialect {}),
        "ansi" => Box::new(sqlparser::dialect::AnsiDialect {}),
        "bigquery" => Box::new(sqlparser::dialect::BigQueryDialect {}),
        "clickhouse" => Box::new(sqlparser::dialect::ClickHouseDialect {}),
        "hive" => Box::new(sqlparser::dialect::HiveDialect {}),
        "oracle" => Box::new(sqlparser::dialect::OracleDialect {}),
        "redshift" => Box::new(sqlparser::dialect::RedshiftSqlDialect {}),
        "snowflake" => Box::new(sqlparser::dialect::SnowflakeDialect {}),
        _ => return None,
    })
}

/// Statements whose syntax `sqlparser` checks reliably.
fn checkable(s: &StmtAnalysis, src: &str, ddl: bool) -> bool {
    let words: Vec<String> = s
        .toks
        .iter()
        .take(6)
        .filter(|t| t.kind == TokenKind::Word)
        .map(|t| t.text(src).to_ascii_lowercase())
        .collect();
    let has = |w: &str| words.iter().any(|x| x == w);
    // T-SQL query hints `OPTION (…)`: not known to sqlparser.
    if s.toks
        .windows(2)
        .any(|w| w[0].is_word(src, "option") && w[1].text(src) == "(")
    {
        return false;
    }
    match words.first().map(String::as_str) {
        // `VALUES current date` (Db2, Derby) is not a row constructor.
        Some("values") if s.toks.get(1).is_none_or(|t| t.text(src) != "(") => false,
        Some("select" | "with" | "insert" | "update" | "delete" | "values") => {
            !s.toks.iter().any(|t| t.kind == TokenKind::Param)
        }
        Some("create") if ddl => {
            (has("table") || has("view") || has("index"))
                && !has("function")
                && !has("procedure")
                && !has("trigger")
        }
        Some("drop" | "alter") if ddl => has("table") || has("view") || has("index"),
        _ => false,
    }
}

/// First words of statements in any supported dialect.
const STATEMENT_WORDS: &[&str] = &[
    "abort",
    "alter",
    "analyze",
    "attach",
    "begin",
    "call",
    "checkpoint",
    "close",
    "cluster",
    "comment",
    "commit",
    "copy",
    "create",
    "deallocate",
    "declare",
    "delete",
    "describe",
    "detach",
    "discard",
    "do",
    "drop",
    "end",
    "exec",
    "execute",
    "explain",
    "fetch",
    "flush",
    "grant",
    "handler",
    "insert",
    "kill",
    "listen",
    "load",
    "lock",
    "merge",
    "move",
    "notify",
    "optimize",
    "pragma",
    "prepare",
    "reindex",
    "release",
    "rename",
    "repair",
    "replace",
    "reset",
    "revoke",
    "rollback",
    "savepoint",
    "select",
    "set",
    "show",
    "start",
    "table",
    "truncate",
    "unlisten",
    "update",
    "use",
    "vacuum",
    "values",
    "with",
];

/// A typo in the first word (`selec`): the statement cannot run at all.
fn statement_word(doc: &Document<'_>, s: &StmtAnalysis, out: &mut Vec<Diag>) {
    let Some(t) = s.toks.first().filter(|t| t.kind == TokenKind::Word) else {
        return;
    };
    let w = t.text(doc.text).to_ascii_lowercase();
    if STATEMENT_WORDS.contains(&w.as_str())
        || doc.dialect().is_keyword(&w)
        || doc.dialect().is_soft_keyword(&w)
    {
        return;
    }
    let Some(best) = STATEMENT_WORDS
        .iter()
        .filter_map(|k| super::resolve::close(&w, k).map(|d| (d, *k)))
        .min()
        .map(|(_, k)| k)
    else {
        return;
    };
    let upper = best.to_ascii_uppercase();
    out.push(Diag {
        range: t.range.clone(),
        severity: Severity::Error,
        message: format!(
            "Unknown statement '{}'; did you mean {upper}?",
            t.text(doc.text)
        ),
        fixes: vec![fix(
            format!("Change to {upper}"),
            vec![edit(t.range.clone(), upper.clone())],
        )],
    });
}

fn syntax(doc: &Document<'_>, s: &StmtAnalysis, out: &mut Vec<Diag>) {
    statement_word(doc, s, out);
    let Some(parser) = doc.dialect().parser.as_deref() else {
        return;
    };
    // A derived dialect parsed as its base (Greenplum as PostgreSQL, H2 as
    // generic SQL…): only DML, its DDL has clauses the base does not know.
    let own = matches!(
        (doc.dialect().id.as_str(), parser),
        (
            "postgresql"
                | "duckdb"
                | "sqlite"
                | "mysql"
                | "mariadb"
                | "redshift"
                | "snowflake"
                | "clickhouse",
            _
        ) | ("tsql", "mssql")
            | ("googlesql", "bigquery")
            | ("hiveql", "hive")
    );
    if !checkable(s, doc.text, own) {
        return;
    }
    let Some(dialect) = sp_dialect(parser) else {
        return;
    };
    let body = s.span.body.clone();
    let text = &doc.text[body.clone()];
    let Some((message, rel)) = parse_error(parser, &*dialect, text) else {
        return;
    };
    let at = rel.map_or(body.end, |o| body.start + o);
    // Highlight the token at the position (or the last one).
    let tok = s
        .toks
        .iter()
        .find(|t| t.range.start >= at)
        .or_else(|| s.toks.last());
    let range = tok.map_or(at..at, |t| t.range.clone());
    out.push(Diag {
        range,
        severity: Severity::Error,
        message: tidy(&message),
        fixes: Vec::new(),
    });
}

/// `sqlparser` result of one statement: (message, offset in the text).
type ParseResult = Option<(String, Option<usize>)>;

/// Parses with `sqlparser`, remembering the results: on every keystroke
/// only the edited statement is parsed again (the parse is most of the
/// diagnostics time of a long script).
fn parse_error(parser: &str, dialect: &dyn SpDialect, text: &str) -> ParseResult {
    static CACHE: OnceLock<Mutex<HashMap<(String, String), ParseResult>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    let key = (parser.to_owned(), text.to_owned());
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    let result = Parser::parse_sql(dialect, text).err().map(|e| {
        let msg = e.to_string();
        let msg = msg
            .strip_prefix("sql parser error: ")
            .unwrap_or(&msg)
            .to_owned();
        let (message, loc) = match msg.rfind(" at Line: ") {
            Some(i) => (msg[..i].to_owned(), parse_location(&msg[i..])),
            None => (msg.clone(), None),
        };
        (
            message,
            loc.and_then(|(line, col)| offset_of(text, line, col)),
        )
    });
    if let Ok(mut c) = cache.lock() {
        // Bounded: a console has at most a few thousand statements.
        if c.len() > 20_000 {
            c.clear();
        }
        c.insert(key, result.clone());
    }
    result
}

fn tidy(m: &str) -> String {
    let m = m
        .replace("Expected: ", "Expected ")
        .replace(", found: ", ", found ");
    let mut c = m.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => m,
    }
}

fn parse_location(s: &str) -> Option<(usize, usize)> {
    let l = s.split("Line: ").nth(1)?;
    let line: usize = l.split(',').next()?.trim().parse().ok()?;
    let c = s.split("Column: ").nth(1)?;
    let col: usize = c
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    Some((line, col))
}

/// Byte offset of a 1-based (line, column in chars).
fn offset_of(text: &str, line: usize, col: usize) -> Option<usize> {
    let mut start = 0;
    for _ in 1..line {
        start += text[start..].find('\n')? + 1;
    }
    let line_text = &text[start..];
    let off = line_text
        .char_indices()
        .nth(col.saturating_sub(1))
        .map_or(line_text.len(), |(i, _)| i);
    Some(start + off)
}

// ----------------------------------------------------------------- semantic

fn semantic(doc: &Document<'_>, si: usize, out: &mut Vec<Diag>) {
    let s = &doc.stmts[si];
    let p = &s.parsed;
    let d = doc.dialect();
    for (r, res) in p.refs.iter().zip(&s.refs) {
        match &res.status {
            Status::Unresolved {
                part,
                what,
                suggestions,
            } => {
                let id = &r.parts[*part];
                if id.range.is_empty() {
                    continue;
                }
                let (kind, sev) = match what {
                    What::Schema => ("schema", Severity::Error),
                    What::Table => ("table", Severity::Error),
                    What::Column => ("column", Severity::Error),
                    What::Qualifier => ("table or alias", Severity::Error),
                };
                let fixes = suggestions
                    .iter()
                    .map(|sug| CodeAction {
                        title: format!("Change to {sug}"),
                        edits: vec![edit(id.range.clone(), sug.clone())],
                        preferred: true,
                    })
                    .collect();
                let mut message = format!("Unknown {kind} '{}'", id.text);
                if let Some(first) = suggestions.first() {
                    message.push_str(&format!("; did you mean {first}?"));
                }
                out.push(Diag {
                    range: id.range.clone(),
                    severity: sev,
                    message,
                    fixes,
                });
            }
            Status::Ambiguous(owners) => {
                let id = r.parts.last().expect("non-empty");
                let fixes = owners
                    .iter()
                    .map(|o| {
                        fix(
                            format!("Qualify as {o}.{}", id.text),
                            vec![edit(id.range.start..id.range.start, format!("{o}."))],
                        )
                    })
                    .collect();
                out.push(Diag {
                    range: id.range.clone(),
                    severity: Severity::Error,
                    message: format!("Column '{}' is ambiguous ({})", id.text, owners.join(", ")),
                    fixes,
                });
            }
            _ => {}
        }
    }
    // `= NULL`
    for (op, null, negated) in &p.null_comparisons {
        let replacement = if *negated { "IS NOT NULL" } else { "IS NULL" };
        out.push(Diag {
            range: op.start..null.end,
            severity: Severity::Warning,
            message: format!("Comparison with NULL is never true; use {replacement}"),
            fixes: vec![fix(
                format!("Replace with {replacement}"),
                vec![edit(op.start..null.end, replacement)],
            )],
        });
    }
    // UPDATE / DELETE without WHERE.
    if matches!(p.kind, StmtKind::Update | StmtKind::Delete) && !p.has_where {
        let first = s
            .toks
            .first()
            .map_or(s.span.body.clone(), |t| t.range.clone());
        let what = if p.kind == StmtKind::Update {
            "UPDATE"
        } else {
            "DELETE"
        };
        out.push(Diag {
            range: first,
            severity: Severity::Warning,
            message: format!("{what} without WHERE affects every row of the table"),
            fixes: vec![CodeAction {
                title: "Add WHERE clause".into(),
                edits: vec![edit(s.span.body.end..s.span.body.end, " WHERE ")],
                preferred: false,
            }],
        });
    }
    // Duplicate aliases / unused CTEs per scope.
    for (sc, scope) in p.scopes.iter().enumerate() {
        let mut seen: Vec<String> = Vec::new();
        for src in &scope.sources {
            if src.range.is_empty() {
                continue;
            }
            let Some(e) = src.exposed() else { continue };
            let n = e.name(d).to_lowercase();
            if seen.contains(&n) {
                out.push(Diag {
                    range: e.range.clone(),
                    severity: Severity::Error,
                    message: format!("Duplicate table name or alias '{}'", e.text),
                    fixes: Vec::new(),
                });
            }
            seen.push(n);
        }
        for (ci, cte) in scope.ctes.iter().enumerate() {
            let used = p.refs.iter().zip(&s.refs).any(|(r, res)| {
                matches!(r.role, Role::Table { .. } | Role::DdlTable)
                    && res.parts.first() == Some(&Some(Symbol::Cte { scope: sc, idx: ci }))
                    && !(cte.range.start <= r.range().start && r.range().end <= cte.range.end)
            });
            if !used {
                out.push(Diag {
                    range: cte.name.range.clone(),
                    severity: Severity::Warning,
                    message: format!("CTE '{}' is never used", cte.name.text),
                    fixes: remove_cte(doc, s, sc, ci).into_iter().collect(),
                });
            }
        }
    }
    // INSERT value counts.
    if let Some(ins) = &p.insert {
        let expected = if ins.columns.is_empty() {
            p.scopes[ins.scope]
                .sources
                .iter()
                .position(|x| x.target)
                .and_then(|i| doc.columns(si, ins.scope, i))
                .map(|c| c.len())
        } else {
            Some(ins.columns.len())
        };
        if let Some(n) = expected {
            for (count, range) in &ins.rows {
                if *count != n && *count != 0 {
                    out.push(Diag {
                        range: range.clone(),
                        severity: Severity::Error,
                        message: format!("Expected {n} values, found {count}"),
                        fixes: Vec::new(),
                    });
                }
            }
        }
    }
}

/// Edit removing CTE `ci` of `scope` with its comma (or the whole `WITH`).
fn remove_cte(doc: &Document<'_>, s: &StmtAnalysis, scope: usize, ci: usize) -> Option<CodeAction> {
    let ctes = &s.parsed.scopes[scope].ctes;
    let cte = &ctes[ci];
    let toks = &s.toks;
    let after = |pos: usize| toks.iter().position(|t| t.range.start >= pos);
    let range = if ctes.len() == 1 {
        // `WITH [RECURSIVE] x AS (…)` up to the main query.
        let with = toks
            .iter()
            .rposition(|t| t.range.end <= cte.name.range.start && t.is_word(doc.text, "with"))?;
        let next = after(cte.range.end)?;
        toks[with].range.start..toks[next].range.start
    } else if ci + 1 < ctes.len() {
        cte.range.start..ctes[ci + 1].range.start
    } else {
        // Last one: from the comma before it.
        let comma = toks
            .iter()
            .rposition(|t| t.range.end <= cte.range.start && t.text(doc.text) == ",")?;
        toks[comma].range.start..cte.range.end
    };
    Some(fix(
        format!("Remove unused CTE '{}'", cte.name.text),
        vec![edit(range, "")],
    ))
}

/// Quick fixes and refactorings at `range`.
pub fn code_actions(doc: &Document<'_>, range: Range<usize>) -> Vec<CodeAction> {
    let mut out: Vec<CodeAction> = diagnostics(doc)
        .into_iter()
        .filter(|d| d.range.start <= range.end && range.start <= d.range.end)
        .flat_map(|d| d.fixes)
        .collect();
    out.extend(expand_star(doc, range.start));
    out.extend(qualify_column(doc, range.start));
    out
}

/// "Expand *": the column list of the sources.
fn expand_star(doc: &Document<'_>, offset: usize) -> Option<CodeAction> {
    let si = doc.statement_at(offset)?;
    let s = &doc.stmts[si];
    let d = doc.dialect();
    for (sc, scope) in s.parsed.scopes.iter().enumerate() {
        for o in &scope.outputs {
            let Some(star) = &o.star else { continue };
            if !(o.range.start <= offset && offset <= o.range.end) {
                continue;
            }
            let several = scope.sources.len() > 1;
            let mut cols = Vec::new();
            for (i, src) in scope.sources.iter().enumerate() {
                let exposed = src.exposed().map(|e| e.text.clone());
                if let Some(q) = star
                    && !exposed
                        .as_deref()
                        .is_some_and(|e| d.is_same(&d.fold(e), &q.name(d)))
                {
                    continue;
                }
                for c in doc.columns(si, sc, i)? {
                    let name = d.quote_if_needed(&c.name);
                    cols.push(match (&exposed, several || star.is_some()) {
                        (Some(e), true) => format!("{e}.{name}"),
                        _ => name,
                    });
                }
            }
            if cols.is_empty() {
                return None;
            }
            return Some(CodeAction {
                title: "Expand column list".into(),
                edits: vec![edit(o.range.clone(), cols.join(", "))],
                preferred: false,
            });
        }
    }
    None
}

/// "Qualify column": prefix an unqualified column with its source.
fn qualify_column(doc: &Document<'_>, offset: usize) -> Option<CodeAction> {
    let (si, ri, _) = doc.ref_at(offset)?;
    let s = &doc.stmts[si];
    let r = &s.parsed.refs[ri];
    if r.parts.len() != 1 || !matches!(r.role, Role::Column | Role::SortColumn) {
        return None;
    }
    let Some(Symbol::Column { table, .. }) = s.refs[ri].parts[0].clone() else {
        return None;
    };
    let d = doc.dialect();
    for (sc, _) in s.parsed.chain(r.scope) {
        for src in &s.parsed.scopes[sc].sources {
            let matches = match &src.kind {
                SourceKind::Table(p) => p.last().is_some_and(|t| d.is_same(&t.name(d), &table)),
                _ => false,
            };
            if matches && let Some(e) = src.exposed() {
                let id = &r.parts[0];
                return Some(CodeAction {
                    title: format!("Qualify as {}.{}", e.text, id.text),
                    edits: vec![edit(id.range.start..id.range.start, format!("{}.", e.text))],
                    preferred: false,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::{resolve::tests::shop, schema::SchemaModel};

    fn diags(sql: &str, dialect: &str, m: &SchemaModel) -> Vec<(String, String)> {
        let doc = Document::analyze(sql, dialect, m, None);
        diagnostics(&doc)
            .into_iter()
            .map(|d| (sql[d.range].to_owned(), d.message))
            .collect()
    }

    fn apply(sql: &str, a: &CodeAction) -> String {
        let mut s = sql.to_owned();
        let mut edits = a.edits.clone();
        edits.sort_by_key(|e| std::cmp::Reverse(e.range.start));
        for e in edits {
            s.replace_range(e.range, &e.new_text);
        }
        s
    }

    #[test]
    fn valid_statements_are_clean() {
        let m = shop("public");
        for sql in [
            "select o.id, c.name from orders o join customers c on c.id = o.customer_id where o.total > 10 order by 2",
            "with t as (select customer_id, sum(total) s from orders group by customer_id) select * from t where s > 0",
            "insert into orders (customer_id, total) values (1, 2.5) returning id",
            "update orders set status = 'paid' where id = 1",
            "delete from order_items where order_id in (select id from orders where status is null)",
            "select count(*) filter (where total > 0) over (partition by customer_id order by created_at) from orders",
            "create table tmp (id int primary key, x text); select id, x from tmp",
            "select * from orders o where exists (select 1 from order_items i where i.order_id = o.id)",
            "select date_trunc('day', created_at)::date d, count(*) from orders group by d order by d desc nulls last",
        ] {
            assert_eq!(diags(sql, "postgresql", &m), vec![], "{sql}");
        }
        let m = shop("shop");
        for sql in [
            "select `o`.`id` from orders o limit 10, 5",
            "insert into orders set customer_id = 1, total = 2",
            "insert into orders (id, total) values (1, 2) on duplicate key update total = values(total)",
        ] {
            assert_eq!(diags(sql, "mysql", &m), vec![], "{sql}");
        }
        let m = shop("main");
        assert_eq!(diags("select rowid, * from orders", "sqlite", &m), vec![]);
    }

    #[test]
    fn errors_and_inspections() {
        let m = shop("public");
        let d = diags(
            "select totl from orders where status = null",
            "postgresql",
            &m,
        );
        assert!(
            d.contains(&(
                "totl".into(),
                "Unknown column 'totl'; did you mean total?".into()
            )),
            "{d:?}"
        );
        assert!(d.iter().any(|x| x.0 == "= null"));
        let d = diags("delete from orders", "postgresql", &m);
        assert!(d[0].1.contains("without WHERE"));
        let d = diags("select * from orders o, customers o", "postgresql", &m);
        assert!(d.iter().any(|x| x.1.contains("Duplicate")));
        let d = diags(
            "insert into orders (id, total) values (1)",
            "postgresql",
            &m,
        );
        assert!(
            d.iter().any(|x| x.1 == "Expected 2 values, found 1"),
            "{d:?}"
        );
        let d = diags("with unused as (select 1) select 2", "postgresql", &m);
        assert!(d.iter().any(|x| x.1.contains("never used")));
        let d = diags("select 'abc from orders", "postgresql", &m);
        assert!(d.iter().any(|x| x.1 == "Unterminated string literal"));
        let d = diags("select (1 from orders", "postgresql", &m);
        assert!(d.iter().any(|x| x.1 == "Unclosed '('"));
        let d = diags("selec 1", "postgresql", &m);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].0, "selec");
        let d = diags(
            "select id from orders o join customers c on true",
            "postgresql",
            &m,
        );
        assert!(d.iter().any(|x| x.1.contains("ambiguous")));
    }

    #[test]
    fn quick_fixes() {
        let m = shop("public");
        let sql = "select totl from orders where status = null";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        let a = code_actions(&doc, 8..8);
        assert_eq!(
            apply(sql, &a[0]),
            "select total from orders where status = null"
        );
        let a = code_actions(&doc, 38..38);
        assert_eq!(
            apply(sql, &a[0]),
            "select totl from orders where status IS NULL"
        );
        let sql = "with a as (select 1), b as (select 2) select * from a";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        let a = code_actions(&doc, 23..23);
        assert_eq!(apply(sql, &a[0]), "with a as (select 1) select * from a");
        let sql = "with b as (select 2) select 1";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        assert_eq!(apply(sql, &code_actions(&doc, 5..5)[0]), "select 1");
        let sql = "select * from orders o join customers c on c.id = o.customer_id";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        let a = code_actions(&doc, 7..7);
        assert!(
            apply(sql, &a[0]).starts_with("select o.id, o.customer_id"),
            "{a:?}"
        );
        let sql = "select id from orders o join customers c on true";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        assert_eq!(
            apply(sql, &code_actions(&doc, 7..7)[0]),
            "select o.id from orders o join customers c on true"
        );
    }
}
