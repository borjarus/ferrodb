//! DynamoDB introspection (`ListTables`, `DescribeTable` +
//! `DescribeTimeToLive` through native calls of the driver), table scripts
//! and PartiQL snippets.
//!
//! Model: one container (the region); a table is a [`Table`] whose columns
//! are the attribute definitions (key attributes of the table and of its
//! indexes, the only typed attributes), whose primary key is the key schema
//! (partition key, then sort key) and whose indexes are the GSIs and LSIs
//! (`method` = `GSI` / `LSI` and the projection; `include` = projected
//! non-key attributes). The billing mode is the comment (shown next to the
//! table in the explorer), the item count the row estimate, and
//! `server_sql` keeps the script: a `CreateTable {…}` request runnable in
//! a DynamoDB console, with TTL and streams noted.

use bson::{Bson, Document};
use ferrodb_api::{Result, ScriptKind};
use ferrodb_drivers::{BoxFuture, CancellationToken, Connection, Request, sink::collect_request};
use serde_json::{Map, Value as Json, json};

use crate::{Introspector, model::*};

pub struct DynamoIntrospector;

/// Runs a native call and decodes its documents.
pub async fn native(
    conn: &mut dyn Connection,
    method: &str,
    args: &[(&str, &str)],
) -> Result<Vec<Document>> {
    let out = collect_request(
        conn,
        Request::native(method, args),
        CancellationToken::new(),
    )
    .await?;
    Ok(out
        .documents
        .into_iter()
        .filter_map(|d| Document::try_from(d).ok())
        .collect())
}

fn key_names(ks: Option<&Vec<Bson>>) -> (Option<String>, Option<String>) {
    let mut p = None;
    let mut s = None;
    for k in ks.into_iter().flatten().filter_map(Bson::as_document) {
        let name = k.get_str("AttributeName").ok().map(str::to_owned);
        match k.get_str("KeyType") {
            Ok("HASH") => p = name,
            _ => s = name,
        }
    }
    (p, s)
}

fn type_label(t: &str) -> &'static str {
    match t {
        "S" => "S (string)",
        "N" => "N (number)",
        "B" => "B (binary)",
        _ => "?",
    }
}

/// A `DescribeTable` document → the table model.
pub fn table_from(d: &Document) -> Table {
    let name = d.get_str("TableName").unwrap_or_default().to_owned();
    let (pk, sk) = key_names(d.get_array("KeySchema").ok());
    let mut columns: Vec<Column> = d
        .get_array("AttributeDefinitions")
        .map(|a| a.iter().filter_map(Bson::as_document).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|a| {
            let n = a.get_str("AttributeName").unwrap_or_default().to_owned();
            let role = if Some(&n) == pk.as_ref() {
                Some("partition key")
            } else if Some(&n) == sk.as_ref() {
                Some("sort key")
            } else {
                Some("index key")
            };
            Column {
                data_type: type_label(a.get_str("AttributeType").unwrap_or_default()).to_owned(),
                comment: role.map(str::to_owned),
                name: n,
                ..Default::default()
            }
        })
        .collect();
    // Partition key, sort key, then the index keys.
    let rank = |c: &Column| match c.comment.as_deref() {
        Some("partition key") => 0,
        Some("sort key") => 1,
        _ => 2,
    };
    columns.sort_by_key(rank);
    let mut indexes = Vec::new();
    for (field, kind) in [
        ("GlobalSecondaryIndexes", "GSI"),
        ("LocalSecondaryIndexes", "LSI"),
    ] {
        for i in d
            .get_array(field)
            .map(|a| a.iter().filter_map(Bson::as_document).collect::<Vec<_>>())
            .unwrap_or_default()
        {
            let (p, s) = key_names(i.get_array("KeySchema").ok());
            let projection = i.get_document("Projection").ok();
            let ptype = projection
                .and_then(|p| p.get_str("ProjectionType").ok())
                .unwrap_or("ALL");
            indexes.push(Index {
                name: i.get_str("IndexName").unwrap_or_default().to_owned(),
                columns: p.into_iter().chain(s).collect(),
                include: projection
                    .and_then(|p| p.get_array("NonKeyAttributes").ok())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
                unique: false,
                method: Some(format!("{kind} {ptype}")),
                predicate: None,
                server_sql: None,
            });
        }
    }
    let billing = d.get_str("BillingMode").unwrap_or("PROVISIONED").to_owned();
    let rows = match d.get("ItemCount") {
        Some(Bson::Int64(n)) => Some(*n),
        Some(Bson::Int32(n)) => Some(*n as i64),
        _ => None,
    };
    Table {
        primary_key: pk.clone().map(|p| Key {
            name: None,
            columns: std::iter::once(p).chain(sk.clone()).collect(),
        }),
        columns,
        indexes,
        comment: Some(billing),
        rows,
        server_sql: Some(create_table_script(d)),
        name,
        ..Default::default()
    }
}

