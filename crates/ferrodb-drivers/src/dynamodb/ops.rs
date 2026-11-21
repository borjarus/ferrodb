//! DynamoDB API operations, from a request in the AWS API / CLI shape
//! (`{"TableName": "t", "Key": {"pk": {"S": "a"}}}`).
//!
//! The request comes from the console (`GetItem {…}`) or from a
//! [`Request::Native`](crate::Request::Native) call, whose arguments are
//! either the whole request (`request={…}`) or short keys mapped to the API
//! fields (`table`, `index`, `key`, `item`, `limit`, `key_condition`,
//! `filter`, `values`, `names`…, see [`ARGS`]).
//!
//! `Limit` of `Query` and `Scan` is the number of items returned in total
//! (pages are requested until it is reached); when items remain, the
//! `LastEvaluatedKey` to continue from is reported as a notice.

use std::collections::HashMap;

use aws_sdk_dynamodb::{
    error::BuildError,
    types::{
        AttributeDefinition, AttributeValue, BillingMode, ConditionCheck, Delete,
        GlobalSecondaryIndex, KeySchemaElement, KeyType, LocalSecondaryIndex,
        ParameterizedStatement, Projection, ProjectionType, ProvisionedThroughput, Put,
        ReturnConsumedCapacity, ReturnValue, ScalarAttributeType, Select, StreamSpecification,
        StreamViewType, TableClass, TableDescription, TimeToLiveDescription, TransactWriteItem,
        Update,
    },
};
use bson::{Bson, DateTime, Document, doc};
use ferrodb_api::{Error, NoticeLevel, QueryError, Result};
use serde_json::{Map, Value as Json};

use super::{
    conn::{Capacity, DocOut, DynamoConnection, call},
    value,
};
use crate::{CancellationToken, ExecSummary, ResultSink};

/// Short argument names of native calls → API fields, and how the value
/// is read.
pub const ARGS: &[(&str, &str, ArgKind)] = &[
    ("table", "TableName", ArgKind::Text),
    ("index", "IndexName", ArgKind::Text),
    ("limit", "Limit", ArgKind::Number),
    ("key", "Key", ArgKind::Json),
    ("item", "Item", ArgKind::Json),
    ("key_condition", "KeyConditionExpression", ArgKind::Text),
    ("filter", "FilterExpression", ArgKind::Text),
    ("update", "UpdateExpression", ArgKind::Text),
    ("condition", "ConditionExpression", ArgKind::Text),
    ("projection", "ProjectionExpression", ArgKind::Text),
    ("values", "ExpressionAttributeValues", ArgKind::Json),
    ("names", "ExpressionAttributeNames", ArgKind::Json),
    ("exclusive_start_key", "ExclusiveStartKey", ArgKind::Json),
    ("segment", "Segment", ArgKind::Number),
    ("total_segments", "TotalSegments", ArgKind::Number),
    ("scan_forward", "ScanIndexForward", ArgKind::Bool),
    ("consistent", "ConsistentRead", ArgKind::Bool),
    ("select", "Select", ArgKind::Text),
    ("return_values", "ReturnValues", ArgKind::Text),
    ("statement", "Statement", ArgKind::Text),
    ("parameters", "Parameters", ArgKind::Json),
    ("statements", "TransactStatements", ArgKind::Json),
    ("items", "TransactItems", ArgKind::Json),
    ("definition", "", ArgKind::Json),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    Text,
    Number,
    Bool,
    Json,
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::Query(QueryError {
        message: msg.into(),
        code: Some("ValidationException".into()),
        range: None,
    })
}

fn built(e: BuildError) -> Error {
    invalid(e.to_string())
}

/// The JSON request typed after the operation name; parse errors point at
/// the problem (`offset` = where the JSON starts in the statement).
pub fn parse_request(json: &str, offset: usize) -> Result<Json> {
    if json.trim().is_empty() {
        return Ok(Json::Object(Map::new()));
    }
    serde_json::from_str(json).map_err(|e| {
        let at: usize = json
            .split_inclusive('\n')
            .take(e.line().saturating_sub(1))
            .map(str::len)
            .sum::<usize>()
            + e.column().saturating_sub(1);
        let at = offset + at.min(json.len());
        Error::Query(QueryError {
            message: format!("request JSON: {e}"),
            code: Some("FERRODB-JSON".into()),
            range: Some(at..at + 1),
        })
    })
}

