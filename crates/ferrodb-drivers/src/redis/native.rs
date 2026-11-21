//! Native calls of the key browser ([`crate::kv::method`]): key scans with
//! metadata, key details, values by type with limits, and key edits.

use std::sync::Arc;

use arrow_array::{ArrayRef, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use ferrodb_api::{Error, KeyType, QueryError, Result};
use redis::{Cmd, Value};

use super::{conn::RedisConnection, reply};
use crate::{
    ExecSummary, ResultSink, SinkEvent,
    kv::{self, KeyRow, ValueRow, method},
};

/// Default element limit of [`method::VALUE`].
pub const VALUE_LIMIT: usize = 10_000;

fn arg<'a>(args: &'a [(String, String)], key: &str) -> Option<&'a str> {
    args.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn required<'a>(args: &'a [(String, String)], key: &str) -> Result<&'a str> {
    arg(args, key).ok_or_else(|| Error::Other(format!("missing argument `{key}`")))
}

fn num<T: std::str::FromStr>(args: &[(String, String)], key: &str, default: T) -> Result<T> {
    match arg(args, key).filter(|v| !v.is_empty()) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| Error::Other(format!("`{key}`: not a number: {v}"))),
    }
}

fn server_error(v: &Value) -> Option<Error> {
    match v {
        Value::ServerError(e) => Some(Error::Query(QueryError {
            message: e.details().unwrap_or_default().to_owned(),
            code: Some(e.code().to_owned()),
            range: None,
        })),
        _ => None,
    }
}

pub(crate) async fn run(
    c: &mut RedisConnection,
    m: &str,
    args: &[(String, String)],
    sink: &ResultSink,
) -> Result<ExecSummary> {
    let write = matches!(
        m,
        method::SET
            | method::RENAME
            | method::DELETE
            | method::EXPIRE
            | method::SET_MEMBER
            | method::ADD_MEMBER
            | method::REMOVE_MEMBER
    );
    if write && c.read_only {
        return Err(Error::ReadOnly);
    }
    match m {
        method::SCAN => scan(c, args, sink).await,
        method::KEY_INFO => key_info(c, required(args, "key")?, sink).await,
        method::VALUE => {
            value(
                c,
                required(args, "key")?,
                num(args, "limit", VALUE_LIMIT)?,
                sink,
            )
            .await
        }
        method::DATABASES => databases(c, sink).await,
        _ if write => edit(c, m, args).await,
        other => Err(Error::Unsupported(format!("native call `{other}`"))),
    }
}

/// Key rows with `TYPE`, `PTTL` and `MEMORY USAGE` read in one pipeline.
async fn with_meta(
    c: &mut RedisConnection,
    keys: Vec<String>,
    node: Option<&(String, u16)>,
) -> Result<Vec<KeyRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut p = redis::pipe();
    for k in &keys {
        p.cmd("TYPE").arg(k.as_str());
        p.cmd("PTTL").arg(k.as_str());
        p.cmd("MEMORY")
            .arg("USAGE")
            .arg(k.as_str())
            .arg("SAMPLES")
            .arg(0);
    }
    p.ignore_errors();
    let r = c.link.pipe(&p, node).await;
    let v = r.map_err(|e| c.lost(e))?;
    Ok(keys
        .into_iter()
        .enumerate()
        .map(|(i, key)| {
            let kind = v.get(i * 3).and_then(reply::text).filter(|t| t != "none");
            let ttl = v.get(i * 3 + 1).and_then(reply::int);
            let size = v
                .get(i * 3 + 2)
                .and_then(reply::int)
                .and_then(|n| u64::try_from(n).ok());
            KeyRow {
                key,
                kind,
                ttl_ms: ttl.filter(|t| *t >= 0),
                size,
            }
        })
        .collect())
}

