//! Local History (plan, phase 7): revisions of console texts and project
//! files kept by the IDE itself, independent of version control. They
//! survive restarts, can be compared with the current text and restored.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

use crate::{BoxFuture, Result};

/// Why a revision was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RevisionReason {
    /// Before a statement / script ran.
    Execute,
    /// The file was saved.
    Save,
    /// Periodic snapshot of unsaved edits.
    Autosave,
    /// The text before a revision was restored over it.
    BeforeRestore,
    /// The file changed on disk outside the IDE.
    External,
    /// The user labelled the current state ("Put Label").
    Label,
}

impl RevisionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Save => "save",
            Self::Autosave => "autosave",
            Self::BeforeRestore => "before-restore",
            Self::External => "external",
            Self::Label => "label",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "execute" => Self::Execute,
            "save" => Self::Save,
            "autosave" => Self::Autosave,
            "before-restore" => Self::BeforeRestore,
            "external" => Self::External,
            "label" => Self::Label,
            _ => return None,
        })
    }
}

/// One revision of a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub id: i64,
    /// What the text belongs to: a file path or `console:<name>`.
    pub document: String,
    pub at: SystemTime,
    pub reason: RevisionReason,
    /// User label (`Label`) or a short note (data source of an execution).
    pub label: Option<String>,
    /// Size of the text in bytes.
    pub size: usize,
}

/// A document with revisions, for the panel's document list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryDocument {
    pub document: String,
    pub revisions: usize,
    pub last: SystemTime,
}

/// How much history is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    /// Revisions older than this are removed.
    pub max_age: Duration,
    /// At most this many revisions per document (oldest removed first;
    /// labelled revisions are kept).
    pub per_document: usize,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(30 * 24 * 3600),
            per_document: 200,
        }
    }
}

pub trait LocalHistoryService: Send + Sync {
    /// Stores a revision unless the text equals the newest revision of the
    /// document. Returns the new revision, `None` when nothing was stored.
    fn record(
        &self,
        document: String,
        text: String,
        reason: RevisionReason,
        label: Option<String>,
    ) -> BoxFuture<'_, Result<Option<Revision>>>;

    /// Revisions of one document, newest first.
    fn revisions(&self, document: String) -> BoxFuture<'_, Result<Vec<Revision>>>;

    /// Documents with revisions, most recently changed first.
    fn documents(&self) -> BoxFuture<'_, Result<Vec<HistoryDocument>>>;

    fn text(&self, revision: i64) -> BoxFuture<'_, Result<String>>;

    fn delete(&self, revision: i64) -> BoxFuture<'_, Result<()>>;

    /// Removes every revision of the document.
    fn clear(&self, document: String) -> BoxFuture<'_, Result<()>>;

    /// Applies the retention rules now (also done on every `record`).
    fn prune(&self, retention: Retention) -> BoxFuture<'_, Result<usize>>;
}

/// Local History kept in memory (mock backend, tests, backends without
/// storage).
#[derive(Default)]
pub struct MemoryLocalHistory {
    inner: Mutex<MemoryHistory>,
}

#[derive(Default)]
struct MemoryHistory {
    next: i64,
    revisions: Vec<(Revision, String)>,
    retention: Option<Retention>,
}

impl MemoryLocalHistory {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryHistory> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Retention over revisions sorted oldest first; returns removed ids.
pub fn retention_victims(
    revisions: &[Revision],
    retention: Retention,
    now: SystemTime,
) -> Vec<i64> {
    let mut out = Vec::new();
    let mut per_doc: HashMap<&str, Vec<&Revision>> = HashMap::new();
    for r in revisions {
        per_doc.entry(r.document.as_str()).or_default().push(r);
    }
    for (_, mut revs) in per_doc {
        revs.sort_by_key(|r| (r.at, r.id));
        let newest = revs.last().map(|r| r.id);
        let mut kept: Vec<&Revision> = Vec::new();
        for r in revs {
            let old = now
                .duration_since(r.at)
                .map(|d| d > retention.max_age)
                .unwrap_or(false);
            // The newest revision and labelled ones are never aged out.
            if old && r.reason != RevisionReason::Label && Some(r.id) != newest {
                out.push(r.id);
            } else {
                kept.push(r);
            }
        }
        let unlabelled: Vec<&&Revision> = kept
            .iter()
            .filter(|r| r.reason != RevisionReason::Label)
            .collect();
        let excess = kept.len().saturating_sub(retention.per_document.max(1));
        out.extend(unlabelled.iter().take(excess).map(|r| r.id));
    }
    out
}

impl LocalHistoryService for MemoryLocalHistory {
    fn record(
        &self,
        document: String,
        text: String,
        reason: RevisionReason,
        label: Option<String>,
    ) -> BoxFuture<'_, Result<Option<Revision>>> {
        let mut h = self.lock();
        let same = h
            .revisions
            .iter()
            .rev()
            .find(|(r, _)| r.document == document)
            .is_some_and(|(_, t)| *t == text);
        let out = if same && reason != RevisionReason::Label {
            None
        } else {
            h.next += 1;
            let rev = Revision {
                id: h.next,
                document,
                at: SystemTime::now(),
                reason,
                label,
                size: text.len(),
            };
            h.revisions.push((rev.clone(), text));
            let retention = h.retention.unwrap_or_default();
            let all: Vec<Revision> = h.revisions.iter().map(|(r, _)| r.clone()).collect();
            let gone = retention_victims(&all, retention, SystemTime::now());
            h.revisions.retain(|(r, _)| !gone.contains(&r.id));
            Some(rev)
        };
        Box::pin(async move { Ok(out) })
    }

