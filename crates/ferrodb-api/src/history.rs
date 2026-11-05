use std::time::{Duration, SystemTime};

use crate::{BoxFuture, DataSourceId, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub data_source: DataSourceId,
    pub text: String,
    pub executed_at: SystemTime,
    pub elapsed: Duration,
    pub succeeded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryFilter {
    pub data_source: Option<DataSourceId>,
    pub text: Option<String>,
    pub limit: Option<usize>,
}

pub trait HistoryService: Send + Sync {
    fn record(&self, entry: HistoryEntry);
    /// Newest first.
    fn query(&self, filter: HistoryFilter) -> BoxFuture<'_, Result<Vec<HistoryEntry>>>;
}
