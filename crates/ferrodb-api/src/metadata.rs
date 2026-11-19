use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{BoxFuture, DataSourceId, Result};

/// Kinds of objects that can appear in the Database Explorer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectKind {
    // containers
    Database,
    Schema,
    Keyspace,
    Bucket,
    Scope,
    Project,
    Dataset,
    Folder,
    // tabular
    Table,
    View,
    MaterializedView,
    Column,
    Index,
    PrimaryKey,
    ForeignKey,
    Procedure,
    Function,
    Sequence,
    Trigger,
    // document
    Collection,
    Field,
    // key-value
    KeyPrefix,
    Key,
}

impl ObjectKind {
    /// Whether nodes of this kind can contain children.
    pub fn is_container(self) -> bool {
        !matches!(
            self,
            Self::Column | Self::Field | Self::Key | Self::Sequence
        )
    }
}

/// Describes which tree levels a data source has, e.g.
/// PostgreSQL: database → schema → {tables, views, …};
/// MongoDB: database → collection → {indexes};
/// Redis: database → key prefixes (by separator) → keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HierarchySpec {
    pub levels: Vec<HierarchyLevel>,
    /// Separator used to group keys into a tree (Redis); `None` otherwise.
    pub key_separator: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HierarchyLevel {
    pub kind: ObjectKind,
    /// Object kinds listed directly under a node of this level.
    pub children: Vec<ObjectKind>,
}

/// A path to an object: data source + names from the root.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeRef {
    pub data_source: DataSourceId,
    pub path: Vec<(ObjectKind, String)>,
}

