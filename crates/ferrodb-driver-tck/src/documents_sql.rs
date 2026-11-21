//! The document TCK for SQL++ (Couchbase) and PartiQL (DynamoDB) sources:
//! the scenarios of the MongoDB suite, in the source's language. Where a
//! database cannot do what a scenario checks, the scenario says so and
//! checks what the database offers instead (capability checks):
//!
//! | scenario | Couchbase | DynamoDB |
//! |---|---|---|
//! | types | JSON types (no binary / date / decimal in JSON) | every attribute type: S, N (int, 38 digits, decimal), B, BOOL, NULL, M, L, SS, NS, BS, and the BSON → item round trip |
//! | streaming | 100k documents | 10k items (DynamoDB Local writes slowly), many 1 MB pages |
//! | cancel | the request disappears from `system:active_requests` | no server operation: the call is dropped promptly |
//! | errors | code and position | code (DynamoDB gives no positions) |
//! | transactions | `BEGIN WORK` … `ROLLBACK` / `COMMIT` | `ExecuteTransaction`: all or nothing |
//! | database switch | the query context's bucket | one container (the region) |

use std::time::{Duration, Instant};

use bson::{Binary, Bson, Decimal128, Document, RawDocumentBuf, spec::BinarySubtype};
use ferrodb_api::{DataModel, Error, QueryLanguage};
use ferrodb_drivers::{
    CancellationToken, Connection, Driver, Request, ResolvedConfig, ResultSink, SinkEvent,
    sink::{CollectedOutput, collect_request},
};

use crate::{Outcome, Report};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Sqlpp,
    Partiql,
}

struct Ctx<'a> {
    driver: &'a dyn Driver,
    conn: Box<dyn Connection>,
    lang: Lang,
    /// Couchbase `bucket.scope.` / DynamoDB: empty.
    prefix: String,
    /// Collection / table of this run.
    coll: String,
    /// Collection / table with the streaming data (kept between runs).
    big: String,
}

impl Ctx<'_> {
    /// The keyspace / table of `name`, as written in statements.
    fn ks(&self, name: &str) -> String {
        match self.lang {
            Lang::Sqlpp => format!("{}`{name}`", self.prefix),
            Lang::Partiql => format!("\"{name}\""),
        }
    }
}

async fn text(conn: &mut dyn Connection, s: &str) -> Result<CollectedOutput, String> {
    collect_request(
        conn,
        Request::Text { text: s.into() },
        CancellationToken::new(),
    )
    .await
    .map_err(|e| format!("`{s}`: {e}"))
}

async fn native(
    conn: &mut dyn Connection,
    method: &str,
    args: &[(&str, &str)],
) -> Result<CollectedOutput, String> {
    collect_request(
        conn,
        Request::native(method, args),
        CancellationToken::new(),
    )
    .await
    .map_err(|e| format!("{method}: {e}"))
}

fn decode(d: &RawDocumentBuf) -> Document {
    Document::try_from(d.clone()).unwrap_or_default()
}

fn int(v: Option<&Bson>) -> Option<i64> {
    match v? {
        Bson::Int32(i) => Some(*i as i64),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) => Some(*d as i64),
        _ => None,
    }
}

/// Retries a statement while a just-created keyspace / index settles.
async fn settle(conn: &mut dyn Connection, s: &str) -> Result<CollectedOutput, String> {
    let mut last = String::new();
    for _ in 0..30 {
        match text(conn, s).await {
            Ok(o) => return Ok(o),
            Err(e) => last = e,
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(last)
}

async fn create(c: &mut Ctx<'_>, name: &str) -> Result<(), String> {
    match c.lang {
        Lang::Sqlpp => {
            let ks = c.ks(name);
            settle(c.conn.as_mut(), &format!("CREATE COLLECTION {ks}")).await?;
            settle(c.conn.as_mut(), &format!("CREATE PRIMARY INDEX ON {ks}")).await?;
        }
        Lang::Partiql => {
            let def = format!(
                r#"{{"TableName": "{name}", "KeySchema": [{{"AttributeName": "pk", "KeyType": "HASH"}}],
                    "AttributeDefinitions": [{{"AttributeName": "pk", "AttributeType": "S"}}],
                    "BillingMode": "PAY_PER_REQUEST"}}"#
            );
            native(c.conn.as_mut(), "CreateTable", &[("request", &def)]).await?;
        }
    }
    Ok(())
}

