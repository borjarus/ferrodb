//! Couchbase introspection (cluster manager listings of buckets, scopes
//! and collections; GSI indexes from `system:indexes`, through native
//! calls of the driver), scripts (`CREATE SCOPE`, `CREATE COLLECTION`,
//! `CREATE INDEX`) and SQL++ snippets.
//!
//! Model: a container is a bucket; its collections carry their scope
//! ([`Collection::scope`]), so the explorer shows bucket → scope →
//! collection → index. Indexes keep the server's `CREATE INDEX` statement
//! (`options` holds the condition, partitioning and replicas).

use std::collections::HashMap;

use bson::{Bson, Document};
use ferrodb_api::{FieldStats, InferredSchema, Result, ScriptKind};
use ferrodb_drivers::{BoxFuture, Connection};

use crate::{Dialect, Introspector, dynamodb::native, model::*};

pub struct CouchbaseIntrospector;

/// Words SQL++ reserves (quoted as identifiers).
const RESERVED: &[&str] = &[
    "all",
    "and",
    "any",
    "array",
    "as",
    "by",
    "case",
    "collection",
    "default",
    "delete",
    "desc",
    "distinct",
    "else",
    "end",
    "every",
    "exists",
    "false",
    "first",
    "from",
    "group",
    "having",
    "in",
    "index",
    "insert",
    "is",
    "join",
    "key",
    "keys",
    "let",
    "like",
    "limit",
    "missing",
    "not",
    "null",
    "offset",
    "on",
    "or",
    "order",
    "path",
    "primary",
    "scope",
    "select",
    "set",
    "some",
    "then",
    "to",
    "true",
    "unnest",
    "unset",
    "update",
    "upsert",
    "use",
    "user",
    "value",
    "values",
    "when",
    "where",
    "with",
];

