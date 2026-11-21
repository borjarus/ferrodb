//! Language services with schema knowledge.
//!
//! SQL: the engine of `ferrodb_lang` gets a schema model built from the
//! source's cached metadata catalog (rebuilt only when the catalog is
//! replaced) and the user's live templates (`templates.toml`).
//! MQL: completion suggests the collections of the cached catalog and the
//! fields of sampled collections (a collection is sampled in the
//! background the first time its fields are needed).

use std::{collections::HashMap, ops::Range, path::Path, sync::Arc};

use ferrodb_api::{
    BoxFuture, CodeAction, CompletionItem, DataSourceId, Declaration, Diagnostic, DocSnapshot,
    FormatStyle, Hover, LanguageService, LiveTemplate, Parameter, QueryLanguage, Result, Risk,
    SampleOptions, SignatureHelp, StatementSpan, StructureItem, TemplateExpansion, TextEdit,
};
use ferrodb_lang::{
    Engine,
    mql::complete::Schema,
    service::complete_mql,
    sql::{schema::SchemaModel, templates},
};
use ferrodb_meta::{model::Catalog, sql_model::sql_model};

use crate::Inner;

/// Per source: the catalog a model was built from, and the model.
pub type ModelCache = HashMap<DataSourceId, (Arc<Catalog>, Arc<SchemaModel>)>;

/// User templates from `templates.toml`; a broken file is logged and ignored.
pub fn load_templates(path: &Path) -> Vec<(String, LiveTemplate)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match templates::parse_user(&text) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "ignoring templates.toml");
            Vec::new()
        }
    }
}

struct CatalogSchema<'a> {
    core: &'a Inner,
    ds: DataSourceId,
    /// (database, collection), the connection's database first.
    collections: Vec<(String, String)>,
}

impl Schema for CatalogSchema<'_> {
    fn collections(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for (_, c) in &self.collections {
            if !v.contains(c) {
                v.push(c.clone());
            }
        }
        v
    }

    fn fields(&self, collection: &str) -> Vec<String> {
        let Some((db, _)) = self.collections.iter().find(|(_, c)| c == collection) else {
            return Vec::new();
        };
        let key = (self.ds.clone(), db.clone(), collection.to_owned());
        let cached = self
            .core
            .fields
            .lock()
            .ok()
            .and_then(|m| m.get(&key).cloned());
        match cached {
            Some(f) => f,
            None => {
                // Mark as requested, sample in the background.
                if let Ok(mut m) = self.core.fields.lock() {
                    m.insert(key, Vec::new());
                }
                let me = self.core.arc();
                let (ds, db, coll) = (self.ds.clone(), db.clone(), collection.to_owned());
                self.core.handle().spawn(async move {
                    let size = SampleOptions::default().sample_size;
                    if let Err(e) = me.sample(&ds, &db, &coll, size).await {
                        tracing::debug!(error = %e, "sampling for completion failed");
                    }
                });
                Vec::new()
            }
        }
    }
}

impl Inner {
    fn mql_schema(&self, ds: &DataSourceId) -> CatalogSchema<'_> {
        let collections = self
            .catalogs
            .peek(ds)
            .map(|cat| {
                cat.loaded()
                    .flat_map(|(c, content)| {
                        content
                            .collections
                            .iter()
                            .map(|x| (c.name.clone(), x.name.clone()))
                            .collect::<Vec<_>>()
                    })
                    .collect()
            })
            .unwrap_or_default();
        CatalogSchema {
            core: self,
            ds: ds.clone(),
            collections,
        }
    }

    /// Schema model of a source (empty without a cached catalog).
    pub(crate) fn sql_model(&self, ds: Option<&DataSourceId>) -> Arc<SchemaModel> {
        let Some(ds) = ds else {
            return Arc::new(SchemaModel::empty());
        };
        let Some(cat) = self.catalogs.peek(ds) else {
            return Arc::new(SchemaModel::empty());
        };
        let mut cache = self.sql_models.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((c, m)) = cache.get(ds)
            && Arc::ptr_eq(c, &cat)
        {
            return m.clone();
        }
        let model = Arc::new(sql_model(&cat));
        cache.insert(ds.clone(), (cat, model.clone()));
        model
    }

    /// Runs `f` with an engine for the document's source.
    fn with_engine<T>(&self, doc: &DocSnapshot, f: impl FnOnce(&Engine<'_>) -> T) -> T {
        let model = self.sql_model(doc.data_source.as_ref());
        let engine = Engine::new(&model, &self.templates);
        f(&engine)
    }
}