async fn drop(c: &mut Ctx<'_>, name: &str) {
    let _ = match c.lang {
        Lang::Sqlpp => {
            let stmt = format!("DROP COLLECTION {}", c.ks(name));
            text(c.conn.as_mut(), &stmt).await
        }
        Lang::Partiql => native(c.conn.as_mut(), "DeleteTable", &[("table", name)]).await,
    };
}

/// Runs every scenario. Never panics; failures are reported.
pub async fn run(driver: &dyn Driver, cfg: &ResolvedConfig) -> Report {
    let mut report = Report {
        driver: driver.descriptor().name,
        ..Default::default()
    };
    let started = Instant::now();
    let conn = match driver.connect(cfg).await {
        Ok(c) => c,
        Err(e) => {
            report.outcomes.push(Outcome {
                scenario: "connect",
                passed: false,
                detail: e.to_string(),
                elapsed: started.elapsed(),
            });
            return report;
        }
    };
    report.server = conn.server_version();
    report.outcomes.push(Outcome {
        scenario: "connect",
        passed: !report.server.is_empty(),
        detail: report.server.clone(),
        elapsed: started.elapsed(),
    });
    let lang = match driver.descriptor().capabilities.language {
        QueryLanguage::SqlPlusPlus => Lang::Sqlpp,
        _ => Lang::Partiql,
    };
    let prefix = match lang {
        Lang::Sqlpp => format!(
            "`{}`.`{}`.",
            cfg.database().unwrap_or("default"),
            cfg.prop("scope").unwrap_or("_default")
        ),
        Lang::Partiql => String::new(),
    };
    let mut ctx = Ctx {
        driver,
        conn,
        lang,
        prefix,
        coll: crate::table_name("docs"),
        big: "tck_docs_big".into(),
    };
    macro_rules! scenario {
        ($name:literal, $fut:expr) => {{
            let t = Instant::now();
            let r = tokio::time::timeout(Duration::from_secs(300), $fut).await;
            let (passed, detail) = match r {
                Ok(Ok(d)) => (true, d),
                Ok(Err(e)) => (false, e),
                Err(_) => (false, "timed out after 300 s".to_owned()),
            };
            report.outcomes.push(Outcome {
                scenario: $name,
                passed,
                detail,
                elapsed: t.elapsed(),
            });
            if !passed {
                let _ = ctx.conn.rollback().await;
            }
        }};
    }
    scenario!("descriptor", descriptor(&mut ctx));
    scenario!("ping", async {
        ctx.conn
            .ping()
            .await
            .map(|_| String::new())
            .map_err(|e| e.to_string())
    });
    let coll = ctx.coll.clone();
    scenario!("setup", async {
        create(&mut ctx, &coll).await.map(|_| coll.clone())
    });
    scenario!("bson-types", types(&mut ctx));
    scenario!("writes-affected", writes(&mut ctx));
    scenario!("streaming-100k", streaming(&mut ctx));
    scenario!("backpressure-truncate", truncate(&mut ctx));
    scenario!("cancel", cancel(&mut ctx));
    scenario!("errors", errors(&mut ctx));
    scenario!("transactions", transactions(&mut ctx));
    scenario!("database-switch", switch(&mut ctx));
    scenario!("listings", listings(&mut ctx));
    drop(&mut ctx, &coll).await;
    report
}

async fn descriptor(c: &mut Ctx<'_>) -> Result<String, String> {
    let d = c.driver.descriptor();
    let caps = d.capabilities;
    let want = match c.lang {
        Lang::Sqlpp => QueryLanguage::SqlPlusPlus,
        Lang::Partiql => QueryLanguage::PartiQl,
    };
    if caps.data_model != DataModel::Document || caps.language != want {
        return Err(format!(
            "unexpected model {:?} / {:?}",
            caps.data_model, caps.language
        ));
    }
    Ok(format!("{} ({:?})", d.name, caps.transactions))
}

/// Sets come back in any order.
fn sorted_sets(d: &mut Document) {
    for (_, v) in d.iter_mut() {
        if let Bson::Document(inner) = v {
            if inner.len() == 1
                && let Some((_, Bson::Array(items))) = inner.iter_mut().next()
            {
                items.sort_by_key(|x| x.to_string());
            }
            sorted_sets(inner);
        }
    }
}