/// `name` or `` `name` `` when it is not a plain identifier.
pub fn ident(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain && !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        name.to_owned()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

/// `bucket`.`scope`.`collection`, quoted where needed.
pub fn keyspace(bucket: &str, scope: &str, collection: &str) -> String {
    [bucket, scope, collection]
        .iter()
        .map(|p| ident(p))
        .collect::<Vec<_>>()
        .join(".")
}

/// System scopes the explorer does not show.
fn hidden(scope: &str) -> bool {
    scope == "_system"
}

fn index_of(i: &Document) -> MongoIndex {
    let name = i.get_str("name").unwrap_or_default().to_owned();
    let primary = i.get_bool("is_primary").unwrap_or(false);
    let keys = if primary {
        "PRIMARY".to_owned()
    } else {
        format!(
            "({})",
            i.get_array("index_key")
                .map(|a| a
                    .iter()
                    .filter_map(Bson::as_str)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default()
        )
    };
    let mut opts = Vec::new();
    if let Ok(c) = i.get_str("condition") {
        opts.push(format!("WHERE {c}"));
    }
    if let Ok(p) = i.get_str("partition") {
        opts.push(format!("PARTITION BY {p}"));
    }
    if let Ok(w) = i.get_document("with")
        && let Some(n) = match w.get("num_partition") {
            Some(Bson::Int32(n)) => Some(*n as i64),
            Some(Bson::Int64(n)) => Some(*n),
            _ => None,
        }
        && n > 1
    {
        opts.push(format!("{n} partitions"));
    }
    if let Ok(s) = i.get_str("state")
        && s != "online"
    {
        opts.push(s.to_owned());
    }
    let definition = i
        .get_document("metadata")
        .ok()
        .and_then(|m| m.get_str("definition").ok())
        .map(str::to_owned);
    MongoIndex {
        name,
        keys,
        options: definition.or_else(|| (!opts.is_empty()).then(|| opts.join(", "))),
        unique: false,
    }
}

/// The content of a bucket from the `scopes` listing and its indexes.
pub fn content_from(bucket: &str, scopes: &[Document], indexes: &[Document]) -> Content {
    let mut collections = Vec::new();
    for s in scopes {
        let scope = s.get_str("name").unwrap_or_default();
        if hidden(scope) {
            continue;
        }
        for c in s
            .get_array("collections")
            .map(|a| a.iter().filter_map(Bson::as_document).collect::<Vec<_>>())
            .unwrap_or_default()
        {
            let name = c.get_str("name").unwrap_or_default().to_owned();
            let ttl = match c.get("maxTTL") {
                Some(Bson::Int32(n)) => *n as i64,
                Some(Bson::Int64(n)) => *n,
                _ => 0,
            };
            // Indexes of a collection; the default collection's indexes
            // may be listed without scope / bucket ids (keyspace = bucket).
            let mut idx: Vec<MongoIndex> = indexes
                .iter()
                .filter(|i| {
                    let b = i.get_str("bucket_id").ok();
                    let sc = i.get_str("scope_id").ok();
                    let k = i.get_str("keyspace_id").unwrap_or_default();
                    match (b, sc) {
                        (Some(b), Some(sc)) => b == bucket && sc == scope && k == name,
                        _ => scope == "_default" && name == "_default" && k == bucket,
                    }
                })
                .map(index_of)
                .collect();
            idx.sort_by(|a, b| a.name.cmp(&b.name));
            let mut definition = format!("CREATE COLLECTION {}", keyspace(bucket, scope, &name));
            if ttl > 0 {
                definition.push_str(&format!(" WITH {{\"maxTTL\": {ttl}}}"));
            }
            definition.push(';');
            collections.push(Collection {
                name,
                kind: CollectionKind::Collection,
                options: (ttl > 0).then(|| format!("maxTTL {ttl} s")),
                validator: None,
                indexes: idx,
                definition: Some(definition),
                scope: Some(scope.to_owned()),
            });
        }
    }
    collections.sort_by(|a, b| (&a.scope, &a.name).cmp(&(&b.scope, &b.name)));
    Content {
        collections,
        ..Default::default()
    }
}

impl Introspector for CouchbaseIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::Couchbase
    }

    fn containers<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
    ) -> BoxFuture<'a, Result<Vec<Container>>> {
        Box::pin(async move {
            let current = conn.current_schema().await.ok().flatten();
            let mut list: Vec<Container> = native(conn, "buckets", &[])
                .await?
                .iter()
                .filter_map(|b| {
                    let name = b.get_str("name").ok()?.to_owned();
                    let items = match b.get("itemCount") {
                        Some(Bson::Int64(n)) => Some(*n),
                        Some(Bson::Int32(n)) => Some(*n as i64),
                        _ => None,
                    };
                    Some(Container {
                        name,
                        comment: items.map(|n| format!("{n} items")),
                        content: None,
                    })
                })
                .collect();
            list.sort_by(|a, b| a.name.cmp(&b.name));
            if let Some(cur) = current
                && let Some(i) = list.iter().position(|c| c.name == cur)
            {
                let c = list.remove(i);
                list.insert(0, c);
            }
            Ok(list)
        })
    }

    fn content<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
        container: &'a str,
        only: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Content>> {
        Box::pin(async move {
            let scopes = native(conn, "scopes", &[("bucket", container)]).await?;
            // Without the query / index service the bucket still lists.
            let indexes = native(conn, "indexes", &[("bucket", container)])
                .await
                .unwrap_or_default();
            let mut c = content_from(container, &scopes, &indexes);
            if let Some(name) = only {
                c.collections.retain(|x| x.name == name);
            }
            Ok(c)
        })
    }
}

/// `CREATE COLLECTION` and the indexes (primary first).
pub fn collection_script(bucket: &str, c: &Collection) -> String {
    let scope = c.scope.as_deref().unwrap_or("_default");
    let mut out = c
        .definition
        .clone()
        .unwrap_or_else(|| format!("CREATE COLLECTION {};", keyspace(bucket, scope, &c.name)));
    out.push('\n');
    let mut idx: Vec<&MongoIndex> = c.indexes.iter().collect();
    idx.sort_by_key(|i| i.keys != "PRIMARY");
    for i in idx {
        out.push_str(&index_script(bucket, c, i));
    }
    out
}

pub fn index_script(bucket: &str, c: &Collection, i: &MongoIndex) -> String {
    let ks = keyspace(bucket, c.scope.as_deref().unwrap_or("_default"), &c.name);
    match &i.options {
        Some(d) if d.trim_start().to_ascii_uppercase().starts_with("CREATE") => {
            format!("{};\n", d.trim().trim_end_matches(';'))
        }
        _ if i.keys == "PRIMARY" => format!("CREATE PRIMARY INDEX {} ON {ks};\n", ident(&i.name)),
        opts => format!(
            "CREATE INDEX {} ON {ks}{}{};\n",
            ident(&i.name),
            i.keys,
            opts.as_deref()
                .filter(|o| o.starts_with("WHERE"))
                .map(|o| format!(" {o}"))
                .unwrap_or_default()
        ),
    }
}

