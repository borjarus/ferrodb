use std::{ops::Range, sync::Arc};

use crate::{BoxFuture, DataSourceId, QueryLanguage, Result};

/// Immutable view of a console document handed to language services.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocSnapshot {
    pub text: Arc<str>,
    pub language: QueryLanguage,
    /// Source whose metadata drives schema-aware features.
    pub data_source: Option<DataSourceId>,
    pub version: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Keyword,
    Function,
    Table,
    Column,
    Collection,
    Field,
    Method,
    Operator,
    Key,
    Snippet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    pub insert_text: String,
    pub detail: Option<String>,
    /// Range replaced on accept.
    pub replace: Range<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
}

/// One statement found by the splitter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementSpan {
    pub range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatStyle {
    pub uppercase_keywords: bool,
    pub indent: usize,
}

impl Default for FormatStyle {
    fn default() -> Self {
        Self {
            uppercase_keywords: true,
            indent: 4,
        }
    }
}

/// A placeholder in a script whose value is asked before execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    /// `name` for `:name`, `1` for `$1`, `?1`, `?2`… for positional `?`.
    pub name: String,
    pub range: Range<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RiskKind {
    /// `DELETE` without `WHERE`.
    DeleteAll,
    /// `UPDATE` without `WHERE`.
    UpdateAll,
    /// `DROP …` (table, schema, database, column…).
    Drop,
    Truncate,
    /// Any statement that changes data or schema (production sources).
    Modification,
}

/// A potentially dangerous statement, confirmed by the user before running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Risk {
    pub kind: RiskKind,
    /// Byte range of the statement in the analysed text.
    pub range: Range<usize>,
}

pub trait LanguageService: Send + Sync {
    fn complete(&self, doc: DocSnapshot, offset: usize) -> BoxFuture<'_, Vec<CompletionItem>>;
    fn diagnostics(&self, doc: DocSnapshot) -> BoxFuture<'_, Vec<Diagnostic>>;
    fn statements(&self, doc: &DocSnapshot) -> Vec<StatementSpan>;
    fn format(&self, text: &str, lang: &QueryLanguage, style: &FormatStyle) -> Result<String>;
    /// Placeholders whose values must be supplied in `ExecOptions::params`,
    /// in order of first appearance, one entry per distinct name.
    fn parameters(&self, text: &str, lang: &QueryLanguage) -> Vec<Parameter>;
    /// Dangerous statements in `text`. With `all_modifications`, every
    /// statement that changes data or schema is reported (production sources).
    fn risks(&self, text: &str, lang: &QueryLanguage, all_modifications: bool) -> Vec<Risk>;
}