async fn types(c: &mut Ctx<'_>) -> Result<String, String> {
    match c.lang {
        Lang::Sqlpp => {
            let original = bson::doc! {
                "int32": 42_i32,
                "int64": 9_007_199_254_740_993_i64,
                "double": 1.5,
                "negative": -0.25,
                "string": "zażółć 🦀 \"quoted\"",
                "bool": true,
                "null": Bson::Null,
                "array": [1, "two", { "three": 3 }],
                "nested": { "a": { "b": [ { "c": -1 } ] } },
            };
            let json = Bson::Document(original.clone()).into_relaxed_extjson();
            let ks = c.ks(&c.coll);
            text(
                c.conn.as_mut(),
                &format!("INSERT INTO {ks} (KEY, VALUE) VALUES (\"types\", {json})"),
            )
            .await?;
            let out = text(
                c.conn.as_mut(),
                &format!("SELECT * FROM {ks} USE KEYS \"types\""),
            )
            .await?;
            let mut back = out
                .documents
                .first()
                .map(decode)
                .ok_or("document not found")?;
            if back.remove("_id") != Some(Bson::String("types".into())) {
                return Err(format!("the key is not returned as _id: {back}"));
            }
            if back != original {
                return Err(format!(
                    "round trip changed the document:\n{original}\n{back}"
                ));
            }
            Ok(format!(
                "{} JSON types (JSON has no binary, date or decimal)",
                original.len()
            ))
        }
        Lang::Partiql => {
            let item = r#"{
                "pk": {"S": "types"}, "s": {"S": "zażółć 🦀"}, "n_int": {"N": "42"},
                "n_big": {"N": "9007199254740993"}, "n_dec": {"N": "12345.6789"},
                "n_huge": {"N": "12345678901234567890123456789012345678"},
                "b": {"B": "AAEC/w=="}, "bool": {"BOOL": true}, "null": {"NULL": true},
                "m": {"M": {"a": {"M": {"b": {"L": [{"M": {"c": {"N": "-1"}}}]}}}}},
                "l": {"L": [{"N": "1"}, {"S": "two"}]},
                "ss": {"SS": ["a", "b"]}, "ns": {"NS": ["1", "2.5"]}, "bs": {"BS": ["AQ==", "Ag=="]}
            }"#;
            let table = c.coll.clone();
            native(
                c.conn.as_mut(),
                "PutItem",
                &[("table", &table), ("item", item)],
            )
            .await?;
            let read = |pk: &str| format!("SELECT * FROM \"{table}\" WHERE pk = '{pk}'");
            let out = text(c.conn.as_mut(), &read("types")).await?;
            let mut back = out.documents.first().map(decode).ok_or("item not found")?;
            let bin = |b: Vec<u8>| {
                Bson::Binary(Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: b,
                })
            };
            let dec = |s: &str| {
                s.parse::<Decimal128>()
                    .map(Bson::Decimal128)
                    .map_err(|e| e.to_string())
            };
            let mut expected = bson::doc! {
                "pk": "types",
                "b": bin(vec![0, 1, 2, 255]),
                "bool": true,
                "bs": { "$BS": [bin(vec![1]), bin(vec![2])] },
                "l": [1, "two"],
                "m": { "a": { "b": [ { "c": -1 } ] } },
                "n_big": 9_007_199_254_740_993_i64,
                "n_dec": dec("12345.6789")?,
                "n_huge": "12345678901234567890123456789012345678",
                "n_int": 42,
                "ns": { "$NS": [1, dec("2.5")?] },
                "null": Bson::Null,
                "s": "zażółć 🦀",
                "ss": { "$SS": ["a", "b"] },
            };
            sorted_sets(&mut back);
            sorted_sets(&mut expected);
            if back != expected {
                return Err(format!("item → BSON:\n{expected}\n{back}"));
            }
            // BSON → item (what the document editor writes) → BSON.
            let mut again = back.clone();
            again.insert("pk", "types2");
            let mut m = serde_json::Map::new();
            for (k, v) in &again {
                let av =
                    ferrodb_drivers::dynamodb::value::from_bson(v).map_err(|e| e.to_string())?;
                m.insert(k.clone(), ferrodb_drivers::dynamodb::value::to_json(&av));
            }
            let json = serde_json::Value::Object(m).to_string();
            native(
                c.conn.as_mut(),
                "PutItem",
                &[("table", &table), ("item", &json)],
            )
            .await?;
            let out = text(c.conn.as_mut(), &read("types2")).await?;
            let mut twice = out
                .documents
                .first()
                .map(decode)
                .ok_or("item 2 not found")?;
            sorted_sets(&mut twice);
            twice.insert("pk", "types");
            if twice != back {
                return Err(format!("BSON → item → BSON changed it:\n{back}\n{twice}"));
            }
            Ok("14 attributes (all 10 DynamoDB types), both directions".into())
        }
    }
}

