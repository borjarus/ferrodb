//! Local History from the UI side (plan, phase 7): revisions are written to
//! the backend's [`LocalHistoryService`](ferrodb_api::LocalHistoryService)
//! (persistent in `app.db` with the core); panels observing
//! [`crate::activity`] reload when a revision was stored.

use std::path::Path;

use ferrodb_api::{DataSourceId, RevisionReason};
use ferrodb_ui_kit::gpui::App;

use crate::{AppState, activity};

/// Seconds of no typing after which unsaved console edits get a revision.
pub const AUTOSAVE_SECS: u64 = 30;

/// Document key of a console: its file, else `console:<source>`.
pub fn document(file: Option<&Path>, data_source: Option<&DataSourceId>) -> String {
    match (file, data_source) {
        (Some(f), _) => f.to_string_lossy().replace('\\', "/"),
        (None, Some(ds)) => format!("console:{ds}"),
        (None, None) => "console:".into(),
    }
}

/// Short name of a document for lists (`orders.sql`, `console: shop-dev`).
pub fn document_label(document: &str) -> String {
    match document.strip_prefix("console:") {
        Some(ds) => format!("console: {ds}"),
        None => Path::new(document)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| document.to_owned()),
    }
}

/// Stores a revision in the background; panels refresh when it is written.
pub fn record(
    document: String,
    text: String,
    reason: RevisionReason,
    label: Option<String>,
    cx: &mut App,
) {
    if text.trim().is_empty() {
        return;
    }
    let backend = AppState::backend(cx);
    let task = cx.background_executor().spawn(async move {
        backend
            .local_history()
            .record(document, text, reason, label)
            .await
    });
    cx.spawn(async move |cx| {
        let stored = task.await;
        match stored {
            Ok(Some(_)) => {
                cx.update(activity::revision_stored);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "local history not written"),
        }
    })
    .detach();
}

/// Label of a reason, a Fluent id.
pub fn reason_label(reason: RevisionReason) -> &'static str {
    match reason {
        RevisionReason::Execute => "local-history-executed",
        RevisionReason::Save => "local-history-saved",
        RevisionReason::Autosave => "local-history-autosaved",
        RevisionReason::BeforeRestore => "local-history-before-restore",
        RevisionReason::External => "local-history-external",
        RevisionReason::Label => "local-history-labelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn documents_and_labels() {
        assert_eq!(document(None, Some(&"pg".into())), "console:pg");
        assert_eq!(
            document(Some(Path::new("C:\\p\\q.sql")), Some(&"pg".into())),
            "C:/p/q.sql"
        );
        assert_eq!(document_label("console:pg"), "console: pg");
        assert_eq!(document_label("C:/p/q.sql"), "q.sql");
    }
}
