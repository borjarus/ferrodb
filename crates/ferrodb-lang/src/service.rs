//! [`LanguageService`] implementations.
//!
//! [`Engine`] serves every language feature for one schema model and the
//! user's live templates; the backend builds it per request from its
//! metadata catalog. [`BasicLanguageService`] is the engine without schema
//! knowledge (keywords, functions, syntax, formatting, local refactorings).
//! MQL keeps its own modules (phase 4b).

use std::{ops::Range, sync::OnceLock};

use ferrodb_api::{
    BoxFuture, CodeAction, CompletionItem, Declaration, Diagnostic, DocSnapshot, Error,
    FormatStyle, Hover, LanguageService, LiveTemplate, Parameter, QueryLanguage, Result, Risk,
    SignatureHelp, StatementSpan, StructureItem, TemplateExpansion, TextEdit,
};

use crate::{
    mql,
    sql::{
        self, Flavor, complete, document::Document, format, inspect, refactor, schema::SchemaModel,
        templates,
    },
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

fn empty_model() -> &'static SchemaModel {
    static EMPTY: OnceLock<SchemaModel> = OnceLock::new();
    EMPTY.get_or_init(SchemaModel::empty)
}

/// All language features for one schema model.
pub struct Engine<'a> {
    pub model: &'a SchemaModel,
    /// User templates: (language key, template), see [`templates::parse_user`].
    pub user_templates: &'a [(String, LiveTemplate)],
}

impl<'a> Engine<'a> {
    pub fn new(model: &'a SchemaModel, user_templates: &'a [(String, LiveTemplate)]) -> Self {
        Self {
            model,
            user_templates,
        }
    }

    fn sql<'d>(&self, doc: &'d DocSnapshot) -> Option<Document<'d>>
    where
        'a: 'd,
    {
        match &doc.language {
            QueryLanguage::Sql(d) => Some(Document::analyze(
                &doc.text,
                d.as_str(),
                self.model,
                doc.schema.as_deref(),
            )),
            _ => None,
        }
    }

    pub fn complete(&self, doc: &DocSnapshot, offset: usize) -> Vec<CompletionItem> {
        if doc.language == QueryLanguage::Mql {
            return complete_mql(&doc.text, offset, &NoSchema);
        }
        let Some(d) = self.sql(doc) else {
            return Vec::new();
        };
        complete::complete(&d, offset, &self.templates(&doc.language))
    }

    pub fn diagnostics(&self, doc: &DocSnapshot) -> Vec<Diagnostic> {
        if doc.language == QueryLanguage::Mql {
            return mql::diagnostics(&doc.text)
                .into_iter()
                .map(|(range, message)| Diagnostic {
                    range,
                    severity: ferrodb_api::Severity::Error,
                    message,
                })
                .collect();
        }
        let Some(d) = self.sql(doc) else {
            return Vec::new();
        };
        inspect::diagnostics(&d)
            .into_iter()
            .map(|x| Diagnostic {
                range: x.range,
                severity: x.severity,
                message: x.message,
            })
            .collect()
    }

    pub fn code_actions(&self, doc: &DocSnapshot, range: Range<usize>) -> Vec<CodeAction> {
        self.sql(doc)
            .map(|d| inspect::code_actions(&d, range))
            .unwrap_or_default()
    }

    pub fn signature_help(&self, doc: &DocSnapshot, offset: usize) -> Option<SignatureHelp> {
        refactor::signature_help(&self.sql(doc)?, offset)
    }

    pub fn hover(&self, doc: &DocSnapshot, offset: usize) -> Option<Hover> {
        refactor::hover(&self.sql(doc)?, offset)
    }

    pub fn structure(&self, doc: &DocSnapshot) -> Vec<StructureItem> {
        self.sql(doc)
            .map(|d| refactor::structure(&d))
            .unwrap_or_default()
    }

    pub fn declaration(&self, doc: &DocSnapshot, offset: usize) -> Option<Declaration> {
        refactor::declaration(&self.sql(doc)?, offset)
    }

    pub fn prepare_rename(
        &self,
        doc: &DocSnapshot,
        offset: usize,
    ) -> Option<(Range<usize>, String)> {
        refactor::prepare_rename(&self.sql(doc)?, offset)
    }

    pub fn rename(
        &self,
        doc: &DocSnapshot,
        offset: usize,
        new_name: &str,
    ) -> Result<Vec<TextEdit>> {
        match self.sql(doc) {
            Some(d) => refactor::rename(&d, offset, new_name),
            None => Err(Error::Unsupported("rename in this language".into())),
        }
    }

    pub fn templates(&self, lang: &QueryLanguage) -> Vec<LiveTemplate> {
        templates::for_language(lang, self.user_templates)
    }

    pub fn expand_template(
        &self,
        doc: &DocSnapshot,
        offset: usize,
        selection: Option<Range<usize>>,
    ) -> Option<TemplateExpansion> {
        templates::expand(&doc.text, offset, selection, &self.templates(&doc.language))
    }
}

