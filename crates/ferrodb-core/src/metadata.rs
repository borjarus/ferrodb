//! The live Database Explorer (plan, phase 4 and 4b) on `ferrodb-meta`:
//!
//! - children come from the source's catalog; a container (schema,
//!   database) is introspected on first expansion and kept in memory and
//!   on disk, so the next start shows the tree without connecting;
//! - `refresh` re-reads the root, one container or one table / collection
//!   (incremental refresh) and keeps what was loaded before;
//! - the `schema.filter` property hides containers in the tree and search;
//! - definitions, scripts, quick documentation and name resolution read the
//!   same catalog; MongoDB collections get schema inference by sampling.
//!
//! Sources without an introspector (none of the built-in ones) fall back to
//! the phase-3 listing queries of their driver variant.

use std::{collections::HashMap, sync::Arc};

use bson::{Bson, Document, doc};
use ferrodb_api::{
    BoxFuture, ConnectionState, DataSourceConfig, DataSourceId, DbObject, DdlOptions, Error,
    HierarchySpec, InferredSchema, MetadataService, NodeRef, ObjectDoc, ObjectKind, QueryLanguage,
    RefreshScope, Result, SampleOptions, SchemaSnapshot, ScriptKind, SearchEntry,
};
use ferrodb_drivers::{CancellationToken, Connection, sink::collect};
use ferrodb_lang::mql::{Command, Op};
use ferrodb_meta::{
    Catalog, Container, Content, Dialect,
    ddl::{self, Object, Target},
    infer, introspector, merge, mongo, now,
    tree::{self, Filter, Found},
};
use tokio::sync::Mutex as AsyncMutex;

use crate::Inner;

type Shared = Arc<AsyncMutex<Box<dyn Connection>>>;

/// Dialect of a source's language, when an introspector exists for it.
pub(crate) fn dialect(lang: &QueryLanguage) -> Option<Dialect> {
    match lang {
        QueryLanguage::Sql(d) => Dialect::from_id(d.as_str()),
        QueryLanguage::Mql => Some(Dialect::Mongo),
        _ => None,
    }
}

pub(crate) fn filter_of(cfg: &DataSourceConfig) -> Filter {
    cfg.properties
        .iter()
        .find(|(k, _)| k == "schema.filter")
        .map(|(_, v)| Filter::parse(v))
        .unwrap_or_default()
}

