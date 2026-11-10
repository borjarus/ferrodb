//! MongoDB introspection (`listDatabases`, `listCollections`,
//! `listIndexes`), collection scripts and shell snippets.

use bson::{Bson, Document, RawDocumentBuf, doc};
use ferrodb_api::{Result, ScriptKind};
use ferrodb_drivers::{BoxFuture, CancellationToken, Connection, Request, sink::collect_request};
use ferrodb_lang::mql::{self, Command, Op, print_inline, print_pretty, quote};

use crate::{Introspector, model::*};

pub struct MongoIntrospector;

/// Runs a command and decodes the documents it returns.
pub async fn documents(conn: &mut dyn Connection, cmd: Command) -> Result<Vec<Document>> {
    let out = collect_request(conn, Request::Mql(cmd), CancellationToken::new()).await?;
    Ok(out
        .documents
        .into_iter()
        .filter_map(|d: RawDocumentBuf| Document::try_from(d).ok())
        .collect())
}

/// `db.name` or `db.getCollection("name")`.
pub fn collection_ref(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !mql::DB_METHODS.contains(&name);
    if plain {
        format!("db.{name}")
    } else {
        format!("db.getCollection({})", quote(name))
    }
}

fn collection_from(info: &Document, indexes: Vec<Document>) -> Collection {
    let name = info.get_str("name").unwrap_or_default().to_owned();
    let kind = match info.get_str("type").unwrap_or("collection") {
        "view" => CollectionKind::View,
        "timeseries" => CollectionKind::TimeSeries,
        _ => CollectionKind::Collection,
    };
    let options = info.get_document("options").cloned().unwrap_or_default();
    let validator = options.get_document("validator").ok().cloned();
    let definition = Some(match kind {
        CollectionKind::View => {
            let source = options.get_str("viewOn").unwrap_or_default();
            let pipeline = options
                .get("pipeline")
                .cloned()
                .unwrap_or(Bson::Array(Vec::new()));
            format!(
                "db.createView({}, {}, {});",
                quote(&name),
                quote(source),
                print_pretty(&pipeline, 78)
            )
        }
        _ if options.is_empty() => format!("db.createCollection({});", quote(&name)),
        _ => format!(
            "db.createCollection({}, {});",
            quote(&name),
            print_pretty(&Bson::Document(options.clone()), 78)
        ),
    });
    let indexes = indexes
        .into_iter()
        .map(|i| {
            let keys = i.get_document("key").cloned().unwrap_or_default();
            let mut opts = i.clone();
            for k in ["v", "key", "ns"] {
                opts.remove(k);
            }
            MongoIndex {
                name: i.get_str("name").unwrap_or_default().to_owned(),
                unique: i.get_bool("unique").unwrap_or(false),
                keys: print_inline(&Bson::Document(keys)),
                options: Some(print_inline(&Bson::Document(opts))),
            }
        })
        .collect();
    Collection {
        name,
        kind,
        options: (!options.is_empty()).then(|| print_pretty(&Bson::Document(options.clone()), 78)),
        validator: validator.map(|v| print_pretty(&Bson::Document(v), 78)),
        indexes,
        definition,
    }
}

impl Introspector for MongoIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::Mongo
    }

    fn containers<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
    ) -> BoxFuture<'a, Result<Vec<Container>>> {
        Box::pin(async move {
            let current = conn.current_schema().await.ok().flatten();
            let mut dbs: Vec<Container> = documents(conn, Command::new(Op::ShowDatabases))
                .await?
                .into_iter()
                .filter_map(|d| d.get_str("name").ok().map(str::to_owned))
                .map(|name| Container {
                    name,
                    comment: None,
                    content: None,
                })
                .collect();
            // The connection's database first (it may not exist yet).
            if let Some(cur) = current {
                match dbs.iter().position(|d| d.name == cur) {
                    Some(i) => {
                        let d = dbs.remove(i);
                        dbs.insert(0, d);
                    }
                    None => dbs.insert(
                        0,
                        Container {
                            name: cur,
                            comment: None,
                            content: None,
                        },
                    ),
                }
            }
            Ok(dbs)
        })
    }

    fn content<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
        container: &'a str,
        only: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Content>> {
        Box::pin(async move {
            let filter = match only {
                Some(n) => doc! { "name": n },
                None => Document::new(),
            };
            let mut infos = documents(
                conn,
                Command::on(
                    container,
                    Op::ListCollections {
                        filter,
                        names_only: false,
                    },
                ),
            )
            .await?;
            infos.sort_by(|a, b| a.get_str("name").ok().cmp(&b.get_str("name").ok()));
            let mut collections = Vec::new();
            for info in infos {
                let name = info.get_str("name").unwrap_or_default().to_owned();
                if name.starts_with("system.") {
                    continue;
                }
                let indexes = if info.get_str("type").unwrap_or("collection") == "view" {
                    Vec::new()
                } else {
                    documents(
                        conn,
                        Command::on(container, Op::GetIndexes { collection: name }),
                    )
                    .await
                    .unwrap_or_default()
                };
                collections.push(collection_from(&info, indexes));
            }
            Ok(Content {
                collections,
                ..Default::default()
            })
        })
    }
}

