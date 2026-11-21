//! Statement analysis: what a statement does (read-only mode, transaction
//! tracking) and whether it is dangerous (confirmation before running).
//!
//! The `sqlparser` AST is used where it parses; a keyword scan over the
//! permissive lexer covers dialect extensions it does not understand, so
//! analysis never fails.

use ferrodb_api::{Risk, RiskKind};
use sqlparser::{
    ast::{AlterTableOperation, Statement as Ast},
    dialect::{Dialect, GenericDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect},
    parser::Parser,
};

use super::{
    lexer::{Flavor, Token, TokenKind, tokenize},
    split::split,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxCommand {
    Begin,
    Commit,
    Rollback,
    /// `SAVEPOINT`, `RELEASE`, `ROLLBACK TO`: do not end the transaction.
    Savepoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementClass {
    /// Reads data (`SELECT`, `SHOW`, `EXPLAIN`…).
    Read,
    /// Changes session state only (`SET search_path`, `USE db`).
    Session,
    Transaction(TxCommand),
    /// Changes data, schema, permissions or anything else.
    Modification,
}

impl StatementClass {
    /// Allowed on a read-only data source.
    pub fn allowed_read_only(self) -> bool {
        !matches!(self, Self::Modification)
    }
}

fn sqlparser_dialect(dialect: &str) -> Box<dyn Dialect> {
    match dialect {
        "postgresql" | "greenplum" | "cockroachdb" => Box::new(PostgreSqlDialect {}),
        "redshift" => Box::new(sqlparser::dialect::RedshiftSqlDialect {}),
        "mysql" | "mariadb" => Box::new(MySqlDialect {}),
        "sqlite" => Box::new(SQLiteDialect {}),
        "duckdb" => Box::new(sqlparser::dialect::DuckDbDialect {}),
        "tsql" | "sybase" | "sqlserver" | "azuresql" => {
            Box::new(sqlparser::dialect::MsSqlDialect {})
        }
        "snowflake" => Box::new(sqlparser::dialect::SnowflakeDialect {}),
        "googlesql" | "bigquery" => Box::new(sqlparser::dialect::BigQueryDialect {}),
        "clickhouse" => Box::new(sqlparser::dialect::ClickHouseDialect {}),
        "hiveql" => Box::new(sqlparser::dialect::HiveDialect {}),
        _ => Box::new(GenericDialect {}),
    }
}

fn words<'a>(src: &'a str, toks: &'a [Token]) -> impl Iterator<Item = &'a str> + 'a {
    toks.iter()
        .filter(|t| t.kind == TokenKind::Word)
        .map(move |t| t.text(src))
}

fn has_word(src: &str, toks: &[Token], w: &str) -> bool {
    words(src, toks).any(|x| x.eq_ignore_ascii_case(w))
}

/// `WHERE` at parenthesis depth 0.
fn has_top_level_where(src: &str, toks: &[Token]) -> bool {
    let mut depth = 0i32;
    for t in toks {
        match (t.kind, t.text(src)) {
            (TokenKind::Punct, "(") => depth += 1,
            (TokenKind::Punct, ")") => depth -= 1,
            (TokenKind::Word, w) if depth == 0 && w.eq_ignore_ascii_case("where") => return true,
            _ => {}
        }
    }
    false
}

/// Classifies one statement (no terminator).
pub fn classify(stmt: &str, dialect: &str) -> StatementClass {
    let flavor = Flavor::for_dialect(dialect);
    let toks: Vec<Token> = tokenize(stmt, &flavor)
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect();
    let mut ws = words(stmt, &toks).map(|w| w.to_ascii_lowercase());
    let Some(first) = ws.next() else {
        // `(SELECT …)` or empty.
        return if toks.first().is_some_and(|t| t.text(stmt) == "(") {
            StatementClass::Read
        } else {
            StatementClass::Session
        };
    };
    let second = ws.next().unwrap_or_default();
    let modifying_words = ["insert", "update", "delete", "merge", "upsert", "replace"];
    let embeds_modification =
        || words(stmt, &toks).any(|w| modifying_words.iter().any(|m| w.eq_ignore_ascii_case(m)));
    match first.as_str() {
        "select" | "values" | "table" | "show" | "describe" | "desc" | "help" => {
            // `SELECT … INTO new_table` creates a table (PostgreSQL, T-SQL).
            if first == "select" && has_word(stmt, &toks, "into") && dialect != "mysql" {
                StatementClass::Modification
            } else {
                StatementClass::Read
            }
        }
        "with" => {
            if embeds_modification() {
                StatementClass::Modification
            } else {
                StatementClass::Read
            }
        }
        "explain" => {
            if has_word(stmt, &toks, "analyze") && embeds_modification() {
                StatementClass::Modification
            } else {
                StatementClass::Read
            }
        }
        "pragma" => {
            if toks.iter().any(|t| t.text(stmt) == "=") {
                StatementClass::Modification
            } else {
                StatementClass::Read
            }
        }
        "set" | "reset" => {
            let lowered = stmt.to_ascii_lowercase();
            if lowered.contains("read_only")
                || lowered.contains("read write")
                || lowered.contains("read_write")
                || lowered.contains("characteristics")
                || second == "global"
                || second == "persist"
            {
                StatementClass::Modification
            } else {
                StatementClass::Session
            }
        }
        "use" | "discard" => StatementClass::Session,
        "begin" | "start" => StatementClass::Transaction(TxCommand::Begin),
        "commit" | "end" => StatementClass::Transaction(TxCommand::Commit),
        "rollback" | "abort" => {
            if has_word(stmt, &toks, "to") {
                StatementClass::Transaction(TxCommand::Savepoint)
            } else {
                StatementClass::Transaction(TxCommand::Rollback)
            }
        }
        "savepoint" | "release" => StatementClass::Transaction(TxCommand::Savepoint),
        _ => StatementClass::Modification,
    }
}

fn ast_risk(ast: &Ast) -> Option<RiskKind> {
    match ast {
        Ast::Delete(d) if d.selection.is_none() => Some(RiskKind::DeleteAll),
        Ast::Update(u) if u.selection.is_none() => Some(RiskKind::UpdateAll),
        Ast::Truncate(_) => Some(RiskKind::Truncate),
        Ast::Drop { .. }
        | Ast::DropFunction(_)
        | Ast::DropDomain(_)
        | Ast::DropProcedure { .. }
        | Ast::DropTrigger(_)
        | Ast::DropPolicy(_)
        | Ast::DropExtension(_)
        | Ast::DropSecret { .. } => Some(RiskKind::Drop),
        Ast::AlterTable(a)
            if a.operations.iter().any(|op| {
                matches!(
                    op,
                    AlterTableOperation::DropColumn { .. }
                        | AlterTableOperation::DropConstraint { .. }
                        | AlterTableOperation::DropPrimaryKey { .. }
                        | AlterTableOperation::DropForeignKey { .. }
                        | AlterTableOperation::DropIndex { .. }
                        | AlterTableOperation::DropPartitions { .. }
                )
            }) =>
        {
            Some(RiskKind::Drop)
        }
        _ => None,
    }
}

/// Keyword fallback for statements `sqlparser` does not parse.
fn lexical_risk(stmt: &str, flavor: &Flavor) -> Option<RiskKind> {
    let toks: Vec<Token> = tokenize(stmt, flavor)
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect();
    let first = words(stmt, &toks).next()?.to_ascii_lowercase();
    match first.as_str() {
        "delete" if !has_top_level_where(stmt, &toks) => Some(RiskKind::DeleteAll),
        "update" if !has_top_level_where(stmt, &toks) => Some(RiskKind::UpdateAll),
        "drop" => Some(RiskKind::Drop),
        "truncate" => Some(RiskKind::Truncate),
        "alter" if has_word(stmt, &toks, "drop") => Some(RiskKind::Drop),
        _ => None,
    }
}

/// Risk of a single statement.
pub fn statement_risk(stmt: &str, dialect: &str, all_modifications: bool) -> Option<RiskKind> {
    let flavor = Flavor::for_dialect(dialect);
    let risk = match Parser::parse_sql(sqlparser_dialect(dialect).as_ref(), stmt) {
        Ok(asts) if asts.len() == 1 => ast_risk(&asts[0]),
        _ => lexical_risk(stmt, &flavor),
    };
    risk.or_else(|| {
        (all_modifications && classify(stmt, dialect) == StatementClass::Modification)
            .then_some(RiskKind::Modification)
    })
}

/// Dangerous statements of a script.
///
/// T-SQL batches and PL/SQL blocks hold several statements in one (often
/// without `;` in T-SQL): every `UPDATE` / `DELETE` inside is checked on
/// its own.
pub fn risks(src: &str, dialect: &str, all_modifications: bool) -> Vec<Risk> {
    let flavor = Flavor::for_dialect(dialect);
    let mut out = Vec::new();
    for s in split(src, &flavor) {
        let text = s.text(src);
        let risk = statement_risk(text, dialect, all_modifications);
        let nested = if flavor.go_batches || flavor.plsql {
            embedded_risks(text, &flavor)
        } else {
            Vec::new()
        };
        // The statement itself is the UPDATE/DELETE: the nested check has it.
        let covered = nested.iter().any(|(r, _)| r.start == 0)
            || (risk == Some(RiskKind::Modification) && !nested.is_empty());
        if let Some(kind) = risk
            && !covered
        {
            out.push(Risk {
                kind,
                range: s.body.clone(),
            });
        }
        out.extend(nested.into_iter().map(|(r, kind)| Risk {
            kind,
            range: s.body.start + r.start..s.body.start + r.end,
        }));
    }
    out
}

/// Words that start another statement inside a batch or a block.
const BOUNDARY: &[&str] = &[
    "select",
    "insert",
    "update",
    "delete",
    "merge",
    "declare",
    "exec",
    "execute",
    "if",
    "while",
    "begin",
    "end",
    "return",
    "print",
    "raiserror",
    "throw",
    "commit",
    "rollback",
    "create",
    "alter",
    "drop",
    "truncate",
    "open",
    "fetch",
    "close",
    "deallocate",
    "waitfor",
    "else",
    "elsif",
    "exception",
    "loop",
    "go",
    "use",
    "grant",
    "revoke",
];

/// Words before `UPDATE` / `DELETE` that make them part of something else
/// (`AFTER UPDATE`, `FOR UPDATE`, `ON DELETE CASCADE`, `INSTEAD OF DELETE`…).
const NOT_A_STATEMENT_AFTER: &[&str] = &[
    "after", "before", "of", "or", "for", "on", "instead", "key", "grant", "revoke",
];

/// `UPDATE` / `DELETE` statements without a top-level `WHERE` inside `stmt`:
/// (range in `stmt`, kind).
fn embedded_risks(stmt: &str, flavor: &Flavor) -> Vec<(std::ops::Range<usize>, RiskKind)> {
    let toks: Vec<Token> = tokenize(stmt, flavor)
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect();
    let is = |i: usize, w: &str| toks.get(i).is_some_and(|t| t.is_word(stmt, w));
    let mut out = Vec::new();
    for i in 0..toks.len() {
        let kind = if is(i, "delete") {
            RiskKind::DeleteAll
        } else if is(i, "update") {
            RiskKind::UpdateAll
        } else {
            continue;
        };
        // `IF UPDATE(col)` in triggers, `GRANT UPDATE ON`…
        if toks.get(i + 1).is_some_and(|t| t.text(stmt) == "(") || is(i + 1, "on") {
            continue;
        }
        if i > 0 {
            let p = &toks[i - 1];
            if p.text(stmt) == ","
                || NOT_A_STATEMENT_AFTER.iter().any(|w| p.is_word(stmt, w))
                // `WHEN MATCHED THEN UPDATE SET …` of MERGE.
                || (p.is_word(stmt, "then")
                    && toks[i.saturating_sub(4)..i].iter().any(|t| t.is_word(stmt, "matched")))
            {
                continue;
            }
        }
        let mut depth = 0i32;
        let mut end = i + 1;
        let mut has_where = false;
        while end < toks.len() {
            let t = &toks[end];
            match t.text(stmt) {
                "(" => depth += 1,
                ")" => depth -= 1,
                ";" if depth <= 0 => break,
                _ => {}
            }
            if depth <= 0 && t.kind == TokenKind::Word {
                if t.is_word(stmt, "where") {
                    has_where = true;
                }
                // `UPDATE t SET …`: SET belongs to it; other statement words end it.
                if BOUNDARY.iter().any(|w| t.is_word(stmt, w)) {
                    break;
                }
            }
            end += 1;
        }
        if !has_where {
            let last = &toks[end - 1];
            out.push((toks[i].range.start..last.range.end, kind));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        use StatementClass::*;
        let pg = "postgresql";
        assert_eq!(classify("select 1", pg), Read);
        assert_eq!(classify("(select 1) union (select 2)", pg), Read);
        assert_eq!(classify("WITH x AS (select 1) select * from x", pg), Read);
        assert_eq!(
            classify("with d as (delete from t returning *) select * from d", pg),
            Modification
        );
        assert_eq!(classify("explain select 1", pg), Read);
        assert_eq!(classify("explain analyze delete from t", pg), Modification);
        assert_eq!(classify("select * into t2 from t", pg), Modification);
        assert_eq!(classify("set search_path to app", pg), Session);
        assert_eq!(
            classify("set default_transaction_read_only = off", pg),
            Modification
        );
        assert_eq!(
            classify("SET GLOBAL max_connections = 10", "mysql"),
            Modification
        );
        assert_eq!(classify("use shop", "mysql"), Session);
        assert_eq!(classify("begin", pg), Transaction(TxCommand::Begin));
        assert_eq!(
            classify("START TRANSACTION", "mysql"),
            Transaction(TxCommand::Begin)
        );
        assert_eq!(classify("commit", pg), Transaction(TxCommand::Commit));
        assert_eq!(classify("rollback", pg), Transaction(TxCommand::Rollback));
        assert_eq!(
            classify("rollback to sp1", pg),
            Transaction(TxCommand::Savepoint)
        );
        assert_eq!(classify("pragma table_info(t)", "sqlite"), Read);
        assert_eq!(classify("pragma foreign_keys = on", "sqlite"), Modification);
        assert_eq!(classify("insert into t values (1)", pg), Modification);
        assert_eq!(classify("vacuum", "sqlite"), Modification);
    }

    #[test]
    fn dangerous_statements() {
        let kinds = |src: &str, d: &str| -> Vec<RiskKind> {
            risks(src, d, false).into_iter().map(|r| r.kind).collect()
        };
        assert_eq!(
            kinds(
                "delete from t; delete from t where id = 1; update t set a = 1; update t set a = 1 where b in (select 1 where true)",
                "postgresql"
            ),
            vec![RiskKind::DeleteAll, RiskKind::UpdateAll]
        );
        assert_eq!(
            kinds(
                "drop table a; truncate b; alter table c drop column d; alter table c add column e int",
                "mysql"
            ),
            vec![RiskKind::Drop, RiskKind::Truncate, RiskKind::Drop]
        );
        // Lexical fallback on syntax sqlparser does not know.
        assert_eq!(
            kinds("delete from t returning * frobnicate (where)", "postgresql"),
            vec![RiskKind::DeleteAll]
        );
        // Production: every modification is reported.
        let all: Vec<_> = risks("select 1; insert into t values (1)", "sqlite", true)
            .into_iter()
            .map(|r| r.kind)
            .collect();
        assert_eq!(all, vec![RiskKind::Modification]);
    }

    #[test]
    fn risk_ranges_point_at_statements() {
        let src = "select 1;\ndrop table x;";
        let r = risks(src, "sqlite", false);
        assert_eq!(&src[r[0].range.clone()], "drop table x");
    }
}
