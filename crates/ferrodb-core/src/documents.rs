//! Editing document results (plan, section 5.6).
//!
//! MongoDB: `SetField` becomes `updateOne({ _id }, { $set: { path: value } })`,
//! `UnsetField` `updateOne({ _id }, { $unset: { path: "" } })`,
//! `InsertDocument` `insertOne(doc)`, `ReplaceDocument`
//! `replaceOne({ _id }, doc)` and `DeleteDocument` `deleteOne({ _id })`.
//!
//! Couchbase (SQL++, by document key, which the driver returns as `_id`)
//! and DynamoDB (PartiQL, by the key attributes of the table) get the
//! statements of [`ferrodb_data::doc_edit`].
//!
//! The identity of every streamed document is kept per result (`_id`; for
//! DynamoDB the item's scalar attributes, from which the key is taken once
//! the table's key schema is known, and the names of the others, which a
//! replace removes). Changes run in one transaction when the server
//! supports transactions (MongoDB replica sets, Couchbase) or as one
//! `ExecuteTransaction` (DynamoDB, up to 100 statements); otherwise one by
//! one.

use bson::{Bson, Document, RawDocumentBuf, doc};
use ferrodb_api::{
    ApplyReport, Change, ChangeSet, DataSourceId, EditInfo, Error, NodeRef, ObjectKind, Result,
    ResultId, RowIdentity, SessionId,
};
use ferrodb_data::doc_edit::{self, DocStatement};
use ferrodb_drivers::{CancellationToken, Request, sink::collect_request};
use ferrodb_lang::mql::{Command, Op, print_inline};
use ferrodb_meta::{Dialect, mongo::collection_ref};

use crate::Inner;

/// `_id`s kept per result (beyond that, documents are not editable).
pub(crate) const MAX_IDS: usize = 1_000_000;

/// DynamoDB key attributes are at most 2 KB (partition) / 1 KB (sort):
/// longer values are not kept as identity.
const MAX_KEY_BYTES: usize = 2048;

#[derive(Clone, Debug)]
pub(crate) struct DocResult {
    pub session: SessionId,
    /// MongoDB database, Couchbase bucket, DynamoDB container (region).
    pub database: String,
    /// MongoDB collection, Couchbase keyspace path (`bucket.scope.coll`),
    /// DynamoDB table.
    pub collection: String,
    pub ids: Vec<Option<Bson>>,
    pub dialect: Dialect,
}

/// Where a document result comes from: its explorer node, and the
/// database / collection kept in [`DocResult`].
pub(crate) fn result_node(
    dialect: Dialect,
    ds: &DataSourceId,
    database: Option<&str>,
    collection: Option<&str>,
) -> Option<(NodeRef, String, String)> {
    let root = NodeRef::root(ds.clone());
    let c = collection?;
    match dialect {
        Dialect::Couchbase => {
            let parts = doc_edit::split_keyspace(c);
            let [b, s, k] = parts.as_slice() else {
                return None;
            };
            Some((
                root.child(ObjectKind::Bucket, b)
                    .child(ObjectKind::Scope, s)
                    .child(ObjectKind::Collection, k),
                b.clone(),
                c.to_owned(),
            ))
        }
        Dialect::DynamoDb => {
            let db = database?;
            Some((
                root.child(ObjectKind::Database, db)
                    .child(ObjectKind::Table, c),
                db.to_owned(),
                c.to_owned(),
            ))
        }
        _ => {
            let db = database?;
            Some((
                root.child(ObjectKind::Database, db)
                    .child(ObjectKind::Collection, c),
                db.to_owned(),
                c.to_owned(),
            ))
        }
    }
}

/// What is kept of a streamed document to address it later.
pub(crate) fn identity(dialect: Dialect, d: &RawDocumentBuf) -> Option<Bson> {
    if dialect != Dialect::DynamoDb {
        return d
            .get("_id")
            .ok()
            .flatten()
            .and_then(|v| Bson::try_from(v).ok());
    }
    let mut out = Document::new();
    for (k, v) in d.iter().flatten() {
        let v = Bson::try_from(v).ok()?;
        let keep = match &v {
            Bson::String(s) => s.len() <= MAX_KEY_BYTES,
            Bson::Binary(b) => b.bytes.len() <= MAX_KEY_BYTES,
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => true,
            _ => false,
        };
        out.insert(k.to_owned(), if keep { v } else { Bson::Null });
    }
    Some(Bson::Document(out))
}

