//! Schema inference of the phase-8 document sources (ADR-011):
//!
//! - Couchbase: `INFER keyspace WITH {"sample_size": n}`; when the query
//!   service cannot infer (no index to sample through, old server), a
//!   sample of `SELECT META(d).id AS _id, d.* … LIMIT n` is analysed like a
//!   MongoDB sample;
//! - DynamoDB: a `Scan` limited to the sample size (it consumes read
//!   capacity: the driver reports the warning and the capacity used); the
//!   key and index attributes of the table are always listed with their
//!   declared types, sampled or not.
//!
//! Field paths go to the same cache as MongoDB's (console completion):
//! key `(source, bucket, "scope.collection")` / `(source, region, table)`.

use bson::Document;
use ferrodb_api::{DataSourceId, Error, FieldStats, InferredSchema, NodeRef, ObjectKind, Result};
use ferrodb_data::doc_edit;
use ferrodb_drivers::{
    CancellationToken, Request,
    sink::{collect, collect_request},
};
use ferrodb_meta::{Dialect, couchbase, infer};

use crate::Inner;

/// The field cache key of an object path: `(database, collection)` —
/// Couchbase collections are named `scope.collection` inside their bucket.
pub(crate) fn field_key(path: &[(ObjectKind, String)]) -> Option<(String, String)> {
    match path {
        [(_, b), (ObjectKind::Scope, s), (_, c), ..] => Some((b.clone(), format!("{s}.{c}"))),
        [(_, db), (_, c), ..] => Some((db.clone(), c.clone())),
        _ => None,
    }
}

fn decode(out: ferrodb_drivers::sink::CollectedOutput) -> Vec<Document> {
    out.documents
        .into_iter()
        .filter_map(|d| Document::try_from(d).ok())
        .collect()
}

/// `{ "$SS": [...] }` paths of a DynamoDB sample become set types of their
/// attribute.
fn dynamo_sets(mut s: InferredSchema) -> InferredSchema {
    for (suffix, name) in [
        (".$SS", "stringSet"),
        (".$NS", "numberSet"),
        (".$BS", "binarySet"),
    ] {
        let sets: Vec<String> = s
            .fields
            .iter()
            .filter_map(|f| f.path.strip_suffix(suffix).map(str::to_owned))
            .collect();
        for parent in sets {
            if let Some(f) = s.fields.iter_mut().find(|f| f.path == parent) {
                for t in &mut f.types {
                    if t.0 == "object" {
                        t.0 = name.to_owned();
                    }
                }
            }
            let child = format!("{parent}{suffix}");
            s.fields.retain(|f| f.path != child);
        }
    }
    s
}

impl Inner {
    /// Schema inference for a Couchbase collection or DynamoDB table node.
    pub(crate) async fn infer_document_source(
        &self,
        d: Dialect,
        node: &NodeRef,
        size: usize,
    ) -> Result<InferredSchema> {
        let ds = node.data_source.clone();
        let unsupported = || Error::Unsupported("schema inference of this object".into());
        let schema = match d {
            Dialect::Couchbase => {
                let parts: Vec<String> = match node.path.as_slice() {
                    [(_, b), (ObjectKind::Scope, s), (_, c), ..] => {
                        vec![b.clone(), s.clone(), c.clone()]
                    }
                    // The document view's container: `bucket.scope.collection`.
                    [(_, ks)] => doc_edit::split_keyspace(ks),
                    _ => return Err(unsupported()),
                };
                let [b, s, c] = parts.as_slice() else {
                    return Err(unsupported());
                };
                let schema = self.sample_couchbase(&ds, b, s, c, size).await?;
                self.remember_fields(&ds, b, &format!("{s}.{c}"), &schema);
                schema
            }
            Dialect::DynamoDb => {
                let (region, table) = match node.path.as_slice() {
                    [(_, r), (_, t), ..] => (r.clone(), t.clone()),
                    [(_, t)] => {
                        let conn = self.meta_connection(&ds).await?;
                        let r = conn
                            .lock()
                            .await
                            .current_schema()
                            .await?
                            .unwrap_or_default();
                        (r, t.clone())
                    }
                    _ => return Err(unsupported()),
                };
                let schema = self.sample_dynamo(&ds, &region, &table, size).await?;
                self.remember_fields(&ds, &region, &table, &schema);
                schema
            }
            _ => return Err(unsupported()),
        };
        Ok(schema)
    }

