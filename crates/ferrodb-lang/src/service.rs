//! [`LanguageService`] implementations.
//!
//! [`Engine`] serves every language feature for one schema model and the
//! user's live templates; the backend builds it per request from its
//! metadata catalog. [`BasicLanguageService`] is the engine without schema
//! knowledge (keywords, functions, syntax, formatting, local refactorings).
//! MQL keeps its own modules (phase 4b); CQL, SQL++, PartiQL and Redis
//! commands have theirs (phase 8), CQL on top of the SQL engine.

use std::{ops::Range, sync::OnceLock};

use ferrodb_api::{
    BoxFuture, CodeAction, CompletionItem, Declaration, Diagnostic, DocSnapshot, Error,
    FormatStyle, Hover, LanguageService, LiveTemplate, Parameter, QueryLanguage, Result, Risk,
    SignatureHelp, StatementSpan, StructureItem, TemplateExpansion, TextEdit,
};

use crate::{
    cql, mql, partiql, redis,
    sql::{
        self, Flavor, complete, document::Document, format, inspect, refactor, schema::SchemaModel,
        templates,
    },
    sqlpp,
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

fn empty_nosql() -> &'static NoSqlSchema {
    static EMPTY: OnceLock<NoSqlSchema> = OnceLock::new();
    EMPTY.get_or_init(NoSqlSchema::default)
}

/// Schema knowledge of the document and key-value languages; CQL uses the
/// SQL [`SchemaModel`] (keyspaces are schemas). The backend fills the part
/// of the document's language.
#[derive(Clone, Debug, Default)]
pub struct NoSqlSchema {
    /// Couchbase buckets, scopes, collections and inferred fields.
    pub couchbase: sqlpp::Schema,
    /// DynamoDB tables with key schemas and inferred attributes.
    pub dynamodb: partiql::Schema,
    /// Redis key names known to the explorer.
    pub redis_keys: Vec<String>,
}

fn diag(range: Range<usize>, severity: ferrodb_api::Severity, message: String) -> Diagnostic {
    Diagnostic {
        range,
        severity,
        message,
    }
}

/// All language features for one schema model.
pub struct Engine<'a> {
    pub model: &'a SchemaModel,
    /// User templates: (language key, template), see [`templates::parse_user`].
    pub user_templates: &'a [(String, LiveTemplate)],
    /// SQL++, PartiQL and Redis schema knowledge (empty by default).
    pub nosql: &'a NoSqlSchema,
}

impl<'a> Engine<'a> {
    pub fn new(model: &'a SchemaModel, user_templates: &'a [(String, LiveTemplate)]) -> Self {
        Self {
            model,
            user_templates,
            nosql: empty_nosql(),
        }
    }

    /// The engine with schema knowledge for SQL++, PartiQL and Redis.
    pub fn with_nosql(mut self, nosql: &'a NoSqlSchema) -> Self {
        self.nosql = nosql;
        self
    }

