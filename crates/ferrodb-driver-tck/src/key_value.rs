//! TCK v3 (phase 8): the key-value model (Redis).
//!
//! Scenarios: connect, descriptor, ping, a round trip of every value type
//! through console commands and the key browser's native calls
//! ([`ferrodb_drivers::kv`]), TTLs, key scans (streamed with backpressure,
//! cursor paging, limits), reply rendering and server errors, `MULTI` /
//! `EXEC`, read-only refusal, cancelling a blocking command (`BLPOP`)
//! within a second, and connection loss.

use std::time::{Duration, Instant};

use ferrodb_api::{DataModel, Error, KeyType, KeyValue, QueryLanguage};
use ferrodb_drivers::{
    CancellationToken, Connection, Driver, Request, ResolvedConfig, ResultSink, SinkEvent,
    kv::{self, method},
    sink::{CollectedOutput, collect, collect_request},
};

use crate::{Outcome, Report};

struct Ctx<'a> {
    driver: &'a dyn Driver,
    cfg: &'a ResolvedConfig,
    conn: Box<dyn Connection>,
    /// Prefix of every key the suite creates.
    prefix: String,
}

async fn cmd(conn: &mut dyn Connection, line: &str) -> Result<CollectedOutput, String> {
    collect(conn, line, CancellationToken::new())
        .await
        .map_err(|e| format!("`{line}`: {e}"))
}

async fn native(
    conn: &mut dyn Connection,
    m: &str,
    args: &[(&str, &str)],
) -> Result<CollectedOutput, Error> {
    collect_request(conn, Request::native(m, args), CancellationToken::new()).await
}

fn first_text(out: &CollectedOutput) -> Option<String> {
    use arrow_array::{Array, Int64Array, StringArray};
    let b = out
        .results
        .first()?
        .batches
        .iter()
        .find(|b| b.num_rows() > 0)?;
    let c = b.column(0);
    if let Some(s) = c.as_any().downcast_ref::<StringArray>() {
        return (!s.is_null(0)).then(|| s.value(0).to_owned());
    }
    c.as_any()
        .downcast_ref::<Int64Array>()
        .map(|i| i.value(0).to_string())
}

impl Ctx<'_> {
    fn key(&self, name: &str) -> String {
        // One hash tag: every key of the suite lives in one cluster slot
        // (RENAME, MULTI across keys).
        format!("{{{}}}:{name}", self.prefix)
    }

    async fn value(&mut self, key: &str) -> Result<KeyValue, String> {
        let out = native(self.conn.as_mut(), method::VALUE, &[("key", key)])
            .await
            .map_err(|e| format!("value of {key}: {e}"))?;
        kv::value_from_results(&out.results, None).map_err(|e| e.to_string())
    }

    async fn info(&mut self, key: &str) -> Result<Vec<ferrodb_api::KeyEntry>, String> {
        let out = native(self.conn.as_mut(), method::KEY_INFO, &[("key", key)])
            .await
            .map_err(|e| format!("key_info {key}: {e}"))?;
        Ok(out
            .results
            .iter()
            .flat_map(|r| r.batches.iter())
            .flat_map(kv::keys_from_batch)
            .collect())
    }
}

