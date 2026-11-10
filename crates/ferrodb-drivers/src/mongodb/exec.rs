//! Shell statements as database commands. Reads stream raw documents from
//! cursors; writes return one result document (as mongosh prints it) and
//! the affected count.

use bson::{Bson, Document, RawDocumentBuf, doc, oid::ObjectId};
use ferrodb_api::{Error, NoticeLevel, QueryError, Result};
use ferrodb_lang::mql::{Command, FindAndModify, Op};

use super::conn::MongoConnection;
use crate::{ExecSummary, ResultSink, SinkEvent};

fn raw(d: &Document) -> Result<RawDocumentBuf> {
    RawDocumentBuf::try_from(d).map_err(|e| Error::Other(format!("BSON: {e}")))
}

/// Server errors reported inside a write response (`writeErrors`).
fn check_write(resp: &Document) -> Result<()> {
    let first = resp
        .get_array("writeErrors")
        .ok()
        .and_then(|a| a.first())
        .and_then(Bson::as_document)
        .or_else(|| resp.get_document("writeConcernError").ok());
    match first {
        Some(e) => Err(Error::Query(QueryError {
            message: e.get_str("errmsg").unwrap_or("write failed").to_owned(),
            code: e
                .get("code")
                .map(|c| c.to_string())
                .map(|c| match e.get_str("codeName") {
                    Ok(n) => format!("{c} {n}"),
                    Err(_) => c,
                }),
            range: None,
        })),
        None => Ok(()),
    }
}

fn count(resp: &Document, key: &str) -> i64 {
    match resp.get(key) {
        Some(Bson::Int32(v)) => *v as i64,
        Some(Bson::Int64(v)) => *v,
        Some(Bson::Double(v)) => *v as i64,
        _ => 0,
    }
}

struct Out<'a> {
    sink: &'a ResultSink,
    db: String,
}

impl Out<'_> {
    async fn begin(&self, collection: Option<&str>) -> bool {
        self.sink
            .send(SinkEvent::BeginDocuments {
                database: Some(self.db.clone()),
                collection: collection.map(str::to_owned),
            })
            .await
            .is_ok()
    }

    /// A result of a few documents.
    async fn docs(&self, collection: Option<&str>, docs: Vec<Document>) -> Result<ExecSummary> {
        if !self.begin(collection).await {
            return Ok(ExecSummary {
                truncated: true,
                results: 1,
                ..Default::default()
            });
        }
        let docs = docs.iter().map(raw).collect::<Result<Vec<_>>>()?;
        let truncated =
            !docs.is_empty() && self.sink.send(SinkEvent::Documents(docs)).await.is_err();
        let _ = self.sink.send(SinkEvent::End).await;
        Ok(ExecSummary {
            results: 1,
            truncated,
            ..Default::default()
        })
    }

    async fn write(
        &self,
        collection: &str,
        result: Document,
        affected: i64,
    ) -> Result<ExecSummary> {
        let mut s = self.docs(Some(collection), vec![result]).await?;
        s.affected = Some(affected.max(0) as u64);
        Ok(s)
    }
}