async fn writes(c: &mut Ctx<'_>) -> Result<String, String> {
    let ks = c.ks(&c.coll);
    let got = match c.lang {
        Lang::Sqlpp => {
            let ins = text(
                c.conn.as_mut(),
                &format!(
                    "INSERT INTO {ks} (KEY, VALUE) VALUES (\"w1\", {{\"k\": 1}}), (\"w2\", {{\"k\": 2}}), (\"w3\", {{\"k\": 3}})"
                ),
            )
            .await?;
            let upd = text(
                c.conn.as_mut(),
                &format!("UPDATE {ks} SET u = true WHERE k >= 2"),
            )
            .await?;
            let del = text(c.conn.as_mut(), &format!("DELETE FROM {ks} WHERE k = 1")).await?;
            (
                ins.summary.affected,
                upd.summary.affected,
                del.summary.affected,
            )
        }
        Lang::Partiql => {
            let mut n = 0;
            for k in 1..=3 {
                let o = text(
                    c.conn.as_mut(),
                    &format!("INSERT INTO {ks} VALUE {{'pk': 'w{k}', 'k': {k}}}"),
                )
                .await?;
                n += o.summary.affected.unwrap_or(0);
            }
            // DynamoDB updates one item per statement: two in one call.
            let stmts = format!(
                r#"[{{"Statement": "UPDATE {t} SET u = true WHERE pk = 'w2'"}}, {{"Statement": "UPDATE {t} SET u = true WHERE pk = 'w3'"}}]"#,
                t = ks.replace('"', "\\\"")
            );
            let upd = native(
                c.conn.as_mut(),
                "ExecuteTransaction",
                &[("statements", &stmts)],
            )
            .await?;
            let del = text(
                c.conn.as_mut(),
                &format!("DELETE FROM {ks} WHERE pk = 'w1'"),
            )
            .await?;
            (Some(n), upd.summary.affected, del.summary.affected)
        }
    };
    if got != (Some(3), Some(2), Some(1)) {
        return Err(format!("affected {got:?}, expected 3 / 2 / 1"));
    }
    let count = match c.lang {
        Lang::Sqlpp => text(
            c.conn.as_mut(),
            &format!("SELECT RAW COUNT(*) FROM {ks} WHERE u = true"),
        )
        .await?
        .documents
        .first()
        .map(decode)
        .and_then(|d| int(d.get("value"))),
        Lang::Partiql => Some(
            text(
                c.conn.as_mut(),
                &format!("SELECT * FROM {ks} WHERE u = true"),
            )
            .await?
            .documents
            .len() as i64,
        ),
    };
    if count != Some(2) {
        return Err(format!("{count:?} documents updated, expected 2"));
    }
    Ok("3 / 2 / 1".into())
}

/// Rows of the streaming collection / table.
fn big_rows(lang: Lang) -> usize {
    match lang {
        Lang::Sqlpp => 100_000,
        Lang::Partiql => 10_000,
    }
}

async fn fill_big(c: &mut Ctx<'_>) -> Result<String, String> {
    let want = big_rows(c.lang);
    let big = c.big.clone();
    let ks = c.ks(&big);
    match c.lang {
        Lang::Sqlpp => {
            let n = text(c.conn.as_mut(), &format!("SELECT RAW COUNT(*) FROM {ks}"))
                .await
                .ok()
                .and_then(|o| o.documents.first().map(decode))
                .and_then(|d| int(d.get("value")));
            if n == Some(want as i64) {
                return Ok(big);
            }
            if n.is_none() {
                create(c, &big).await?;
            }
            settle(
                c.conn.as_mut(),
                &format!(
                    "UPSERT INTO {ks} (KEY k, VALUE v) SELECT 'big::' || TOSTRING(i) AS k, \
                     {{'i': i, 's': 'document ' || TOSTRING(i)}} AS v FROM ARRAY_RANGE(0, {want}) AS i"
                ),
            )
            .await?;
        }
        Lang::Partiql => {
            let have = native(
                c.conn.as_mut(),
                "Scan",
                &[("table", &big), ("select", "COUNT")],
            )
            .await
            .ok()
            .and_then(|o| o.documents.last().map(decode))
            .and_then(|d| int(d.get("Count")));
            if have == Some(want as i64) {
                return Ok(big);
            }
            if have.is_none() {
                create(c, &big).await?;
            }
            // ~500 bytes per item: the table spans several 1 MB pages.
            let pad = "x".repeat(480);
            for chunk in 0..want / 100 {
                let stmts: Vec<String> = (0..100)
                    .map(|i| {
                        let n = chunk * 100 + i;
                        format!(
                            r#"{{"Statement": "INSERT INTO \"{big}\" VALUE {{'pk': 'big{n:06}', 'i': {n}, 's': '{pad}'}}"}}"#
                        )
                    })
                    .collect();
                native(
                    c.conn.as_mut(),
                    "ExecuteTransaction",
                    &[("statements", &format!("[{}]", stmts.join(",")))],
                )
                .await?;
            }
        }
    }
    Ok(big)
}

