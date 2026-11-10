//! TCK for the document model (plan, phase 4b): the same expectations as
//! the tabular suite, in MQL — connection, BSON type round trip, write
//! counts, streaming 100k documents with backpressure, cancellation of a
//! running server operation, errors with codes and positions, transactions
//! (replica set), database switching and listings.

use std::time::{Duration, Instant};

use bson::{
    Binary, Bson, DateTime, Decimal128, Document, RawDocumentBuf, Regex, Timestamp, doc,
    oid::ObjectId, spec::BinarySubtype,
};
use ferrodb_api::{DataModel, Error, QueryLanguage};
use ferrodb_drivers::{
    CancellationToken, Connection, Driver, Request, ResolvedConfig, ResultSink, SinkEvent,
    sink::{CollectedOutput, collect_request},
};
use ferrodb_lang::mql;

use crate::{Outcome, Report};

struct Ctx<'a> {
    driver: &'a dyn Driver,
    conn: Box<dyn Connection>,
    /// Collection used by this run.
    coll: String,
}

async fn run_mql(conn: &mut dyn Connection, text: &str) -> Result<CollectedOutput, String> {
    let cmd = mql::parse(text).map_err(|e| format!("`{text}`: {}", e.message))?;
    collect_request(conn, Request::Mql(cmd), CancellationToken::new())
        .await
        .map_err(|e| format!("`{text}`: {e}"))
}

fn decode(d: &RawDocumentBuf) -> Document {
    Document::try_from(d.clone()).unwrap_or_default()
}