/// Extended JSON (as sent by the document view) → BSON.
fn parse_value(json: &str) -> Result<Bson> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| Error::Other(format!("value: {e}")))?;
    Bson::try_from(v).map_err(|e| Error::Other(format!("value: {e}")))
}

/// An Extended JSON object (inserted / replacing document).
fn parse_document(json: &str) -> Result<Document> {
    match parse_value(json)? {
        Bson::Document(d) => Ok(d),
        _ => Err(Error::Other("a document must be a JSON object".into())),
    }
}

/// One generated edit.
#[derive(Clone, Debug)]
pub(crate) enum Planned {
    Mql(Box<Command>),
    /// SQL++ run as it is shown.
    Sqlpp(DocStatement),
    /// PartiQL with parameters.
    Partiql(DocStatement),
}

impl Planned {
    fn preview(&self) -> String {
        match self {
            Self::Mql(c) => preview(c),
            Self::Sqlpp(s) | Self::Partiql(s) => s.preview.clone(),
        }
    }

    fn request(&self) -> Request {
        match self {
            Self::Mql(c) => Request::Mql((**c).clone()),
            Self::Sqlpp(s) => Request::Text {
                text: s.preview.clone(),
            },
            Self::Partiql(s) => {
                let (text, params) = s
                    .parameterized
                    .clone()
                    .unwrap_or_else(|| (s.preview.clone(), Vec::new()));
                Request::Native {
                    method: "ExecuteStatement".into(),
                    args: vec![
                        ("statement".into(), text),
                        (
                            "parameters".into(),
                            serde_json::Value::Array(params).to_string(),
                        ),
                    ],
                }
            }
        }
    }

    /// `{"Statement": …, "Parameters": […]}` of an `ExecuteTransaction`.
    fn transact_statement(&self) -> Option<serde_json::Value> {
        let Self::Partiql(s) = self else { return None };
        let (text, params) = s.parameterized.clone()?;
        Some(serde_json::json!({ "Statement": text, "Parameters": params }))
    }
}

fn id_of(r: &DocResult, ix: usize) -> Result<Bson> {
    r.ids.get(ix).cloned().flatten().ok_or_else(|| {
        Error::Other(match r.dialect {
            Dialect::DynamoDb => format!("item {} cannot be addressed", ix + 1),
            _ => format!("document {} has no _id: it cannot be edited", ix + 1),
        })
    })
}

fn mongo_commands(r: &DocResult, changes: &ChangeSet) -> Result<Vec<Planned>> {
    let id = |ix: usize| id_of(r, ix);
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
                Change::UnsetField { document, path } => {
                    if path == "_id" || path.starts_with("_id.") {
                        return Err(Error::Other("_id cannot be removed".into()));
                    }
                    Op::Update {
                        collection: r.collection.clone(),
                        filter: doc! { "_id": id(*document)? },
                        update: Bson::Document(doc! { "$unset": { path.clone(): "" } }),
                        many: false,
                        options: Document::new(),
                    }
                }
                Change::InsertDocument { json } => Op::Insert {
                    collection: r.collection.clone(),
                    documents: vec![parse_document(json)?],
                    many: false,
                    ordered: true,
                },
                Change::ReplaceDocument { document, json } => {
                    let id = id(*document)?;
                    let mut replacement = parse_document(json)?;
                    match replacement.get("_id") {
                        Some(other) if other != &id => {
                            return Err(Error::Other("_id cannot be changed".into()));
                        }
                        _ => {
                            replacement.remove("_id");
                        }
                    }
                    Op::Replace {
                        collection: r.collection.clone(),
                        filter: doc! { "_id": id },
                        replacement,
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
            Ok(Planned::Mql(Box::new(Command::on(&r.database, op))))
        })
        .collect()
}