fn stream_all(c: &Ctx<'_>, big: &str) -> String {
    match c.lang {
        // Not `SELECT *`: the rows are read as they are.
        Lang::Sqlpp => format!("SELECT d.i, d.s FROM {} AS d", c.ks(big)),
        Lang::Partiql => format!("SELECT * FROM {}", c.ks(big)),
    }
}

async fn streaming(c: &mut Ctx<'_>) -> Result<String, String> {
    let big = fill_big(c).await?;
    let want = big_rows(c.lang);
    let (sink, mut rx) = ResultSink::channel(4, 1000);
    let t = Instant::now();
    let q = stream_all(c, &big);
    let exec = c
        .conn
        .execute(Request::Text { text: q }, sink, CancellationToken::new());
    let reader = async move {
        let (mut n, mut batches, mut largest, mut first) = (0usize, 0, 0, None);
        while let Some(ev) = rx.recv().await {
            if let SinkEvent::Documents(d) = ev {
                first.get_or_insert_with(|| t.elapsed());
                n += d.len();
                batches += 1;
                largest = largest.max(d.len());
            }
        }
        (n, batches, largest, first)
    };
    let (r, (n, batches, largest, first)) = futures::join!(exec, reader);
    r.map_err(|e| e.to_string())?;
    if n != want {
        return Err(format!("{n} documents streamed, expected {want}"));
    }
    if largest > 1000 {
        return Err(format!("a batch of {largest} documents (limit 1000)"));
    }
    Ok(format!(
        "{n} documents in {batches} batches, first after {} ms, all in {} ms",
        first.unwrap_or_default().as_millis(),
        t.elapsed().as_millis()
    ))
}

async fn truncate(c: &mut Ctx<'_>) -> Result<String, String> {
    let big = fill_big(c).await?;
    let (sink, mut rx) = ResultSink::channel(2, 100);
    let q = stream_all(c, &big);
    let exec = c
        .conn
        .execute(Request::Text { text: q }, sink, CancellationToken::new());
    let reader = async move {
        let mut n = 0;
        while let Some(ev) = rx.recv().await {
            if let SinkEvent::Documents(d) = ev {
                n += d.len();
                if n >= 500 {
                    break;
                }
            }
        }
        n
    };
    let (summary, n) = futures::join!(exec, reader);
    let summary = summary.map_err(|e| e.to_string())?;
    if !summary.truncated {
        return Err("summary not marked truncated".into());
    }
    c.conn
        .ping()
        .await
        .map_err(|e| format!("connection unusable: {e}"))?;
    Ok(format!("stopped after {n} documents"))
}

