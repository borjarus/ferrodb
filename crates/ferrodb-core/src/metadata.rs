//! Explorer listings, phase-3 level: each driver variant provides listing
//! queries per tree level (`FamilyVariant::list_queries`), run on a
//! dedicated explorer connection. Full introspection with a metadata cache
//! arrives in phase 4 (`ferrodb-meta`).

use std::sync::Arc;

use arrow_array::{Array, ArrayRef, BinaryArray, Int64Array, LargeStringArray, StringArray};
use ferrodb_api::{
    BoxFuture, ConnectionState, DataSourceId, DbObject, DdlOptions, Error, HierarchySpec,
    InferredSchema, MetadataService, NodeRef, ObjectKind, RefreshScope, Result, SampleOptions,
    SchemaSnapshot, SearchEntry, SearchTarget,
};
use ferrodb_drivers::{CancellationToken, Connection, sink::collect};
use tokio::sync::Mutex as AsyncMutex;

use crate::Inner;

type Shared = Arc<AsyncMutex<Box<dyn Connection>>>;

fn text(a: &ArrayRef, i: usize) -> Option<String> {
    if a.is_null(i) {
        return None;
    }
    let any = a.as_any();
    if let Some(s) = any.downcast_ref::<StringArray>() {
        return Some(s.value(i).to_owned());
    }
    if let Some(s) = any.downcast_ref::<LargeStringArray>() {
        return Some(s.value(i).to_owned());
    }
    if let Some(s) = any.downcast_ref::<Int64Array>() {
        return Some(s.value(i).to_string());
    }
    if let Some(s) = any.downcast_ref::<BinaryArray>() {
        return Some(String::from_utf8_lossy(s.value(i)).into_owned());
    }
    None
}

impl Inner {
    /// The explorer connection of `id`, opened on first use.
    pub(crate) async fn meta_connection(&self, id: &DataSourceId) -> Result<Shared> {
        let existing = self.state().meta.get(id).cloned();
        if let Some(c) = existing {
            if c.lock().await.is_alive() {
                return Ok(c);
            }
            self.state().meta.remove(id);
        }
        let conn = self.checkout(id).await?;
        let shared = Arc::new(AsyncMutex::new(conn));
        self.state().meta.insert(id.clone(), shared.clone());
        self.set_state(id, ConnectionState::Connected);
        Ok(shared)
    }

    async fn list(&self, node: &NodeRef) -> Result<Vec<DbObject>> {
        let cfg = self.source_config(&node.data_source)?;
        let variant = self.driver_for(&cfg)?.variant();
        let queries = variant.list_queries(&node.path);
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.meta_connection(&node.data_source).await?;
        let mut guard = conn.lock().await;
        let mut out = Vec::new();
        for q in queries {
            let res = collect(guard.as_mut(), q.sql.clone(), CancellationToken::new()).await?;
            for r in &res.results {
                for b in &r.batches {
                    for row in 0..b.num_rows() {
                        let Some(name) = text(b.column(0), row) else {
                            continue;
                        };
                        let detail = (b.num_columns() > 1)
                            .then(|| text(b.column(1), row))
                            .flatten()
                            .filter(|d| !d.is_empty());
                        out.push(DbObject {
                            node: node.child(q.kind, name.clone()),
                            kind: q.kind,
                            name,
                            detail,
                            has_children: q.has_children,
                        });
                    }
                }
            }
        }
        drop(guard);
        self.remember(&cfg.name, &out);
        Ok(out)
    }

    /// Tables and views seen in the explorer feed Search Everywhere.
    fn remember(&self, source: &str, objects: &[DbObject]) {
        let mut st = self.state();
        for o in objects {
            if !matches!(o.kind, ObjectKind::Table | ObjectKind::View) {
                continue;
            }
            let target = SearchTarget::Object(o.node.clone());
            if st.seen.iter().any(|e| e.target == target) {
                continue;
            }
            let parent: Vec<&str> = o.node.path[..o.node.path.len().saturating_sub(1)]
                .iter()
                .map(|(_, n)| n.as_str())
                .collect();
            st.seen.push(SearchEntry {
                label: o.name.clone(),
                location: std::iter::once(source)
                    .chain(parent)
                    .collect::<Vec<_>>()
                    .join(" / "),
                kind: Some(o.kind),
                target,
            });
        }
    }
}

impl MetadataService for Inner {
    fn hierarchy(&self, ds: &DataSourceId) -> Arc<HierarchySpec> {
        let spec = self
            .source_config(ds)
            .and_then(|c| self.driver_for(&c))
            .map(|d| d.variant().hierarchy())
            .unwrap_or(HierarchySpec {
                levels: Vec::new(),
                key_separator: None,
            });
        Arc::new(spec)
    }

    fn children(&self, node: NodeRef) -> BoxFuture<'_, Result<Vec<DbObject>>> {
        let me = self.arc();
        self.spawn(async move {
            match me.list(&node).await {
                Err(Error::ConnectionLost) => {
                    // The explorer connection died: reconnect once.
                    me.state().meta.remove(&node.data_source);
                    me.list(&node).await
                }
                other => other,
            }
        })
    }

    fn snapshot(&self, _ds: &DataSourceId) -> Option<Arc<SchemaSnapshot>> {
        None
    }

    fn refresh(&self, _ds: &DataSourceId, _scope: RefreshScope) -> BoxFuture<'_, Result<()>> {
        // Nothing is cached yet: every expansion queries the server.
        Box::pin(async { Ok(()) })
    }

    fn infer_schema(
        &self,
        _container: NodeRef,
        _sample: SampleOptions,
    ) -> BoxFuture<'_, Result<InferredSchema>> {
        Box::pin(async {
            Err(Error::Unsupported(
                "schema inference (document databases)".into(),
            ))
        })
    }

    fn definition(
        &self,
        _objects: Vec<NodeRef>,
        _opts: DdlOptions,
    ) -> BoxFuture<'_, Result<String>> {
        Box::pin(async { Err(Error::Unsupported("object definitions (phase 4)".into())) })
    }
}