/// Runs a cursor command and streams its documents (raw, no decoding).
async fn stream(
    c: &mut MongoConnection,
    out: &Out<'_>,
    collection: Option<&str>,
    cmd: Document,
    limit: Option<usize>,
) -> Result<ExecSummary> {
    let d = c.client.database(&out.db);
    if !out.begin(collection).await {
        return Ok(ExecSummary {
            truncated: true,
            results: 1,
            ..Default::default()
        });
    }
    let batch = out.sink.batch_rows();
    let mut buf: Vec<RawDocumentBuf> = Vec::with_capacity(batch.min(1024));
    let mut n = 0usize;
    let mut truncated = false;
    macro_rules! drain {
        ($advance:expr, $cursor:ident) => {
            loop {
                if limit.is_some_and(|l| n >= l) {
                    break;
                }
                let more = $advance;
                let more = c.check(more)?;
                if !more {
                    break;
                }
                buf.push($cursor.current().to_owned());
                n += 1;
                if buf.len() >= batch
                    && out
                        .sink
                        .send(SinkEvent::Documents(std::mem::take(&mut buf)))
                        .await
                        .is_err()
                {
                    truncated = true;
                    break;
                }
            }
        };
    }
    if c.session.is_some() {
        let r = {
            let s = c.session.as_mut().expect("session is open");
            d.run_cursor_command(cmd).session(s).await
        };
        let mut cursor = c.check(r)?;
        loop {
            if limit.is_some_and(|l| n >= l) {
                break;
            }
            let sess = c.session.as_mut().expect("session is open");
            let more = cursor.advance(sess).await;
            if !c.check(more)? {
                break;
            }
            buf.push(cursor.current().to_owned());
            n += 1;
            if buf.len() >= batch
                && out
                    .sink
                    .send(SinkEvent::Documents(std::mem::take(&mut buf)))
                    .await
                    .is_err()
            {
                truncated = true;
                break;
            }
        }
    } else {
        let r = d.run_cursor_command(cmd).await;
        let mut cursor = c.check(r)?;
        drain!(cursor.advance().await, cursor);
    }
    if !truncated && !buf.is_empty() && out.sink.send(SinkEvent::Documents(buf)).await.is_err() {
        truncated = true;
    }
    let _ = out.sink.send(SinkEvent::End).await;
    Ok(ExecSummary {
        affected: None,
        results: 1,
        truncated,
    })
}

fn with_comment(mut cmd: Document, options: &Document, tag: &str) -> Document {
    if !options.contains_key("comment") {
        cmd.insert("comment", tag);
    }
    cmd
}