fn sqlpp_commands(r: &DocResult, changes: &ChangeSet) -> Result<Vec<Planned>> {
    let ks = r.collection.as_str();
    changes
        .changes
        .iter()
        .map(|c| {
            let s = match c {
                Change::SetField {
                    document,
                    path,
                    value,
                } => {
                    if path == "_id" || path.starts_with("_id.") {
                        return Err(Error::Other("the document key cannot be changed".into()));
                    }
                    doc_edit::sqlpp_set(ks, &id_of(r, *document)?, path, &parse_value(value)?)
                }
                Change::UnsetField { document, path } => {
                    if path == "_id" || path.starts_with("_id.") {
                        return Err(Error::Other("the document key cannot be removed".into()));
                    }
                    doc_edit::sqlpp_unset(ks, &id_of(r, *document)?, path)
                }
                Change::InsertDocument { json } => {
                    doc_edit::sqlpp_insert(ks, &parse_document(json)?).map_err(Error::Other)?
                }
                Change::ReplaceDocument { document, json } => {
                    let id = id_of(r, *document)?;
                    let d = parse_document(json)?;
                    if d.get("_id").is_some_and(|k| k != &id) {
                        return Err(Error::Other("the document key cannot be changed".into()));
                    }
                    doc_edit::sqlpp_replace(ks, &id, &d)
                }
                Change::DeleteDocument { document } => {
                    doc_edit::sqlpp_delete(ks, &id_of(r, *document)?)
                }
                other => {
                    return Err(Error::Unsupported(format!(
                        "{other:?} on a document result"
                    )));
                }
            };
            Ok(Planned::Sqlpp(s))
        })
        .collect()
}