async fn cancel(c: &mut Ctx<'_>) -> Result<String, String> {
    let token = CancellationToken::new();
    let t2 = token.clone();
    let slow = match c.lang {
        Lang::Sqlpp => {
            "SELECT RAW COUNT(*) FROM ARRAY_RANGE(0, 20000) AS a UNNEST ARRAY_RANGE(0, 20000) AS b WHERE a + b = -1"
                .to_owned()
        }
        Lang::Partiql => {
            let big = fill_big(c).await?;
            format!("SELECT * FROM \"{big}\" WHERE i < 0")
        }
    };
    let canceller = async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        t2.cancel();
        Instant::now()
    };
    let (r, at) = futures::join!(
        collect_request(c.conn.as_mut(), Request::Text { text: slow }, token),
        canceller
    );
    let took = at.elapsed();
    match r {
        Err(Error::Cancelled) => {}
        // DynamoDB Local may finish a filtered scan of 10k items first.
        Ok(_) if c.lang == Lang::Partiql => {
            return Ok("finished before the cancel (no server-side operation to stop)".into());
        }
        other => {
            return Err(format!(
                "expected Cancelled, got {:?}",
                other.map(|o| o.summary)
            ));
        }
    }
    if took > Duration::from_secs(1) {
        return Err(format!("cancel took {} ms", took.as_millis()));
    }
    if c.lang == Lang::Sqlpp {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let left = text(
            c.conn.as_mut(),
            "SELECT RAW COUNT(*) FROM system:active_requests WHERE CONTAINS(statement, 'ARRAY_RANGE(0, 20000)') AND NOT CONTAINS(statement, 'system:active_requests')",
        )
        .await?
        .documents
        .first()
        .map(decode)
        .and_then(|d| int(d.get("value")));
        if left != Some(0) {
            return Err(format!("the request still runs on the server ({left:?})"));
        }
    }
    c.conn
        .ping()
        .await
        .map_err(|e| format!("connection unusable after cancel: {e}"))?;
    Ok(format!("{} ms", took.as_millis()))
}

async fn errors(c: &mut Ctx<'_>) -> Result<String, String> {
    let ks = c.ks(&c.coll);
    let insert = match c.lang {
        Lang::Sqlpp => format!("INSERT INTO {ks} (KEY, VALUE) VALUES (\"dup\", {{}})"),
        Lang::Partiql => format!("INSERT INTO {ks} VALUE {{'pk': 'dup'}}"),
    };
    text(c.conn.as_mut(), &insert).await?;
    let dup = collect_request(
        c.conn.as_mut(),
        Request::Text {
            text: insert.clone(),
        },
        CancellationToken::new(),
    )
    .await;
    let code = match dup {
        Err(Error::Query(q)) => q.code.unwrap_or_default(),
        other => return Err(format!("duplicate key: {:?}", other.map(|o| o.summary))),
    };
    // DynamoDB: `DuplicateItemException` (AWS), `DuplicateItem` (Local).
    let expected = match c.lang {
        Lang::Sqlpp => "12009",
        Lang::Partiql => "DuplicateItem",
    };
    if !code.starts_with(expected) {
        return Err(format!("duplicate key code {code}, expected {expected}"));
    }
    let bad = format!("SELECT * FROM {ks} WHERE");
    let parse = collect_request(
        c.conn.as_mut(),
        Request::Text { text: bad.clone() },
        CancellationToken::new(),
    )
    .await;
    let position = match (c.lang, parse) {
        (Lang::Sqlpp, Err(Error::Query(q))) if q.range.is_some() => {
            let r = q.range.unwrap_or_default();
            if r.start > bad.len() {
                return Err(format!("position {r:?} outside the statement"));
            }
            format!(", syntax error at {}", r.start)
        }
        (Lang::Partiql, Err(Error::Query(q))) => {
            format!(
                ", syntax error {} (no positions in DynamoDB errors)",
                q.code.unwrap_or_default()
            )
        }
        (_, other) => return Err(format!("syntax error: {:?}", other.map(|o| o.summary))),
    };
    c.conn.ping().await.map_err(|e| e.to_string())?;
    Ok(format!("duplicate key {code}{position}"))
}

