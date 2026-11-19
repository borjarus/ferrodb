//! Local History in `app.db` (plan, phase 7): revisions survive restarts;
//! every write applies the retention policy to its document.

use ferrodb_api::{
    BoxFuture, Error, HistoryDocument, LocalHistoryService, Result, Retention, Revision,
    RevisionReason,
};

use crate::Inner;

fn storage_err(e: ferrodb_storage::StorageError) -> Error {
    Error::Other(format!("local history: {e}"))
}

impl LocalHistoryService for Inner {
    fn record(
        &self,
        document: String,
        text: String,
        reason: RevisionReason,
        label: Option<String>,
    ) -> BoxFuture<'_, Result<Option<Revision>>> {
        let storage = self.storage.clone();
        let me = self.arc();
        self.spawn(async move {
            me.blocking(move || {
                storage
                    .record_revision(&document, &text, reason, label.as_deref(), Retention::default())
                    .map_err(storage_err)
            })
            .await
        })
    }

    fn revisions(&self, document: String) -> BoxFuture<'_, Result<Vec<Revision>>> {
        let storage = self.storage.clone();
        let me = self.arc();
        self.spawn(async move {
            me.blocking(move || storage.revisions(&document).map_err(storage_err))
                .await
        })
    }

    fn documents(&self) -> BoxFuture<'_, Result<Vec<HistoryDocument>>> {
        let storage = self.storage.clone();
        let me = self.arc();
        self.spawn(async move {
            me.blocking(move || storage.history_documents().map_err(storage_err))
                .await
        })
    }

    fn text(&self, revision: i64) -> BoxFuture<'_, Result<String>> {
        let storage = self.storage.clone();
        let me = self.arc();
        self.spawn(async move {
            me.blocking(move || {
                storage
                    .revision_text(revision)
                    .map_err(storage_err)?
                    .ok_or_else(|| Error::NotFound(format!("revision {revision}")))
            })
            .await
        })
    }

    fn delete(&self, revision: i64) -> BoxFuture<'_, Result<()>> {
        let storage = self.storage.clone();
        let me = self.arc();
        self.spawn(async move {
            me.blocking(move || storage.delete_revision(revision).map_err(storage_err))
                .await
        })
    }

    fn clear(&self, document: String) -> BoxFuture<'_, Result<()>> {
        let storage = self.storage.clone();
        let me = self.arc();
        self.spawn(async move {
            me.blocking(move || storage.clear_document(&document).map_err(storage_err))
                .await
        })
    }

    fn prune(&self, retention: Retention) -> BoxFuture<'_, Result<usize>> {
        let storage = self.storage.clone();
        let me = self.arc();
        self.spawn(async move {
            me.blocking(move || storage.prune_documents(None, retention).map_err(storage_err))
                .await
        })
    }
}
