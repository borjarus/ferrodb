//! Editing document results (plan, section 5.6): `SetField` becomes
//! `updateOne({ _id }, { $set: { path: value } })`, `DeleteDocument`
//! becomes `deleteOne({ _id })`. The `_id`s of streamed documents are kept
//! per result. Changes run in one transaction when the server supports
//! transactions, otherwise one by one.

use bson::{Bson, Document, doc};
use ferrodb_api::{ApplyReport, Change, ChangeSet, Error, Result, ResultId, SessionId};
use ferrodb_drivers::{CancellationToken, Request, sink::collect_request};
use ferrodb_lang::mql::{Command, Op, print_inline};
use ferrodb_meta::mongo::collection_ref;

use crate::Inner;

/// `_id`s kept per result (beyond that, documents are not editable).
pub(crate) const MAX_IDS: usize = 1_000_000;

#[derive(Clone, Debug)]
pub(crate) struct DocResult {
    pub session: SessionId,
    pub database: String,
    pub collection: String,
    pub ids: Vec<Option<Bson>>,
}

/// Extended JSON (as sent by the document view) → BSON.
fn parse_value(json: &str) -> Result<Bson> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| Error::Other(format!("value: {e}")))?;
    Bson::try_from(v).map_err(|e| Error::Other(format!("value: {e}")))
}

fn commands(r: &DocResult, changes: &ChangeSet) -> Result<Vec<Command>> {
    let id = |ix: usize| -> Result<Bson> {
        r.ids.get(ix).cloned().flatten().ok_or_else(|| {
            Error::Other(format!(
                "document {} has no _id: it cannot be edited",
                ix + 1
            ))
        })
    };
    changes
        .changes
        .iter()
        .map(|c| {
            let op = match c {
                Change::SetField {
                    document,
                    path,
                    value,
                } => {
                    if path == "_id" || path.starts_with("_id.") {
                        return Err(Error::Other("_id cannot be changed".into()));
                    }
                    Op::Update {
                        collection: r.collection.clone(),
                        filter: doc! { "_id": id(*document)? },
                        update: Bson::Document(
                            doc! { "$set": { path.clone(): parse_value(value)? } },
                        ),
                        many: false,
                        options: Document::new(),
                    }
                }
                Change::DeleteDocument { document } => Op::Delete {
                    collection: r.collection.clone(),
                    filter: doc! { "_id": id(*document)? },
                    many: false,
                },
                other => {
                    return Err(Error::Unsupported(format!(
                        "{other:?} on a document result"
                    )));
                }
            };
            Ok(Command::on(&r.database, op))
        })
        .collect()
}

/// The statement shown in the operation preview.
pub(crate) fn preview(cmd: &Command) -> String {
    let coll = cmd.collection().unwrap_or_default();
    let r = collection_ref(coll);
    match &cmd.op {
        Op::Update { filter, update, .. } => format!(
            "{r}.updateOne({}, {})",
            print_inline(&Bson::Document(filter.clone())),
            print_inline(update)
        ),
        Op::Delete { filter, .. } => {
            format!(
                "{r}.deleteOne({})",
                print_inline(&Bson::Document(filter.clone()))
            )
        }
        _ => String::new(),
    }
}

impl Inner {
    fn doc_result(&self, result: ResultId) -> Result<DocResult> {
        self.state().documents.get(&result).cloned().ok_or_else(|| {
            Error::Unsupported(
                "editing this result (only MongoDB documents are editable before phase 6)".into(),
            )
        })
    }

    pub(crate) fn preview_documents(
        &self,
        result: ResultId,
        changes: &ChangeSet,
    ) -> Result<Vec<String>> {
        let r = self.doc_result(result)?;
        Ok(commands(&r, changes)?.iter().map(preview).collect())
    }

    pub(crate) async fn apply_documents(
        &self,
        result: ResultId,
        changes: ChangeSet,
    ) -> Result<ApplyReport> {
        let r = self.doc_result(result)?;
        let cfg = self.source_config(&self.session_source(r.session)?)?;
        if cfg.read_only {
            return Err(Error::ReadOnly);
        }
        let cmds = commands(&r, &changes)?;
        let statements: Vec<String> = cmds.iter().map(preview).collect();
        let mut conn = self.checkout(&cfg.id).await?;
        let tx = conn.begin().await.is_ok();
        let mut applied = 0;
        for c in cmds {
            match collect_request(conn.as_mut(), Request::Mql(c), CancellationToken::new()).await {
                Ok(out) => applied += out.summary.affected.unwrap_or(0) as usize,
                Err(e) => {
                    if tx {
                        let _ = conn.rollback().await;
                    }
                    self.checkin(&cfg.id, conn);
                    return Err(e);
                }
            }
        }
        if tx {
            conn.commit().await?;
        }
        self.checkin(&cfg.id, conn);
        Ok(ApplyReport {
            applied,
            statements,
        })
    }

    fn session_source(&self, s: SessionId) -> Result<ferrodb_api::DataSourceId> {
        self.state()
            .sessions
            .get(&s)
            .map(|x| x.data_source.clone())
            .ok_or_else(|| Error::NotFound(format!("session {s}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_become_commands() {
        let r = DocResult {
            session: SessionId(1),
            database: "shop".into(),
            collection: "orders".into(),
            ids: vec![Some(Bson::Int32(7)), None],
        };
        let set = ChangeSet {
            changes: vec![
                Change::SetField {
                    document: 0,
                    path: "address.city".into(),
                    value: "\"Kraków\"".into(),
                },
                Change::DeleteDocument { document: 0 },
            ],
        };
        let cmds = commands(&r, &set).unwrap();
        assert_eq!(
            preview(&cmds[0]),
            "db.orders.updateOne({ _id: 7 }, { $set: { \"address.city\": \"Kraków\" } })"
        );
        assert_eq!(preview(&cmds[1]), "db.orders.deleteOne({ _id: 7 })");
        assert_eq!(cmds[0].database.as_deref(), Some("shop"));
        let no_id = ChangeSet {
            changes: vec![Change::DeleteDocument { document: 1 }],
        };
        assert!(commands(&r, &no_id).is_err());
        let date = ChangeSet {
            changes: vec![Change::SetField {
                document: 0,
                path: "d".into(),
                value: r#"{"$date":"2024-01-02T00:00:00Z"}"#.into(),
            }],
        };
        assert!(
            preview(&commands(&r, &date).unwrap()[0]).contains("ISODate(\"2024-01-02T00:00:00Z\")")
        );
    }
}
