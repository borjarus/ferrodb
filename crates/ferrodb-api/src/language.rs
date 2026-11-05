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

pub trait LanguageService: Send + Sync {
    fn complete(&self, doc: DocSnapshot, offset: usize) -> BoxFuture<'_, Vec<CompletionItem>>;
    fn diagnostics(&self, doc: DocSnapshot) -> BoxFuture<'_, Vec<Diagnostic>>;
    fn statements(&self, doc: &DocSnapshot) -> Vec<StatementSpan>;
    fn format(&self, text: &str, lang: &QueryLanguage, style: &FormatStyle) -> Result<String>;
}