impl NodeRef {
    pub fn root(ds: DataSourceId) -> Self {
        Self {
            data_source: ds,
            path: Vec::new(),
        }
    }
    pub fn child(&self, kind: ObjectKind, name: impl Into<String>) -> Self {
        let mut path = self.path.clone();
        path.push((kind, name.into()));
        Self {
            data_source: self.data_source.clone(),
            path,
        }
    }
    pub fn name(&self) -> Option<&str> {
        self.path.last().map(|(_, n)| n.as_str())
    }
    pub fn kind(&self) -> Option<ObjectKind> {
        self.path.last().map(|(k, _)| *k)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DbObject {
    pub node: NodeRef,
    pub kind: ObjectKind,
    pub name: String,
    /// Short secondary text, e.g. a column type or row estimate.
    pub detail: Option<String>,
    pub has_children: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SchemaSnapshot {
    pub objects: Vec<DbObject>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshScope {
    All,
    Node(NodeRef),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleOptions {
    pub sample_size: usize,
}

impl Default for SampleOptions {
    fn default() -> Self {
        Self { sample_size: 1000 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldStats {
    /// Dotted path, e.g. `address.city`.
    pub path: String,
    /// BSON type names with frequency (0.0–1.0).
    pub types: Vec<(String, f32)>,
    /// Share of sampled documents containing the field.
    pub presence: f32,
    pub samples: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InferredSchema {
    pub sampled: usize,
    pub fields: Vec<FieldStats>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdlOptions {
    pub include_indexes: bool,
    pub include_comments: bool,
}

impl Default for DdlOptions {
    fn default() -> Self {
        Self {
            include_indexes: true,
            include_comments: true,
        }
    }
}

/// Scripts generated for an object ("Generate" in the explorer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScriptKind {
    /// `SELECT` listing every column / `db.c.find({})`.
    Select,
    /// `INSERT` with a parameter per column / `insertOne`.
    Insert,
    /// `UPDATE … WHERE <key>` / `updateOne`.
    Update,
    /// `DELETE … WHERE <key>` / `deleteOne`.
    Delete,
    /// Procedure / function call with a parameter per argument.
    Call,
    /// `DROP …` of the object.
    Drop,
}

/// Quick documentation of an object (Ctrl+Q), shown in the inspector.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectDoc {
    pub title: String,
    /// Kind and location, e.g. `table in public`.
    pub subtitle: String,
    /// Columns, arguments, keys… as (name, description).
    pub fields: Vec<(String, String)>,
    /// Comment or definition.
    pub body: Option<String>,
}

pub trait MetadataService: Send + Sync {
    fn hierarchy(&self, ds: &DataSourceId) -> Arc<HierarchySpec>;
    fn children(&self, node: NodeRef) -> BoxFuture<'_, Result<Vec<DbObject>>>;
    /// Every cached object of the source (completion, search); no I/O.
    fn snapshot(&self, ds: &DataSourceId) -> Option<Arc<SchemaSnapshot>>;
    /// Drops cached metadata in `scope` and introspects it again.
    fn refresh(&self, ds: &DataSourceId, scope: RefreshScope) -> BoxFuture<'_, Result<()>>;
    fn infer_schema(
        &self,
        container: NodeRef,
        sample: SampleOptions,
    ) -> BoxFuture<'_, Result<InferredSchema>>;
    /// DDL of the objects (server-side definition or generated from the
    /// metadata model); several tables come in dependency order.
    fn definition(&self, objects: Vec<NodeRef>, opts: DdlOptions) -> BoxFuture<'_, Result<String>>;

    /// A script for the object: snippets, procedure calls, `DROP`.
    fn script(&self, object: NodeRef, kind: ScriptKind) -> BoxFuture<'_, Result<String>> {
        let _ = (object, kind);
        Box::pin(async { Err(crate::Error::Unsupported("object scripts".into())) })
    }

    /// Objects named by `parts` (`["orders"]`, `["sales", "orders"]`,
    /// `["orders", "id"]`), resolved against cached metadata only (no I/O).
    /// Used by "Go to declaration". Best matches first.
    fn resolve(&self, ds: &DataSourceId, parts: &[String]) -> Vec<DbObject> {
        let _ = (ds, parts);
        Vec::new()
    }

    /// Quick documentation of the object.
    fn documentation(&self, object: NodeRef) -> BoxFuture<'_, Result<ObjectDoc>> {
        let _ = object;
        Box::pin(async { Err(crate::Error::Unsupported("quick documentation".into())) })
    }

    /// Tables and foreign keys of a schema / database (ER diagram).
    fn er_model(&self, container: NodeRef) -> BoxFuture<'_, Result<crate::ErModel>> {
        let _ = container;
        Box::pin(async { Err(crate::Error::Unsupported("ER diagrams".into())) })
    }

    /// Differences between two schemas / databases (possibly of different
    /// data sources) and the script making `target` like `source`.
    fn compare_schemas(
        &self,
        source: NodeRef,
        target: NodeRef,
        opts: crate::CompareOptions,
    ) -> BoxFuture<'_, Result<crate::SchemaComparison>> {
        let _ = (source, target, opts);
        Box::pin(async { Err(crate::Error::Unsupported("schema comparison".into())) })
    }

    /// A table as the table editor shows it.
    fn table_design(&self, table: NodeRef) -> BoxFuture<'_, Result<crate::TableDesign>> {
        let _ = table;
        Box::pin(async { Err(crate::Error::Unsupported("the table editor".into())) })
    }

    /// `CREATE TABLE` / `ALTER TABLE` statements for a design in
    /// `container` (the table named `design.original` is altered).
    fn design_script(
        &self,
        container: NodeRef,
        design: crate::TableDesign,
    ) -> BoxFuture<'_, Result<crate::DesignScript>> {
        let _ = (container, design);
        Box::pin(async { Err(crate::Error::Unsupported("the table editor".into())) })
    }

    /// Runs DDL statements in `container` (in one transaction where the
    /// database has transactional DDL) and refreshes its metadata. Returns
    /// the statements run.
    fn apply_script(
        &self,
        container: NodeRef,
        statements: Vec<String>,
    ) -> BoxFuture<'_, Result<usize>> {
        let _ = (container, statements);
        Box::pin(async { Err(crate::Error::Unsupported("running DDL".into())) })
    }
}
