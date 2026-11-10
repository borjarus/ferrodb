//! Language services with schema knowledge: MQL completion suggests the
//! collections of the cached catalog and the fields of sampled collections
//! (a collection is sampled in the background the first time its fields
//! are needed). Everything else is `ferrodb_lang::BasicLanguageService`.

use ferrodb_api::{
    BoxFuture, CompletionItem, DataSourceId, Diagnostic, DocSnapshot, FormatStyle, LanguageService,
    Parameter, QueryLanguage, Result, Risk, SampleOptions, StatementSpan,
};
use ferrodb_lang::{mql::complete::Schema, service::complete_mql};

use crate::Inner;

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
}

impl LanguageService for Inner {
    fn complete(&self, doc: DocSnapshot, offset: usize) -> BoxFuture<'_, Vec<CompletionItem>> {
        match (&doc.language, &doc.data_source) {
            (QueryLanguage::Mql, Some(ds)) => {
                let items = complete_mql(&doc.text, offset, &self.mql_schema(ds));
                Box::pin(async move { items })
            }
            _ => self.language.complete(doc, offset),
        }
    }

    fn diagnostics(&self, doc: DocSnapshot) -> BoxFuture<'_, Vec<Diagnostic>> {
        self.language.diagnostics(doc)
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
        self.language.risks(text, lang, all_modifications)
    }
}
