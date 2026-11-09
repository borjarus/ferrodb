//! [`LanguageService`] for the real backend, phase-3 level: statements,
//! parameters and risks for every SQL dialect; keyword completion only.

use ferrodb_api::{
    BoxFuture, CompletionItem, CompletionKind, Diagnostic, DocSnapshot, Error, FormatStyle,
    LanguageService, Parameter, QueryLanguage, Result, Risk, StatementSpan,
};

use crate::sql::{self, Flavor};

/// Dialect id used for analysis of a language (`generic` for non-SQL).
pub fn dialect_of(lang: &QueryLanguage) -> &str {
    match lang {
        QueryLanguage::Sql(d) => d.as_str(),
        QueryLanguage::Cql => "cql",
        QueryLanguage::SqlPlusPlus => "sqlpp",
        QueryLanguage::PartiQl => "partiql",
        QueryLanguage::Mql | QueryLanguage::RedisCommands => "generic",
    }
}

const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "JOIN",
    "LEFT JOIN",
    "INNER JOIN",
    "ON",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "EXISTS",
    "BETWEEN",
    "LIKE",
    "AS",
    "DISTINCT",
    "UNION",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "CREATE TABLE",
    "ALTER TABLE",
    "DROP TABLE",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "WITH",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct BasicLanguageService;

impl LanguageService for BasicLanguageService {
    fn complete(&self, doc: DocSnapshot, offset: usize) -> BoxFuture<'_, Vec<CompletionItem>> {
        Box::pin(async move {
            if !matches!(doc.language, QueryLanguage::Sql(_)) {
                return Vec::new();
            }
            let offset = offset.min(doc.text.len());
            let head = &doc.text[..offset];
            let start = head
                .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
                .map_or(0, |i| {
                    i + head[i..].chars().next().map_or(1, char::len_utf8)
                });
            let prefix = head[start..].to_ascii_uppercase();
            KEYWORDS
                .iter()
                .filter(|k| !prefix.is_empty() && k.starts_with(&prefix))
                .map(|k| CompletionItem {
                    label: (*k).to_owned(),
                    kind: CompletionKind::Keyword,
                    insert_text: (*k).to_owned(),
                    detail: None,
                    replace: start..offset,
                })
                .collect()
        })
    }

    fn diagnostics(&self, _doc: DocSnapshot) -> BoxFuture<'_, Vec<Diagnostic>> {
        Box::pin(async { Vec::new() })
    }

    fn statements(&self, doc: &DocSnapshot) -> Vec<StatementSpan> {
        sql::split(&doc.text, &Flavor::for_dialect(dialect_of(&doc.language)))
            .into_iter()
            .map(|s| StatementSpan { range: s.range })
            .collect()
    }

    fn format(&self, _text: &str, _lang: &QueryLanguage, _style: &FormatStyle) -> Result<String> {
        Err(Error::Unsupported("formatting (phase 5)".into()))
    }

    fn parameters(&self, text: &str, lang: &QueryLanguage) -> Vec<Parameter> {
        // `:` and `?` mean other things in MQL, Redis patterns (`app:user:*`)…
        if !matches!(lang, QueryLanguage::Sql(_)) {
            return Vec::new();
        }
        sql::parameters(text, &Flavor::for_dialect(dialect_of(lang)))
    }

    fn risks(&self, text: &str, lang: &QueryLanguage, all_modifications: bool) -> Vec<Risk> {
        // Statement analysis understands SQL; other languages get their own
        // analysers with their language modules (MQL in phase 4b…).
        if !matches!(lang, QueryLanguage::Sql(_)) {
            return Vec::new();
        }
        sql::risks(text, dialect_of(lang), all_modifications)
    }
}