fn scan_cmd(cursor: &str, pattern: &str, count: u64, kind: Option<&str>) -> Cmd {
    let mut cmd = redis::cmd("SCAN");
    cmd.arg(cursor)
        .arg("MATCH")
        .arg(pattern)
        .arg("COUNT")
        .arg(count);
    if let Some(t) = kind {
        cmd.arg("TYPE").arg(t);
    }
    cmd
}

fn parse_scan(v: &Value) -> Result<(String, Vec<String>)> {
    if let Some(e) = server_error(v) {
        return Err(e);
    }
    match reply::items(v) {
        Some([cursor, keys]) => Ok((
            reply::text(cursor).unwrap_or_else(|| "0".into()),
            reply::items(keys)
                .unwrap_or_default()
                .iter()
                .filter_map(reply::text)
                .collect(),
        )),
        _ => Err(Error::Other("unexpected SCAN reply".into())),
    }
}

async fn scan(
    c: &mut RedisConnection,
    args: &[(String, String)],
    sink: &ResultSink,
) -> Result<ExecSummary> {
    let pattern = arg(args, "pattern")
        .filter(|p| !p.is_empty())
        .unwrap_or("*");
    let count: u64 = num(args, "count", 500)?;
    let limit: usize = num(args, "limit", usize::MAX)?;
    let meta = arg(args, "meta") != Some("false");
    let kind = arg(args, "type").filter(|t| !t.is_empty());
    let schema = kv::keys_schema();
    if sink
        .send(SinkEvent::Begin {
            schema: schema.clone(),
        })
        .await
        .is_err()
    {
        return Ok(truncated());
    }
    // One round from a given cursor. A cluster cursor is `node/cursor`
    // (node = index of the master): rounds walk one master after another,
    // `0` starts at the first and ends after the last.
    if let Some(cursor) = arg(args, "cursor") {
        if matches!(c.link, super::conn::Link::Cluster(_)) && c.masters.is_empty() {
            c.refresh_masters().await?;
        }
        let nodes = c.nodes();
        let (index, cursor) = match cursor.split_once('/') {
            Some((i, cur)) => (i.parse::<usize>().unwrap_or(0), cur),
            None => (0, cursor),
        };
        let node = nodes.get(index).cloned().flatten();
        let v = c
            .link
            .on_node(scan_cmd(cursor, pattern, count, kind), node.as_ref())
            .await
            .map_err(|e| c.lost(e))?;
        let (next, mut keys) = parse_scan(&v)?;
        let next = match (next.as_str(), nodes.len() > 1) {
            (_, false) => next,
            ("0", true) if index + 1 < nodes.len() => format!("{}/0", index + 1),
            ("0", true) => "0".to_owned(),
            (n, true) => format!("{index}/{n}"),
        };
        keys.truncate(limit);
        let rows = if meta {
            with_meta(c, keys, node.as_ref()).await?
        } else {
            keys.into_iter()
                .map(|key| KeyRow {
                    key,
                    ..Default::default()
                })
                .collect()
        };
        if sink
            .send(SinkEvent::Batch(kv::keys_batch(&rows)))
            .await
            .is_err()
            || sink.send(SinkEvent::End).await.is_err()
        {
            return Ok(truncated());
        }
        let cursor_schema = Arc::new(Schema::new(vec![Field::new(
            "cursor",
            DataType::Utf8,
            false,
        )]));
        let b = RecordBatch::try_new(
            cursor_schema.clone(),
            vec![Arc::new(StringArray::from(vec![next])) as ArrayRef],
        )
        .expect("one column");
        let _ = sink
            .send(SinkEvent::Begin {
                schema: cursor_schema,
            })
            .await;
        let _ = sink.send(SinkEvent::Batch(b)).await;
        let _ = sink.send(SinkEvent::End).await;
        return Ok(ExecSummary {
            affected: None,
            results: 2,
            truncated: false,
        });
    }
    // The whole keyspace, every master of a cluster, streamed in batches.
    if matches!(c.link, super::conn::Link::Cluster(_)) {
        c.refresh_masters().await?;
    }
    let mut sent = 0usize;
    for node in c.nodes() {
        let mut cursor = "0".to_owned();
        let mut pending: Vec<String> = Vec::new();
        loop {
            let v = c
                .link
                .on_node(scan_cmd(&cursor, pattern, count, kind), node.as_ref())
                .await
                .map_err(|e| c.lost(e))?;
            let (next, keys) = parse_scan(&v)?;
            pending.extend(keys);
            cursor = next;
            let done = cursor == "0";
            while pending.len() >= sink.batch_rows() || (done && !pending.is_empty()) {
                let take = pending.len().min(sink.batch_rows()).min(limit - sent);
                let chunk: Vec<String> = pending.drain(..take).collect();
                let rows = if meta {
                    with_meta(c, chunk, node.as_ref()).await?
                } else {
                    chunk
                        .into_iter()
                        .map(|key| KeyRow {
                            key,
                            ..Default::default()
                        })
                        .collect()
                };
                // Keys deleted between SCAN and TYPE are left out.
                let rows: Vec<KeyRow> = rows
                    .into_iter()
                    .filter(|r| !meta || r.kind.is_some())
                    .collect();
                sent += take;
                if sink
                    .send(SinkEvent::Batch(kv::keys_batch(&rows)))
                    .await
                    .is_err()
                {
                    return Ok(truncated());
                }
                if sent >= limit {
                    let _ = sink.send(SinkEvent::End).await;
                    return Ok(ExecSummary {
                        affected: None,
                        results: 1,
                        truncated: true,
                    });
                }
            }
            if done {
                break;
            }
        }
    }
    let _ = sink.send(SinkEvent::End).await;
    Ok(ExecSummary {
        affected: None,
        results: 1,
        truncated: false,
    })
}

