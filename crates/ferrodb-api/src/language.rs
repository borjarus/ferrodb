use std::{ops::Range, sync::Arc};

use crate::{BoxFuture, DataSourceId, QueryLanguage, Result};

/// Immutable view of a console document handed to language services.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocSnapshot {
    pub text: Arc<str>,
    pub language: QueryLanguage,
    /// Source whose metadata drives schema-aware features.
    pub data_source: Option<DataSourceId>,
    /// Schema / database of unqualified names (the console's selection);
    /// `None` = the connection's default.
    pub schema: Option<String>,
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

/// A replacement of `range` (byte offsets in the analysed text) by `new_text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub new_text: String,
}

/// A quick fix or refactoring offered for a range ("Alt+Enter").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeAction {
    pub title: String,
    /// Edits in the analysed text; they never overlap.
    pub edits: Vec<TextEdit>,
    /// Fixes the diagnostic at the cursor (listed first).
    pub preferred: bool,
}

/// One overload shown by "Parameter info".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    /// `round(x numeric, digits int) -> numeric`
    pub label: String,
    /// Byte ranges of the parameters within `label`.
    pub parameters: Vec<Range<usize>>,
    pub documentation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureHelp {
    pub signatures: Vec<Signature>,
    /// Best matching overload for the number of arguments typed.
    pub active_signature: usize,
    /// Index of the argument under the cursor.
    pub active_parameter: usize,
}

/// Text shown on hover (Markdown).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub range: Range<usize>,
    pub contents: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructureKind {
    Statement,
    Query,
    Cte,
    Table,
    Column,
    Function,
}

/// A node of the structure view of a console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureItem {
    pub label: String,
    pub detail: Option<String>,
    pub kind: StructureKind,
    /// Whole range of the element.
    pub range: Range<usize>,
    /// Range selected when navigating to the element.
    pub selection: Range<usize>,
    pub children: Vec<StructureItem>,
}

/// Where the name under the cursor is declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declaration {
    /// In the console itself: an alias, a CTE, an output column name.
    Local(Range<usize>),
    /// A database object as qualified name parts (`["public", "orders", "id"]`),
    /// for [`MetadataService::resolve`](crate::MetadataService::resolve).
    Object(Vec<String>),
}

/// A live template (abbreviation expanded into code).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveTemplate {
    pub abbreviation: String,
    pub description: String,
    /// Text with `$NAME$` variables; `$END$` is the final cursor position,
    /// `$SELECTION$` the selected text when surrounding.
    pub body: String,
    /// Built-in or user-defined (`templates.toml`).
    pub builtin: bool,
}

/// An expanded live template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateExpansion {
    /// Range replaced (the abbreviation, or the selection when surrounding).
    pub replace: Range<usize>,
    pub text: String,
    /// Selection after the expansion, relative to the start of `text`: the
    /// first variable's placeholder, else the `$END$` position.
    pub selection: Range<usize>,
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

    /// Quick fixes and refactorings for `range` (usually the cursor).
    fn code_actions(&self, _doc: DocSnapshot, _range: Range<usize>) -> Vec<CodeAction> {
        Vec::new()
    }
    /// "Parameter info" of the function call around `offset`.
    fn signature_help(&self, _doc: DocSnapshot, _offset: usize) -> Option<SignatureHelp> {
        None
    }
    fn hover(&self, _doc: DocSnapshot, _offset: usize) -> Option<Hover> {
        None
    }
    /// Statements and their parts, for the structure view.
    fn structure(&self, _doc: &DocSnapshot) -> Vec<StructureItem> {
        Vec::new()
    }
    /// The declaration of the name under the cursor.
    fn declaration(&self, _doc: DocSnapshot, _offset: usize) -> Option<Declaration> {
        None
    }
    /// The range of the renameable name under the cursor and its current
    /// spelling; `None` = nothing to rename there.
    fn prepare_rename(&self, _doc: DocSnapshot, _offset: usize) -> Option<(Range<usize>, String)> {
        None
    }
    /// Edits renaming the name under the cursor everywhere it refers to the
    /// same thing. Database objects are renamed in the text only.
    fn rename(&self, _doc: DocSnapshot, _offset: usize, _new_name: &str) -> Result<Vec<TextEdit>> {
        Err(crate::Error::Unsupported("rename".into()))
    }
    /// Live templates available for a language.
    fn templates(&self, _lang: &QueryLanguage) -> Vec<LiveTemplate> {
        Vec::new()
    }
    /// Expands the template whose abbreviation ends at `offset`; with a
    /// selection, surrounds it (templates using `$SELECTION$`).
    fn expand_template(
        &self,
        _doc: DocSnapshot,
        _offset: usize,
        _selection: Option<Range<usize>>,
    ) -> Option<TemplateExpansion> {
        None
    }
}