async fn transactions(c: &mut Ctx<'_>) -> Result<String, String> {
    let ks = c.ks(&c.coll);
    if c.lang == Lang::Partiql {
        match c.conn.begin().await {
            Err(Error::Unsupported(_)) => {}
            other => return Err(format!("begin: expected Unsupported, got {other:?}")),
        }
        // ExecuteTransaction is all or nothing: the duplicate fails both.
        let t = ks.replace('"', "\\\"");
        let stmts = format!(
            r#"[{{"Statement": "INSERT INTO {t} VALUE {{'pk': 'tx-new'}}"}}, {{"Statement": "INSERT INTO {t} VALUE {{'pk': 'dup'}}"}}]"#
        );
        if native(
            c.conn.as_mut(),
            "ExecuteTransaction",
            &[("statements", &stmts)],
        )
        .await
        .is_ok()
        {
            return Err("a transaction with a duplicate key succeeded".into());
        }
        let left = text(
            c.conn.as_mut(),
            &format!("SELECT * FROM {ks} WHERE pk = 'tx-new'"),
        )
        .await?
        .documents
        .len();
        if left != 0 {
            return Err("the first statement of a failed transaction was applied".into());
        }
        return Ok("single-call transactions: all or nothing".into());
    }
    match c.conn.begin().await {
        Err(Error::Unsupported(why)) => return Ok(format!("not supported here: {why}")),
        Err(e) => return Err(e.to_string()),
        Ok(()) => {}
    }
    text(
        c.conn.as_mut(),
        &format!("INSERT INTO {ks} (KEY, VALUE) VALUES (\"tx-rb\", {{\"tx\": \"rolled back\"}})"),
    )
    .await?;
    c.conn.rollback().await.map_err(|e| e.to_string())?;
    c.conn.begin().await.map_err(|e| e.to_string())?;
    text(
        c.conn.as_mut(),
        &format!("INSERT INTO {ks} (KEY, VALUE) VALUES (\"tx-cm\", {{\"tx\": \"committed\"}})"),
    )
    .await?;
    c.conn.commit().await.map_err(|e| e.to_string())?;
    let n = |o: CollectedOutput| o.documents.len();
    let rb = n(text(
        c.conn.as_mut(),
        &format!("SELECT * FROM {ks} USE KEYS \"tx-rb\""),
    )
    .await?);
    let cm = n(text(
        c.conn.as_mut(),
        &format!("SELECT * FROM {ks} USE KEYS \"tx-cm\""),
    )
    .await?);
    if (rb, cm) != (0, 1) {
        return Err(format!("rolled back {rb}, committed {cm}"));
    }
    Ok("rollback and commit".into())
}

async fn switch(c: &mut Ctx<'_>) -> Result<String, String> {
    let before = c.conn.current_schema().await.map_err(|e| e.to_string())?;
    if c.lang == Lang::Partiql {
        return match before {
            Some(r) => Ok(format!("one container per region ({r})")),
            None => Err("no current container".into()),
        };
    }
    c.conn
        .set_schema("other-bucket")
        .await
        .map_err(|e| e.to_string())?;
    let after = c.conn.current_schema().await.map_err(|e| e.to_string())?;
    if after.as_deref() != Some("other-bucket") {
        return Err(format!("set_schema left {after:?}"));
    }
    c.conn
        .set_schema(before.as_deref().unwrap_or_default())
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("{before:?} → other-bucket → back"))
}

async fn listings(c: &mut Ctx<'_>) -> Result<String, String> {
    let coll = c.coll.clone();
    match c.lang {
        Lang::Sqlpp => {
            let buckets = native(c.conn.as_mut(), "buckets", &[]).await?;
            let bucket = c.prefix.split('`').nth(1).unwrap_or_default().to_owned();
            if !buckets
                .documents
                .iter()
                .map(decode)
                .any(|d| d.get_str("name").ok() == Some(bucket.as_str()))
            {
                return Err(format!("`buckets` misses {bucket}"));
            }
            let scopes = native(c.conn.as_mut(), "scopes", &[("bucket", &bucket)]).await?;
            let has = scopes.documents.iter().map(decode).any(|s| {
                s.get_array("collections").is_ok_and(|cs| {
                    cs.iter()
                        .filter_map(Bson::as_document)
                        .any(|x| x.get_str("name").ok() == Some(coll.as_str()))
                })
            });
            if !has {
                return Err("`scopes` misses the test collection".into());
            }
            let idx = native(c.conn.as_mut(), "indexes", &[("bucket", &bucket)]).await?;
            if idx.documents.is_empty() {
                return Err("no indexes listed".into());
            }
            Ok(format!(
                "{} buckets, {} scopes, {} indexes",
                buckets.documents.len(),
                scopes.documents.len(),
                idx.documents.len()
            ))
        }
        Lang::Partiql => {
            let tables = native(c.conn.as_mut(), "ListTables", &[]).await?;
            if !tables
                .documents
                .iter()
                .map(decode)
                .any(|d| d.get_str("name").ok() == Some(coll.as_str()))
            {
                return Err("`ListTables` misses the test table".into());
            }
            let desc = native(c.conn.as_mut(), "DescribeTable", &[("table", &coll)]).await?;
            let d = desc.documents.first().map(decode).ok_or("no description")?;
            if d.get_array("KeySchema").map(Vec::len).ok() != Some(1) {
                return Err(format!("description {d}"));
            }
            Ok(format!("{} tables", tables.documents.len()))
        }
    }
}