    fn remember_fields(&self, ds: &DataSourceId, db: &str, coll: &str, s: &InferredSchema) {
        if let Ok(mut m) = self.fields.lock() {
            m.insert(
                (ds.clone(), db.to_owned(), coll.to_owned()),
                s.fields.iter().map(|f| f.path.clone()).collect(),
            );
        }
    }

    async fn sample_couchbase(
        &self,
        ds: &DataSourceId,
        bucket: &str,
        scope: &str,
        collection: &str,
        size: usize,
    ) -> Result<InferredSchema> {
        let ks = couchbase::keyspace(bucket, scope, collection);
        let infer_stmt =
            format!("INFER {ks} WITH {{\"sample_size\": {size}, \"num_sample_values\": 3}}");
        let inferred = self
            .with_meta(ds, move |c| {
                let stmt = infer_stmt.clone();
                Box::pin(async move { collect(c, stmt, CancellationToken::new()).await })
            })
            .await
            .ok()
            .map(decode)
            .and_then(|rows| couchbase::infer_schema(&rows));
        if let Some(s) = inferred {
            return Ok(s);
        }
        let sample = format!("SELECT META(d).id AS _id, d.* FROM {ks} AS d LIMIT {size}");
        let docs = self
            .with_meta(ds, move |c| {
                let stmt = sample.clone();
                Box::pin(async move { collect(c, stmt, CancellationToken::new()).await })
            })
            .await
            .map(decode)?;
        Ok(infer::infer(&docs))
    }

    async fn sample_dynamo(
        &self,
        ds: &DataSourceId,
        region: &str,
        table: &str,
        size: usize,
    ) -> Result<InferredSchema> {
        let (t, n) = (table.to_owned(), size.to_string());
        tracing::info!(
            table,
            size,
            "DynamoDB schema sample: a Scan consumes read capacity"
        );
        let docs = self
            .with_meta(ds, move |c| {
                let req = Request::native("Scan", &[("table", &t), ("limit", &n)]);
                Box::pin(async move { collect_request(c, req, CancellationToken::new()).await })
            })
            .await
            .map(decode)?;
        let mut s = dynamo_sets(infer::infer(&docs));
        // Key and index attributes are declared: always listed.
        let cat = self
            .ensure_table(ds, Dialect::DynamoDb, region, table)
            .await?;
        if let Some(t) = cat
            .container(region)
            .and_then(|c| c.content.as_ref())
            .and_then(|c| c.table(table))
        {
            let key = t
                .primary_key
                .as_ref()
                .map(|k| k.columns.clone())
                .unwrap_or_default();
            for (i, col) in t.columns.iter().enumerate() {
                let ty = match col.data_type.chars().next() {
                    Some('S') => "string",
                    Some('N') => "number",
                    Some('B') => "binData",
                    _ => "unknown",
                };
                match s.fields.iter().position(|f| f.path == col.name) {
                    Some(at) => {
                        let f = s.fields.remove(at);
                        s.fields.insert(i.min(s.fields.len()), f);
                    }
                    None => s.fields.insert(
                        i.min(s.fields.len()),
                        FieldStats {
                            path: col.name.clone(),
                            types: vec![(ty.to_owned(), 1.0)],
                            presence: if key.contains(&col.name) { 1.0 } else { 0.0 },
                            samples: Vec::new(),
                        },
                    ),
                }
            }
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use bson::{Bson, doc};

    use super::*;

    #[test]
    fn field_keys_and_sets() {
        let p = |v: &[(ObjectKind, &str)]| -> Vec<(ObjectKind, String)> {
            v.iter().map(|(k, n)| (*k, (*n).to_owned())).collect()
        };
        assert_eq!(
            field_key(&p(&[
                (ObjectKind::Bucket, "b"),
                (ObjectKind::Scope, "s"),
                (ObjectKind::Collection, "c")
            ])),
            Some(("b".into(), "s.c".into()))
        );
        assert_eq!(
            field_key(&p(&[(ObjectKind::Database, "r"), (ObjectKind::Table, "t")])),
            Some(("r".into(), "t".into()))
        );
        let s = dynamo_sets(infer::infer(&[
            doc! { "pk": "a", "tags": { "$SS": ["x"] } },
            doc! { "pk": "b", "n": Bson::Int32(1) },
        ]));
        let paths: Vec<&str> = s.fields.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["pk", "tags", "n"]);
        assert_eq!(s.fields[1].types[0].0, "stringSet");
    }
}
