//! Data transfer (plan, phase 6): export of query results and tables to
//! files, import of files into tables and collections, copying between
//! sources and data models, materializing a result as a table.
//!
//! A transfer always streams: rows / documents are read in batches and
//! written as they arrive, so memory does not grow with the data.

use std::{path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{BoxFuture, CancelHandle, DataSourceId, NodeRef, Result};

/// File formats. Not every format can be read back: see
/// [`DataFormat::importable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataFormat {
    Csv,
    Tsv,
    /// A JSON array of objects (import also accepts JSON Lines).
    Json,
    /// One JSON object per line.
    JsonLines,
    /// MongoDB canonical Extended JSON, one document per line (types are
    /// kept: `$numberLong`, `$date`, `$oid`…).
    ExtendedJson,
    Markdown,
    Html,
    Xlsx,
    /// `INSERT` statements in the source's dialect.
    SqlInsert,
    Parquet,
    /// A user extractor script (phase 9): `FormatOptions::script` names it.
    /// Export only; not part of [`DataFormat::ALL`] (scripts are listed by
    /// `ScriptService::list`).
    Script,
}

impl DataFormat {
    pub const ALL: [DataFormat; 10] = [
        Self::Csv,
        Self::Tsv,
        Self::Json,
        Self::JsonLines,
        Self::ExtendedJson,
        Self::Markdown,
        Self::Html,
        Self::Xlsx,
        Self::SqlInsert,
        Self::Parquet,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Tsv => "TSV",
            Self::Json => "JSON",
            Self::JsonLines => "JSON Lines",
            Self::ExtendedJson => "Extended JSON",
            Self::Markdown => "Markdown",
            Self::Html => "HTML",
            Self::Xlsx => "Excel (xlsx)",
            Self::SqlInsert => "SQL INSERT",
            Self::Parquet => "Parquet",
            Self::Script => "Script",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
            Self::JsonLines => "jsonl",
            Self::ExtendedJson => "ejson",
            Self::Markdown => "md",
            Self::Html => "html",
            Self::Xlsx => "xlsx",
            Self::SqlInsert => "sql",
            Self::Parquet => "parquet",
            // The real one comes from the script (`ExtScriptInfo::extension`).
            Self::Script => "txt",
        }
    }

    /// Formats that can be imported.
    pub fn importable(self) -> bool {
        matches!(
            self,
            Self::Csv
                | Self::Tsv
                | Self::Json
                | Self::JsonLines
                | Self::ExtendedJson
                | Self::Xlsx
                | Self::Parquet
        )
    }

    /// The format of a file name, by extension.
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "csv" => Self::Csv,
            "tsv" | "tab" => Self::Tsv,
            "json" => Self::Json,
            "jsonl" | "ndjson" => Self::JsonLines,
            "ejson" => Self::ExtendedJson,
            "md" | "markdown" => Self::Markdown,
            "html" | "htm" => Self::Html,
            "xlsx" | "xlsm" => Self::Xlsx,
            "sql" => Self::SqlInsert,
            "parquet" => Self::Parquet,
            _ => return None,
        })
    }
}

/// Options of a file format; fields that do not apply are ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FormatOptions {
    /// CSV / TSV / XLSX / Markdown / HTML: a header row with column names.
    pub header: bool,
    /// CSV: field separator (default `,`).
    pub delimiter: Option<char>,
    /// CSV / TSV: how NULL is written and recognized (default empty).
    pub null_text: String,
    /// SQL INSERT: target table name (default: the source table or `data`).
    pub table: Option<String>,
    /// SQL INSERT: rows per statement.
    pub rows_per_statement: usize,
    /// JSON: indented output.
    pub pretty: bool,
    /// XLSX import: sheet name (default: the first sheet).
    pub sheet: Option<String>,
    /// [`DataFormat::Script`]: id of the extractor (`ExtScriptInfo::id`).
    #[serde(default)]
    pub script: Option<String>,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            header: true,
            delimiter: None,
            null_text: String::new(),
            table: None,
            rows_per_statement: 100,
            pretty: false,
            sheet: None,
            script: None,
        }
    }
}