/// Field paths of sampled collections: (source, database, collection).
pub(crate) type FieldCache = HashMap<(DataSourceId, String, String), Vec<String>>;

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

    pub(crate) fn source_dialect(
        &self,
        ds: &DataSourceId,
    ) -> Result<(DataSourceConfig, Option<Dialect>)> {
        let cfg = self.source_config(ds)?;
        let d = self
            .driver_for(&cfg)
            .ok()
            .and_then(|d| dialect(&d.capabilities().language));
        Ok((cfg, d))
    }

    /// The cached catalog (memory or disk), or an empty one.
    fn catalog(&self, ds: &DataSourceId) -> Arc<Catalog> {
        self.catalogs
            .get(ds)
            .unwrap_or_else(|| Arc::new(Catalog::new()))
    }

    async fn store(&self, ds: &DataSourceId, catalog: Catalog) -> Arc<Catalog> {
        let catalogs = self.catalogs.clone();
        let ds = ds.clone();
        self.handle()
            .spawn_blocking(move || catalogs.put(&ds, catalog))
            .await
            .unwrap_or_else(|_| Arc::new(Catalog::new()))
    }

    /// Runs `f` on the explorer connection; reconnects once when it died.
    async fn with_meta<T, F>(&self, ds: &DataSourceId, f: F) -> Result<T>
    where
        F: for<'c> Fn(&'c mut dyn Connection) -> BoxFuture<'c, Result<T>>,
    {
        for attempt in 0..2 {
            let conn = self.meta_connection(ds).await?;
            let mut guard = conn.lock().await;
            match f(guard.as_mut()).await {
                Err(Error::ConnectionLost) if attempt == 0 => {
                    drop(guard);
                    self.state().meta.remove(ds);
                }
                other => return other,
            }
        }
        Err(Error::ConnectionLost)
    }

    /// Containers, listed on first use.
    async fn ensure_containers(&self, ds: &DataSourceId, d: Dialect) -> Result<Arc<Catalog>> {
        let cat = self.catalog(ds);
        if cat.containers.is_some() {
            return Ok(cat);
        }
        let list = self
            .with_meta(ds, |c| introspector(d).containers(c))
            .await?;
        // Another expansion may have stored content meanwhile: keep it.
        let mut cat = (*self.catalog(ds)).clone();
        let mut list = list;
        if let Some(old) = &cat.containers {
            for c in &mut list {
                c.content = old
                    .iter()
                    .find(|o| o.name == c.name)
                    .and_then(|o| o.content.clone());
            }
        }
        cat.containers = Some(list);
        cat.refreshed_at = now();
        Ok(self.store(ds, cat).await)
    }

    /// Introspects one container (or one object of it) and stores it.
    async fn load_container(
        &self,
        ds: &DataSourceId,
        d: Dialect,
        name: &str,
        only: Option<&str>,
    ) -> Result<Arc<Catalog>> {
        let n = name.to_owned();
        let o = only.map(str::to_owned);
        let mut fresh = self
            .with_meta(ds, move |c| {
                let (n, o) = (n.clone(), o.clone());
                Box::pin(async move { introspector(d).content(c, &n, o.as_deref()).await })
            })
            .await?;
        fresh.refreshed_at = now();
        let mut cat = (*self.ensure_containers(ds, d).await?).clone();
        let list = cat.containers.get_or_insert_with(Vec::new);
        let entry = match list.iter_mut().position(|c| c.name == name) {
            Some(i) => &mut list[i],
            None => {
                list.push(Container {
                    name: name.to_owned(),
                    comment: None,
                    content: None,
                });
                list.last_mut().expect("just pushed")
            }
        };
        match (&mut entry.content, only) {
            (Some(existing), Some(_)) => merge(existing, fresh, only),
            (slot, _) if only.is_none() => *slot = Some(fresh),
            // One object of a container never loaded: load all of it.
            (slot, _) => {
                let n = name.to_owned();
                let mut all = self
                    .with_meta(ds, move |c| {
                        let n = n.clone();
                        Box::pin(async move { introspector(d).content(c, &n, None).await })
                    })
                    .await?;
                all.refreshed_at = now();
                *slot = Some(all);
            }
        }
        Ok(self.store(ds, cat).await)
    }

    /// The catalog with `container` loaded.
    async fn ensure_content(
        &self,
        ds: &DataSourceId,
        d: Dialect,
        container: &str,
    ) -> Result<Arc<Catalog>> {
        let cat = self.ensure_containers(ds, d).await?;
        if cat
            .container(container)
            .is_some_and(|c| c.content.is_some())
        {
            return Ok(cat);
        }
        self.load_container(ds, d, container, None).await
    }

    async fn children_of(&self, node: &NodeRef) -> Result<Vec<DbObject>> {
        let (cfg, d) = self.source_dialect(&node.data_source)?;
        let Some(d) = d else {
            return self.list(node).await;
        };
        let ds = &node.data_source;
        let Some(((_, container), rest)) = node.path.split_first() else {
            let cat = self.ensure_containers(ds, d).await?;
            return Ok(tree::containers(
                ds,
                d,
                cat.containers.as_deref().unwrap_or_default(),
                &filter_of(&cfg),
            ));
        };
        let cat = self.ensure_content(ds, d, container).await?;
        let Some(c) = cat.container(container) else {
            return Ok(Vec::new());
        };
        let content = c.content.as_ref().expect("ensured above");
        let cnode = NodeRef {
            data_source: ds.clone(),
            path: node.path[..1].to_vec(),
        };
        if rest.is_empty() {
            return Ok(tree::container_children(&cnode, content));
        }
        Ok(match tree::find(c, content, rest) {
            Some(Found::Object(Object::Table(t))) => tree::table_children(node, t),
            Some(Found::Collection(col)) => tree::collection_children(node, col),
            _ => Vec::new(),
        })
    }

    /// Phase-3 listing queries (sources without an introspector).
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
            for row in ferrodb_meta::rows::rows(&res.results) {
                let Some(name) = row.text("name") else {
                    continue;
                };
                let detail = row.text("type").or_else(|| row.text("detail"));
                out.push(DbObject {
                    node: node.child(q.kind, name.clone()),
                    kind: q.kind,
                    name,
                    detail,
                    has_children: q.has_children,
                });
            }
        }
        Ok(out)
    }

    async fn refresh_scope(&self, ds: &DataSourceId, scope: RefreshScope) -> Result<()> {
        let (_, d) = self.source_dialect(ds)?;
        let Some(d) = d else { return Ok(()) };
        let path = match &scope {
            RefreshScope::All => Vec::new(),
            RefreshScope::Node(n) => n.path.clone(),
        };
        match path.as_slice() {
            [] => {
                let loaded: Vec<String> = self
                    .catalog(ds)
                    .loaded()
                    .map(|(c, _)| c.name.clone())
                    .collect();
                let mut cat = (*self.catalog(ds)).clone();
                cat.containers = None;
                self.store(ds, cat).await;
                let cat = self.ensure_containers(ds, d).await?;
                for name in loaded {
                    if cat.container(&name).is_some() {
                        self.load_container(ds, d, &name, None).await?;
                    }
                }
            }
            [(_, c)] => {
                self.load_container(ds, d, c, None).await?;
            }
            [
                (_, c),
                (
                    ObjectKind::Table
                    | ObjectKind::View
                    | ObjectKind::MaterializedView
                    | ObjectKind::Collection,
                    name,
                ),
                ..,
            ] => {
                self.load_container(ds, d, c, Some(name)).await?;
            }
            [(_, c), ..] => {
                self.load_container(ds, d, c, None).await?;
            }
        }
        Ok(())
    }

    /// Looks up a node; `f` gets the dialect, container name and what was found.
    async fn with_found<T>(
        &self,
        node: &NodeRef,
        f: impl FnOnce(Dialect, &str, Found<'_>) -> Result<T>,
    ) -> Result<T> {
        let (_, d) = self.source_dialect(&node.data_source)?;
        let d = d.ok_or_else(|| Error::Unsupported("metadata of this data source".into()))?;
        let Some(((_, container), rest)) = node.path.split_first() else {
            return Err(Error::Unsupported("the data source itself".into()));
        };
        let cat = self.ensure_content(&node.data_source, d, container).await?;
        let c = cat
            .container(container)
            .ok_or_else(|| Error::NotFound(container.clone()))?;
        let content = c.content.as_ref().expect("ensured above");
        let found = tree::find(c, content, rest)
            .ok_or_else(|| Error::NotFound(node.name().unwrap_or_default().to_owned()))?;
        f(d, container, found)
    }

    async fn definitions(&self, objects: Vec<NodeRef>, opts: DdlOptions) -> Result<String> {
        let mut parts = Vec::new();
        // Tables of one container together, in dependency order.
        let mut groups: Vec<(NodeRef, Vec<NodeRef>)> = Vec::new();
        for o in objects {
            let table = o.path.len() == 2 && matches!(o.path[1].0, ObjectKind::Table);
            if table {
                let key = NodeRef {
                    data_source: o.data_source.clone(),
                    path: o.path[..1].to_vec(),
                };
                match groups.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, v)) => v.push(o),
                    None => groups.push((key, vec![o])),
                }
            } else {
                groups.push((o.clone(), vec![o]));
            }
        }
        for (key, nodes) in groups {
            if nodes.len() > 1 {
                let (_, d) = self.source_dialect(&key.data_source)?;
                let d = d.ok_or_else(|| Error::Unsupported("DDL of this data source".into()))?;
                let container = key.path[0].1.clone();
                let cat = self.ensure_content(&key.data_source, d, &container).await?;
                let content = cat
                    .container(&container)
                    .and_then(|c| c.content.as_ref())
                    .ok_or_else(|| Error::NotFound(container.clone()))?;
                let tables: Vec<&ferrodb_meta::Table> = nodes
                    .iter()
                    .filter_map(|n| content.table(&n.path[1].1))
                    .collect();
                let t = Target {
                    opts: opts.clone(),
                    ..Target::new(d, &container)
                };
                parts.push(ddl::tables_ddl(&t, &tables));
                continue;
            }
            let node = &nodes[0];
            if node.path.len() == 1 {
                let (_, d) = self.source_dialect(&node.data_source)?;
                let d = d.ok_or_else(|| Error::Unsupported("DDL of this data source".into()))?;
                let container = node.path[0].1.clone();
                let cat = self
                    .ensure_content(&node.data_source, d, &container)
                    .await?;
                let content: Content = cat
                    .container(&container)
                    .and_then(|c| c.content.clone())
                    .unwrap_or_default();
                parts.push(if d == Dialect::Mongo {
                    format!("use {container}\n\n{}", mongo::database_script(&content))
                } else {
                    let t = Target {
                        opts: opts.clone(),
                        ..Target::new(d, &container)
                    };
                    ddl::container_ddl(&t, &content)
                });
                continue;
            }
            let opts = opts.clone();
            let s = self
                .with_found(node, move |d, container, found| {
                    let t = Target {
                        opts,
                        ..Target::new(d, container)
                    };
                    Ok(match found {
                        Found::Object(o) => ddl::object_ddl(&t, o),
                        Found::Collection(c) => mongo::collection_script(c),
                        Found::MongoIndex(c, i) => mongo::index_script(c, i),
                        Found::Container(_, content) => ddl::container_ddl(&t, content),
                    })
                })
                .await?;
            parts.push(s);
        }
        Ok(parts
            .iter()
            .map(|p| p.trim_end())
            .collect::<Vec<_>>()
            .join("\n\n")
            + "\n")
    }

    /// Field paths of a collection: sampled ones, else the validator's.
    pub(crate) fn known_fields(&self, ds: &DataSourceId, db: &str, coll: &str) -> Vec<String> {
        if let Some(f) = self.fields.lock().ok().and_then(|m| {
            m.get(&(ds.clone(), db.to_owned(), coll.to_owned()))
                .cloned()
        }) {
            return f;
        }
        Vec::new()
    }

    /// Samples a collection (`$sample`) and infers its schema.
    pub(crate) async fn sample(
        &self,
        ds: &DataSourceId,
        db: &str,
        coll: &str,
        size: usize,
    ) -> Result<InferredSchema> {
        let cmd = Command::on(
            db,
            Op::Aggregate {
                collection: Some(coll.to_owned()),
                pipeline: vec![doc! { "$sample": { "size": size as i64 } }],
                options: Document::new(),
            },
        );
        let docs = self
            .with_meta(ds, move |c| {
                let cmd = cmd.clone();
                Box::pin(async move { mongo::documents(c, cmd).await })
            })
            .await?;
        let validator = self
            .catalogs
            .peek(ds)
            .and_then(|cat| {
                cat.container(db)?
                    .content
                    .as_ref()?
                    .collections
                    .iter()
                    .find(|c| c.name == coll)?
                    .validator
                    .clone()
            })
            .and_then(|v| match ferrodb_lang::mql::value(&v) {
                Ok(Bson::Document(d)) => Some(d),
                _ => None,
            });
        let schema = infer::with_validator(validator.as_ref(), infer::infer(&docs));
        if let Ok(mut m) = self.fields.lock() {
            m.insert(
                (ds.clone(), db.to_owned(), coll.to_owned()),
                schema.fields.iter().map(|f| f.path.clone()).collect(),
            );
        }
        Ok(schema)
    }

    /// Search entries of every cached catalog.
    pub(crate) fn catalog_entries(&self) -> Vec<SearchEntry> {
        let sources: Vec<DataSourceConfig> = self
            .state()
            .sources
            .values()
            .map(|e| e.config.clone())
            .collect();
        let mut out = Vec::new();
        for cfg in sources {
            let Ok((_, Some(d))) = self.source_dialect(&cfg.id) else {
                continue;
            };
            if let Some(cat) = self.catalogs.peek(&cfg.id) {
                out.extend(tree::search_entries(
                    &cfg.id,
                    &cfg.name,
                    d,
                    &cat,
                    &filter_of(&cfg),
                ));
            }
        }
        out
    }
}