/// `db.createCollection(…)` and the indexes (except `_id_`).
pub fn collection_script(c: &Collection) -> String {
    let mut out = c
        .definition
        .clone()
        .unwrap_or_else(|| format!("db.createCollection({});", quote(&c.name)));
    out.push('\n');
    for i in c.indexes.iter().filter(|i| i.name != "_id_") {
        out.push_str(&index_script(c, i));
    }
    out
}

pub fn index_script(c: &Collection, i: &MongoIndex) -> String {
    format!(
        "{}.createIndex({}, {});\n",
        collection_ref(&c.name),
        i.keys,
        i.options.as_deref().unwrap_or("{}")
    )
}

/// Every collection and view of a database (views last).
pub fn database_script(content: &Content) -> String {
    let mut cols: Vec<&Collection> = content.collections.iter().collect();
    cols.sort_by_key(|c| c.kind == CollectionKind::View);
    cols.iter()
        .map(|c| collection_script(c))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shell snippets for a collection; `fields` are known field paths.
pub fn script(c: &Collection, kind: ScriptKind, fields: &[String]) -> Option<String> {
    let r = collection_ref(&c.name);
    let top: Vec<&String> = fields
        .iter()
        .filter(|f| !f.contains('.') && f.as_str() != "_id")
        .take(20)
        .collect();
    let placeholder = || {
        if top.is_empty() {
            "{}".to_owned()
        } else {
            format!(
                "{{ {} }}",
                top.iter()
                    .map(|f| format!("{}: null", key(f)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    };
    Some(match kind {
        ScriptKind::Select => format!("{r}.find({{}})"),
        ScriptKind::Insert => format!("{r}.insertOne({})", placeholder()),
        ScriptKind::Update => format!(
            "{r}.updateOne({{ _id: ObjectId(\"\") }}, {{ $set: {} }})",
            placeholder()
        ),
        ScriptKind::Delete => format!("{r}.deleteOne({{ _id: ObjectId(\"\") }})"),
        ScriptKind::Drop => format!("{r}.drop()"),
        ScriptKind::Call => return None,
    })
}

fn key(f: &str) -> String {
    if f.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && f.chars().next().is_some_and(|c| !c.is_ascii_digit())
    {
        f.to_owned()
    } else {
        quote(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_from_listings() {
        let info = doc! { "name": "orders", "type": "collection", "options": {
        "validator": { "$jsonSchema": { "required": ["total"] } } } };
        let idx = vec![
            doc! { "v": 2, "key": { "_id": 1 }, "name": "_id_" },
            doc! { "v": 2, "key": { "customer": 1, "ts": -1 }, "name": "customer_1_ts_-1", "unique": true },
        ];
        let c = collection_from(&info, idx);
        assert_eq!(c.indexes.len(), 2);
        assert!(c.indexes[1].unique);
        let s = collection_script(&c);
        assert_eq!(
            s,
            "db.createCollection(\"orders\", { validator: { $jsonSchema: { required: [\"total\"] } } });\n\
             db.orders.createIndex({ customer: 1, ts: -1 }, { name: \"customer_1_ts_-1\", unique: true });\n"
        );
        for line in s.lines() {
            mql::parse(line).unwrap();
        }
        let v = collection_from(
            &doc! { "name": "big orders", "type": "view", "options": { "viewOn": "orders", "pipeline": [ { "$match": { "total": { "$gt": 100 } } } ] } },
            vec![],
        );
        assert_eq!(v.kind, CollectionKind::View);
        let d = v.definition.unwrap();
        assert!(
            matches!(mql::parse(&d).unwrap().op, Op::CreateView { .. }),
            "{d}"
        );
        assert_eq!(
            collection_ref("big orders"),
            "db.getCollection(\"big orders\")"
        );
        let ins = script(
            &c,
            ScriptKind::Insert,
            &["_id".into(), "total".into(), "a.b".into()],
        )
        .unwrap();
        assert_eq!(ins, "db.orders.insertOne({ total: null })");
        mql::parse(&ins).unwrap();
        mql::parse(&script(&c, ScriptKind::Update, &[]).unwrap()).unwrap_err();
    }
}