/// Runs the key-value scenarios. Never panics; failures are reported.
pub async fn run_key_value(driver: &dyn Driver, cfg: &ResolvedConfig) -> Report {
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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut ctx = Ctx {
        driver,
        cfg,
        conn,
        prefix: format!("tck:{}{nanos}", std::process::id()),
    };
    macro_rules! scenario {
        ($name:literal, $fut:expr) => {{
            let t = Instant::now();
            let r = tokio::time::timeout(Duration::from_secs(60), $fut).await;
            let (passed, detail) = match r {
                Ok(Ok(d)) => (true, d),
                Ok(Err(e)) => (false, e),
                Err(_) => (false, "timed out after 60 s".to_owned()),
            };
            report.outcomes.push(Outcome {
                scenario: $name,
                passed,
                detail,
                elapsed: t.elapsed(),
            });
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
    scenario!("types-round-trip", types(&mut ctx));
    scenario!("json", json(&mut ctx));
    scenario!("ttl", ttl(&mut ctx));
    scenario!("scan-paging", scan(&mut ctx));
    scenario!("console-replies", replies(&mut ctx));
    scenario!("multi-exec", multi(&mut ctx));
    scenario!("edits", edits(&mut ctx));
    scenario!("read-only", read_only(&mut ctx));
    scenario!("cancel-blocking", cancel(&mut ctx));
    cleanup(&mut ctx).await;
    scenario!("connection-loss", connection_loss(&mut ctx));
    report
}

async fn descriptor(c: &mut Ctx<'_>) -> Result<String, String> {
    let d = c.driver.descriptor();
    if d.capabilities.data_model != DataModel::KeyValue {
        return Err("not a key-value driver".into());
    }
    if d.capabilities.language != QueryLanguage::RedisCommands {
        return Err(format!("language {:?}", d.capabilities.language));
    }
    let h = c.driver.variant().hierarchy();
    if h.key_separator.is_none() {
        return Err("no key separator in the hierarchy".into());
    }
    Ok(format!("{} / {}", d.family, c.driver.variant().dialect()))
}

async fn types(c: &mut Ctx<'_>) -> Result<String, String> {
    let (s, h, l, st, z, x) = (
        c.key("string"),
        c.key("hash"),
        c.key("list"),
        c.key("set"),
        c.key("zset"),
        c.key("stream"),
    );
    let conn = c.conn.as_mut();
    cmd(conn, &kv::command_line(&["SET", &s, "zażółć \"gęślą\""])).await?;
    cmd(conn, &format!("HSET {h} name Ann age 30")).await?;
    cmd(conn, &format!("RPUSH {l} a b c")).await?;
    cmd(conn, &format!("SADD {st} x y")).await?;
    cmd(conn, &format!("ZADD {z} 1.5 low 10 high")).await?;
    cmd(conn, &format!("XADD {x} 1-1 type click user 7")).await?;
    let mut got = Vec::new();
    match c.value(&s).await? {
        KeyValue::String(v) if v == "zażółć \"gęślą\"" => got.push("string"),
        other => return Err(format!("string: {other:?}")),
    }
    match c.value(&h).await? {
        KeyValue::Hash(mut v) => {
            v.sort();
            if v != [("age".into(), "30".into()), ("name".into(), "Ann".into())] {
                return Err(format!("hash: {v:?}"));
            }
            got.push("hash");
        }
        other => return Err(format!("hash: {other:?}")),
    }
    match c.value(&l).await? {
        KeyValue::List(v) if v == ["a", "b", "c"] => got.push("list"),
        other => return Err(format!("list: {other:?}")),
    }
    match c.value(&st).await? {
        KeyValue::Set(mut v) => {
            v.sort();
            if v != ["x", "y"] {
                return Err(format!("set: {v:?}"));
            }
            got.push("set");
        }
        other => return Err(format!("set: {other:?}")),
    }
    match c.value(&z).await? {
        KeyValue::ZSet(v) if v == [("low".to_owned(), 1.5), ("high".to_owned(), 10.0)] => {
            got.push("zset")
        }
        other => return Err(format!("zset: {other:?}")),
    }
    match c.value(&x).await? {
        KeyValue::Stream(v)
            if v.len() == 1
                && v[0].id == "1-1"
                && v[0].fields
                    == [("type".into(), "click".into()), ("user".into(), "7".into())] =>
        {
            got.push("stream")
        }
        other => return Err(format!("stream: {other:?}")),
    }
    let info = c.info(&h).await?;
    if info.first().map(|k| k.kind) != Some(KeyType::Hash) {
        return Err(format!("key_info: {info:?}"));
    }
    // A limit caps the elements read.
    let out = native(
        c.conn.as_mut(),
        method::VALUE,
        &[("key", &l), ("limit", "2")],
    )
    .await
    .map_err(|e| e.to_string())?;
    match kv::value_from_results(&out.results, None) {
        Ok(KeyValue::List(v)) if v.len() == 2 => {}
        other => return Err(format!("limited list: {other:?}")),
    }
    Ok(got.join(", "))
}

async fn json(c: &mut Ctx<'_>) -> Result<String, String> {
    let j = c.key("json");
    let r = collect(
        c.conn.as_mut(),
        kv::command_line(&["JSON.SET", &j, "$", r#"{"a":1,"b":[true]}"#]),
        CancellationToken::new(),
    )
    .await;
    match r {
        Err(Error::Query(q)) if q.message.to_ascii_lowercase().contains("unknown command") => {
            return Ok("skipped: RedisJSON is not loaded".into());
        }
        Err(e) => return Err(e.to_string()),
        Ok(_) => {}
    }
    match c.value(&j).await? {
        KeyValue::Json(v) if v.replace(' ', "") == r#"{"a":1,"b":[true]}"# => Ok(v),
        other => Err(format!("json: {other:?}")),
    }
}

async fn ttl(c: &mut Ctx<'_>) -> Result<String, String> {
    let k = c.key("ttl");
    cmd(c.conn.as_mut(), &format!("SET {k} v EX 100")).await?;
    let ttl = c.info(&k).await?.first().and_then(|e| e.ttl);
    if !ttl.is_some_and(|t| t > Duration::from_secs(90) && t <= Duration::from_secs(100)) {
        return Err(format!("TTL after SET EX 100: {ttl:?}"));
    }
    native(c.conn.as_mut(), method::EXPIRE, &[("key", &k), ("ttl", "")])
        .await
        .map_err(|e| e.to_string())?;
    let after = c.info(&k).await?.first().and_then(|e| e.ttl);
    if after.is_some() {
        return Err(format!("TTL after PERSIST: {after:?}"));
    }
    native(
        c.conn.as_mut(),
        method::EXPIRE,
        &[("key", &k), ("ttl", "5")],
    )
    .await
    .map_err(|e| e.to_string())?;
    let out = cmd(c.conn.as_mut(), &format!("TTL {k}")).await?;
    let t = first_text(&out).and_then(|t| t.parse::<i64>().ok());
    if !t.is_some_and(|t| (1..=5).contains(&t)) {
        return Err(format!("TTL after EXPIRE 5: {t:?}"));
    }
    Ok(format!(
        "{} s → persist → {} s",
        ttl.unwrap_or_default().as_secs(),
        t.unwrap_or(0)
    ))
}

async fn scan(c: &mut Ctx<'_>) -> Result<String, String> {
    let n = 2500usize;
    // MSET in chunks (one slot per chunk in a cluster: hash tags).
    for chunk in 0..(n / 500) {
        let mut line = String::from("MSET");
        for i in chunk * 500..(chunk + 1) * 500 {
            line.push_str(&format!(" {{{}}}:scan:{i} {i}", c.prefix));
        }
        cmd(c.conn.as_mut(), &line).await?;
    }
    let pattern = format!("{{{}}}:scan:*", c.prefix);
    // Whole keyspace, streamed in small batches.
    let (sink, mut rx) = ResultSink::channel(2, 300);
    let exec = c.conn.execute(
        Request::native(method::SCAN, &[("pattern", &pattern), ("count", "200")]),
        sink,
        CancellationToken::new(),
    );
    let read = async move {
        let (mut keys, mut batches, mut largest) = (0usize, 0usize, 0usize);
        while let Some(ev) = rx.recv().await {
            if let SinkEvent::Batch(b) = ev {
                let e = kv::keys_from_batch(&b);
                if e.iter()
                    .any(|k| k.kind != KeyType::String || k.size.is_none())
                {
                    return Err("keys without type or size".to_owned());
                }
                keys += e.len();
                batches += 1;
                largest = largest.max(b.num_rows());
            }
        }
        Ok((keys, batches, largest))
    };
    let (summary, read) = futures::join!(exec, read);
    summary.map_err(|e| e.to_string())?;
    let (keys, batches, largest) = read?;
    if keys != n || largest > 300 {
        return Err(format!(
            "{keys} keys in {batches} batches (largest {largest})"
        ));
    }
    // Cursor paging: one SCAN round per call.
    let mut cursor = "0".to_owned();
    let mut paged = 0usize;
    let mut rounds = 0;
    loop {
        let out = native(
            c.conn.as_mut(),
            method::SCAN,
            &[
                ("pattern", &pattern),
                ("count", "1000"),
                ("cursor", &cursor),
                ("meta", "false"),
            ],
        )
        .await
        .map_err(|e| e.to_string())?;
        paged += out.results.first().map(|r| r.num_rows()).unwrap_or(0);
        cursor = out
            .results
            .get(1)
            .and_then(|r| {
                let co = CollectedOutput {
                    results: vec![r.clone()],
                    ..Default::default()
                };
                first_text(&co)
            })
            .ok_or("no cursor result")?;
        rounds += 1;
        if cursor == "0" || rounds > 10_000 {
            break;
        }
    }
    if paged < n {
        return Err(format!("cursor paging found {paged} keys"));
    }
    // A limit stops the scan.
    let out = native(
        c.conn.as_mut(),
        method::SCAN,
        &[("pattern", &pattern), ("limit", "100")],
    )
    .await
    .map_err(|e| e.to_string())?;
    let limited = out.results.first().map(|r| r.num_rows()).unwrap_or(0);
    if limited != 100 {
        return Err(format!("limit 100 gave {limited} keys"));
    }
    Ok(format!(
        "{n} keys in {batches} batches; {rounds} cursor rounds; limit 100"
    ))
}

async fn replies(c: &mut Ctx<'_>) -> Result<String, String> {
    let h = c.key("hash");
    let out = cmd(c.conn.as_mut(), &format!("HGETALL {h}")).await?;
    let r = out.results.first().ok_or("no result for HGETALL")?;
    let cols: Vec<&str> = r
        .schema
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    if cols != ["field", "value"] || r.num_rows() != 2 {
        return Err(format!("HGETALL: {cols:?}, {} rows", r.num_rows()));
    }
    let out = cmd(c.conn.as_mut(), &format!("INCRBY {}:counter 5", c.prefix)).await?;
    let ty = out.results[0].schema.field(0).data_type().clone();
    if ty != arrow_schema::DataType::Int64 {
        return Err(format!("INCRBY gave {ty:?}"));
    }
    match collect(
        c.conn.as_mut(),
        format!("LPUSH {h} x"),
        CancellationToken::new(),
    )
    .await
    {
        Err(Error::Query(q)) if q.code.as_deref() == Some("WRONGTYPE") => {}
        other => return Err(format!("expected WRONGTYPE, got {other:?}")),
    }
    match collect(c.conn.as_mut(), "NOSUCHCOMMAND", CancellationToken::new()).await {
        Err(Error::Query(q)) if q.code.is_some() => {}
        other => return Err(format!("expected an error, got {other:?}")),
    }
    c.conn.ping().await.map_err(|e| e.to_string())?;
    Ok("field/value, Int64, WRONGTYPE".into())
}

async fn multi(c: &mut Ctx<'_>) -> Result<String, String> {
    let k = c.key("tx");
    let conn = c.conn.as_mut();
    cmd(conn, "MULTI").await?;
    let q = cmd(conn, &format!("SET {k} 1")).await?;
    if first_text(&q).as_deref() != Some("QUEUED") {
        return Err(format!("SET in MULTI: {:?}", first_text(&q)));
    }
    cmd(conn, &format!("INCR {k}")).await?;
    let out = cmd(conn, "EXEC").await?;
    let rows = out.results.first().map(|r| r.num_rows()).unwrap_or(0);
    let v = cmd(conn, &format!("GET {k}")).await?;
    if rows != 2 || first_text(&v).as_deref() != Some("2") {
        return Err(format!("EXEC gave {rows} rows, value {:?}", first_text(&v)));
    }
    Ok("2 queued commands applied".into())
}

async fn edits(c: &mut Ctx<'_>) -> Result<String, String> {
    let (h, st, z, s) = (c.key("hash"), c.key("set"), c.key("zset"), c.key("string"));
    let conn = c.conn.as_mut();
    let e = |e: Error| e.to_string();
    native(
        conn,
        method::SET_MEMBER,
        &[("key", &h), ("member", "age"), ("value", "31")],
    )
    .await
    .map_err(e)?;
    native(
        conn,
        method::SET_MEMBER,
        &[("key", &st), ("member", "x"), ("value", "z")],
    )
    .await
    .map_err(e)?;
    native(
        conn,
        method::SET_MEMBER,
        &[("key", &z), ("member", "low"), ("value", "2")],
    )
    .await
    .map_err(e)?;
    native(conn, method::SET, &[("key", &s), ("value", "new")])
        .await
        .map_err(e)?;
    let renamed = format!("{s}2");
    native(conn, method::RENAME, &[("key", &s), ("new_key", &renamed)])
        .await
        .map_err(e)?;
    match c.value(&h).await? {
        KeyValue::Hash(v) if v.contains(&("age".into(), "31".into())) => {}
        other => return Err(format!("hash after edit: {other:?}")),
    }
    match c.value(&st).await? {
        KeyValue::Set(mut v) => {
            v.sort();
            if v != ["y", "z"] {
                return Err(format!("set after edit: {v:?}"));
            }
        }
        other => return Err(format!("set after edit: {other:?}")),
    }
    match c.value(&z).await? {
        KeyValue::ZSet(v) if v.contains(&("low".to_owned(), 2.0)) => {}
        other => return Err(format!("zset after edit: {other:?}")),
    }
    match c.value(&renamed).await? {
        KeyValue::String(v) if v == "new" => {}
        other => return Err(format!("renamed string: {other:?}")),
    }
    let out = native(c.conn.as_mut(), method::DELETE, &[("key", &renamed)])
        .await
        .map_err(e)?;
    if out.summary.affected != Some(1) {
        return Err(format!("DEL affected {:?}", out.summary.affected));
    }
    Ok("hash field, set member, zset score, set, rename, delete".into())
}

async fn read_only(c: &mut Ctx<'_>) -> Result<String, String> {
    let cfg = ResolvedConfig {
        read_only: true,
        ..c.cfg.clone()
    };
    let mut ro = c.driver.connect(&cfg).await.map_err(|e| e.to_string())?;
    ro.set_read_only(true).await.map_err(|e| e.to_string())?;
    let k = c.key("hash");
    cmd(ro.as_mut(), &format!("HGETALL {k}")).await?;
    let mut refused = Vec::new();
    for line in [
        format!("SET {k}:ro 1"),
        "FLUSHALL".to_owned(),
        "FLUSHDB".to_owned(),
        "CONFIG SET maxmemory 1".to_owned(),
        "DEBUG SLEEP 0".to_owned(),
        "SHUTDOWN".to_owned(),
    ] {
        match collect(ro.as_mut(), line.clone(), CancellationToken::new()).await {
            Err(Error::ReadOnly) => refused.push(line.split(' ').next().unwrap_or("").to_owned()),
            other => return Err(format!("`{line}` on a read-only source: {other:?}")),
        }
    }
    match native(ro.as_mut(), method::DELETE, &[("key", &k)]).await {
        Err(Error::ReadOnly) => {}
        other => return Err(format!("native delete on a read-only source: {other:?}")),
    }
    Ok(format!("refused: {}", refused.join(", ")))
}

async fn cancel(c: &mut Ctx<'_>) -> Result<String, String> {
    let token = CancellationToken::new();
    let t2 = token.clone();
    let q = c.key("empty-queue");
    let canceller = async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        t2.cancel();
        Instant::now()
    };
    let (r, at) = futures::join!(
        collect(c.conn.as_mut(), format!("BLPOP {q} 0"), token),
        canceller
    );
    let took = at.elapsed();
    match r {
        Err(Error::Cancelled) => {}
        other => return Err(format!("expected Cancelled, got {other:?}")),
    }
    if took > Duration::from_secs(1) {
        return Err(format!("cancel took {} ms", took.as_millis()));
    }
    if !c.conn.is_alive() {
        c.conn = c.driver.connect(c.cfg).await.map_err(|e| e.to_string())?;
    }
    c.conn
        .ping()
        .await
        .map_err(|e| format!("connection unusable after cancel: {e}"))?;
    let gone = c.key("string2");
    let v = cmd(c.conn.as_mut(), &format!("GET {gone}")).await?;
    if first_text(&v).is_some() {
        return Err("stale reply after cancel".into());
    }
    Ok(format!("{} ms", took.as_millis()))
}

async fn cleanup(c: &mut Ctx<'_>) {
    for pattern in [format!("{}:*", c.prefix), format!("{{{}}}:*", c.prefix)] {
        let Ok(out) = native(
            c.conn.as_mut(),
            method::SCAN,
            &[("pattern", &pattern), ("meta", "false")],
        )
        .await
        else {
            continue;
        };
        let keys: Vec<String> = out
            .results
            .iter()
            .flat_map(|r| r.batches.iter())
            .flat_map(kv::keys_from_batch)
            .map(|k| k.key)
            .collect();
        for k in keys {
            let _ = native(c.conn.as_mut(), method::DELETE, &[("key", &k)]).await;
        }
    }
}

async fn connection_loss(c: &mut Ctx<'_>) -> Result<String, String> {
    let id = match collect(c.conn.as_mut(), "CLIENT ID", CancellationToken::new()).await {
        Ok(out) => first_text(&out).ok_or("no CLIENT ID")?,
        Err(e) => return Ok(format!("skipped: CLIENT ID unavailable ({e})")),
    };
    if c.cfg.prop("mode") == Some("cluster") {
        return Ok("skipped: cluster connections reconnect per node".into());
    }
    let mut killer = c.driver.connect(c.cfg).await.map_err(|e| e.to_string())?;
    cmd(killer.as_mut(), &format!("CLIENT KILL ID {id}")).await?;
    let r = c.conn.ping().await;
    match r {
        Err(Error::ConnectionLost) if !c.conn.is_alive() => Ok("ConnectionLost, not alive".into()),
        other => Err(format!(
            "after CLIENT KILL: {other:?}, alive = {}",
            c.conn.is_alive()
        )),
    }
}
