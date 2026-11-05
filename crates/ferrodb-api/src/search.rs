use crate::{DataSourceId, NodeRef, ObjectKind};

/// What a "Search Everywhere" hit points to. Actions are contributed by the UI
/// itself; the backend contributes data objects (tables, collections, keys…).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchTarget {
    Object(NodeRef),
    DataSource(DataSourceId),
    File(std::path::PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchEntry {
    pub label: String,
    /// Secondary text, e.g. `prod-pg / public`.
    pub location: String,
    pub kind: Option<ObjectKind>,
    pub target: SearchTarget,
}

/// Index used by "Search Everywhere". Matching/ranking happens in the UI so
/// that the same fuzzy matcher scores actions and objects consistently.
pub trait SearchIndexService: Send + Sync {
    /// All indexed entries (cheap: entries are reference-counted strings in
    /// real implementations, the mock just clones).
    fn entries(&self) -> Vec<SearchEntry>;
}