/// `CREATE SCOPE` and the scripts of its collections.
pub fn scope_script(bucket: &str, scope: &str, content: &Content) -> String {
    let mut out = String::new();
    if scope != "_default" {
        out.push_str(&format!(
            "CREATE SCOPE {}.{};\n",
            ident(bucket),
            ident(scope)
        ));
    }
    for c in content
        .collections
        .iter()
        .filter(|c| c.scope.as_deref() == Some(scope))
    {
        if c.name == "_default" {
            // Exists with its scope.
            let mut s = collection_script(bucket, c);
            if let Some(nl) = s.find('\n') {
                s.replace_range(..=nl, "");
            }
            out.push_str(&s);
        } else {
            out.push_str(&collection_script(bucket, c));
        }
    }
    out
}

/// Every scope of a bucket (the bucket itself is created by the cluster
/// manager, not SQL++).
pub fn bucket_script(bucket: &str, content: &Content) -> String {
    let mut scopes: Vec<&str> = Vec::new();
    for c in &content.collections {
        let s = c.scope.as_deref().unwrap_or("_default");
        if !scopes.contains(&s) {
            scopes.push(s);
        }
    }
    let mut out = format!("-- bucket {bucket}: couchbase-cli bucket-create --bucket {bucket} …\n");
    for s in scopes {
        out.push_str(&scope_script(bucket, s, content));
    }
    out
}

