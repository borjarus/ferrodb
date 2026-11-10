//! Result views (plan 2.4–2.6) and the model they share.
//!
//! A [`ResultModel`] accumulates the items of one result as `Data` events
//! stream in (the console owns the event loop). Views observe the model:
//! [`grid::GridView`] for tables, [`documents::DocumentView`] for documents,
//! [`keys::KeyView`] for keys. Paging goes through [`ResultModel::fetch_more`].

pub mod changes;
pub mod documents;
pub mod format;
pub mod grid;
pub mod keys;

use std::sync::Arc;

use ferrodb_api::{
    Capabilities, DataModel, DataSourceId, KeyEntry, ResultChunk, ResultId, ResultShape, SessionId,
    arrow_array::{ArrayRef, RecordBatch},
    arrow_schema::Schema,
    bson::RawDocumentBuf,
};
use ferrodb_ui_kit::gpui::{prelude::*, *};

use crate::{AppState, output};

/// Page sizes offered by the views (plan 5.5: default 500).
pub const PAGE_SIZES: [usize; 4] = [100, 500, 1000, 5000];

/// Rows of a tabular result: Arrow batches as received (never copied).
#[derive(Clone)]
pub struct TableData {
    pub schema: Arc<Schema>,
    pub batches: Vec<Arc<RecordBatch>>,
    /// First row index of each batch.
    starts: Vec<usize>,
    pub rows: usize,
}

impl TableData {
    pub fn new(schema: Arc<Schema>) -> Self {
        Self {
            schema,
            batches: Vec::new(),
            starts: Vec::new(),
            rows: 0,
        }
    }

    pub fn push(&mut self, batch: Arc<RecordBatch>) {
        self.starts.push(self.rows);
        self.rows += batch.num_rows();
        self.batches.push(batch);
    }

    /// Column array and index inside it for a row.
    pub fn locate(&self, row: usize, col: usize) -> Option<(&ArrayRef, usize)> {
        if row >= self.rows {
            return None;
        }
        let b = self.starts.partition_point(|s| *s <= row) - 1;
        let batch = &self.batches[b];
        Some((batch.columns().get(col)?, row - self.starts[b]))
    }

    pub fn cell(&self, row: usize, col: usize) -> Option<String> {
        self.locate(row, col)
            .and_then(|(a, i)| format::arrow_cell(a, i))
    }

    pub fn number(&self, row: usize, col: usize) -> Option<f64> {
        self.locate(row, col)
            .and_then(|(a, i)| format::arrow_number(a, i))
    }
}

pub enum ResultData {
    Table(TableData),
    Documents {
        container: Option<String>,
        /// The container in the explorer tree, when the source knows it.
        node: Option<ferrodb_api::NodeRef>,
        docs: Vec<RawDocumentBuf>,
    },
    Keys {
        pattern: String,
        keys: Vec<KeyEntry>,
    },
}

impl ResultData {
    pub fn from_shape(shape: &ResultShape) -> Self {
        match shape {
            ResultShape::Table(s) => Self::Table(TableData::new(s.clone())),
            ResultShape::Documents { container, node } => Self::Documents {
                container: container.clone(),
                node: node.clone(),
                docs: Vec::new(),
            },
            ResultShape::Keys { pattern } => Self::Keys {
                pattern: pattern.clone(),
                keys: Vec::new(),
            },
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Table(t) => t.rows,
            Self::Documents { docs, .. } => docs.len(),
            Self::Keys { keys, .. } => keys.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn model(&self) -> DataModel {
        match self {
            Self::Table(_) => DataModel::Tabular,
            Self::Documents { .. } => DataModel::Document,
            Self::Keys { .. } => DataModel::KeyValue,
        }
    }
}

/// One result of a query, filled by the console's event loop.
pub struct ResultModel {
    pub id: ResultId,
    pub session: SessionId,
    pub data_source: DataSourceId,
    pub capabilities: Capabilities,
    pub read_only: bool,
    pub title: String,
    /// Statement that produced the result (re-run with filters).
    pub statement: String,
    pub data: ResultData,
    pub has_more: bool,
    pub fetching: bool,
    pub page_size: usize,
}

/// Emitted by result views to the console that owns them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResultViewEvent {
    /// Re-run the result's statement with a different query text
    /// (filter / sort / projection fields).
    Rerun(String),
    /// The data was changed in the database (refresh).
    Applied,
}

impl ResultModel {
    pub fn append(&mut self, chunk: ResultChunk) {
        match (&mut self.data, chunk) {
            (ResultData::Table(t), ResultChunk::Rows(b)) => t.push(b),
            (ResultData::Documents { docs, .. }, ResultChunk::Documents(d)) => {
                docs.extend(d.iter().cloned())
            }
            (ResultData::Keys { keys, .. }, ResultChunk::Keys(k)) => keys.extend(k.iter().cloned()),
            _ => tracing::warn!(result = %self.id, "chunk does not match result shape"),
        }
    }

    pub fn paused(&mut self, has_more: bool) {
        self.has_more = has_more;
        self.fetching = false;
    }

    /// Whether results may be edited (capabilities and read-only flag).
    pub fn editable(&self) -> bool {
        self.capabilities.editable_results && !self.read_only
    }

    /// Requests the next page (`all` = everything that is left). Items arrive
    /// through the console's event loop.
    pub fn fetch_more(this: &Entity<Self>, all: bool, cx: &mut App) {
        let (id, n) = {
            let m = this.read(cx);
            if !m.has_more || m.fetching {
                return;
            }
            (m.id, if all { usize::MAX / 2 } else { m.page_size })
        };
        this.update(cx, |m, cx| {
            m.fetching = true;
            cx.notify();
        });
        let backend = AppState::backend(cx);
        let this = this.downgrade();
        // The mock blocks while it streams; never run it on the UI thread.
        let task = cx
            .background_executor()
            .spawn(async move { backend.queries().fetch_more(id, n).await });
        cx.spawn(async move |cx| {
            if let Err(e) = task.await {
                cx.update(|cx| {
                    output::log(output::Level::Error, e.to_string(), cx);
                    let _ = this.update(cx, |m, cx| {
                        m.fetching = false;
                        cx.notify();
                    });
                });
            }
        })
        .detach();
    }
}

/// A result view of any model, as hosted in a console tab.
#[derive(Clone)]
pub enum ResultView {
    Grid(Entity<grid::GridView>),
    Documents(Entity<documents::DocumentView>),
    Keys(Entity<keys::KeyView>),
}

impl ResultView {
    pub fn new(model: Entity<ResultModel>, window: &mut Window, cx: &mut App) -> Self {
        match model.read(cx).data.model() {
            DataModel::Tabular => Self::Grid(cx.new(|cx| grid::GridView::new(model, window, cx))),
            DataModel::Document => {
                Self::Documents(cx.new(|cx| documents::DocumentView::new(model, window, cx)))
            }
            DataModel::KeyValue => Self::Keys(cx.new(|cx| keys::KeyView::new(model, window, cx))),
        }
    }

    pub fn model(&self, cx: &App) -> Entity<ResultModel> {
        match self {
            Self::Grid(v) => v.read(cx).model().clone(),
            Self::Documents(v) => v.read(cx).model().clone(),
            Self::Keys(v) => v.read(cx).model().clone(),
        }
    }

    pub fn element(&self) -> AnyElement {
        match self {
            Self::Grid(v) => v.clone().into_any_element(),
            Self::Documents(v) => v.clone().into_any_element(),
            Self::Keys(v) => v.clone().into_any_element(),
        }
    }
}