/// Where the data comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum TransferSource {
    /// A read-only query (SQL `SELECT`, MongoDB `find` / `aggregate`) run on
    /// its own connection; `schema` = default schema / database.
    Query {
        data_source: DataSourceId,
        text: String,
        schema: Option<String>,
    },
    /// Every row / document of a table, view or collection.
    Object(NodeRef),
    File {
        path: PathBuf,
        format: DataFormat,
        options: FormatOptions,
    },
}

/// One target column and the source column feeding it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnMapping {
    /// Source column name; `None` = leave the target column to its default.
    pub source: Option<String>,
    pub target: String,
}

/// Where the data goes.
#[derive(Clone, Debug, PartialEq)]
pub enum TransferTarget {
    File {
        path: PathBuf,
        format: DataFormat,
        options: FormatOptions,
    },
    /// A table (tabular sources) or collection (document sources).
    Object {
        data_source: DataSourceId,
        /// Schema / database; `None` = the source's default.
        container: Option<String>,
        name: String,
        /// Create the table from the source columns when it does not exist
        /// (collections are created by the first insert anyway).
        create: bool,
        /// Remove existing rows / documents first.
        truncate: bool,
        /// Empty = by name (case-insensitive); unmatched columns are skipped.
        mapping: Vec<ColumnMapping>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransferRequest {
    pub source: TransferSource,
    pub target: TransferTarget,
}

/// Outcome of a finished transfer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransferReport {
    /// Rows / documents written.
    pub items: u64,
    /// Bytes written (file targets).
    pub bytes: u64,
    pub elapsed: Duration,
    /// How the target was written, e.g. `COPY`, `LOAD DATA`,
    /// `insertMany`, `INSERT`, `CSV file`.
    pub method: String,
    /// Statements run before the data (e.g. `CREATE TABLE`).
    pub statements: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransferEvent {
    /// Items written so far.
    Progress {
        items: u64,
    },
    Warning(String),
    Finished(TransferReport),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct TransferHandle {
    pub events: async_channel::Receiver<TransferEvent>,
    pub cancel: CancelHandle,
}

/// The first rows of a file, for the import mapping step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FilePreview {
    pub columns: Vec<String>,
    /// Guessed type per column (`integer`, `text`, `timestamp`…).
    pub types: Vec<String>,
    /// Cells as text; `None` = NULL.
    pub rows: Vec<Vec<Option<String>>>,
}

pub trait TransferService: Send + Sync {
    /// Starts a transfer; progress and the outcome arrive as events.
    fn start(&self, request: TransferRequest) -> TransferHandle;

    /// Reads the first `rows` rows of a file.
    fn preview_file(
        &self,
        path: PathBuf,
        format: DataFormat,
        options: FormatOptions,
        rows: usize,
    ) -> BoxFuture<'_, Result<FilePreview>>;

    /// Column names of a target table / collection (empty when it does not
    /// exist yet), for the mapping step.
    fn target_columns(
        &self,
        data_source: DataSourceId,
        container: Option<String>,
        name: String,
    ) -> BoxFuture<'_, Result<Vec<String>>>;
}

/// A backend without transfers.
pub struct NoTransfers;

impl TransferService for NoTransfers {
    fn start(&self, _request: TransferRequest) -> TransferHandle {
        let (tx, rx) = async_channel::bounded(1);
        let _ = tx.try_send(TransferEvent::Failed(
            "data transfer is not available in this backend".into(),
        ));
        TransferHandle {
            events: rx,
            cancel: CancelHandle::new(),
        }
    }

    fn preview_file(
        &self,
        _path: PathBuf,
        _format: DataFormat,
        _options: FormatOptions,
        _rows: usize,
    ) -> BoxFuture<'_, Result<FilePreview>> {
        Box::pin(async { Err(crate::Error::Unsupported("data transfer".into())) })
    }

    fn target_columns(
        &self,
        _data_source: DataSourceId,
        _container: Option<String>,
        _name: String,
    ) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_by_extension() {
        for f in DataFormat::ALL {
            let p = PathBuf::from(format!("x.{}", f.extension()));
            assert_eq!(DataFormat::from_path(&p), Some(f));
        }
        assert!(DataFormat::Parquet.importable());
        assert!(!DataFormat::Markdown.importable());
    }
}