    /// The structural analysis of the SQL engine: SQL dialects and CQL with
    /// the schema model; SQL++ and PartiQL with their dialect only (hover,
    /// parameter info and structure work on function names).
    fn sql<'d>(&self, doc: &'d DocSnapshot) -> Option<Document<'d>>
    where
        'a: 'd,
    {
        let (dialect, model) = match &doc.language {
            QueryLanguage::Sql(d) => (d.as_str(), self.model),
            QueryLanguage::Cql => (cql::DIALECT, self.model),
            QueryLanguage::SqlPlusPlus => (sqlpp::DIALECT, empty_model()),
            QueryLanguage::PartiQl => (partiql::DIALECT, empty_model()),
            QueryLanguage::Mql | QueryLanguage::RedisCommands => return None,
        };
        Some(Document::analyze(
            &doc.text,
            dialect,
            model,
            doc.schema.as_deref(),
        ))
    }

    /// SQL dialects and CQL: name resolution, rename, quick fixes.
    fn resolved<'d>(&self, doc: &'d DocSnapshot) -> Option<Document<'d>>
    where
        'a: 'd,
    {
        match doc.language {
            QueryLanguage::Sql(_) | QueryLanguage::Cql => self.sql(doc),
            _ => None,
        }
    }

    pub fn complete(&self, doc: &DocSnapshot, offset: usize) -> Vec<CompletionItem> {
        let templates = self.templates(&doc.language);
        match &doc.language {
            QueryLanguage::Mql => complete_mql(&doc.text, offset, &NoSchema),
            QueryLanguage::RedisCommands => {
                redis::complete(&doc.text, offset, &self.nosql.redis_keys)
            }
            QueryLanguage::SqlPlusPlus => sqlpp::complete(
                &doc.text,
                offset,
                &self.nosql.couchbase,
                doc.schema.as_deref(),
                &templates,
            ),
            QueryLanguage::PartiQl => {
                let model = self.nosql.dynamodb.to_model();
                partiql::complete(&doc.text, offset, &self.nosql.dynamodb, &model, &templates)
            }
            QueryLanguage::Cql => self
                .sql(doc)
                .map(|d| cql::complete(&d, offset, &templates))
                .unwrap_or_default(),
            QueryLanguage::Sql(_) => self
                .sql(doc)
                .map(|d| complete::complete(&d, offset, &templates))
                .unwrap_or_default(),
        }
    }

    pub fn diagnostics(&self, doc: &DocSnapshot) -> Vec<Diagnostic> {
        match &doc.language {
            QueryLanguage::Mql => mql::diagnostics(&doc.text)
                .into_iter()
                .map(|(range, message)| diag(range, ferrodb_api::Severity::Error, message))
                .collect(),
            QueryLanguage::RedisCommands => redis::diagnostics(&doc.text),
            QueryLanguage::SqlPlusPlus => {
                sqlpp::diagnostics(&doc.text, &self.nosql.couchbase, doc.schema.as_deref())
            }
            QueryLanguage::PartiQl => partiql::diagnostics(&doc.text, &self.nosql.dynamodb),
            QueryLanguage::Cql => self
                .sql(doc)
                .map(|d| cql::diagnostics(&d))
                .unwrap_or_default(),
            QueryLanguage::Sql(_) => self
                .sql(doc)
                .map(|d| {
                    inspect::diagnostics(&d)
                        .into_iter()
                        .map(|x| diag(x.range, x.severity, x.message))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub fn code_actions(&self, doc: &DocSnapshot, range: Range<usize>) -> Vec<CodeAction> {
        self.resolved(doc)
            .map(|d| inspect::code_actions(&d, range))
            .unwrap_or_default()
    }

    pub fn signature_help(&self, doc: &DocSnapshot, offset: usize) -> Option<SignatureHelp> {
        if doc.language == QueryLanguage::RedisCommands {
            return redis::signature_help(&doc.text, offset);
        }
        refactor::signature_help(&self.sql(doc)?, offset)
    }

    pub fn hover(&self, doc: &DocSnapshot, offset: usize) -> Option<Hover> {
        if doc.language == QueryLanguage::RedisCommands {
            return redis::hover(&doc.text, offset);
        }
        refactor::hover(&self.sql(doc)?, offset)
    }

    pub fn structure(&self, doc: &DocSnapshot) -> Vec<StructureItem> {
        self.sql(doc)
            .map(|d| refactor::structure(&d))
            .unwrap_or_default()
    }

    pub fn declaration(&self, doc: &DocSnapshot, offset: usize) -> Option<Declaration> {
        refactor::declaration(&self.resolved(doc)?, offset)
    }

    pub fn prepare_rename(
        &self,
        doc: &DocSnapshot,
        offset: usize,
    ) -> Option<(Range<usize>, String)> {
        refactor::prepare_rename(&self.resolved(doc)?, offset)
    }

    pub fn rename(
        &self,
        doc: &DocSnapshot,
        offset: usize,
        new_name: &str,
    ) -> Result<Vec<TextEdit>> {
        match self.resolved(doc) {
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
    match doc.language {
        QueryLanguage::Mql => mql::split(&doc.text)
            .into_iter()
            .map(|s| StatementSpan { range: s.range })
            .collect(),
        QueryLanguage::RedisCommands => redis::split(&doc.text)
            .into_iter()
            .map(|range| StatementSpan { range })
            .collect(),
        _ => sql::split(&doc.text, &Flavor::for_dialect(dialect_of(&doc.language)))
            .into_iter()
            .map(|s| StatementSpan { range: s.range })
            .collect(),
    }
}

/// Formats SQL (every dialect), CQL, SQL++, PartiQL, MQL and Redis commands.
pub fn format_text(text: &str, lang: &QueryLanguage, style: &FormatStyle) -> Result<String> {
    match lang {
        QueryLanguage::Mql => Ok(mql::format(text)),
        QueryLanguage::RedisCommands => Ok(redis::format(text)),
        QueryLanguage::Sql(d) => Ok(format::format(text, d.as_str(), style)),
        QueryLanguage::Cql | QueryLanguage::SqlPlusPlus | QueryLanguage::PartiQl => {
            Ok(format::format(text, dialect_of(lang), style))
        }
    }
}

pub fn parameters(text: &str, lang: &QueryLanguage) -> Vec<Parameter> {
    // `:` and `?` mean other things in MQL, Redis patterns (`app:user:*`)…
    if matches!(lang, QueryLanguage::Mql | QueryLanguage::RedisCommands) {
        return Vec::new();
    }
    sql::parameters(text, &Flavor::for_dialect(dialect_of(lang)))
}

pub fn risks(text: &str, lang: &QueryLanguage, all_modifications: bool) -> Vec<Risk> {
    match lang {
        QueryLanguage::Mql => mql::risks(text, all_modifications),
        QueryLanguage::RedisCommands => redis::risks(text, all_modifications),
        QueryLanguage::Cql => cql::risks(text, all_modifications),
        QueryLanguage::SqlPlusPlus => sqlpp::risks(text, all_modifications),
        QueryLanguage::PartiQl => partiql::risks(text, all_modifications),
        QueryLanguage::Sql(_) => sql::risks(text, dialect_of(lang), all_modifications),
    }
}

/// What one statement does (read-only sources, transaction tracking) in
/// any language except MQL (classified by its command model).
pub fn classify(stmt: &str, lang: &QueryLanguage) -> sql::StatementClass {
    use sql::StatementClass as C;
    match lang {
        QueryLanguage::Cql => cql::classify(stmt),
        QueryLanguage::SqlPlusPlus => sqlpp::classify(stmt),
        QueryLanguage::PartiQl => partiql::classify(stmt),
        QueryLanguage::RedisCommands => {
            let name = redis::parse(stmt)
                .first()
                .and_then(redis::Line::command)
                .map(|c| c.name.as_str());
            match name {
                Some("SELECT" | "AUTH" | "HELLO" | "CLIENT SETNAME" | "PING" | "ECHO" | "QUIT") => {
                    C::Session
                }
                Some("MULTI") => C::Transaction(sql::TxCommand::Begin),
                Some("EXEC") => C::Transaction(sql::TxCommand::Commit),
                Some("DISCARD") => C::Transaction(sql::TxCommand::Rollback),
                _ if redis::is_write(stmt) => C::Modification,
                _ => C::Read,
            }
        }
        QueryLanguage::Sql(d) => sql::classify(stmt, d.as_str()),
        QueryLanguage::Mql => C::Modification,
    }
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