impl MetadataService for Inner {
    fn hierarchy(&self, ds: &DataSourceId) -> Arc<HierarchySpec> {
        let spec = match self.source_dialect(ds) {
            Ok((_, Some(d))) => tree::hierarchy(d),
            Ok((cfg, None)) => self
                .driver_for(&cfg)
                .map(|d| d.variant().hierarchy())
                .unwrap_or(HierarchySpec {
                    levels: Vec::new(),
                    key_separator: None,
                }),
            Err(_) => HierarchySpec {
                levels: Vec::new(),
                key_separator: None,
            },
        };
        Arc::new(spec)
    }

    fn children(&self, node: NodeRef) -> BoxFuture<'_, Result<Vec<DbObject>>> {
        let me = self.arc();
        self.spawn(async move { me.children_of(&node).await })
    }

    fn snapshot(&self, ds: &DataSourceId) -> Option<Arc<SchemaSnapshot>> {
        let (cfg, d) = self.source_dialect(ds).ok()?;
        let cat = self.catalogs.peek(ds)?;
        Some(Arc::new(SchemaSnapshot {
            objects: tree::snapshot(ds, d?, &cat, &filter_of(&cfg)),
        }))
    }

    fn refresh(&self, ds: &DataSourceId, scope: RefreshScope) -> BoxFuture<'_, Result<()>> {
        let me = self.arc();
        let ds = ds.clone();
        self.spawn(async move { me.refresh_scope(&ds, scope).await })
    }

    fn infer_schema(
        &self,
        container: NodeRef,
        sample: SampleOptions,
    ) -> BoxFuture<'_, Result<InferredSchema>> {
        let me = self.arc();
        self.spawn(async move {
            let ds = container.data_source.clone();
            let (db, coll) = match container.path.as_slice() {
                [(ObjectKind::Database, db), (_, coll), ..] => (db.clone(), coll.clone()),
                // A collection without its database: the connection's one.
                [(_, coll)] => {
                    let conn = me.meta_connection(&ds).await?;
                    let db = conn
                        .lock()
                        .await
                        .current_schema()
                        .await?
                        .unwrap_or_default();
                    (db, coll.clone())
                }
                _ => return Err(Error::Unsupported("schema inference of this object".into())),
            };
            me.sample(&ds, &db, &coll, sample.sample_size.max(1)).await
        })
    }

    fn definition(&self, objects: Vec<NodeRef>, opts: DdlOptions) -> BoxFuture<'_, Result<String>> {
        let me = self.arc();
        self.spawn(async move { me.definitions(objects, opts).await })
    }

    fn script(&self, object: NodeRef, kind: ScriptKind) -> BoxFuture<'_, Result<String>> {
        let me = self.arc();
        self.spawn(async move {
            let ds = object.data_source.clone();
            let db = object
                .path
                .first()
                .map(|(_, n)| n.clone())
                .unwrap_or_default();
            let fields = object
                .path
                .get(1)
                .map(|(_, c)| me.known_fields(&ds, &db, c))
                .unwrap_or_default();
            me.with_found(&object, move |d, container, found| {
                let t = Target::new(d, container);
                match found {
                    Found::Object(o) => ddl::script(&t, o, kind),
                    Found::Collection(c) => mongo::script(c, kind, &fields),
                    _ => None,
                }
                .ok_or_else(|| Error::Unsupported(format!("{kind:?} script for this object")))
            })
            .await
        })
    }

    fn resolve(&self, ds: &DataSourceId, parts: &[String]) -> Vec<DbObject> {
        let Ok((_, Some(d))) = self.source_dialect(ds) else {
            return Vec::new();
        };
        self.catalogs
            .peek(ds)
            .map(|cat| tree::resolve(ds, d, &cat, parts))
            .unwrap_or_default()
    }

    fn documentation(&self, object: NodeRef) -> BoxFuture<'_, Result<ObjectDoc>> {
        let me = self.arc();
        self.spawn(async move {
            me.with_found(&object, |d, container, found| {
                Ok(tree::documentation(d, container, found))
            })
            .await
        })
    }
}