fn bson_json(v: &Bson) -> Json {
    v.clone().into_relaxed_extjson()
}

/// The `CreateTable` request for a described table, plus TTL / streams /
/// sizes as comments.
pub fn create_table_script(d: &Document) -> String {
    let mut req = Map::new();
    for k in [
        "TableName",
        "AttributeDefinitions",
        "KeySchema",
        "BillingMode",
        "ProvisionedThroughput",
        "StreamSpecification",
        "TableClass",
        "DeletionProtectionEnabled",
    ] {
        if let Some(v) = d.get(k) {
            req.insert(k.to_owned(), bson_json(v));
        }
    }
    if req.get("BillingMode").and_then(Json::as_str) == Some("PAY_PER_REQUEST") {
        req.remove("ProvisionedThroughput");
    }
    for k in ["GlobalSecondaryIndexes", "LocalSecondaryIndexes"] {
        if let Ok(list) = d.get_array(k) {
            let items: Vec<Json> = list
                .iter()
                .filter_map(Bson::as_document)
                .map(|i| {
                    let mut m = Map::new();
                    for f in [
                        "IndexName",
                        "KeySchema",
                        "Projection",
                        "ProvisionedThroughput",
                    ] {
                        if let Some(v) = i.get(f) {
                            m.insert(f.to_owned(), bson_json(v));
                        }
                    }
                    Json::Object(m)
                })
                .collect();
            req.insert(k.to_owned(), Json::Array(items));
        }
    }
    let name = d.get_str("TableName").unwrap_or_default();
    let mut out = format!("-- aws dynamodb create-table --cli-input-json file://{name}.json\n");
    if let Ok(ttl) = d.get_document("TimeToLive")
        && let Ok(attr) = ttl.get_str("AttributeName")
    {
        out.push_str(&format!(
            "-- TTL {} on `{attr}`: aws dynamodb update-time-to-live --table-name {name} \
             --time-to-live-specification Enabled=true,AttributeName={attr}\n",
            ttl.get_str("TimeToLiveStatus").unwrap_or("ENABLED")
        ));
    }
    if let Ok(arn) = d.get_str("LatestStreamArn") {
        out.push_str(&format!("-- stream: {arn}\n"));
    }
    if let (Some(n), Some(b)) = (d.get("ItemCount"), d.get("TableSizeBytes")) {
        out.push_str(&format!("-- {n} items, {b} bytes\n"));
    }
    out.push_str("CreateTable ");
    out.push_str(&serde_json::to_string_pretty(&Json::Object(req)).unwrap_or_default());
    out.push('\n');
    out
}

impl Introspector for DynamoIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::DynamoDb
    }

    fn containers<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
    ) -> BoxFuture<'a, Result<Vec<Container>>> {
        Box::pin(async move {
            let region = conn
                .current_schema()
                .await?
                .unwrap_or_else(|| "dynamodb".into());
            Ok(vec![Container {
                name: region,
                comment: None,
                content: None,
            }])
        })
    }

    fn content<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
        _container: &'a str,
        only: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Content>> {
        Box::pin(async move {
            let names: Vec<String> = match only {
                Some(t) => vec![t.to_owned()],
                None => native(conn, "ListTables", &[])
                    .await?
                    .iter()
                    .filter_map(|d| d.get_str("name").ok().map(str::to_owned))
                    .collect(),
            };
            let mut tables = Vec::new();
            for n in names {
                let d = native(conn, "DescribeTable", &[("table", &n)]).await?;
                if let Some(d) = d.first() {
                    tables.push(table_from(d));
                }
            }
            tables.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(Content {
                tables,
                ..Default::default()
            })
        })
    }
}