fn truncated() -> ExecSummary {
    ExecSummary {
        affected: None,
        results: 1,
        truncated: true,
    }
}

async fn key_type(c: &mut RedisConnection, key: &str) -> Result<Option<String>> {
    let v = c.run(redis::cmd("TYPE").arg(key)).await?;
    Ok(reply::text(&v).filter(|t| t != "none"))
}

/// The command giving the number of elements of a key type.
fn length_cmd(kind: KeyType, key: &str) -> Option<Cmd> {
    let name = match kind {
        KeyType::String => "STRLEN",
        KeyType::Hash => "HLEN",
        KeyType::List => "LLEN",
        KeyType::Set => "SCARD",
        KeyType::ZSet => "ZCARD",
        KeyType::Stream => "XLEN",
        KeyType::Json => {
            let mut c = redis::cmd("JSON.DEBUG");
            c.arg("MEMORY").arg(key);
            return Some(c);
        }
        KeyType::Other => return None,
    };
    let mut c = redis::cmd(name);
    c.arg(key);
    Some(c)
}

async fn key_info(c: &mut RedisConnection, key: &str, sink: &ResultSink) -> Result<ExecSummary> {
    let schema = kv::key_info_schema();
    let mut rows = Vec::new();
    if let Some(kind) = key_type(c, key).await? {
        let mut p = redis::pipe();
        p.cmd("PTTL").arg(key);
        p.cmd("MEMORY").arg("USAGE").arg(key).arg("SAMPLES").arg(0);
        p.cmd("OBJECT").arg("ENCODING").arg(key);
        let length = length_cmd(kv::key_type(&kind), key);
        if let Some(l) = &length {
            p.add_command(l.clone());
        }
        p.ignore_errors();
        let r = c.link.pipe(&p, None).await;
        let v = r.map_err(|e| c.lost(e))?;
        let ttl = v.first().and_then(reply::int).filter(|t| *t >= 0);
        let size = v
            .get(1)
            .and_then(reply::int)
            .and_then(|n| u64::try_from(n).ok());
        let encoding = v
            .get(2)
            .and_then(reply::text)
            .filter(|t| !t.starts_with("(error)"));
        let len = v
            .get(3)
            .and_then(reply::int)
            .and_then(|n| u64::try_from(n).ok());
        rows.push((key.to_owned(), kind, ttl, size, encoding, len));
    }
    let b = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(
                rows.iter()
                    .map(|r| Some(r.0.as_str()))
                    .collect::<StringArray>(),
            ) as ArrayRef,
            Arc::new(
                rows.iter()
                    .map(|r| Some(r.1.as_str()))
                    .collect::<StringArray>(),
            ),
            Arc::new(
                rows.iter()
                    .map(|r| r.2)
                    .collect::<arrow_array::Int64Array>(),
            ),
            Arc::new(rows.iter().map(|r| r.3).collect::<UInt64Array>()),
            Arc::new(rows.iter().map(|r| r.4.as_deref()).collect::<StringArray>()),
            Arc::new(rows.iter().map(|r| r.5).collect::<UInt64Array>()),
        ],
    )
    .expect("columns match the schema");
    let _ = sink.send(SinkEvent::Begin { schema }).await;
    let _ = sink.send(SinkEvent::Batch(b)).await;
    let _ = sink.send(SinkEvent::End).await;
    Ok(ExecSummary {
        affected: None,
        results: 1,
        truncated: false,
    })
}