/// Native call arguments → the API request.
pub fn args_to_json(args: &[(String, String)]) -> Result<Json> {
    if let Some((_, r)) = args.iter().find(|(k, _)| k == "request") {
        return parse_request(r, 0);
    }
    let mut m = Map::new();
    for (k, v) in args {
        let Some((_, field, kind)) = ARGS.iter().find(|(a, ..)| a == k) else {
            // API field names are accepted as they are.
            m.insert(k.clone(), Json::String(v.clone()));
            continue;
        };
        let parsed = match kind {
            ArgKind::Text => Json::String(v.clone()),
            ArgKind::Number => Json::from(
                v.trim()
                    .parse::<i64>()
                    .map_err(|_| invalid(format!("{k}: not a number: {v}")))?,
            ),
            ArgKind::Bool => Json::Bool(matches!(v.trim(), "true" | "1" | "yes")),
            ArgKind::Json => parse_request(v, 0)?,
        };
        if field.is_empty() {
            // `definition`: the whole request (CreateTable).
            if let Json::Object(o) = parsed {
                m.extend(o);
            }
        } else {
            m.insert((*field).to_owned(), parsed);
        }
    }
    Ok(Json::Object(m))
}

fn text<'a>(v: &'a Json, field: &str) -> Option<&'a str> {
    v.get(field).and_then(Json::as_str)
}

fn required<'a>(v: &'a Json, field: &str) -> Result<&'a str> {
    text(v, field).ok_or_else(|| invalid(format!("{field} is required")))
}