impl LanguageService for Inner {
    fn complete(&self, doc: DocSnapshot, offset: usize) -> BoxFuture<'_, Vec<CompletionItem>> {
        let items = match (&doc.language, &doc.data_source) {
            (QueryLanguage::Mql, Some(ds)) => complete_mql(&doc.text, offset, &self.mql_schema(ds)),
            _ => self.with_engine(&doc, |e| e.complete(&doc, offset)),
        };
        Box::pin(async move { items })
    }

    fn diagnostics(&self, doc: DocSnapshot) -> BoxFuture<'_, Vec<Diagnostic>> {
        let d = self.with_engine(&doc, |e| e.diagnostics(&doc));
        Box::pin(async move { d })
    }

    fn statements(&self, doc: &DocSnapshot) -> Vec<StatementSpan> {
        self.language.statements(doc)
    }

    fn format(&self, text: &str, lang: &QueryLanguage, style: &FormatStyle) -> Result<String> {
        self.language.format(text, lang, style)
    }

    fn parameters(&self, text: &str, lang: &QueryLanguage) -> Vec<Parameter> {
        self.language.parameters(text, lang)
    }

    fn risks(&self, text: &str, lang: &QueryLanguage, all_modifications: bool) -> Vec<Risk> {
        match lang {
            QueryLanguage::RedisCommands => crate::kv::redis_risks(text, all_modifications),
            QueryLanguage::Cql => ferrodb_lang::sql::risks(text, "cql", all_modifications),
            _ => self.language.risks(text, lang, all_modifications),
        }
    }

    fn code_actions(&self, doc: DocSnapshot, range: Range<usize>) -> Vec<CodeAction> {
        self.with_engine(&doc, |e| e.code_actions(&doc, range))
    }

    fn signature_help(&self, doc: DocSnapshot, offset: usize) -> Option<SignatureHelp> {
        self.with_engine(&doc, |e| e.signature_help(&doc, offset))
    }

    fn hover(&self, doc: DocSnapshot, offset: usize) -> Option<Hover> {
        self.with_engine(&doc, |e| e.hover(&doc, offset))
    }

    fn structure(&self, doc: &DocSnapshot) -> Vec<StructureItem> {
        self.with_engine(doc, |e| e.structure(doc))
    }

    fn declaration(&self, doc: DocSnapshot, offset: usize) -> Option<Declaration> {
        self.with_engine(&doc, |e| e.declaration(&doc, offset))
    }

    fn prepare_rename(&self, doc: DocSnapshot, offset: usize) -> Option<(Range<usize>, String)> {
        self.with_engine(&doc, |e| e.prepare_rename(&doc, offset))
    }

    fn rename(&self, doc: DocSnapshot, offset: usize, new_name: &str) -> Result<Vec<TextEdit>> {
        self.with_engine(&doc, |e| e.rename(&doc, offset, new_name))
    }

    fn templates(&self, lang: &QueryLanguage) -> Vec<LiveTemplate> {
        templates::for_language(lang, &self.templates)
    }

    fn expand_template(
        &self,
        doc: DocSnapshot,
        offset: usize,
        selection: Option<Range<usize>>,
    ) -> Option<TemplateExpansion> {
        self.with_engine(&doc, |e| e.expand_template(&doc, offset, selection))
    }
}