/// Reads a collection with `*SCAN` until `limit` elements.
async fn scan_members(
    c: &mut RedisConnection,
    cmd: &str,
    key: &str,
    limit: usize,
    pairs: bool,
) -> Result<Vec<(String, Option<String>)>> {
    let mut out = Vec::new();
    let mut cursor = "0".to_owned();
    loop {
        let v = c
            .run(redis::cmd(cmd).arg(key).arg(&cursor).arg("COUNT").arg(1000))
            .await?;
        if let Some(e) = server_error(&v) {
            return Err(e);
        }
        let Some([next, items]) = reply::items(&v) else {
            return Err(Error::Other(format!("unexpected {cmd} reply")));
        };
        cursor = reply::text(next).unwrap_or_else(|| "0".into());
        let items = reply::items(items).unwrap_or_default();
        if pairs {
            for p in items.chunks(2) {
                out.push((
                    reply::text(&p[0]).unwrap_or_default(),
                    p.get(1).and_then(reply::text),
                ));
            }
        } else {
            out.extend(
                items
                    .iter()
                    .map(|i| (reply::text(i).unwrap_or_default(), None)),
            );
        }
        if cursor == "0" || out.len() >= limit {
            out.truncate(limit);
            return Ok(out);
        }
    }
}

async fn value(
    c: &mut RedisConnection,
    key: &str,
    limit: usize,
    sink: &ResultSink,
) -> Result<ExecSummary> {
    let limit = limit.max(1);
    let kind = key_type(c, key)
        .await?
        .ok_or_else(|| Error::NotFound(format!("key {key}")))?;
    let row = |member: Option<String>, value: Option<String>| ValueRow {
        member,
        value,
        ..Default::default()
    };
    let rows: Vec<ValueRow> = match kv::key_type(&kind) {
        KeyType::String => {
            let v = c.run(redis::cmd("GET").arg(key)).await?;
            vec![row(None, reply::text(&v))]
        }
        KeyType::Json => {
            let v = c.run(redis::cmd("JSON.GET").arg(key)).await?;
            if let Some(e) = server_error(&v) {
                return Err(e);
            }
            vec![row(None, reply::text(&v))]
        }
        KeyType::Hash => scan_members(c, "HSCAN", key, limit, true)
            .await?
            .into_iter()
            .map(|(f, v)| row(Some(f), v))
            .collect(),
        KeyType::Set => scan_members(c, "SSCAN", key, limit, false)
            .await?
            .into_iter()
            .map(|(m, _)| row(Some(m), None))
            .collect(),
        KeyType::List => {
            let v = c
                .run(redis::cmd("LRANGE").arg(key).arg(0).arg(limit as i64 - 1))
                .await?;
            reply::items(&v)
                .unwrap_or_default()
                .iter()
                .enumerate()
                .map(|(i, x)| row(Some(i.to_string()), reply::text(x)))
                .collect()
        }
        KeyType::ZSet => {
            let v = c
                .run(
                    redis::cmd("ZRANGE")
                        .arg(key)
                        .arg(0)
                        .arg(limit as i64 - 1)
                        .arg("WITHSCORES"),
                )
                .await?;
            let a = reply::items(&v).unwrap_or_default();
            if a.iter().all(|x| reply::items(x).is_some()) {
                // RESP3: [[member, score], …]
                a.iter()
                    .filter_map(|x| {
                        let p = reply::items(x)?;
                        Some(ValueRow {
                            member: p.first().and_then(reply::text),
                            score: p.get(1).and_then(reply::float),
                            ..Default::default()
                        })
                    })
                    .collect()
            } else {
                a.chunks(2)
                    .map(|p| ValueRow {
                        member: reply::text(&p[0]),
                        score: p.get(1).and_then(reply::float),
                        ..Default::default()
                    })
                    .collect()
            }
        }
        KeyType::Stream => {
            let v = c
                .run(
                    redis::cmd("XRANGE")
                        .arg(key)
                        .arg("-")
                        .arg("+")
                        .arg("COUNT")
                        .arg(limit),
                )
                .await?;
            let mut rows = Vec::new();
            for e in reply::items(&v).unwrap_or_default() {
                let Some([id, fields]) = reply::items(e) else {
                    continue;
                };
                let id = reply::text(id);
                for (f, v) in reply::pairs(fields).unwrap_or_default() {
                    rows.push(ValueRow {
                        member: id.clone(),
                        field: reply::text(f),
                        value: reply::text(v),
                        score: None,
                    });
                }
            }
            rows
        }
        KeyType::Other => {
            return Err(Error::Unsupported(format!(
                "values of type {kind} in the key browser"
            )));
        }
    };
    let schema = kv::value_schema();
    if sink.send(SinkEvent::Begin { schema }).await.is_err() {
        return Ok(truncated());
    }
    for chunk in rows.chunks(sink.batch_rows()) {
        if sink
            .send(SinkEvent::Batch(kv::value_batch(&kind, chunk)))
            .await
            .is_err()
        {
            return Ok(truncated());
        }
    }
    if rows.is_empty() {
        let _ = sink
            .send(SinkEvent::Batch(kv::value_batch(&kind, &[])))
            .await;
    }
    let _ = sink.send(SinkEvent::End).await;
    Ok(ExecSummary {
        affected: None,
        results: 1,
        truncated: false,
    })
}