/// Statement spans of a document.
pub fn statements(doc: &DocSnapshot) -> Vec<StatementSpan> {
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

/// Formats SQL (every dialect) and MQL.
pub fn format_text(text: &str, lang: &QueryLanguage, style: &FormatStyle) -> Result<String> {
    match lang {
        QueryLanguage::Mql => Ok(mql::format(text)),
        QueryLanguage::Sql(d) => Ok(format::format(text, d.as_str(), style)),
        _ => Err(Error::Unsupported(format!(
            "formatting {}",
            lang.display_name()
        ))),
    }
}

pub fn parameters(text: &str, lang: &QueryLanguage) -> Vec<Parameter> {
    // `:` and `?` mean other things in MQL, Redis patterns (`app:user:*`)…
    if !matches!(lang, QueryLanguage::Sql(_)) {
        return Vec::new();
    }
    sql::parameters(text, &Flavor::for_dialect(dialect_of(lang)))
}

pub fn risks(text: &str, lang: &QueryLanguage, all_modifications: bool) -> Vec<Risk> {
    if *lang == QueryLanguage::Mql {
        return mql::risks(text, all_modifications);
    }
    if !matches!(lang, QueryLanguage::Sql(_)) {
        return Vec::new();
    }
    sql::risks(text, dialect_of(lang), all_modifications)
}

/// Language service without schema knowledge.
#[derive(Clone, Copy, Debug, Default)]
pub struct BasicLanguageService;

impl BasicLanguageService {
    fn engine(&self) -> Engine<'static> {
        Engine::new(empty_model(), &[])
    }
}

impl LanguageService for BasicLanguageService {
    fn complete(&self, doc: DocSnapshot, offset: usize) -> BoxFuture<'_, Vec<CompletionItem>> {
        let items = self.engine().complete(&doc, offset);
        Box::pin(async move { items })
    }

    fn diagnostics(&self, doc: DocSnapshot) -> BoxFuture<'_, Vec<Diagnostic>> {
        let d = self.engine().diagnostics(&doc);
        Box::pin(async move { d })
    }

    fn statements(&self, doc: &DocSnapshot) -> Vec<StatementSpan> {
        statements(doc)
    }

    fn format(&self, text: &str, lang: &QueryLanguage, style: &FormatStyle) -> Result<String> {
        format_text(text, lang, style)
    }

    fn parameters(&self, text: &str, lang: &QueryLanguage) -> Vec<Parameter> {
        parameters(text, lang)
    }

    fn risks(&self, text: &str, lang: &QueryLanguage, all_modifications: bool) -> Vec<Risk> {
        risks(text, lang, all_modifications)
    }

    fn code_actions(&self, doc: DocSnapshot, range: Range<usize>) -> Vec<CodeAction> {
        self.engine().code_actions(&doc, range)
    }

    fn signature_help(&self, doc: DocSnapshot, offset: usize) -> Option<SignatureHelp> {
        self.engine().signature_help(&doc, offset)
    }

    fn hover(&self, doc: DocSnapshot, offset: usize) -> Option<Hover> {
        self.engine().hover(&doc, offset)
    }

    fn structure(&self, doc: &DocSnapshot) -> Vec<StructureItem> {
        self.engine().structure(doc)
    }

    fn declaration(&self, doc: DocSnapshot, offset: usize) -> Option<Declaration> {
        self.engine().declaration(&doc, offset)
    }

    fn prepare_rename(&self, doc: DocSnapshot, offset: usize) -> Option<(Range<usize>, String)> {
        self.engine().prepare_rename(&doc, offset)
    }

    fn rename(&self, doc: DocSnapshot, offset: usize, new_name: &str) -> Result<Vec<TextEdit>> {
        self.engine().rename(&doc, offset, new_name)
    }

    fn templates(&self, lang: &QueryLanguage) -> Vec<LiveTemplate> {
        self.engine().templates(lang)
    }

    fn expand_template(
        &self,
        doc: DocSnapshot,
        offset: usize,
        selection: Option<Range<usize>>,
    ) -> Option<TemplateExpansion> {
        self.engine().expand_template(&doc, offset, selection)
    }
}