/// Runs every document scenario. Never panics; failures are reported.
pub async fn run_documents(driver: &dyn Driver, cfg: &ResolvedConfig) -> Report {
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
    let mut ctx = Ctx {
        driver,
        conn,
        coll: crate::table_name("docs"),
    };
    macro_rules! scenario {
        ($name:literal, $fut:expr) => {{
            let t = Instant::now();
            let r = tokio::time::timeout(Duration::from_secs(120), $fut).await;
            let (passed, detail) = match r {
                Ok(Ok(d)) => (true, d),
                Ok(Err(e)) => (false, e),
                Err(_) => (false, "timed out after 120 s".to_owned()),
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
    scenario!("bson-types", types(&mut ctx));
    scenario!("writes-affected", writes(&mut ctx));
    scenario!("streaming-100k", streaming(&mut ctx));
    scenario!("backpressure-truncate", truncate(&mut ctx));
    scenario!("cancel", cancel(&mut ctx));
    scenario!("errors", errors(&mut ctx));
    scenario!("transactions", transactions(&mut ctx));
    scenario!("database-switch", switch(&mut ctx));
    scenario!("listings", listings(&mut ctx));
    let _ = run_mql(
        ctx.conn.as_mut(),
        &format!("db.getCollection('{}').drop()", ctx.coll),
    )
    .await;
    let _ = run_mql(
        ctx.conn.as_mut(),
        &format!("db.getCollection('{}_big').drop()", ctx.coll),
    )
    .await;
    report
}

async fn descriptor(c: &mut Ctx<'_>) -> Result<String, String> {
    let d = c.driver.descriptor();
    let caps = d.capabilities;
    if caps.data_model != DataModel::Document || caps.language != QueryLanguage::Mql {
        return Err(format!(
            "unexpected model {:?} / {:?}",
            caps.data_model, caps.language
        ));
    }
    Ok(format!("{} ({:?})", d.name, caps.transactions))
}

/// Every BSON type survives insert → find unchanged.
async fn types(c: &mut Ctx<'_>) -> Result<String, String> {
    let original = doc! {
        "_id": ObjectId::new(),
        "int32": 42_i32,
        "int64": 9_007_199_254_740_993_i64,
        "double": 1.5,
        "decimal": "12345.6789".parse::<Decimal128>().map_err(|e| e.to_string())?,
        "string": "zażółć 🦀",
        "bool": true,
        "null": Bson::Null,
        "date": DateTime::from_millis(1_704_164_645_123),
        "binary": Binary { subtype: BinarySubtype::Generic, bytes: vec![0, 1, 2, 255] },
        "uuid": Binary { subtype: BinarySubtype::Uuid, bytes: (0u8..16).collect() },
        "regex": Regex { pattern: "^a.c$".to_owned().try_into().map_err(|e| format!("{e:?}"))?, options: "i".to_owned().try_into().map_err(|e| format!("{e:?}"))? },
        "timestamp": Timestamp { time: 1_700_000_000, increment: 7 },
        "array": [1, "two", { "three": 3 }],
        "nested": { "a": { "b": [ { "c": Bson::MinKey }, Bson::MaxKey ] } },
    };
    // Through the shell syntax, as the console sends it.
    let text = format!(
        "db.getCollection('{}').insertOne({})",
        c.coll,
        mql::print_inline(&Bson::Document(original.clone()))
    );
    run_mql(c.conn.as_mut(), &text).await?;
    let out = run_mql(
        c.conn.as_mut(),
        &format!(
            "db.getCollection('{}').findOne({{ _id: {} }})",
            c.coll,
            mql::print_inline(original.get("_id").unwrap())
        ),
    )
    .await?;
    let back = out
        .documents
        .first()
        .map(decode)
        .ok_or("document not found")?;
    if back != original {
        return Err(format!(
            "round trip changed the document:\n{original}\n{back}"
        ));
    }
    Ok(format!("{} types", original.len() - 1))
}

async fn writes(c: &mut Ctx<'_>) -> Result<String, String> {
    let coll = format!("db.getCollection('{}')", c.coll);
    let ins = run_mql(
        c.conn.as_mut(),
        &format!("{coll}.insertMany([{{k: 1}}, {{k: 2}}, {{k: 3}}])"),
    )
    .await?;
    let upd = run_mql(
        c.conn.as_mut(),
        &format!("{coll}.updateMany({{k: {{$gte: 2}}}}, {{$set: {{u: true}}}})"),
    )
    .await?;
    let del = run_mql(c.conn.as_mut(), &format!("{coll}.deleteOne({{k: 1}})")).await?;
    let got = (
        ins.summary.affected,
        upd.summary.affected,
        del.summary.affected,
    );
    if got != (Some(3), Some(2), Some(1)) {
        return Err(format!("affected {got:?}, expected 3 / 2 / 1"));
    }
    let r = decode(ins.documents.first().ok_or("no insert result")?);
    if r.get_document("insertedIds").map(Document::len).ok() != Some(3) {
        return Err(format!("insert result {r}"));
    }
    let n = run_mql(
        c.conn.as_mut(),
        &format!("{coll}.countDocuments({{k: {{$exists: true}}}})"),
    )
    .await?;
    let count = n
        .documents
        .first()
        .map(decode)
        .and_then(|d| d.get("count").cloned());
    if count != Some(Bson::Int32(2)) && count != Some(Bson::Int64(2)) {
        return Err(format!("countDocuments returned {count:?}"));
    }
    Ok("3 / 2 / 1".into())
}

async fn fill_big(c: &mut Ctx<'_>) -> Result<String, String> {
    let big = format!("{}_big", c.coll);
    let have = run_mql(
        c.conn.as_mut(),
        &format!("db.getCollection('{big}').estimatedDocumentCount()"),
    )
    .await?;
    let n = have
        .documents
        .first()
        .map(decode)
        .and_then(|d| d.get("count").cloned());
    if matches!(n, Some(Bson::Int32(100_000) | Bson::Int64(100_000))) {
        return Ok(big);
    }
    for chunk in 0..10 {
        let docs: Vec<String> = (0..10_000)
            .map(|i| format!("{{i: {}, s: 'document {}'}}", chunk * 10_000 + i, i))
            .collect();
        run_mql(
            c.conn.as_mut(),
            &format!(
                "db.getCollection('{big}').insertMany([{}], {{ordered: false}})",
                docs.join(",")
            ),
        )
        .await?;
    }
    Ok(big)
}

async fn streaming(c: &mut Ctx<'_>) -> Result<String, String> {
    let big = fill_big(c).await?;
    let (sink, mut rx) = ResultSink::channel(4, 1000);
    let cmd =
        mql::parse(&format!("db.getCollection('{big}').find({{}})")).map_err(|e| e.message)?;
    let t = Instant::now();
    let exec = c
        .conn
        .execute(Request::Mql(cmd), sink, CancellationToken::new());
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
    if n != 100_000 {
        return Err(format!("{n} documents streamed"));
    }
    if largest > 1000 {
        return Err(format!("a batch of {largest} documents (limit 1000)"));
    }
    Ok(format!(
        "{batches} batches, first after {} ms, all in {} ms",
        first.unwrap_or_default().as_millis(),
        t.elapsed().as_millis()
    ))
}

async fn truncate(c: &mut Ctx<'_>) -> Result<String, String> {
    let big = fill_big(c).await?;
    let (sink, mut rx) = ResultSink::channel(2, 100);
    let cmd =
        mql::parse(&format!("db.getCollection('{big}').find({{}})")).map_err(|e| e.message)?;
    let exec = c
        .conn
        .execute(Request::Mql(cmd), sink, CancellationToken::new());
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

/// A server-side JavaScript sleep, cancelled after 300 ms: the operation
/// must be gone from `currentOp` (killOp), not only the client future.
async fn cancel(c: &mut Ctx<'_>) -> Result<String, String> {
    let big = fill_big(c).await?;
    let token = CancellationToken::new();
    let t2 = token.clone();
    let text = format!("db.getCollection('{big}').find({{$where: 'sleep(100) || true'}})");
    let cmd = mql::parse(&text).map_err(|e| e.message)?;
    let canceller = async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        t2.cancel();
        Instant::now()
    };
    let (r, at) = futures::join!(
        collect_request(c.conn.as_mut(), Request::Mql(cmd), token),
        canceller
    );
    let took = at.elapsed();
    match r {
        Err(Error::Cancelled) => {}
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
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ops = run_mql(
        c.conn.as_mut(),
        "db.adminCommand({currentOp: 1, 'command.$where': {$exists: true}})",
    )
    .await
    .or_else(|_| Ok::<_, String>(CollectedOutput::default()))?;
    let left = ops
        .documents
        .first()
        .map(decode)
        .and_then(|d| d.get_array("inprog").ok().map(Vec::len))
        .unwrap_or(0);
    let left_where = ops
        .documents
        .first()
        .map(decode)
        .map(|d| d.to_string().contains("sleep(100)"))
        .unwrap_or(false);
    if left_where {
        return Err(format!(
            "the operation still runs on the server ({left} ops)"
        ));
    }
    c.conn
        .ping()
        .await
        .map_err(|e| format!("connection unusable after cancel: {e}"))?;
    Ok(format!("{} ms", took.as_millis()))
}

async fn errors(c: &mut Ctx<'_>) -> Result<String, String> {
    let coll = format!("db.getCollection('{}')", c.coll);
    let id = ObjectId::new().to_hex();
    run_mql(
        c.conn.as_mut(),
        &format!("{coll}.insertOne({{_id: ObjectId('{id}')}})"),
    )
    .await?;
    let cmd = mql::parse(&format!("{coll}.insertOne({{_id: ObjectId('{id}')}})"))
        .map_err(|e| e.message)?;
    let dup = collect_request(c.conn.as_mut(), Request::Mql(cmd), CancellationToken::new()).await;
    let code = match dup {
        Err(Error::Query(q)) => q.code.unwrap_or_default(),
        other => return Err(format!("duplicate key: {:?}", other.map(|o| o.summary))),
    };
    if !code.starts_with("11000") {
        return Err(format!("duplicate key code {code}"));
    }
    // A parse error points at the problem.
    let parse = collect_request(
        c.conn.as_mut(),
        Request::Text {
            text: "db.c.find({a: 1".into(),
        },
        CancellationToken::new(),
    )
    .await;
    match parse {
        Err(Error::Query(q)) if q.range.is_some() => {}
        other => return Err(format!("parse error: {:?}", other.map(|o| o.summary))),
    }
    let bad = run_mql(c.conn.as_mut(), "db.runCommand({noSuchCommand: 1})").await;
    if bad.is_ok() {
        return Err("an unknown command succeeded".into());
    }
    c.conn.ping().await.map_err(|e| e.to_string())?;
    Ok(format!("duplicate key {code}"))
}

async fn transactions(c: &mut Ctx<'_>) -> Result<String, String> {
    let coll = format!("db.getCollection('{}')", c.coll);
    let count = |n: &CollectedOutput| {
        n.documents
            .first()
            .map(decode)
            .and_then(|d| d.get("count").cloned())
    };
    match c.conn.begin().await {
        Err(Error::Unsupported(why)) => return Ok(format!("not supported here: {why}")),
        Err(e) => return Err(e.to_string()),
        Ok(()) => {}
    }
    run_mql(
        c.conn.as_mut(),
        &format!("{coll}.insertOne({{tx: 'rolled back'}})"),
    )
    .await?;
    c.conn.rollback().await.map_err(|e| e.to_string())?;
    c.conn.begin().await.map_err(|e| e.to_string())?;
    run_mql(
        c.conn.as_mut(),
        &format!("{coll}.insertOne({{tx: 'committed'}})"),
    )
    .await?;
    c.conn.commit().await.map_err(|e| e.to_string())?;
    let rb = run_mql(
        c.conn.as_mut(),
        &format!("{coll}.countDocuments({{tx: 'rolled back'}})"),
    )
    .await?;
    let cm = run_mql(
        c.conn.as_mut(),
        &format!("{coll}.countDocuments({{tx: 'committed'}})"),
    )
    .await?;
    let (rb, cm) = (count(&rb), count(&cm));
    let is = |v: &Option<Bson>, n: i64| {
        matches!(v, Some(Bson::Int32(x)) if *x as i64 == n)
            || matches!(v, Some(Bson::Int64(x)) if *x == n)
    };
    if !is(&rb, 0) || !is(&cm, 1) {
        return Err(format!("rolled back {rb:?}, committed {cm:?}"));
    }
    Ok("rollback and commit".into())
}

async fn switch(c: &mut Ctx<'_>) -> Result<String, String> {
    let before = c.conn.current_schema().await.map_err(|e| e.to_string())?;
    run_mql(c.conn.as_mut(), "use admin").await?;
    let after = c.conn.current_schema().await.map_err(|e| e.to_string())?;
    if after.as_deref() != Some("admin") {
        return Err(format!("`use admin` left {after:?}"));
    }
    c.conn
        .set_schema(before.as_deref().unwrap_or("test"))
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("{before:?} → admin → back"))
}

async fn listings(c: &mut Ctx<'_>) -> Result<String, String> {
    let dbs = run_mql(c.conn.as_mut(), "show dbs").await?;
    if !dbs
        .documents
        .iter()
        .map(decode)
        .any(|d| d.get_str("name").ok() == Some("admin"))
    {
        return Err("`show dbs` does not list admin".into());
    }
    let cols = run_mql(c.conn.as_mut(), "show collections").await?;
    if !cols
        .documents
        .iter()
        .map(decode)
        .any(|d| d.get_str("name").ok() == Some(c.coll.as_str()))
    {
        return Err("`show collections` misses the test collection".into());
    }
    let idx = run_mql(
        c.conn.as_mut(),
        &format!("db.getCollection('{}').getIndexes()", c.coll),
    )
    .await?;
    if idx.documents.is_empty() {
        return Err("no _id index".into());
    }
    Ok(format!(
        "{} databases, {} collections",
        dbs.documents.len(),
        cols.documents.len()
    ))
}