/// Commands of an edit call (also used to preview edits).
pub fn edit_commands(
    m: &str,
    args: &[(String, String)],
    kind: Option<KeyType>,
) -> Result<Vec<Vec<String>>> {
    let key = required(args, "key")?.to_owned();
    let v = |k: &str| required(args, k).map(str::to_owned);
    let s = |x: &str| x.to_owned();
    Ok(match m {
        method::SET => match arg(args, "kind") {
            Some("json") => vec![vec![s("JSON.SET"), key, s("$"), v("value")?]],
            _ => {
                let mut c = vec![s("SET"), key, v("value")?];
                match arg(args, "ttl").filter(|t| !t.is_empty()) {
                    Some(t) => c.extend([s("EX"), t.to_owned()]),
                    None => c.push(s("KEEPTTL")),
                }
                vec![c]
            }
        },
        method::RENAME => {
            let cmd = if arg(args, "nx") == Some("true") {
                "RENAMENX"
            } else {
                "RENAME"
            };
            vec![vec![s(cmd), key, v("new_key")?]]
        }
        method::DELETE => {
            let cmd = if arg(args, "async") == Some("true") {
                "UNLINK"
            } else {
                "DEL"
            };
            vec![vec![s(cmd), key]]
        }
        method::EXPIRE => match arg(args, "ttl").filter(|t| !t.is_empty()) {
            Some(t) => vec![vec![s("EXPIRE"), key, t.to_owned()]],
            None => vec![vec![s("PERSIST"), key]],
        },
        method::SET_MEMBER => {
            let (member, value) = (v("member")?, v("value")?);
            match kind {
                Some(KeyType::Hash) => vec![vec![s("HSET"), key, member, value]],
                Some(KeyType::List) => vec![vec![s("LSET"), key, member, value]],
                Some(KeyType::ZSet) => vec![vec![s("ZADD"), key, value, member]],
                Some(KeyType::Set) => vec![
                    vec![s("SREM"), key.clone(), member],
                    vec![s("SADD"), key, value],
                ],
                Some(KeyType::Stream) => {
                    let mut c = vec![s("XADD"), key, member];
                    for line in value.lines() {
                        let (f, x) = line.split_once('=').unwrap_or((line, ""));
                        c.extend([f.to_owned(), x.to_owned()]);
                    }
                    vec![c]
                }
                Some(KeyType::String) | None => vec![vec![s("SET"), key, value, s("KEEPTTL")]],
                Some(KeyType::Json) => vec![vec![s("JSON.SET"), key, member, value]],
                Some(KeyType::Other) => {
                    return Err(Error::Unsupported("editing this key type".into()));
                }
            }
        }
        method::ADD_MEMBER => {
            let value = v("value")?;
            match kind {
                Some(KeyType::Hash) => vec![vec![s("HSET"), key, v("member")?, value]],
                Some(KeyType::List) => vec![vec![s("RPUSH"), key, value]],
                Some(KeyType::ZSet) => vec![vec![s("ZADD"), key, value, v("member")?]],
                _ => vec![vec![s("SADD"), key, value]],
            }
        }
        method::REMOVE_MEMBER => {
            let member = v("member")?;
            match kind {
                Some(KeyType::Hash) => vec![vec![s("HDEL"), key, member]],
                Some(KeyType::List) => vec![vec![s("LREM"), key, s("1"), member]],
                Some(KeyType::ZSet) => vec![vec![s("ZREM"), key, member]],
                Some(KeyType::Stream) => vec![vec![s("XDEL"), key, member]],
                _ => vec![vec![s("SREM"), key, member]],
            }
        }
        other => return Err(Error::Unsupported(format!("native call `{other}`"))),
    })
}

