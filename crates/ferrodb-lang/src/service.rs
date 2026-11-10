//! [`LanguageService`] for the real backend: statements, parameters and
//! risks for every SQL dialect (keyword completion only until phase 5);
//! MQL statements, risks, diagnostics, formatting and completion (phase 4b).

use ferrodb_api::{
    BoxFuture, CompletionItem, CompletionKind, Diagnostic, DocSnapshot, Error, FormatStyle,
    LanguageService, Parameter, QueryLanguage, Result, Risk, StatementSpan,
};

use crate::{
    mql,
    sql::{self, Flavor},
};

/// No schema knowledge (completion of methods and operators only).
pub struct NoSchema;

impl mql::complete::Schema for NoSchema {
    fn collections(&self) -> Vec<String> {
        Vec::new()
    }
    fn fields(&self, _collection: &str) -> Vec<String> {
        Vec::new()
    }
}

/// MQL completion items at `offset`.
pub fn complete_mql(
    text: &str,
    offset: usize,
    schema: &dyn mql::complete::Schema,
) -> Vec<CompletionItem> {
    let (replace, items) = mql::complete::complete(text, offset, schema);
    items
        .into_iter()
        .take(200)
        .map(|s| CompletionItem {
            insert_text: s.label.clone(),
            label: s.label,
            kind: s.kind,
            detail: s.detail.map(str::to_owned),
            replace: replace.clone(),
        })
        .collect()
}

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
            if doc.language == QueryLanguage::Mql {
                return complete_mql(&doc.text, offset, &NoSchema);
            }
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

    fn diagnostics(&self, doc: DocSnapshot) -> BoxFuture<'_, Vec<Diagnostic>> {
        Box::pin(async move {
            if doc.language != QueryLanguage::Mql {
                return Vec::new();
            }
            mql::diagnostics(&doc.text)
                .into_iter()
                .map(|(range, message)| Diagnostic {
                    range,
                    severity: ferrodb_api::Severity::Error,
                    message,
                })
                .collect()
        })
    }

    fn statements(&self, doc: &DocSnapshot) -> Vec<StatementSpan> {
        if doc.language == QueryLanguage::Mql {
            return mql::split(&doc.text)
                .into_iter()
                .map(|s| StatementSpan { range: s.range })
                .collect();
        }
        sql::split(&doc.text, &Flavor::for_dialect(dialect_of(&doc.language)))
            .into_iter()
            .map(|s| StatementSpan { range: s.range })
            .collect()
    }

    fn format(&self, text: &str, lang: &QueryLanguage, _style: &FormatStyle) -> Result<String> {
        match lang {
            QueryLanguage::Mql => Ok(mql::format(text)),
            _ => Err(Error::Unsupported("SQL formatting (phase 5)".into())),
        }
    }

    fn parameters(&self, text: &str, lang: &QueryLanguage) -> Vec<Parameter> {
        // `:` and `?` mean other things in MQL, Redis patterns (`app:user:*`)…
        if !matches!(lang, QueryLanguage::Sql(_)) {
            return Vec::new();
        }
        sql::parameters(text, &Flavor::for_dialect(dialect_of(lang)))
    }

    fn risks(&self, text: &str, lang: &QueryLanguage, all_modifications: bool) -> Vec<Risk> {
        if *lang == QueryLanguage::Mql {
            return mql::risks(text, all_modifications);
        }
        // Other languages get their analysers with their language modules.
        if !matches!(lang, QueryLanguage::Sql(_)) {
            return Vec::new();
        }
        sql::risks(text, dialect_of(lang), all_modifications)
    }
}
