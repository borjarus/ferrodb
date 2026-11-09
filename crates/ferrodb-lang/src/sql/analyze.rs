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
pub fn risks(src: &str, dialect: &str, all_modifications: bool) -> Vec<Risk> {
    split(src, &Flavor::for_dialect(dialect))
        .into_iter()
        .filter_map(|s| {
            statement_risk(s.text(src), dialect, all_modifications).map(|kind| Risk {
                kind,
                range: s.body,
            })
        })
        .collect()
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
