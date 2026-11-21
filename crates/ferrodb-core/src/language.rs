//! Language services with schema knowledge.
//!
//! SQL: the engine of `ferrodb_lang` gets a schema model built from the
//! source's cached metadata catalog (rebuilt only when the catalog is
//! replaced) and the user's live templates (`templates.toml`).
//! MQL: completion suggests the collections of the cached catalog and the
//! fields of sampled collections (a collection is sampled in the
//! background the first time its fields are needed).
//! CQL: keyspaces are schemas of the SQL model. SQL++, PartiQL and Redis
//! get a [`NoSqlSchema`] built from the catalog on each request: buckets,
//! scopes, collections and inferred fields (Couchbase), tables with key
//! schemas, indexes and inferred attributes (DynamoDB), key names (Redis).

use std::{collections::HashMap, ops::Range, path::Path, sync::Arc};

use ferrodb_api::{
    BoxFuture, CodeAction, CompletionItem, DataSourceId, Declaration, Diagnostic, DocSnapshot,
    FormatStyle, Hover, LanguageService, LiveTemplate, Parameter, QueryLanguage, Result, Risk,
    SampleOptions, SignatureHelp, StatementSpan, StructureItem, TemplateExpansion, TextEdit,
};
use ferrodb_lang::{
    Engine,
    mql::complete::Schema,
    partiql,
    service::{NoSqlSchema, complete_mql},
    sql::{schema::SchemaModel, templates},
    sqlpp,
};
use ferrodb_meta::{
    model::{Catalog, Table},
    sql_model::sql_model,
};

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

    /// Fields known for a document collection: catalog columns, then the
    /// sampled fields cache.
    fn doc_fields(
        &self,
        ds: &DataSourceId,
        container: &str,
        name: &str,
        table: Option<&Table>,
    ) -> Vec<String> {
        let mut out: Vec<String> = table
            .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default();
        let key = (ds.clone(), container.to_owned(), name.to_owned());
        if let Some(sampled) = self.fields.lock().ok().and_then(|m| m.get(&key).cloned()) {
            for f in sampled {
                if !out.contains(&f) {
                    out.push(f);
                }
            }
        }
        out
    }

    /// Schema knowledge of SQL++, PartiQL and Redis consoles.
    pub(crate) fn nosql_schema(
        &self,
        ds: Option<&DataSourceId>,
        lang: &QueryLanguage,
    ) -> NoSqlSchema {
        let mut out = NoSqlSchema::default();
        let (Some(ds), true) = (
            ds,
            matches!(
                lang,
                QueryLanguage::SqlPlusPlus | QueryLanguage::PartiQl | QueryLanguage::RedisCommands
            ),
        ) else {
            return out;
        };
        let Some(cat) = self.catalogs.peek(ds) else {
            return out;
        };
        match lang {
            QueryLanguage::SqlPlusPlus => out.couchbase = couchbase_schema(self, ds, &cat),
            QueryLanguage::PartiQl => out.dynamodb = dynamodb_schema(self, ds, &cat),
            _ => out.redis_keys = redis_keys(&cat),
        }
        out
    }

    /// Runs `f` with an engine for the document's source.
    fn with_engine<T>(&self, doc: &DocSnapshot, f: impl FnOnce(&Engine<'_>) -> T) -> T {
        let model = self.sql_model(doc.data_source.as_ref());
        let nosql = self.nosql_schema(doc.data_source.as_ref(), &doc.language);
        let engine = Engine::new(&model, &self.templates).with_nosql(&nosql);
        f(&engine)
    }
}

/// Couchbase: containers are buckets (or `bucket.scope`); collections are
/// `scope.collection` names (or plain names in the `_default` scope / the
/// container's scope), listed as collections or as tables whose columns
/// are the inferred fields.
fn couchbase_schema(core: &Inner, ds: &DataSourceId, cat: &Catalog) -> sqlpp::Schema {
    let mut out = sqlpp::Schema {
        loaded: cat.loaded().next().is_some(),
        ..Default::default()
    };
    for (c, content) in cat.loaded() {
        let (bucket, scope) = match c.name.split_once('.') {
            Some((b, s)) => (b.to_owned(), Some(s.to_owned())),
            None => (c.name.clone(), None),
        };
        let names = content
            .collections
            .iter()
            .map(|x| (x.name.clone(), None))
            .chain(content.tables.iter().map(|t| (t.name.clone(), Some(t))));
        for (name, table) in names {
            let (sc, coll) = match (&scope, name.split_once('.')) {
                (_, Some((s, c))) => (s.to_owned(), c.to_owned()),
                (Some(s), None) => (s.clone(), name.clone()),
                (None, None) => ("_default".to_owned(), name.clone()),
            };
            if out
                .keyspaces
                .iter()
                .any(|k| k.bucket == bucket && k.scope == sc && k.collection == coll)
            {
                continue;
            }
            let fields = core.doc_fields(ds, &c.name, &name, table);
            out.keyspaces.push(sqlpp::Keyspace {
                bucket: bucket.clone(),
                scope: sc,
                collection: coll,
                fields,
            });
        }
    }
    out
}

/// DynamoDB: tables with the key schema as primary key (partition key,
/// then sort key), secondary indexes as indexes (their first columns are
/// the index keys), inferred attributes as columns.
fn dynamodb_schema(core: &Inner, ds: &DataSourceId, cat: &Catalog) -> partiql::Schema {
    let mut out = partiql::Schema {
        loaded: cat.loaded().next().is_some(),
        ..Default::default()
    };
    for (c, content) in cat.loaded() {
        for t in &content.tables {
            let key: &[String] = t.primary_key.as_ref().map_or(&[], |k| k.columns.as_slice());
            out.tables.push(partiql::KeyTable {
                name: t.name.clone(),
                partition_key: key.first().cloned(),
                sort_key: key.get(1).cloned(),
                indexes: t
                    .indexes
                    .iter()
                    .filter_map(|i| {
                        Some(partiql::KeyIndex {
                            name: i.name.clone(),
                            partition_key: i.columns.first()?.clone(),
                            sort_key: i.columns.get(1).cloned(),
                        })
                    })
                    .collect(),
                attributes: core.doc_fields(ds, &c.name, &t.name, Some(t)),
            });
        }
    }
    out
}

/// Redis: key names loaded in the explorer (tables or collections of the
/// database containers).
fn redis_keys(cat: &Catalog) -> Vec<String> {
    let mut out = Vec::new();
    for (_, content) in cat.loaded() {
        out.extend(content.tables.iter().map(|t| t.name.clone()));
        out.extend(content.collections.iter().map(|c| c.name.clone()));
    }
    out
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
