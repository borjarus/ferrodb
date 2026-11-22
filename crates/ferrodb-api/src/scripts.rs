//! Extension scripts (plan, phase 9): `rhai` scripts that add export formats
//! ("extractors") and aggregates of selected cells ("aggregators"), in the
//! spirit of the data extractors and aggregators of JetBrains IDEs.
//!
//! Scripts live in `<config>/scripts/extractors/*.rhai` and
//! `<config>/scripts/aggregators/*.rhai`; a few built-in ones ship with the
//! IDE. They run sandboxed: no file, network or process access, bounded
//! operations and memory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{BoxFuture, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtScriptKind {
    /// Writes rows as text in a custom format (export, Copy As).
    Extractor,
    /// Reduces the selected cells to one value (status bar of the grid).
    Aggregator,
}

impl ExtScriptKind {
    /// Sub-directory of `<config>/scripts`.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Extractor => "extractors",
            Self::Aggregator => "aggregators",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtScriptInfo {
    /// Stable id: the file stem (`sql-in-list`), `builtin:<stem>` for built-ins.
    pub id: String,
    pub kind: ExtScriptKind,
    /// Display name (the script's `// name:` header, else the file stem).
    pub name: String,
    /// The script's `// description:` header.
    pub description: String,
    /// Extractors: file extension of the output (`// extension:` header,
    /// default `txt`).
    pub extension: String,
    /// `None` for built-ins.
    pub path: Option<PathBuf>,
    pub builtin: bool,
    /// Compile error; such a script is listed but cannot run.
    pub error: Option<String>,
}

/// A cell value handed to a script.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScriptValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Text and every other type rendered as text (dates, decimals, JSON…).
    Text(String),
}

/// A rectangular selection or a page of rows: column names and rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptTable {
    pub columns: Vec<String>,
    /// SQL-ish type names of the columns, when known (`integer`, `text`…).
    pub types: Vec<String>,
    pub rows: Vec<Vec<ScriptValue>>,
}

pub trait ScriptService: Send + Sync {
    fn available(&self) -> bool {
        true
    }
    /// Scripts of one kind, built-ins first, then by name.
    fn list(&self, kind: ExtScriptKind) -> Vec<ExtScriptInfo>;
    /// Re-reads the scripts directory.
    fn reload(&self) -> BoxFuture<'_, Result<()>>;
    /// `<config>/scripts` (`None` when nothing is stored on disk).
    fn directory(&self) -> Option<PathBuf>;
    /// Creates a new script from a template and returns its path.
    fn create(&self, kind: ExtScriptKind, name: &str) -> BoxFuture<'_, Result<PathBuf>>;
    /// Runs an aggregator over the selected cells; returns the text shown.
    fn aggregate(&self, id: &str, cells: ScriptTable) -> BoxFuture<'_, Result<String>>;
    /// Runs an extractor over rows held in memory (Copy As, preview).
    /// Export to a file goes through the transfer service with
    /// `DataFormat::Script` and `FormatOptions::script`.
    fn extract(&self, id: &str, table: ScriptTable) -> BoxFuture<'_, Result<String>>;
}

/// Default: no scripts.
pub struct NoScripts;

impl ScriptService for NoScripts {
    fn available(&self) -> bool {
        false
    }
    fn list(&self, _kind: ExtScriptKind) -> Vec<ExtScriptInfo> {
        Vec::new()
    }
    fn reload(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn directory(&self) -> Option<PathBuf> {
        None
    }
    fn create(&self, _kind: ExtScriptKind, _name: &str) -> BoxFuture<'_, Result<PathBuf>> {
        Box::pin(async { Err(crate::Error::Unsupported("scripts".into())) })
    }
    fn aggregate(&self, _id: &str, _cells: ScriptTable) -> BoxFuture<'_, Result<String>> {
        Box::pin(async { Err(crate::Error::Unsupported("scripts".into())) })
    }
    fn extract(&self, _id: &str, _table: ScriptTable) -> BoxFuture<'_, Result<String>> {
        Box::pin(async { Err(crate::Error::Unsupported("scripts".into())) })
    }
}