fn partiql_commands(r: &DocResult, key: &[String], changes: &ChangeSet) -> Result<Vec<Planned>> {
    let t = r.collection.as_str();
    let item = |ix: usize| -> Result<Document> {
        match id_of(r, ix)? {
            Bson::Document(d) => Ok(d),
            _ => Err(Error::Other(format!("item {} cannot be addressed", ix + 1))),
        }
    };
    let key_of = |ix: usize| -> Result<Vec<(String, Bson)>> {
        let k = doc_edit::dynamo_key(&item(ix)?, key).map_err(Error::Other)?;
        if k.iter().any(|(_, v)| v == &Bson::Null) {
            return Err(Error::Other(format!("item {} cannot be addressed", ix + 1)));
        }
        Ok(k)
    };
    let e = Error::Other;
    changes
        .changes
        .iter()
        .map(|c| {
            let s = match c {
                Change::SetField {
                    document,
                    path,
                    value,
                } => doc_edit::partiql_set(t, &key_of(*document)?, path, &parse_value(value)?)
                    .map_err(e)?,
                Change::UnsetField { document, path } => {
                    doc_edit::partiql_unset(t, &key_of(*document)?, path).map_err(e)?
                }
                Change::InsertDocument { json } => {
                    let d = parse_document(json)?;
                    doc_edit::dynamo_key(&d, key).map_err(e)?;
                    doc_edit::partiql_insert(t, &d).map_err(e)?
                }
                Change::ReplaceDocument { document, json } => {
                    let old: Vec<String> = item(*document)?.keys().cloned().collect();
                    doc_edit::partiql_replace(t, &key_of(*document)?, &old, &parse_document(json)?)
                        .map_err(e)?
                }
                Change::DeleteDocument { document } => {
                    doc_edit::partiql_delete(t, &key_of(*document)?).map_err(e)?
                }
                other => {
                    return Err(Error::Unsupported(format!(
                        "{other:?} on a document result"
                    )));
                }
            };
            Ok(Planned::Partiql(s))
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
        Op::Insert { documents, .. } => format!(
            "{r}.insertOne({})",
            documents
                .first()
                .map(|d| print_inline(&Bson::Document(d.clone())))
                .unwrap_or_default()
        ),
        Op::Replace {
            filter,
            replacement,
            ..
        } => format!(
            "{r}.replaceOne({}, {})",
            print_inline(&Bson::Document(filter.clone())),
            print_inline(&Bson::Document(replacement.clone()))
        ),
        _ => String::new(),
    }
}

impl Inner {
    fn doc_result(&self, result: ResultId) -> Result<DocResult> {
        self.state().documents.get(&result).cloned().ok_or_else(|| {
            Error::Unsupported(
                "editing this result (it is not a collection's documents, or no longer kept)"
                    .into(),
            )
        })
    }

    /// Key attributes of a DynamoDB table, from the cached catalog.
    fn dynamo_key(&self, r: &DocResult) -> Result<Vec<String>> {
        let ds = self.session_source(r.session)?;
        self.catalogs
            .peek(&ds)
            .and_then(|cat| {
                cat.container(&r.database)?
                    .content
                    .as_ref()?
                    .table(&r.collection)?
                    .primary_key
                    .as_ref()
                    .map(|k| k.columns.clone())
            })
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                Error::Other(format!(
                    "the key schema of `{}` is not loaded yet: expand the table in the \
                     explorer (or wait a moment) and try again",
                    r.collection
                ))
            })
    }

    fn plan(&self, r: &DocResult, changes: &ChangeSet) -> Result<Vec<Planned>> {
        match r.dialect {
            Dialect::Couchbase => sqlpp_commands(r, changes),
            Dialect::DynamoDb => partiql_commands(r, &self.dynamo_key(r)?, changes),
            _ => mongo_commands(r, changes),
        }
    }

    pub(crate) fn preview_documents(
        &self,
        result: ResultId,
        changes: &ChangeSet,
    ) -> Result<Vec<String>> {
        let r = self.doc_result(result)?;
        Ok(self
            .plan(&r, changes)?
            .iter()
            .map(Planned::preview)
            .collect())
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
        let planned = self.plan(&r, &changes)?;
        let statements: Vec<String> = planned.iter().map(Planned::preview).collect();
        let mut conn = self.checkout(&cfg.id).await?;
        // DynamoDB: several edits are one ExecuteTransaction (atomic).
        if r.dialect == Dialect::DynamoDb && (2..=100).contains(&planned.len()) {
            let all: Vec<serde_json::Value> = planned
                .iter()
                .filter_map(Planned::transact_statement)
                .collect();
            let req = Request::Native {
                method: "ExecuteTransaction".into(),
                args: vec![(
                    "statements".into(),
                    serde_json::Value::Array(all).to_string(),
                )],
            };
            let out = collect_request(conn.as_mut(), req, CancellationToken::new()).await;
            self.checkin(&cfg.id, conn);
            let out = out?;
            return Ok(ApplyReport {
                applied: out.summary.affected.unwrap_or(0) as usize,
                statements,
                pending_commit: false,
            });
        }
        let tx = conn.begin().await.is_ok();
        let mut applied = 0;
        for p in &planned {
            match collect_request(conn.as_mut(), p.request(), CancellationToken::new()).await {
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
        if tx && let Err(e) = conn.commit().await {
            self.checkin(&cfg.id, conn);
            return Err(e);
        }
        self.checkin(&cfg.id, conn);
        Ok(ApplyReport {
            applied,
            statements,
            pending_commit: false,
        })
    }

    pub(crate) fn document_edit_info(&self, result: ResultId) -> Result<EditInfo> {
        let r = self.doc_result(result)?;
        let ds = self.session_source(r.session)?;
        let (object, _, _) = result_node(r.dialect, &ds, Some(&r.database), Some(&r.collection))
            .ok_or_else(|| Error::Unsupported("editing this result".into()))?;
        let identity = match r.dialect {
            Dialect::DynamoDb => RowIdentity::Key(self.dynamo_key(&r).unwrap_or_default()),
            _ => RowIdentity::Key(vec!["_id".into()]),
        };
        Ok(EditInfo {
            object,
            columns: Vec::new(),
            identity,
            warning: None,
        })
    }

    /// The quick filter of a document result: `{ path: value }` (MQL) or a
    /// `WHERE` condition (SQL++, PartiQL).
    pub(crate) fn document_value_filter(
        &self,
        result: ResultId,
        path: &str,
        value: &str,
    ) -> Option<Result<String>> {
        let dialect = self.state().documents.get(&result)?.dialect;
        Some(parse_value(value).map(|v| match dialect {
            Dialect::Couchbase => doc_edit::sqlpp_condition(path, &v),
            Dialect::DynamoDb => doc_edit::partiql_condition(path, &v),
            _ => {
                let mut d = Document::new();
                d.insert(path, v);
                print_inline(&Bson::Document(d))
            }
        }))
    }

    fn session_source(&self, s: SessionId) -> Result<ferrodb_api::DataSourceId> {
        self.state()
            .sessions
            .get(&s)
            .map(|x| x.data_source.clone())
            .ok_or_else(|| Error::NotFound(format!("session {s}")))
    }
}

/// `SELECT *` of a Couchbase collection / DynamoDB table node (exports of
/// an explorer object); `None` for other sources.
pub(crate) fn select_all(lang: &ferrodb_api::QueryLanguage, node: &NodeRef) -> Option<String> {
    use ferrodb_api::QueryLanguage as L;
    match (lang, node.path.as_slice()) {
        (L::SqlPlusPlus, [(_, b), (_, s), (_, c), ..]) => Some(format!(
            "SELECT * FROM {}.{}.{}",
            doc_edit::sqlpp_ident(b),
            doc_edit::sqlpp_ident(s),
            doc_edit::sqlpp_ident(c)
        )),
        (L::PartiQl, [.., (ObjectKind::Table, t)]) => {
            Some(format!("SELECT * FROM {}", doc_edit::partiql_ident(t)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(dialect: Dialect, collection: &str, ids: Vec<Option<Bson>>) -> DocResult {
        DocResult {
            session: SessionId(1),
            database: "shop".into(),
            collection: collection.into(),
            ids,
            dialect,
        }
    }

    fn previews(p: Result<Vec<Planned>>) -> Vec<String> {
        p.unwrap().iter().map(Planned::preview).collect()
    }

    #[test]
    fn changes_become_commands() {
        let r = result(Dialect::Mongo, "orders", vec![Some(Bson::Int32(7)), None]);
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
        let cmds = mongo_commands(&r, &set).unwrap();
        assert_eq!(
            cmds[0].preview(),
            "db.orders.updateOne({ _id: 7 }, { $set: { \"address.city\": \"Kraków\" } })"
        );
        assert_eq!(cmds[1].preview(), "db.orders.deleteOne({ _id: 7 })");
        let Planned::Mql(c) = &cmds[0] else { panic!() };
        assert_eq!(c.database.as_deref(), Some("shop"));
        let no_id = ChangeSet {
            changes: vec![Change::DeleteDocument { document: 1 }],
        };
        assert!(mongo_commands(&r, &no_id).is_err());
        let date = ChangeSet {
            changes: vec![Change::SetField {
                document: 0,
                path: "d".into(),
                value: r#"{"$date":"2024-01-02T00:00:00Z"}"#.into(),
            }],
        };
        assert!(
            mongo_commands(&r, &date).unwrap()[0]
                .preview()
                .contains("ISODate(\"2024-01-02T00:00:00Z\")")
        );
        let more = ChangeSet {
            changes: vec![
                Change::UnsetField {
                    document: 0,
                    path: "tmp".into(),
                },
                Change::InsertDocument {
                    json: r#"{"a": 1}"#.into(),
                },
                Change::ReplaceDocument {
                    document: 0,
                    json: r#"{"_id": 7, "b": 2}"#.into(),
                },
            ],
        };
        assert_eq!(
            previews(mongo_commands(&r, &more)),
            vec![
                "db.orders.updateOne({ _id: 7 }, { $unset: { tmp: \"\" } })",
                "db.orders.insertOne({ a: 1 })",
                "db.orders.replaceOne({ _id: 7 }, { b: 2 })",
            ]
        );
        let bad = ChangeSet {
            changes: vec![Change::ReplaceDocument {
                document: 0,
                json: r#"{"_id": 8}"#.into(),
            }],
        };
        assert!(mongo_commands(&r, &bad).is_err());
    }

    #[test]
    fn couchbase_edits_by_key() {
        let r = result(
            Dialect::Couchbase,
            "tck.app.docs",
            vec![Some(Bson::String("k1".into()))],
        );
        let set = ChangeSet {
            changes: vec![
                Change::SetField {
                    document: 0,
                    path: "a.b".into(),
                    value: "[1, 2]".into(),
                },
                Change::UnsetField {
                    document: 0,
                    path: "tmp".into(),
                },
                Change::InsertDocument {
                    json: r#"{"_id": "k2", "x": true}"#.into(),
                },
                Change::ReplaceDocument {
                    document: 0,
                    json: r#"{"_id": "k1", "y": null}"#.into(),
                },
                Change::DeleteDocument { document: 0 },
            ],
        };
        assert_eq!(
            previews(sqlpp_commands(&r, &set)),
            vec![
                r#"UPDATE tck.app.docs USE KEYS "k1" SET a.b = [1, 2]"#,
                r#"UPDATE tck.app.docs USE KEYS "k1" UNSET tmp"#,
                r#"INSERT INTO tck.app.docs (KEY, VALUE) VALUES ("k2", {"x": true})"#,
                r#"UPSERT INTO tck.app.docs (KEY, VALUE) VALUES ("k1", {"y": null})"#,
                r#"DELETE FROM tck.app.docs USE KEYS "k1""#,
            ]
        );
        let key = ChangeSet {
            changes: vec![Change::SetField {
                document: 0,
                path: "_id".into(),
                value: "\"x\"".into(),
            }],
        };
        assert!(sqlpp_commands(&r, &key).is_err());
        let p = sqlpp_commands(&r, &set).unwrap();
        assert!(matches!(p[0].request(), Request::Text { .. }));
    }

    #[test]
    fn dynamodb_edits_by_key_attributes() {
        let raw = RawDocumentBuf::try_from(&doc! {
            "pk": "u#1", "sk": 3, "tags": { "$SS": ["a"] }, "n": "1.5".parse::<bson::Decimal128>().unwrap()
        })
        .unwrap();
        let id = identity(Dialect::DynamoDb, &raw).unwrap();
        assert_eq!(
            id,
            Bson::Document(
                doc! { "pk": "u#1", "sk": 3, "tags": Bson::Null, "n": "1.5".parse::<bson::Decimal128>().unwrap() }
            )
        );
        let r = result(Dialect::DynamoDb, "Orders", vec![Some(id)]);
        let key = vec!["pk".to_owned(), "sk".to_owned()];
        let set = ChangeSet {
            changes: vec![
                Change::SetField {
                    document: 0,
                    path: "tags".into(),
                    value: r#"{"$SS": ["a", "b"]}"#.into(),
                },
                Change::ReplaceDocument {
                    document: 0,
                    json: r#"{"pk": "u#1", "sk": 3, "n": 2}"#.into(),
                },
                Change::DeleteDocument { document: 0 },
            ],
        };
        let p = partiql_commands(&r, &key, &set).unwrap();
        assert_eq!(
            p.iter().map(Planned::preview).collect::<Vec<_>>(),
            vec![
                r#"UPDATE "Orders" SET "tags" = <<'a', 'b'>> WHERE "pk" = 'u#1' AND "sk" = 3"#,
                r#"UPDATE "Orders" SET "n" = 2 REMOVE "tags" WHERE "pk" = 'u#1' AND "sk" = 3"#,
                r#"DELETE FROM "Orders" WHERE "pk" = 'u#1' AND "sk" = 3"#,
            ]
        );
        let Request::Native { method, args } = p[0].request() else {
            panic!()
        };
        assert_eq!(method, "ExecuteStatement");
        assert_eq!(args[1].1, r#"[{"SS":["a","b"]},{"S":"u#1"},{"N":"3"}]"#);
        let ins = ChangeSet {
            changes: vec![Change::InsertDocument {
                json: r#"{"pk": "u#9"}"#.into(),
            }],
        };
        // The sort key is missing.
        assert!(partiql_commands(&r, &key, &ins).is_err());
        let node = select_all(
            &ferrodb_api::QueryLanguage::PartiQl,
            &NodeRef::root("ds".into())
                .child(ObjectKind::Database, "local")
                .child(ObjectKind::Table, "Orders"),
        );
        assert_eq!(node.as_deref(), Some(r#"SELECT * FROM "Orders""#));
        let (n, db, c) = result_node(
            Dialect::Couchbase,
            &"ds".into(),
            Some("tck"),
            Some("tck.app.docs"),
        )
        .unwrap();
        assert_eq!(n.path.len(), 3);
        assert_eq!((db.as_str(), c.as_str()), ("tck", "tck.app.docs"));
    }
}