async fn edit(c: &mut RedisConnection, m: &str, args: &[(String, String)]) -> Result<ExecSummary> {
    let needs_type = matches!(
        m,
        method::SET_MEMBER | method::ADD_MEMBER | method::REMOVE_MEMBER
    );
    let kind = if needs_type {
        key_type(c, required(args, "key")?)
            .await?
            .map(|k| kv::key_type(&k))
    } else {
        None
    };
    let mut affected = 0u64;
    for cmd in edit_commands(m, args, kind)? {
        let mut r = redis::cmd(&cmd[0]);
        for a in &cmd[1..] {
            r.arg(a.as_str());
        }
        let v = c.run(&r).await?;
        if let Some(e) = server_error(&v) {
            return Err(e);
        }
        affected += match &v {
            Value::Int(n) => (*n).max(0) as u64,
            Value::Nil => 0,
            _ => 1,
        };
    }
    Ok(ExecSummary {
        affected: Some(affected),
        results: 0,
        truncated: false,
    })
}

/// `db0` … with key counts (`INFO keyspace`), every configured database
/// (`CONFIG GET databases`, when allowed).
async fn databases(c: &mut RedisConnection, sink: &ResultSink) -> Result<ExecSummary> {
    let cluster = matches!(c.link, super::conn::Link::Cluster(_));
    let mut dbs: Vec<(i64, u64, u64)> = Vec::new();
    if cluster {
        let v = c.run(&redis::cmd("DBSIZE")).await?;
        dbs.push((0, reply::int(&v).unwrap_or(0).max(0) as u64, 0));
    } else {
        let v = c.run(redis::cmd("INFO").arg("keyspace")).await?;
        let info = reply::text(&v).unwrap_or_default();
        dbs = parse_keyspace(&info);
        let configured = match c
            .run(redis::cmd("CONFIG").arg("GET").arg("databases"))
            .await
        {
            Ok(v) => reply::pairs(&v)
                .and_then(|p| p.first().and_then(|(_, n)| reply::int(n)))
                .unwrap_or(0),
            Err(_) => 0,
        };
        for i in 0..configured.max(1) {
            if !dbs.iter().any(|d| d.0 == i) {
                dbs.push((i, 0, 0));
            }
        }
        if !dbs.iter().any(|d| d.0 == c.db) {
            dbs.push((c.db, 0, 0));
        }
        dbs.sort();
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("database", DataType::Utf8, false),
        Field::new("keys", DataType::UInt64, false),
        Field::new("expires", DataType::UInt64, false),
    ]));
    let b = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(
                dbs.iter()
                    .map(|d| Some(format!("db{}", d.0)))
                    .collect::<StringArray>(),
            ) as ArrayRef,
            Arc::new(dbs.iter().map(|d| Some(d.1)).collect::<UInt64Array>()),
            Arc::new(dbs.iter().map(|d| Some(d.2)).collect::<UInt64Array>()),
        ],
    )
    .expect("columns match the schema");
    let _ = sink.send(SinkEvent::Begin { schema }).await;
    let _ = sink.send(SinkEvent::Batch(b)).await;
    let _ = sink.send(SinkEvent::End).await;
    Ok(ExecSummary {
        affected: None,
        results: 1,
        truncated: false,
    })
}