/// The script of a table (kept at introspection).
pub fn table_script(t: &Table) -> String {
    t.server_sql
        .clone()
        .unwrap_or_else(|| format!("DescribeTable {}\n", json!({ "TableName": t.name })))
}

/// The scripts of every table of the region.
pub fn container_script(content: &Content) -> String {
    content
        .tables
        .iter()
        .map(table_script)
        .collect::<Vec<_>>()
        .join("\n")
}

/// An index as the `UpdateTable` request that creates it (GSI) or as a
/// note (an LSI exists only from `CreateTable`).
pub fn index_script(t: &Table, i: &Index) -> String {
    let keys: Vec<Json> = i
        .columns
        .iter()
        .enumerate()
        .map(|(n, c)| json!({ "AttributeName": c, "KeyType": if n == 0 { "HASH" } else { "RANGE" } }))
        .collect();
    let ptype = i
        .method
        .as_deref()
        .and_then(|m| m.split_whitespace().nth(1))
        .unwrap_or("ALL");
    let mut projection = json!({ "ProjectionType": ptype });
    if !i.include.is_empty() {
        projection["NonKeyAttributes"] = json!(i.include);
    }
    if i.method.as_deref().is_some_and(|m| m.starts_with("LSI")) {
        return format!(
            "-- local secondary index of {}: created with the table (CreateTable \
             \"LocalSecondaryIndexes\")\n{}\n",
            t.name,
            serde_json::to_string_pretty(&json!({
                "IndexName": i.name, "KeySchema": keys, "Projection": projection
            }))
            .unwrap_or_default()
        );
    }
    format!(
        "-- aws dynamodb update-table --table-name {} --global-secondary-index-updates \
         '[{{\"Create\": …}}]'\n{}\n",
        t.name,
        serde_json::to_string_pretty(&json!({
            "Create": { "IndexName": i.name, "KeySchema": keys, "Projection": projection }
        }))
        .unwrap_or_default()
    )
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// PartiQL snippets for a table; `fields` are known attribute names.
pub fn script(t: &Table, kind: ScriptKind, fields: &[String]) -> Option<String> {
    let key: Vec<&String> = t
        .primary_key
        .as_ref()
        .map(|k| k.columns.iter().collect())
        .unwrap_or_default();
    let cond = key
        .iter()
        .map(|k| format!("{} = ?", ident(k)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let others: Vec<&String> = fields
        .iter()
        .filter(|f| !f.contains('.') && !key.contains(f))
        .take(20)
        .collect();
    let table = ident(&t.name);
    Some(match kind {
        ScriptKind::Select => match key.first() {
            Some(pk) => format!("SELECT * FROM {table} WHERE {} = ?", ident(pk)),
            None => format!("SELECT * FROM {table}"),
        },
        ScriptKind::Insert => format!(
            "INSERT INTO {table} VALUE {{{}}}",
            key.iter()
                .copied()
                .chain(others.iter().copied())
                .map(|k| format!("'{}': ?", k.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ScriptKind::Update => format!(
            "UPDATE {table} SET {} WHERE {cond}",
            if others.is_empty() {
                "\"attribute\" = ?".to_owned()
            } else {
                others
                    .iter()
                    .map(|f| format!("{} = ?", ident(f)))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
        ScriptKind::Delete => format!("DELETE FROM {table} WHERE {cond}"),
        ScriptKind::Drop => format!("DeleteTable {}", json!({ "TableName": t.name })),
        ScriptKind::Call => return None,
    })
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    /// `DescribeTable` as the driver returns it (DynamoDB Local).
    fn described() -> Document {
        doc! {
            "TableName": "Orders",
            "TableStatus": "ACTIVE",
            "KeySchema": [ { "AttributeName": "pk", "KeyType": "HASH" }, { "AttributeName": "sk", "KeyType": "RANGE" } ],
            "AttributeDefinitions": [
                { "AttributeName": "customer", "AttributeType": "S" },
                { "AttributeName": "sk", "AttributeType": "N" },
                { "AttributeName": "pk", "AttributeType": "S" },
            ],
            "BillingMode": "PAY_PER_REQUEST",
            "GlobalSecondaryIndexes": [ {
                "IndexName": "byCustomer",
                "KeySchema": [ { "AttributeName": "customer", "KeyType": "HASH" } ],
                "Projection": { "ProjectionType": "INCLUDE", "NonKeyAttributes": ["total"] },
                "IndexStatus": "ACTIVE",
            } ],
            "LocalSecondaryIndexes": [ {
                "IndexName": "bySk2",
                "KeySchema": [ { "AttributeName": "pk", "KeyType": "HASH" }, { "AttributeName": "customer", "KeyType": "RANGE" } ],
                "Projection": { "ProjectionType": "KEYS_ONLY" },
            } ],
            "ItemCount": 42_i64,
            "TableSizeBytes": 2048_i64,
            "TimeToLive": { "TimeToLiveStatus": "ENABLED", "AttributeName": "expires" },
        }
    }

    #[test]
    fn tables_from_descriptions() {
        let t = table_from(&described());
        assert_eq!(t.name, "Orders");
        assert_eq!(t.primary_key.as_ref().unwrap().columns, vec!["pk", "sk"]);
        let cols: Vec<(&str, &str)> = t
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.data_type.as_str()))
            .collect();
        assert_eq!(
            cols,
            vec![
                ("pk", "S (string)"),
                ("sk", "N (number)"),
                ("customer", "S (string)")
            ]
        );
        assert_eq!(t.indexes[0].name, "byCustomer");
        assert_eq!(t.indexes[0].method.as_deref(), Some("GSI INCLUDE"));
        assert_eq!(t.indexes[0].include, vec!["total"]);
        assert_eq!(t.indexes[1].columns, vec!["pk", "customer"]);
        assert_eq!(t.comment.as_deref(), Some("PAY_PER_REQUEST"));
        assert_eq!(t.rows, Some(42));
        let s = table_script(&t);
        assert!(s.contains("-- TTL ENABLED on `expires`"), "{s}");
        // The CreateTable request is valid JSON after the operation name.
        let json = s.split_once("CreateTable ").unwrap().1;
        let v: Json = serde_json::from_str(json).unwrap();
        assert_eq!(v["TableName"], "Orders");
        assert_eq!(
            v["GlobalSecondaryIndexes"][0]["Projection"]["NonKeyAttributes"][0],
            "total"
        );
        assert!(index_script(&t, &t.indexes[0]).contains("\"Create\""));
        assert!(index_script(&t, &t.indexes[1]).contains("local secondary index"));
    }

    #[test]
    fn partiql_snippets() {
        let t = table_from(&described());
        let fields = vec!["pk".into(), "sk".into(), "total".into(), "a.b".into()];
        assert_eq!(
            script(&t, ScriptKind::Select, &fields).unwrap(),
            r#"SELECT * FROM "Orders" WHERE "pk" = ?"#
        );
        assert_eq!(
            script(&t, ScriptKind::Insert, &fields).unwrap(),
            r#"INSERT INTO "Orders" VALUE {'pk': ?, 'sk': ?, 'total': ?}"#
        );
        assert_eq!(
            script(&t, ScriptKind::Update, &fields).unwrap(),
            r#"UPDATE "Orders" SET "total" = ? WHERE "pk" = ? AND "sk" = ?"#
        );
        assert_eq!(
            script(&t, ScriptKind::Delete, &fields).unwrap(),
            r#"DELETE FROM "Orders" WHERE "pk" = ? AND "sk" = ?"#
        );
        assert_eq!(
            script(&t, ScriptKind::Drop, &fields).unwrap(),
            r#"DeleteTable {"TableName":"Orders"}"#
        );
    }
}