/// mongosh's default index name: `a_1_b_-1`.
pub fn index_name(keys: &Document) -> String {
    keys.iter()
        .map(|(k, v)| {
            let v = match v {
                Bson::Int32(i) => i.to_string(),
                Bson::Int64(i) => i.to_string(),
                Bson::Double(d) => d.to_string(),
                Bson::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{k}_{v}")
        })
        .collect::<Vec<_>>()
        .join("_")
}

pub(crate) async fn run(
    c: &mut MongoConnection,
    cmd: Command,
    sink: &ResultSink,
    tag: &str,
) -> Result<ExecSummary> {
    let db = cmd.database.clone().unwrap_or_else(|| c.db.clone());
    let out = Out {
        sink,
        db: db.clone(),
    };
    match cmd.op {
        Op::Use(name) => {
            c.db = name.clone();
            sink.notice(NoticeLevel::Info, format!("switched to db {name}"))
                .await;
            Ok(ExecSummary::default())
        }
        Op::ShowDatabases => {
            let r = c.command("admin", doc! { "listDatabases": 1 }).await?;
            let dbs: Vec<Document> = r
                .get_array("databases")
                .map(|a| a.iter().filter_map(Bson::as_document).cloned().collect())
                .unwrap_or_default();
            out.docs(None, dbs).await
        }
        Op::ShowCollections => {
            stream(
                c,
                &out,
                None,
                doc! { "listCollections": 1, "nameOnly": true, "authorizedCollections": true },
                None,
            )
            .await
        }
        Op::ListCollections { filter, names_only } => {
            stream(
                c,
                &out,
                None,
                doc! { "listCollections": 1, "filter": filter, "nameOnly": names_only, "authorizedCollections": true },
                None,
            )
            .await
        }
        Op::Find {
            collection,
            filter,
            projection,
            sort,
            skip,
            limit,
            mut options,
            one,
            count: as_count,
        } => {
            if as_count {
                let mut pipeline = vec![doc! { "$match": filter }];
                if let Some(s) = skip {
                    pipeline.push(doc! { "$skip": s as i64 });
                }
                if let Some(l) = limit.filter(|l| *l != 0) {
                    pipeline.push(doc! { "$limit": l.abs() });
                }
                pipeline.push(doc! { "$count": "count" });
                let r = c
                    .command(
                        &db,
                        with_comment(
                            doc! { "aggregate": &collection, "pipeline": pipeline, "cursor": {} },
                            &options,
                            tag,
                        ),
                    )
                    .await?;
                let n = r
                    .get_document("cursor")
                    .ok()
                    .and_then(|cur| cur.get_array("firstBatch").ok())
                    .and_then(|b| b.first())
                    .and_then(Bson::as_document)
                    .map_or(0, |d| count(d, "count"));
                return out.docs(Some(&collection), vec![doc! { "count": n }]).await;
            }
            let mut f = doc! { "find": &collection, "filter": filter };
            if let Some(p) = projection {
                f.insert("projection", p);
            }
            if let Some(s) = sort {
                f.insert("sort", s);
            }
            if let Some(s) = skip {
                f.insert("skip", s as i64);
            }
            let limit = if one { Some(1) } else { limit.filter(|l| *l != 0) };
            if let Some(l) = limit {
                f.insert("limit", l.abs());
                if l < 0 || one {
                    f.insert("singleBatch", true);
                }
            }
            if let Some(Bson::Document(h)) = options.get("hint").cloned() {
                options.insert("hint", h);
            }
            f = with_comment(f, &options, tag);
            f.extend(options);
            stream(c, &out, Some(&collection), f, limit.map(|l| l.unsigned_abs() as usize))
                .await
        }
        Op::Aggregate {
            collection,
            pipeline,
            mut options,
        } => {
            let mut cursor = Document::new();
            if let Some(b) = options.remove("batchSize") {
                cursor.insert("batchSize", b);
            }
            let target = match &collection {
                Some(cname) => Bson::String(cname.clone()),
                None => Bson::Int32(1),
            };
            let mut a = doc! { "aggregate": target, "pipeline": pipeline, "cursor": cursor };
            a = with_comment(a, &options, tag);
            a.extend(options);
            stream(c, &out, collection.as_deref(), a, None).await
        }
        Op::CountDocuments {
            collection,
            filter,
            options,
        } => {
            let mut pipeline = vec![doc! { "$match": filter }];
            if let Some(s) = options.get("skip") {
                pipeline.push(doc! { "$skip": s.clone() });
            }
            if let Some(l) = options.get("limit") {
                pipeline.push(doc! { "$limit": l.clone() });
            }
            pipeline.push(doc! { "$group": { "_id": 1, "n": { "$sum": 1 } } });
            let r = c
                .command(
                    &db,
                    with_comment(
                        doc! { "aggregate": &collection, "pipeline": pipeline, "cursor": {} },
                        &options,
                        tag,
                    ),
                )
                .await?;
            let n = r
                .get_document("cursor")
                .ok()
                .and_then(|cur| cur.get_array("firstBatch").ok())
                .and_then(|b| b.first())
                .and_then(Bson::as_document)
                .map_or(0, |d| count(d, "n"));
            out.docs(Some(&collection), vec![doc! { "count": n }]).await
        }
        Op::EstimatedCount { collection } => {
            let r = c.command(&db, doc! { "count": &collection }).await?;
            out.docs(Some(&collection), vec![doc! { "count": count(&r, "n") }])
                .await
        }
        Op::Distinct {
            collection,
            field,
            filter,
        } => {
            let r = c
                .command(
                    &db,
                    with_comment(
                        doc! { "distinct": &collection, "key": &field, "query": filter },
                        &Document::new(),
                        tag,
                    ),
                )
                .await?;
            let docs = r
                .get_array("values")
                .map(|v| v.iter().map(|x| doc! { field.clone(): x.clone() }).collect())
                .unwrap_or_default();
            out.docs(Some(&collection), docs).await
        }
        Op::Insert {
            collection,
            mut documents,
            many,
            ordered,
        } => {
            let mut ids = Vec::with_capacity(documents.len());
            for d in &mut documents {
                let id = match d.get("_id") {
                    Some(id) => id.clone(),
                    None => {
                        let id = Bson::ObjectId(ObjectId::new());
                        let mut with_id = doc! { "_id": id.clone() };
                        with_id.extend(std::mem::take(d));
                        *d = with_id;
                        id
                    }
                };
                ids.push(id);
            }
            let r = c
                .command(
                    &db,
                    with_comment(
                        doc! { "insert": &collection, "documents": documents, "ordered": ordered },
                        &Document::new(),
                        tag,
                    ),
                )
                .await?;
            check_write(&r)?;
            let n = count(&r, "n");
            let result = if many {
                let mut m = Document::new();
                for (i, id) in ids.into_iter().enumerate() {
                    m.insert(i.to_string(), id);
                }
                doc! { "acknowledged": true, "insertedIds": m }
            } else {
                doc! { "acknowledged": true, "insertedId": ids.into_iter().next().unwrap_or(Bson::Null) }
            };
            out.write(&collection, result, n).await
        }
        Op::Update {
            collection,
            filter,
            update,
            many,
            mut options,
        } => {
            let mut st = doc! { "q": filter, "u": update, "multi": many };
            for k in ["upsert", "arrayFilters", "collation", "hint"] {
                if let Some(v) = options.remove(k) {
                    st.insert(k, v);
                }
            }
            update_result(c, &out, &db, &collection, st, options, tag).await
        }
        Op::Replace {
            collection,
            filter,
            replacement,
            mut options,
        } => {
            let mut st = doc! { "q": filter, "u": replacement, "multi": false };
            for k in ["upsert", "collation", "hint"] {
                if let Some(v) = options.remove(k) {
                    st.insert(k, v);
                }
            }
            update_result(c, &out, &db, &collection, st, options, tag).await
        }
        Op::Delete {
            collection,
            filter,
            many,
        } => {
            let r = c
                .command(
                    &db,
                    with_comment(
                        doc! { "delete": &collection, "deletes": [ { "q": filter, "limit": if many { 0 } else { 1 } } ] },
                        &Document::new(),
                        tag,
                    ),
                )
                .await?;
            check_write(&r)?;
            let n = count(&r, "n");
            out.write(
                &collection,
                doc! { "acknowledged": true, "deletedCount": n },
                n,
            )
            .await
        }
        Op::FindAndModify {
            collection,
            kind,
            filter,
            argument,
            options,
        } => {
            let mut f = doc! { "findAndModify": &collection, "query": filter };
            match kind {
                FindAndModify::Delete => {
                    f.insert("remove", true);
                }
                _ => {
                    f.insert("update", argument);
                }
            }
            let after = options.get_str("returnDocument").is_ok_and(|r| r == "after")
                || options.get_bool("returnNewDocument").unwrap_or(false)
                || options.get_bool("new").unwrap_or(false);
            if kind != FindAndModify::Delete {
                f.insert("new", after);
            }
            for (k, to) in [
                ("projection", "fields"),
                ("sort", "sort"),
                ("upsert", "upsert"),
                ("arrayFilters", "arrayFilters"),
                ("collation", "collation"),
                ("hint", "hint"),
            ] {
                if let Some(v) = options.get(k) {
                    f.insert(to, v.clone());
                }
            }
            let r = c.command(&db, with_comment(f, &options, tag)).await?;
            let n = r
                .get_document("lastErrorObject")
                .map_or(0, |l| count(l, "n"));
            let docs = match r.get("value") {
                Some(Bson::Document(d)) => vec![d.clone()],
                _ => Vec::new(),
            };
            let mut s = out.docs(Some(&collection), docs).await?;
            s.affected = Some(n.max(0) as u64);
            Ok(s)
        }
        Op::CreateIndex {
            collection,
            keys,
            options,
        } => {
            let indexes: Vec<Document> = keys
                .into_iter()
                .map(|k| {
                    let mut i = doc! { "key": k.clone() };
                    if !options.contains_key("name") {
                        i.insert("name", index_name(&k));
                    }
                    i.extend(options.clone());
                    i
                })
                .collect();
            let r = c
                .command(&db, doc! { "createIndexes": &collection, "indexes": indexes })
                .await?;
            out.docs(Some(&collection), vec![r]).await
        }
        Op::DropIndex { collection, index } => {
            let r = c
                .command(
                    &db,
                    doc! { "dropIndexes": &collection, "index": index.unwrap_or(Bson::String("*".into())) },
                )
                .await?;
            out.docs(Some(&collection), vec![r]).await
        }
        Op::GetIndexes { collection } => {
            stream(c, &out, Some(&collection), doc! { "listIndexes": &collection }, None).await
        }
        Op::DropCollection { collection } => {
            let r = c.command(&db, doc! { "drop": &collection }).await?;
            out.docs(Some(&collection), vec![r]).await
        }
        Op::RenameCollection {
            collection,
            to,
            drop_target,
        } => {
            let r = c
                .command(
                    "admin",
                    doc! {
                        "renameCollection": format!("{db}.{collection}"),
                        "to": format!("{db}.{to}"),
                        "dropTarget": drop_target,
                    },
                )
                .await?;
            out.docs(None, vec![r]).await
        }
        Op::CreateCollection { name, options } => {
            let mut cmd = doc! { "create": &name };
            cmd.extend(options);
            let r = c.command(&db, cmd).await?;
            out.docs(None, vec![r]).await
        }
        Op::CreateView {
            name,
            source,
            pipeline,
            options,
        } => {
            let mut cmd = doc! { "create": &name, "viewOn": source, "pipeline": pipeline };
            cmd.extend(options);
            let r = c.command(&db, cmd).await?;
            out.docs(None, vec![r]).await
        }
        Op::DropDatabase => {
            let r = c.command(&db, doc! { "dropDatabase": 1 }).await?;
            out.docs(None, vec![r]).await
        }
        Op::RunCommand { command, admin } => {
            let target = if admin { "admin".to_owned() } else { db.clone() };
            let r = c.command(&target, command).await?;
            let out = Out { sink, db: target };
            out.docs(None, vec![r]).await
        }
    }
}

async fn update_result(
    c: &mut MongoConnection,
    out: &Out<'_>,
    db: &str,
    collection: &str,
    statement: Document,
    options: Document,
    tag: &str,
) -> Result<ExecSummary> {
    let mut cmd = doc! { "update": collection, "updates": [statement] };
    cmd = with_comment(cmd, &options, tag);
    let r = c.command(db, cmd).await?;
    check_write(&r)?;
    let n = count(&r, "n");
    let modified = count(&r, "nModified");
    let upserted: Vec<Bson> = r
        .get_array("upserted")
        .map(|u| {
            u.iter()
                .filter_map(|x| x.as_document().and_then(|d| d.get("_id")).cloned())
                .collect()
        })
        .unwrap_or_default();
    let mut result = doc! {
        "acknowledged": true,
        "matchedCount": n - upserted.len() as i64,
        "modifiedCount": modified,
        "upsertedCount": upserted.len() as i64,
    };
    if let Some(id) = upserted.first() {
        result.insert("upsertedId", id.clone());
    }
    out.write(collection, result, modified + upserted.len() as i64)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_names_and_write_errors() {
        assert_eq!(index_name(&doc! { "a": 1, "b": -1 }), "a_1_b_-1");
        assert_eq!(index_name(&doc! { "t": "text" }), "t_text");
        let e = check_write(&doc! { "n": 0, "writeErrors": [ { "index": 0, "code": 11000, "errmsg": "E11000 duplicate key" } ] })
            .unwrap_err();
        assert!(matches!(e, Error::Query(q) if q.code.as_deref() == Some("11000")));
        assert!(check_write(&doc! { "n": 1 }).is_ok());
    }
}
