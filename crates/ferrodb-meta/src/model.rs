//! The metadata model: what introspectors read and the cache stores.
//!
//! One [`Catalog`] per data source. Top-level containers (schemas,
//! databases) are listed cheaply; the content of a container is introspected
//! on first use and can be refreshed on its own (incremental refresh). The
//! model is plain data (serde), so the disk cache is just its JSON form.

use serde::{Deserialize, Serialize};

/// Bumped when the cached JSON is no longer readable by this build.
pub const CACHE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dialect {
    Postgres,
    MySql,
    MariaDb,
    Sqlite,
    Mongo,
}

impl Dialect {
    /// From an SQL dialect id or the MQL language.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "postgresql" | "greenplum" | "cockroachdb" | "redshift" => Self::Postgres,
            "mysql" => Self::MySql,
            "mariadb" => Self::MariaDb,
            "sqlite" => Self::Sqlite,
            "mql" | "mongodb" => Self::Mongo,
            _ => return None,
        })
    }

    pub fn is_mysql(self) -> bool {
        matches!(self, Self::MySql | Self::MariaDb)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    pub version: u32,
    /// `None` until the containers were listed once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub containers: Option<Vec<Container>>,
    /// Unix seconds of the last introspection.
    pub refreshed_at: u64,
}

impl Catalog {
    pub fn new() -> Self {
        Self {
            version: CACHE_VERSION,
            ..Default::default()
        }
    }

    pub fn container(&self, name: &str) -> Option<&Container> {
        self.containers.as_ref()?.iter().find(|c| c.name == name)
    }

    pub fn container_mut(&mut self, name: &str) -> Option<&mut Container> {
        self.containers
            .as_mut()?
            .iter_mut()
            .find(|c| c.name == name)
    }

    /// Containers whose content is loaded.
    pub fn loaded(&self) -> impl Iterator<Item = (&Container, &Content)> {
        self.containers
            .iter()
            .flatten()
            .filter_map(|c| c.content.as_ref().map(|x| (c, x)))
    }

    /// Number of objects (containers, tables, columns…), for diagnostics.
    pub fn object_count(&self) -> usize {
        self.containers
            .iter()
            .flatten()
            .map(|c| 1 + c.content.as_ref().map_or(0, Content::object_count))
            .sum()
    }
}

/// A schema (PostgreSQL, SQLite) or database (MySQL, MongoDB).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Container {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// `None` = not introspected yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Content {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<Table>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub routines: Vec<Routine>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sequences: Vec<Sequence>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<Collection>,
    pub refreshed_at: u64,
}

impl Content {
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
    }

    pub fn object_count(&self) -> usize {
        self.tables
            .iter()
            .map(|t| {
                1 + t.columns.len()
                    + t.indexes.len()
                    + t.foreign_keys.len()
                    + t.triggers.len()
                    + usize::from(t.primary_key.is_some())
            })
            .sum::<usize>()
            + self.routines.len()
            + self.sequences.len()
            + self
                .collections
                .iter()
                .map(|c| 1 + c.indexes.len())
                .sum::<usize>()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TableKind {
    #[default]
    Table,
    View,
    MaterializedView,
    /// PostgreSQL foreign table.
    Foreign,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Table {
    pub name: String,
    pub kind: TableKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<Column>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<Key>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uniques: Vec<Key>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub foreign_keys: Vec<ForeignKey>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Index>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<Trigger>,
    /// View query (`SELECT …`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_definition: Option<String>,
    /// Full `CREATE` statement kept by the server (SQLite).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_sql: Option<String>,
    /// PostgreSQL: `PARTITION BY …` of a partitioned table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition_by: Option<String>,
    /// PostgreSQL: (parent, `FOR VALUES …`) of a partition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition_of: Option<(String, String)>,
    /// MySQL table options: engine, charset, collation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collation: Option<String>,
    /// Estimated rows (statistics), for the explorer detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<i64>,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn is_view(&self) -> bool {
        matches!(self.kind, TableKind::View | TableKind::MaterializedView)
    }

    /// Columns identifying a row: primary key, else the first unique key.
    pub fn row_key(&self) -> Option<&[String]> {
        self.primary_key
            .as_ref()
            .or_else(|| self.uniques.first())
            .map(|k| k.columns.as_slice())
            .or_else(|| {
                self.indexes
                    .iter()
                    .find(|i| i.unique && i.predicate.is_none() && !i.columns.is_empty())
                    .map(|i| i.columns.as_slice())
            })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Column {
    pub name: String,
    /// Type as the database spells it: `character varying(20)`, `int unsigned`.
    pub data_type: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub nullable: bool,
    /// Default expression in SQL (already quoted where needed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// `GENERATED ALWAYS|BY DEFAULT AS IDENTITY` (PostgreSQL): `ALWAYS` / `BY DEFAULT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// PostgreSQL `serial` column (default from an owned sequence).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub serial: bool,
    /// MySQL `AUTO_INCREMENT`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub auto_increment: bool,
    /// Generated column expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated: Option<String>,
    /// Stored (`true`) or virtual generated column.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub generated_stored: bool,
    /// Collation when it differs from the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collation: Option<String>,
    /// MySQL `ON UPDATE …` clause.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_update: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Key {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ForeignKey {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    /// `None` = same container.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_container: Option<String>,
    pub ref_table: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ref_columns: Vec<String>,
    /// `CASCADE`, `SET NULL`… (`None` = `NO ACTION`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_update: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deferrable: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub initially_deferred: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Check {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Boolean expression without the `CHECK` keyword.
    pub expression: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    pub name: String,
    /// Columns or expressions, with `DESC` where applicable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    /// PostgreSQL `INCLUDE (…)` columns.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unique: bool,
    /// Access method (`gin`, `FULLTEXT`…); `None` = default (b-tree).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Partial index predicate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    /// Statement kept by the server (SQLite), used verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_sql: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Trigger {
    pub name: String,
    /// `BEFORE INSERT`, `AFTER UPDATE OR DELETE`…
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// Full `CREATE TRIGGER …` statement (server-side where available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoutineKind {
    #[default]
    Function,
    Procedure,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Argument {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub data_type: String,
    /// `IN`, `OUT`, `INOUT`, `VARIADIC`, `TABLE`.
    pub mode: String,
}

impl Argument {
    pub fn is_input(&self) -> bool {
        matches!(self.mode.as_str(), "IN" | "INOUT" | "VARIADIC")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Routine {
    pub name: String,
    pub kind: RoutineKind,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<Argument>,
    /// Result type of a function (`integer`, `SETOF orders`, `TABLE(…)`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returns: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Full `CREATE …` statement (server-side where available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    /// Distinguishes overloads in the tree: `name(int, text)`.
    pub signature: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sequence {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub increment: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<i64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cycle: bool,
}

// ------------------------------------------------------------------ MongoDB

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CollectionKind {
    #[default]
    Collection,
    View,
    TimeSeries,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Collection {
    pub name: String,
    pub kind: CollectionKind,
    /// `listCollections` options in shell syntax (capped, collation,
    /// timeseries, viewOn, pipeline, validator…).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<String>,
    /// `validator` in shell syntax.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validator: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<MongoIndex>,
    /// `db.createCollection(…)` / `db.createView(…)` statement.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MongoIndex {
    pub name: String,
    /// Key specification in shell syntax (`{ a: 1, b: -1 }`).
    pub keys: String,
    /// Options with the name (`unique`, `sparse`, `expireAfterSeconds`,
    /// `partialFilterExpression`…) in shell syntax.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unique: bool,
}