/// SQL++ snippets for a collection; `fields` are known field paths.
pub fn script(bucket: &str, c: &Collection, kind: ScriptKind, fields: &[String]) -> Option<String> {
    let ks = keyspace(bucket, c.scope.as_deref().unwrap_or("_default"), &c.name);
    let top: Vec<&String> = fields
        .iter()
        .filter(|f| !f.contains('.') && f.as_str() != "_id")
        .take(20)
        .collect();
    let object = if top.is_empty() {
        "{}".to_owned()
    } else {
        format!(
            "{{{}}}",
            top.iter()
                .map(|f| format!("\"{}\": null", f.replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Some(match kind {
        ScriptKind::Select => format!("SELECT * FROM {ks} LIMIT 100"),
        ScriptKind::Insert => format!("INSERT INTO {ks} (KEY, VALUE) VALUES (\"key\", {object})"),
        ScriptKind::Update => format!(
            "UPDATE {ks} USE KEYS \"key\" SET {}",
            if top.is_empty() {
                "field = null".to_owned()
            } else {
                top.iter()
                    .map(|f| format!("{} = null", ident(f)))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
        ScriptKind::Delete => format!("DELETE FROM {ks} USE KEYS \"key\""),
        ScriptKind::Drop => format!("DROP COLLECTION {ks}"),
        ScriptKind::Call => return None,
    })
}

/// `INFER` output (one row with the flavors, wrapped as `{ value: [...] }`
/// by the driver) → field statistics. `None` when it is not an INFER
/// answer. The key (`META().id`) becomes `_id`, as in results.
pub fn infer_schema(rows: &[Document]) -> Option<InferredSchema> {
    let flavors: Vec<&Document> = rows
        .iter()
        .flat_map(|r| match r.get("value") {
            Some(Bson::Array(a)) => a.iter().filter_map(Bson::as_document).collect(),
            _ if r.contains_key("properties") => vec![r],
            _ => Vec::new(),
        })
        .collect();
    if flavors.is_empty() {
        return None;
    }
    let count = |d: &Document, k: &str| match d.get(k) {
        Some(Bson::Int32(n)) => *n as f64,
        Some(Bson::Int64(n)) => *n as f64,
        Some(Bson::Double(n)) => *n,
        _ => 0.0,
    };
    let total: f64 = flavors
        .iter()
        .map(|f| count(f, "#docs"))
        .sum::<f64>()
        .max(1.0);
    // path → (docs, type → docs, samples, order)
    #[derive(Default)]
    struct Acc {
        docs: f64,
        types: HashMap<String, f64>,
        samples: Vec<String>,
        order: usize,
    }
    let mut acc: HashMap<String, Acc> = HashMap::new();
    let mut next = 0;
    fn walk(
        prefix: &str,
        props: &Document,
        acc: &mut HashMap<String, Acc>,
        next: &mut usize,
        count: &dyn Fn(&Document, &str) -> f64,
    ) {
        for (k, p) in props {
            let Some(p) = p.as_document() else { continue };
            if prefix.is_empty() && k == "~meta" {
                continue;
            }
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            let docs = count(p, "#docs");
            let a = acc.entry(path.clone()).or_insert_with(|| {
                *next += 1;
                Acc {
                    order: *next,
                    ..Default::default()
                }
            });
            a.docs += docs;
            let types: Vec<String> = match p.get("type") {
                Some(Bson::String(t)) => vec![t.clone()],
                Some(Bson::Array(ts)) => ts
                    .iter()
                    .filter_map(|t| t.as_str().map(str::to_owned))
                    .collect(),
                _ => Vec::new(),
            };
            let share = docs / types.len().max(1) as f64;
            for t in types {
                *a.types.entry(t).or_default() += share;
            }
            for s in p
                .get_array("samples")
                .map(|x| x.as_slice())
                .unwrap_or_default()
            {
                if a.samples.len() >= 3 {
                    break;
                }
                let text = match s {
                    Bson::String(x) => x.clone(),
                    Bson::Array(_) | Bson::Document(_) => continue,
                    other => other.to_string(),
                };
                if !a.samples.contains(&text) {
                    a.samples.push(text);
                }
            }
            if let Ok(inner) = p.get_document("properties") {
                walk(&path, inner, acc, next, count);
            }
            if let Ok(items) = p.get_document("items")
                && let Ok(inner) = items.get_document("properties")
            {
                walk(&path, inner, acc, next, count);
            }
        }
    }
    for f in &flavors {
        if let Ok(props) = f.get_document("properties") {
            walk("", props, &mut acc, &mut next, &count);
        }
    }
    let mut fields: Vec<(usize, FieldStats)> = acc
        .into_iter()
        .map(|(path, a)| {
            let n: f64 = a.types.values().sum::<f64>().max(1.0);
            let mut types: Vec<(String, f32)> = a
                .types
                .into_iter()
                .map(|(t, c)| (t, (c / n) as f32))
                .collect();
            types.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
            (
                a.order,
                FieldStats {
                    path,
                    types,
                    presence: ((a.docs / total) as f32).min(1.0),
                    samples: a.samples,
                },
            )
        })
        .collect();
    fields.sort_by(|(oa, a), (ob, b)| b.presence.total_cmp(&a.presence).then(oa.cmp(ob)));
    let mut out = vec![FieldStats {
        path: "_id".into(),
        types: vec![("string".into(), 1.0)],
        presence: 1.0,
        samples: Vec::new(),
    }];
    out.extend(fields.into_iter().map(|(_, f)| f));
    Some(InferredSchema {
        sampled: total as usize,
        fields: out,
    })
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    /// Listings recorded from Couchbase Server 8.0 (community).
    fn scopes() -> Vec<Document> {
        vec![
            doc! { "name": "app", "uid": "9", "collections": [ { "name": "docs", "uid": "a", "maxTTL": 0 }, { "name": "events", "uid": "b", "maxTTL": 3600 } ] },
            doc! { "name": "_default", "uid": "0", "collections": [ { "name": "_default", "uid": "0", "maxTTL": 0 } ] },
            doc! { "name": "_system", "uid": "8", "collections": [ { "name": "_query", "uid": "9", "maxTTL": -1 } ] },
        ]
    }

    fn indexes() -> Vec<Document> {
        vec![
            doc! { "bucket_id": "tck", "condition": "(`z` is not missing)", "index_key": ["`z`"], "keyspace_id": "docs",
            "metadata": { "definition": "CREATE INDEX `ix_z` ON `tck`.`app`.`docs`(`z`) WHERE (`z` is not missing)" },
            "name": "ix_z", "scope_id": "app", "state": "online", "using": "gsi", "with": { "num_partition": 1 } },
            doc! { "bucket_id": "tck", "index_key": [], "is_primary": true, "keyspace_id": "docs",
            "metadata": { "definition": "CREATE PRIMARY INDEX `#primary` ON `tck`.`app`.`docs`" },
            "name": "#primary", "scope_id": "app", "state": "online", "using": "gsi" },
            doc! { "keyspace_id": "tck", "index_key": ["`a`"], "name": "legacy", "partition": "HASH(`a`)",
            "state": "deferred", "using": "gsi", "with": { "num_partition": 8 } },
        ]
    }

    #[test]
    fn buckets_scopes_collections_and_indexes() {
        let c = content_from("tck", &scopes(), &indexes());
        let names: Vec<(Option<&str>, &str)> = c
            .collections
            .iter()
            .map(|x| (x.scope.as_deref(), x.name.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                (Some("_default"), "_default"),
                (Some("app"), "docs"),
                (Some("app"), "events")
            ]
        );
        let docs = &c.collections[1];
        assert_eq!(docs.indexes.len(), 2);
        assert_eq!(docs.indexes[0].name, "#primary");
        assert_eq!(docs.indexes[0].keys, "PRIMARY");
        assert_eq!(docs.indexes[1].keys, "(`z`)");
        let legacy = &c.collections[0].indexes[0];
        assert_eq!(legacy.name, "legacy");
        assert_eq!(
            legacy.options.as_deref(),
            Some("PARTITION BY HASH(`a`), 8 partitions, deferred")
        );
        assert_eq!(c.collections[2].options.as_deref(), Some("maxTTL 3600 s"));
        let s = collection_script("tck", docs);
        assert_eq!(
            s,
            "CREATE COLLECTION tck.app.docs;\n\
             CREATE PRIMARY INDEX `#primary` ON `tck`.`app`.`docs`;\n\
             CREATE INDEX `ix_z` ON `tck`.`app`.`docs`(`z`) WHERE (`z` is not missing);\n"
        );
        let b = bucket_script("tck", &c);
        assert!(b.contains("CREATE SCOPE tck.app;"), "{b}");
        assert!(
            !b.contains("CREATE COLLECTION tck._default._default"),
            "{b}"
        );
        assert!(b.contains("CREATE COLLECTION tck.app.events WITH {\"maxTTL\": 3600};"));
        assert!(
            index_script("tck", &c.collections[0], legacy)
                .starts_with("CREATE INDEX legacy ON tck._default._default(`a`)")
        );
    }

    #[test]
    fn infer_output_becomes_field_statistics() {
        // INFER recorded from Couchbase Server 8.0, plus a second flavor.
        let rows = vec![doc! { "value": [
            { "#docs": 3, "Flavor": "`z` = 9", "properties": {
                "z": { "#docs": 3, "%docs": 100, "samples": [9, 10], "type": "number" },
                "addr": { "#docs": 1, "type": "object", "properties": {
                    "city": { "#docs": 1, "samples": ["Kraków"], "type": "string" } } },
                "~meta": { "#docs": 3, "properties": { "id": { "#docs": 3, "type": "string" } } },
            }, "type": "object" },
            { "#docs": 1, "properties": { "z": { "#docs": 1, "samples": ["x"], "type": ["string", "null"] } } },
        ] }];
        let s = infer_schema(&rows).unwrap();
        assert_eq!(s.sampled, 4);
        let paths: Vec<&str> = s.fields.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["_id", "z", "addr", "addr.city"]);
        let z = &s.fields[1];
        assert_eq!(z.presence, 1.0);
        assert_eq!(z.types[0].0, "number");
        assert_eq!(z.samples, vec!["9", "10", "x"]);
        assert_eq!(s.fields[2].presence, 0.25);
        assert!(infer_schema(&[doc! { "a": 1 }]).is_none());
    }

    #[test]
    fn sqlpp_snippets() {
        let c = content_from("travel-sample", &scopes(), &[]);
        let docs = &c.collections[1];
        let f = vec!["_id".into(), "name".into(), "value".into(), "a.b".into()];
        assert_eq!(
            script("travel-sample", docs, ScriptKind::Select, &f).unwrap(),
            "SELECT * FROM `travel-sample`.app.docs LIMIT 100"
        );
        assert_eq!(
            script("travel-sample", docs, ScriptKind::Update, &f).unwrap(),
            "UPDATE `travel-sample`.app.docs USE KEYS \"key\" SET name = null, `value` = null"
        );
        assert_eq!(
            script("travel-sample", docs, ScriptKind::Insert, &f).unwrap(),
            "INSERT INTO `travel-sample`.app.docs (KEY, VALUE) VALUES (\"key\", {\"name\": null, \"value\": null})"
        );
        assert_eq!(
            script("travel-sample", docs, ScriptKind::Drop, &f).unwrap(),
            "DROP COLLECTION `travel-sample`.app.docs"
        );
    }
}