/// `db0:keys=5,expires=1,avg_ttl=0` lines of `INFO keyspace`.
pub fn parse_keyspace(info: &str) -> Vec<(i64, u64, u64)> {
    info.lines()
        .filter_map(|l| {
            let (db, rest) = l.trim().split_once(':')?;
            let idx = db.strip_prefix("db")?.parse().ok()?;
            let field = |name: &str| {
                rest.split(',')
                    .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
            };
            Some((idx, field("keys"), field("expires")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn keyspace() {
        let info = "# Keyspace\r\ndb0:keys=5,expires=1,avg_ttl=0\r\ndb3:keys=2,expires=0,avg_ttl=0,subexpiry=0\r\n";
        assert_eq!(parse_keyspace(info), vec![(0, 5, 1), (3, 2, 0)]);
    }

    #[test]
    fn edits() {
        let c = edit_commands(
            method::SET_MEMBER,
            &a(&[("key", "s"), ("member", "old"), ("value", "new")]),
            Some(KeyType::Set),
        )
        .unwrap();
        assert_eq!(c, vec![vec!["SREM", "s", "old"], vec!["SADD", "s", "new"]]);
        let c = edit_commands(
            method::SET_MEMBER,
            &a(&[("key", "z"), ("member", "m"), ("value", "2.5")]),
            Some(KeyType::ZSet),
        )
        .unwrap();
        assert_eq!(c, vec![vec!["ZADD", "z", "2.5", "m"]]);
        let c = edit_commands(method::EXPIRE, &a(&[("key", "k"), ("ttl", "")]), None).unwrap();
        assert_eq!(c, vec![vec!["PERSIST", "k"]]);
        let c = edit_commands(method::SET, &a(&[("key", "k"), ("value", "v")]), None).unwrap();
        assert_eq!(c, vec![vec!["SET", "k", "v", "KEEPTTL"]]);
        assert!(edit_commands(method::RENAME, &a(&[("key", "k")]), None).is_err());
    }
}