fn number(v: &Json, field: &str) -> Option<i64> {
    match v.get(field)? {
        Json::Number(n) => n.as_i64(),
        Json::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn boolean(v: &Json, field: &str) -> Option<bool> {
    v.get(field).and_then(Json::as_bool)
}

fn item(v: &Json, field: &str) -> Result<Option<HashMap<String, AttributeValue>>> {
    v.get(field).map(value::item_from_json).transpose()
}

fn required_item(v: &Json, field: &str) -> Result<HashMap<String, AttributeValue>> {
    item(v, field)?.ok_or_else(|| invalid(format!("{field} is required")))
}

fn names(v: &Json) -> Result<Option<HashMap<String, String>>> {
    v.get("ExpressionAttributeNames")
        .map(|n| {
            n.as_object()
                .ok_or_else(|| invalid("ExpressionAttributeNames: expected an object"))?
                .iter()
                .map(|(k, x)| {
                    Ok((
                        k.clone(),
                        x.as_str()
                            .ok_or_else(|| invalid("ExpressionAttributeNames: expected strings"))?
                            .to_owned(),
                    ))
                })
                .collect()
        })
        .transpose()
}

fn values(v: &Json) -> Result<Option<HashMap<String, AttributeValue>>> {
    item(v, "ExpressionAttributeValues")
}

fn list<'a>(v: &'a Json, field: &str) -> &'a [Json] {
    v.get(field)
        .and_then(Json::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn key_schema(v: &Json) -> Result<Vec<KeySchemaElement>> {
    list(v, "KeySchema")
        .iter()
        .map(|k| {
            KeySchemaElement::builder()
                .attribute_name(required(k, "AttributeName")?)
                .key_type(KeyType::from(required(k, "KeyType")?))
                .build()
                .map_err(built)
        })
        .collect()
}

fn projection(v: &Json) -> Projection {
    let p = v.get("Projection").cloned().unwrap_or(Json::Null);
    Projection::builder()
        .set_projection_type(text(&p, "ProjectionType").map(ProjectionType::from))
        .set_non_key_attributes({
            let a: Vec<String> = list(&p, "NonKeyAttributes")
                .iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect();
            (!a.is_empty()).then_some(a)
        })
        .build()
}

fn throughput(v: &Json) -> Result<Option<ProvisionedThroughput>> {
    v.get("ProvisionedThroughput")
        .map(|p| {
            ProvisionedThroughput::builder()
                .set_read_capacity_units(number(p, "ReadCapacityUnits"))
                .set_write_capacity_units(number(p, "WriteCapacityUnits"))
                .build()
                .map_err(built)
        })
        .transpose()
}

/// Item documents with the table's key attributes first.
async fn order_of(
    c: &mut DynamoConnection,
    table: &str,
    cancel: &CancellationToken,
) -> Vec<String> {
    c.table_keys(table, cancel)
        .await
        .map(|k| k.order())
        .unwrap_or_default()
}

/// One result of a few documents.
async fn documents(
    c: &DynamoConnection,
    sink: &ResultSink,
    collection: Option<&str>,
    docs: &[Document],
) -> Result<ExecSummary> {
    let mut out = DocOut::new(sink);
    out.begin(&c.container, collection).await;
    for d in docs {
        if !out.push(d).await? {
            break;
        }
    }
    out.finish().await;
    Ok(ExecSummary {
        affected: None,
        results: 1,
        truncated: out.truncated,
    })
}

fn time(t: Option<&aws_sdk_dynamodb::primitives::DateTime>) -> Option<Bson> {
    t.map(|t| Bson::DateTime(DateTime::from_millis(t.to_millis().unwrap_or(0))))
}

fn key_schema_doc(ks: &[KeySchemaElement]) -> Bson {
    Bson::Array(
        ks.iter()
            .map(|k| {
                Bson::Document(doc! {
                    "AttributeName": k.attribute_name(),
                    "KeyType": k.key_type().as_str(),
                })
            })
            .collect(),
    )
}

fn projection_doc(p: Option<&Projection>) -> Document {
    let mut d = Document::new();
    if let Some(p) = p {
        if let Some(t) = p.projection_type() {
            d.insert("ProjectionType", t.as_str());
        }
        if !p.non_key_attributes().is_empty() {
            d.insert("NonKeyAttributes", p.non_key_attributes().to_vec());
        }
    }
    d
}

/// `DescribeTable` (+ `DescribeTimeToLive`) as one document, with the
/// field names of the API.
pub fn describe_doc(t: &TableDescription, ttl: Option<&TimeToLiveDescription>) -> Document {
    let mut d = Document::new();
    let mut put = |k: &str, v: Option<Bson>| {
        if let Some(v) = v {
            d.insert(k, v);
        }
    };
    put("TableName", t.table_name().map(Bson::from));
    put("TableStatus", t.table_status().map(|s| s.as_str().into()));
    put("KeySchema", Some(key_schema_doc(t.key_schema())));
    put(
        "AttributeDefinitions",
        Some(Bson::Array(
            t.attribute_definitions()
                .iter()
                .map(|a| {
                    Bson::Document(doc! {
                        "AttributeName": a.attribute_name(),
                        "AttributeType": a.attribute_type().as_str(),
                    })
                })
                .collect(),
        )),
    );
    put(
        "BillingMode",
        Some(Bson::from(
            t.billing_mode_summary()
                .and_then(|b| b.billing_mode())
                .map_or("PROVISIONED", |b| b.as_str()),
        )),
    );
    let tp = |p: Option<&aws_sdk_dynamodb::types::ProvisionedThroughputDescription>| {
        p.filter(|p| p.read_capacity_units().unwrap_or(0) > 0)
            .map(|p| {
                Bson::Document(doc! {
                    "ReadCapacityUnits": p.read_capacity_units().unwrap_or(0),
                    "WriteCapacityUnits": p.write_capacity_units().unwrap_or(0),
                })
            })
    };
    put("ProvisionedThroughput", tp(t.provisioned_throughput()));
    if !t.global_secondary_indexes().is_empty() {
        put(
            "GlobalSecondaryIndexes",
            Some(Bson::Array(
                t.global_secondary_indexes()
                    .iter()
                    .map(|g| {
                        let mut i = doc! {
                            "IndexName": g.index_name().unwrap_or_default(),
                            "KeySchema": key_schema_doc(g.key_schema()),
                            "Projection": projection_doc(g.projection()),
                        };
                        if let Some(s) = g.index_status() {
                            i.insert("IndexStatus", s.as_str());
                        }
                        if let Some(p) = tp(g.provisioned_throughput()) {
                            i.insert("ProvisionedThroughput", p);
                        }
                        if let Some(n) = g.item_count() {
                            i.insert("ItemCount", n);
                        }
                        Bson::Document(i)
                    })
                    .collect(),
            )),
        );
    }
    if !t.local_secondary_indexes().is_empty() {
        put(
            "LocalSecondaryIndexes",
            Some(Bson::Array(
                t.local_secondary_indexes()
                    .iter()
                    .map(|l| {
                        let mut i = doc! {
                            "IndexName": l.index_name().unwrap_or_default(),
                            "KeySchema": key_schema_doc(l.key_schema()),
                            "Projection": projection_doc(l.projection()),
                        };
                        if let Some(n) = l.item_count() {
                            i.insert("ItemCount", n);
                        }
                        Bson::Document(i)
                    })
                    .collect(),
            )),
        );
    }
    put("ItemCount", t.item_count().map(Bson::Int64));
    put("TableSizeBytes", t.table_size_bytes().map(Bson::Int64));
    put(
        "StreamSpecification",
        t.stream_specification().map(|s| {
            let mut x = doc! { "StreamEnabled": s.stream_enabled() };
            if let Some(v) = s.stream_view_type() {
                x.insert("StreamViewType", v.as_str());
            }
            Bson::Document(x)
        }),
    );
    put("LatestStreamArn", t.latest_stream_arn().map(Bson::from));
    put(
        "TableClass",
        t.table_class_summary()
            .and_then(|c| c.table_class())
            .map(|c| c.as_str().into()),
    );
    put(
        "DeletionProtectionEnabled",
        t.deletion_protection_enabled().map(Bson::Boolean),
    );
    put("TableArn", t.table_arn().map(Bson::from));
    put("CreationDateTime", time(t.creation_date_time()));
    put(
        "TimeToLive",
        ttl.map(|x| {
            let mut v = doc! {
                "TimeToLiveStatus": x.time_to_live_status().map_or("DISABLED", |s| s.as_str()),
            };
            if let Some(a) = x.attribute_name() {
                v.insert("AttributeName", a);
            }
            Bson::Document(v)
        }),
    );
    d
}

/// Reports where to continue a limited `Query` / `Scan`.
async fn continuation(sink: &ResultSink, last: Option<&HashMap<String, AttributeValue>>) {
    if let Some(k) = last.filter(|k| !k.is_empty()) {
        sink.notice(
            NoticeLevel::Info,
            format!(
                "More items remain; continue with \"ExclusiveStartKey\": {}",
                value::item_to_json(k)
            ),
        )
        .await;
    }
}

async fn scan_warning(sink: &ResultSink, table: &str) {
    sink.notice(
        NoticeLevel::Warning,
        format!(
            "Scan reads every item of `{table}` it passes (FilterExpression is applied after \
             reading): the read capacity consumed grows with the table size"
        ),
    )
    .await;
}

pub(crate) async fn run(
    c: &mut DynamoConnection,
    op: &str,
    v: &Json,
    sink: &ResultSink,
    cancel: &CancellationToken,
) -> Result<ExecSummary> {
    let total = ReturnConsumedCapacity::Total;
    match op {
        "ListTables" => {
            let mut names: Vec<Document> = Vec::new();
            let mut start: Option<String> = None;
            loop {
                let r = call(
                    cancel,
                    c.client
                        .list_tables()
                        .set_exclusive_start_table_name(start.take())
                        .send(),
                )
                .await?;
                names.extend(r.table_names().iter().map(|n| doc! { "name": n }));
                match r.last_evaluated_table_name() {
                    Some(n) => start = Some(n.to_owned()),
                    None => break,
                }
            }
            documents(c, sink, None, &names).await
        }
        "DescribeTable" => {
            let table = required(v, "TableName")?;
            c.forget(table);
            let r = call(cancel, c.client.describe_table().table_name(table).send()).await?;
            let ttl = call(
                cancel,
                c.client.describe_time_to_live().table_name(table).send(),
            )
            .await
            .ok();
            let d = r
                .table()
                .map(|t| describe_doc(t, ttl.as_ref().and_then(|x| x.time_to_live_description())))
                .unwrap_or_default();
            documents(c, sink, None, &[d]).await
        }
        "Query" | "Scan" => {
            let table = required(v, "TableName")?.to_owned();
            let is_scan = op == "Scan";
            if is_scan {
                scan_warning(sink, &table).await;
            }
            let order = order_of(c, &table, cancel).await;
            let limit = number(v, "Limit").filter(|l| *l > 0).map(|l| l as usize);
            let mut out = DocOut::new(sink);
            let editable = (v.get("ProjectionExpression").is_none()
                && text(v, "Select").is_none_or(|s| s == "ALL_ATTRIBUTES"))
            .then_some(table.as_str());
            out.begin(&c.container, editable).await;
            let mut capacity = Capacity::default();
            let mut start = item(v, "ExclusiveStartKey")?;
            let mut counted: i64 = 0;
            let last = loop {
                let page_limit = limit.map(|l| (l - out.count.min(l)).max(1) as i32);
                let (items, last, count) = if is_scan {
                    let r = call(
                        cancel,
                        c.client
                            .scan()
                            .table_name(&table)
                            .set_index_name(text(v, "IndexName").map(str::to_owned))
                            .set_filter_expression(text(v, "FilterExpression").map(str::to_owned))
                            .set_projection_expression(
                                text(v, "ProjectionExpression").map(str::to_owned),
                            )
                            .set_expression_attribute_names(names(v)?)
                            .set_expression_attribute_values(values(v)?)
                            .set_segment(number(v, "Segment").map(|s| s as i32))
                            .set_total_segments(number(v, "TotalSegments").map(|s| s as i32))
                            .set_consistent_read(boolean(v, "ConsistentRead"))
                            .set_select(text(v, "Select").map(Select::from))
                            .set_limit(page_limit)
                            .set_exclusive_start_key(start.take())
                            .return_consumed_capacity(total.clone())
                            .send(),
                    )
                    .await?;
                    capacity.add(r.consumed_capacity());
                    (
                        r.items().to_vec(),
                        r.last_evaluated_key().cloned(),
                        r.count() as i64,
                    )
                } else {
                    let r = call(
                        cancel,
                        c.client
                            .query()
                            .table_name(&table)
                            .set_index_name(text(v, "IndexName").map(str::to_owned))
                            .set_key_condition_expression(
                                text(v, "KeyConditionExpression").map(str::to_owned),
                            )
                            .set_filter_expression(text(v, "FilterExpression").map(str::to_owned))
                            .set_projection_expression(
                                text(v, "ProjectionExpression").map(str::to_owned),
                            )
                            .set_expression_attribute_names(names(v)?)
                            .set_expression_attribute_values(values(v)?)
                            .set_scan_index_forward(boolean(v, "ScanIndexForward"))
                            .set_consistent_read(boolean(v, "ConsistentRead"))
                            .set_select(text(v, "Select").map(Select::from))
                            .set_limit(page_limit)
                            .set_exclusive_start_key(start.take())
                            .return_consumed_capacity(total.clone())
                            .send(),
                    )
                    .await?;
                    capacity.add(r.consumed_capacity());
                    (
                        r.items().to_vec(),
                        r.last_evaluated_key().cloned(),
                        r.count() as i64,
                    )
                };
                counted += count;
                for i in &items {
                    if limit.is_some_and(|l| out.count >= l) {
                        break;
                    }
                    if !out.push(&value::map_to_doc(i, &order)).await? {
                        break;
                    }
                }
                let done = out.truncated || limit.is_some_and(|l| out.count >= l);
                match last {
                    Some(k) if !done => start = Some(k),
                    other => break other,
                }
            };
            if text(v, "Select") == Some("COUNT") {
                out.push(&doc! { "Count": counted }).await?;
            }
            out.finish().await;
            capacity.report(sink).await;
            if !out.truncated {
                continuation(sink, last.as_ref()).await;
            }
            Ok(ExecSummary {
                affected: None,
                results: 1,
                truncated: out.truncated,
            })
        }
        "GetItem" => {
            let table = required(v, "TableName")?.to_owned();
            let order = order_of(c, &table, cancel).await;
            let r = call(
                cancel,
                c.client
                    .get_item()
                    .table_name(&table)
                    .set_key(Some(required_item(v, "Key")?))
                    .set_projection_expression(text(v, "ProjectionExpression").map(str::to_owned))
                    .set_expression_attribute_names(names(v)?)
                    .set_consistent_read(boolean(v, "ConsistentRead"))
                    .return_consumed_capacity(total)
                    .send(),
            )
            .await?;
            let docs: Vec<Document> = r
                .item()
                .map(|i| value::map_to_doc(i, &order))
                .into_iter()
                .collect();
            let s = documents(c, sink, Some(&table), &docs).await?;
            let mut cap = Capacity::default();
            cap.add(r.consumed_capacity());
            cap.report(sink).await;
            Ok(s)
        }
        "PutItem" | "UpdateItem" | "DeleteItem" => {
            let table = required(v, "TableName")?.to_owned();
            let rv = text(v, "ReturnValues").map(ReturnValue::from);
            let (attrs, cap) = match op {
                "PutItem" => {
                    let r = call(
                        cancel,
                        c.client
                            .put_item()
                            .table_name(&table)
                            .set_item(Some(required_item(v, "Item")?))
                            .set_condition_expression(
                                text(v, "ConditionExpression").map(str::to_owned),
                            )
                            .set_expression_attribute_names(names(v)?)
                            .set_expression_attribute_values(values(v)?)
                            .set_return_values(rv)
                            .return_consumed_capacity(total)
                            .send(),
                    )
                    .await?;
                    (r.attributes().cloned(), r.consumed_capacity().cloned())
                }
                "UpdateItem" => {
                    let r = call(
                        cancel,
                        c.client
                            .update_item()
                            .table_name(&table)
                            .set_key(Some(required_item(v, "Key")?))
                            .set_update_expression(text(v, "UpdateExpression").map(str::to_owned))
                            .set_condition_expression(
                                text(v, "ConditionExpression").map(str::to_owned),
                            )
                            .set_expression_attribute_names(names(v)?)
                            .set_expression_attribute_values(values(v)?)
                            .set_return_values(rv)
                            .return_consumed_capacity(total)
                            .send(),
                    )
                    .await?;
                    (r.attributes().cloned(), r.consumed_capacity().cloned())
                }
                _ => {
                    let r = call(
                        cancel,
                        c.client
                            .delete_item()
                            .table_name(&table)
                            .set_key(Some(required_item(v, "Key")?))
                            .set_condition_expression(
                                text(v, "ConditionExpression").map(str::to_owned),
                            )
                            .set_expression_attribute_names(names(v)?)
                            .set_expression_attribute_values(values(v)?)
                            .set_return_values(rv)
                            .return_consumed_capacity(total)
                            .send(),
                    )
                    .await?;
                    (r.attributes().cloned(), r.consumed_capacity().cloned())
                }
            };
            let mut s = match attrs {
                Some(a) => {
                    let order = order_of(c, &table, cancel).await;
                    documents(c, sink, None, &[value::map_to_doc(&a, &order)]).await?
                }
                None => ExecSummary::default(),
            };
            let mut capacity = Capacity::default();
            capacity.add(cap.as_ref());
            capacity.report(sink).await;
            s.affected = Some(1);
            Ok(s)
        }
        "TransactWriteItems" => {
            let mut items = Vec::new();
            for x in list(v, "TransactItems") {
                let o = x.as_object().filter(|o| o.len() == 1).ok_or_else(|| {
                    invalid("TransactItems: one Put / Update / Delete / ConditionCheck per element")
                })?;
                let (kind, b) = o.iter().next().expect("one entry");
                let t = required(b, "TableName")?;
                let cond = text(b, "ConditionExpression").map(str::to_owned);
                let item = match kind.as_str() {
                    "Put" => TransactWriteItem::builder().put(
                        Put::builder()
                            .table_name(t)
                            .set_item(Some(required_item(b, "Item")?))
                            .set_condition_expression(cond)
                            .set_expression_attribute_names(names(b)?)
                            .set_expression_attribute_values(values(b)?)
                            .build()
                            .map_err(built)?,
                    ),
                    "Update" => TransactWriteItem::builder().update(
                        Update::builder()
                            .table_name(t)
                            .set_key(Some(required_item(b, "Key")?))
                            .update_expression(required(b, "UpdateExpression")?)
                            .set_condition_expression(cond)
                            .set_expression_attribute_names(names(b)?)
                            .set_expression_attribute_values(values(b)?)
                            .build()
                            .map_err(built)?,
                    ),
                    "Delete" => TransactWriteItem::builder().delete(
                        Delete::builder()
                            .table_name(t)
                            .set_key(Some(required_item(b, "Key")?))
                            .set_condition_expression(cond)
                            .set_expression_attribute_names(names(b)?)
                            .set_expression_attribute_values(values(b)?)
                            .build()
                            .map_err(built)?,
                    ),
                    "ConditionCheck" => TransactWriteItem::builder().condition_check(
                        ConditionCheck::builder()
                            .table_name(t)
                            .set_key(Some(required_item(b, "Key")?))
                            .condition_expression(required(b, "ConditionExpression")?)
                            .set_expression_attribute_names(names(b)?)
                            .set_expression_attribute_values(values(b)?)
                            .build()
                            .map_err(built)?,
                    ),
                    other => return Err(invalid(format!("TransactItems: unknown `{other}`"))),
                };
                items.push(item.build());
            }
            let n = items.len();
            let r = call(
                cancel,
                c.client
                    .transact_write_items()
                    .set_transact_items(Some(items))
                    .return_consumed_capacity(total)
                    .send(),
            )
            .await?;
            let mut capacity = Capacity::default();
            capacity.add_all(r.consumed_capacity());
            capacity.report(sink).await;
            Ok(ExecSummary {
                affected: Some(n as u64),
                ..Default::default()
            })
        }
        "ExecuteStatement" => {
            let statement = required(v, "Statement")?.to_owned();
            let params = list(v, "Parameters")
                .iter()
                .map(value::from_json)
                .collect::<Result<Vec<_>>>()?;
            c.statement(&statement, params, sink, cancel).await
        }
        "ExecuteTransaction" => {
            let mut statements = Vec::new();
            for s in list(v, "TransactStatements") {
                let params = list(s, "Parameters")
                    .iter()
                    .map(value::from_json)
                    .collect::<Result<Vec<_>>>()?;
                statements.push(
                    ParameterizedStatement::builder()
                        .statement(required(s, "Statement")?)
                        .set_parameters((!params.is_empty()).then_some(params))
                        .build()
                        .map_err(built)?,
                );
            }
            let n = statements.len();
            let r = call(
                cancel,
                c.client
                    .execute_transaction()
                    .set_transact_statements(Some(statements))
                    .return_consumed_capacity(total)
                    .send(),
            )
            .await?;
            let docs: Vec<Document> = r
                .responses()
                .iter()
                .filter_map(|x| x.item())
                .map(|i| value::map_to_doc(i, &[]))
                .collect();
            let mut s = if docs.is_empty() {
                ExecSummary::default()
            } else {
                documents(c, sink, None, &docs).await?
            };
            let mut capacity = Capacity::default();
            capacity.add_all(r.consumed_capacity());
            capacity.report(sink).await;
            s.affected = Some(n as u64);
            Ok(s)
        }
        "CreateTable" => {
            let table = required(v, "TableName")?.to_owned();
            let defs = list(v, "AttributeDefinitions")
                .iter()
                .map(|a| {
                    AttributeDefinition::builder()
                        .attribute_name(required(a, "AttributeName")?)
                        .attribute_type(ScalarAttributeType::from(required(a, "AttributeType")?))
                        .build()
                        .map_err(built)
                })
                .collect::<Result<Vec<_>>>()?;
            let gsis = list(v, "GlobalSecondaryIndexes")
                .iter()
                .map(|g| {
                    GlobalSecondaryIndex::builder()
                        .index_name(required(g, "IndexName")?)
                        .set_key_schema(Some(key_schema(g)?))
                        .projection(projection(g))
                        .set_provisioned_throughput(throughput(g)?)
                        .build()
                        .map_err(built)
                })
                .collect::<Result<Vec<_>>>()?;
            let lsis = list(v, "LocalSecondaryIndexes")
                .iter()
                .map(|l| {
                    LocalSecondaryIndex::builder()
                        .index_name(required(l, "IndexName")?)
                        .set_key_schema(Some(key_schema(l)?))
                        .projection(projection(l))
                        .build()
                        .map_err(built)
                })
                .collect::<Result<Vec<_>>>()?;
            let stream = v
                .get("StreamSpecification")
                .map(|s| {
                    StreamSpecification::builder()
                        .stream_enabled(boolean(s, "StreamEnabled").unwrap_or(false))
                        .set_stream_view_type(text(s, "StreamViewType").map(StreamViewType::from))
                        .build()
                        .map_err(built)
                })
                .transpose()?;
            let r = call(
                cancel,
                c.client
                    .create_table()
                    .table_name(&table)
                    .set_key_schema(Some(key_schema(v)?))
                    .set_attribute_definitions(Some(defs))
                    .set_billing_mode(text(v, "BillingMode").map(BillingMode::from))
                    .set_provisioned_throughput(throughput(v)?)
                    .set_global_secondary_indexes((!gsis.is_empty()).then_some(gsis))
                    .set_local_secondary_indexes((!lsis.is_empty()).then_some(lsis))
                    .set_stream_specification(stream)
                    .set_table_class(text(v, "TableClass").map(TableClass::from))
                    .set_deletion_protection_enabled(boolean(v, "DeletionProtectionEnabled"))
                    .send(),
            )
            .await?;
            c.forget(&table);
            let d = r
                .table_description()
                .map(|t| describe_doc(t, None))
                .unwrap_or_default();
            let mut s = documents(c, sink, None, &[d]).await?;
            s.affected = Some(0);
            Ok(s)
        }
        "DeleteTable" => {
            let table = required(v, "TableName")?.to_owned();
            call(cancel, c.client.delete_table().table_name(&table).send()).await?;
            c.forget(&table);
            Ok(ExecSummary {
                affected: Some(0),
                ..Default::default()
            })
        }
        other => Err(Error::Unsupported(format!("DynamoDB operation `{other}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_arguments_become_api_requests() {
        let v = args_to_json(&[
            ("table".into(), "Orders".into()),
            ("limit".into(), "10".into()),
            ("key".into(), r#"{"pk": {"S": "a"}}"#.into()),
            ("consistent".into(), "true".into()),
            ("ReturnValues".into(), "ALL_OLD".into()),
        ])
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"TableName": "Orders", "Limit": 10, "Key": {"pk": {"S": "a"}},
                "ConsistentRead": true, "ReturnValues": "ALL_OLD"})
        );
        let v = args_to_json(&[("request".into(), r#"{"TableName": "T"}"#.into())]).unwrap();
        assert_eq!(text(&v, "TableName"), Some("T"));
        assert!(args_to_json(&[("limit".into(), "x".into())]).is_err());
    }

    #[test]
    fn json_errors_point_at_the_problem() {
        let e = parse_request("{\n  \"a\": ,\n}", 5).unwrap_err();
        let Error::Query(q) = e else { panic!() };
        // Line 2, column 8 → offset 2 + 7, plus the statement prefix.
        assert_eq!(q.range, Some(14..15));
    }

    #[test]
    fn create_table_requests_are_read() {
        let v: Json = serde_json::from_str(
            r#"{"TableName": "T", "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "GlobalSecondaryIndexes": [{"IndexName": "g", "KeySchema": [{"AttributeName": "x", "KeyType": "HASH"}],
                  "Projection": {"ProjectionType": "INCLUDE", "NonKeyAttributes": ["a"]}}]}"#,
        )
        .unwrap();
        assert_eq!(key_schema(&v).unwrap()[0].attribute_name(), "pk");
        let g = &list(&v, "GlobalSecondaryIndexes")[0];
        let p = projection(g);
        assert_eq!(p.projection_type().map(|t| t.as_str()), Some("INCLUDE"));
        assert_eq!(p.non_key_attributes(), ["a"]);
        assert!(key_schema(&serde_json::json!({"KeySchema": [{"AttributeName": "x"}]})).is_err());
    }
}
