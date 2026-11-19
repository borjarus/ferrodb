//! Schema tools (plan, phase 7): ER diagrams, schema comparison with a
//! migration script, and the visual table editor generating `ALTER`.
//!
//! All of them work on cached metadata (the explorer catalog) and generate
//! SQL in the dialect of the target data source; nothing runs until the
//! user applies a script with [`crate::MetadataService::apply_script`].

use serde::{Deserialize, Serialize};

use crate::{NodeRef, ObjectKind};

// ------------------------------------------------------------- ER diagram

/// Tables of a schema / database with their relationships.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ErModel {
    /// The schema / database the diagram shows.
    pub container: String,
    pub entities: Vec<ErEntity>,
    pub relations: Vec<ErRelation>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ErEntity {
    pub name: String,
    /// `Table` or `View`.
    pub kind: Option<ObjectKind>,
    pub columns: Vec<ErColumn>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ErColumn {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary: bool,
    /// Part of a foreign key.
    pub foreign: bool,
}

/// A foreign key: `from(from_columns) → to(to_columns)`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ErRelation {
    pub name: Option<String>,
    pub from: String,
    pub from_columns: Vec<String>,
    pub to: String,
    pub to_columns: Vec<String>,
    /// The referencing columns may be NULL (0..1 on the referenced side).
    pub optional: bool,
    /// The referencing columns are unique (one-to-one).
    pub one_to_one: bool,
}

// ------------------------------------------------------ schema comparison

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiffKind {
    /// Only in the source: the script creates it in the target.
    Added,
    /// Only in the target: the script drops it (when enabled).
    Removed,
    /// In both, different: the script alters the target.
    Changed,
}

/// One difference between the source and the target schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaDiffItem {
    pub kind: DiffKind,
    pub object_kind: ObjectKind,
    /// `orders`, `orders.customer_id`, `orders.ix_status`.
    pub name: String,
    /// What differs, e.g. `type integer → bigint, NOT NULL added`.
    pub detail: String,
    /// DDL of the object in the source / target (for the side-by-side view).
    pub source_ddl: Option<String>,
    pub target_ddl: Option<String>,
    /// Statements of the migration script for this difference.
    pub statements: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareOptions {
    /// Generate `DROP` for objects missing in the source.
    pub drop_removed: bool,
    pub ignore_comments: bool,
    /// Compare functions, procedures and triggers (by definition text).
    pub include_routines: bool,
    /// Ignore case and spacing in type names (`INT` = `integer`).
    pub normalize_types: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            drop_removed: false,
            ignore_comments: false,
            include_routines: true,
            normalize_types: true,
        }
    }
}

/// Result of [`crate::MetadataService::compare_schemas`]: what to change
/// in `target` to make it like `source`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaComparison {
    pub source: NodeRef,
    pub target: NodeRef,
    pub items: Vec<SchemaDiffItem>,
    /// The whole migration script in the target's dialect, in a valid order
    /// (drops of foreign keys first, creates of foreign keys last).
    pub script: String,
    /// E.g. different dialects, a change that loses data, a table rebuilt
    /// (SQLite).
    pub warnings: Vec<String>,
}

// ----------------------------------------------------------- table editor

/// A table as edited in the visual table editor.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TableDesign {
    /// Name of the table as loaded; `None` = a new table.
    pub original: Option<String>,
    pub name: String,
    pub comment: Option<String>,
    pub columns: Vec<ColumnDesign>,
    /// Primary key columns (empty = none).
    pub primary_key: Vec<String>,
    pub uniques: Vec<Vec<String>>,
    pub foreign_keys: Vec<ForeignKeyDesign>,
    pub indexes: Vec<IndexDesign>,
    pub checks: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnDesign {
    /// Name of the column as loaded (`None` = added): a different `name`
    /// is a rename, not a drop and add.
    pub original: Option<String>,
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    /// SQL expression, e.g. `0`, `'new'`, `now()`.
    pub default: Option<String>,
    pub comment: Option<String>,
    /// Identity / `AUTO_INCREMENT` / `AUTOINCREMENT`.
    pub auto_increment: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKeyDesign {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// `CASCADE`, `SET NULL`… (`None` = no action).
    pub on_delete: Option<String>,
    pub on_update: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDesign {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

/// Statements turning the table as loaded into the design.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignScript {
    pub statements: Vec<String>,
    /// E.g. dropped columns lose data, SQLite rebuilds the table.
    pub warnings: Vec<String>,
}

impl DesignScript {
    pub fn text(&self) -> String {
        self.statements
            .iter()
            .map(|s| {
                let s = s.trim_end();
                if s.ends_with(';') {
                    s.to_owned()
                } else {
                    format!("{s};")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