    fn revisions(&self, document: String) -> BoxFuture<'_, Result<Vec<Revision>>> {
        let mut out: Vec<Revision> = self
            .lock()
            .revisions
            .iter()
            .filter(|(r, _)| r.document == document)
            .map(|(r, _)| r.clone())
            .collect();
        out.reverse();
        Box::pin(async move { Ok(out) })
    }

    fn documents(&self) -> BoxFuture<'_, Result<Vec<HistoryDocument>>> {
        let mut docs: Vec<HistoryDocument> = Vec::new();
        for (r, _) in &self.lock().revisions {
            match docs.iter_mut().find(|d| d.document == r.document) {
                Some(d) => {
                    d.revisions += 1;
                    d.last = d.last.max(r.at);
                }
                None => docs.push(HistoryDocument {
                    document: r.document.clone(),
                    revisions: 1,
                    last: r.at,
                }),
            }
        }
        docs.sort_by_key(|a| std::cmp::Reverse(a.last));
        Box::pin(async move { Ok(docs) })
    }

    fn text(&self, revision: i64) -> BoxFuture<'_, Result<String>> {
        let t = self
            .lock()
            .revisions
            .iter()
            .find(|(r, _)| r.id == revision)
            .map(|(_, t)| t.clone())
            .ok_or_else(|| crate::Error::NotFound(format!("revision {revision}")));
        Box::pin(async move { t })
    }

    fn delete(&self, revision: i64) -> BoxFuture<'_, Result<()>> {
        self.lock().revisions.retain(|(r, _)| r.id != revision);
        Box::pin(async { Ok(()) })
    }

    fn clear(&self, document: String) -> BoxFuture<'_, Result<()>> {
        self.lock()
            .revisions
            .retain(|(r, _)| r.document != document);
        Box::pin(async { Ok(()) })
    }

    fn prune(&self, retention: Retention) -> BoxFuture<'_, Result<usize>> {
        let mut h = self.lock();
        h.retention = Some(retention);
        let all: Vec<Revision> = h.revisions.iter().map(|(r, _)| r.clone()).collect();
        let gone = retention_victims(&all, retention, SystemTime::now());
        h.revisions.retain(|(r, _)| !gone.contains(&r.id));
        let n = gone.len();
        Box::pin(async move { Ok(n) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block<T>(f: BoxFuture<'_, T>) -> T {
        futures::executor::block_on(f)
    }

    #[test]
    fn memory_history_dedups_and_prunes() {
        let h = MemoryLocalHistory::new();
        let doc = "console:pg".to_owned();
        assert!(
            block(h.record(doc.clone(), "a".into(), RevisionReason::Execute, None))
                .unwrap()
                .is_some()
        );
        // Same text again: nothing stored.
        assert!(
            block(h.record(doc.clone(), "a".into(), RevisionReason::Execute, None))
                .unwrap()
                .is_none()
        );
        for t in ["b", "c", "d"] {
            block(h.record(doc.clone(), t.into(), RevisionReason::Autosave, None)).unwrap();
        }
        block(h.record(
            doc.clone(),
            "d".into(),
            RevisionReason::Label,
            Some("v1".into()),
        ))
        .unwrap();
        let revs = block(h.revisions(doc.clone())).unwrap();
        assert_eq!(revs.len(), 5);
        assert_eq!(revs[0].label.as_deref(), Some("v1"));
        assert_eq!(block(h.text(revs[1].id)).unwrap(), "d");
        let removed = block(h.prune(Retention {
            max_age: Duration::from_secs(3600),
            per_document: 2,
        }))
        .unwrap();
        assert_eq!(removed, 3);
        let revs = block(h.revisions(doc.clone())).unwrap();
        // The label survives, plus the newest unlabelled one.
        assert!(revs.iter().any(|r| r.reason == RevisionReason::Label));
        assert_eq!(revs.len(), 2);
        let docs = block(h.documents()).unwrap();
        assert_eq!(docs[0].revisions, 2);
        block(h.clear(doc.clone())).unwrap();
        assert!(block(h.revisions(doc)).unwrap().is_empty());
    }
}
