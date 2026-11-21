//! Phase 8 flows through the whole backend on real servers:
//!
//! - Redis (`FERRODB_TCK_REDIS=redis://127.0.0.1:56401/0`): console
//!   commands as tables, `SCAN` as a paged key browser result, key values,
//!   key edits through the operation preview, the explorer (databases →
//!   prefixes → keys), read-only refusal, risks;
//! - Cassandra (`FERRODB_TCK_CASSANDRA=cassandra://127.0.0.1:56442/tck?
//!   address_translation=contact_point`): CQL console with paging, the
//!   `ALLOW FILTERING` warning, the explorer from `system_schema`, DDL,
//!   result editing by full primary key, consistency per console.

#![allow(clippy::disallowed_methods)]

use std::collections::HashSet;

use ferrodb_api::*;
use futures::executor::block_on;

use crate::{CoreBackend, CoreOptions};

fn source(id: &str, driver: &str, url: &str, read_only: bool) -> DataSourceConfig {
    let r = ferrodb_drivers::ResolvedConfig::from_url(driver, url).expect("valid URL");
    DataSourceConfig {
        id: id.into(),
        name: id.into(),
        driver: driver.into(),
        folder: vec![],
        environment: Environment::None,
        read_only,
        tx_mode: TxMode::Auto,
        properties: r.properties.into_iter().collect(),
        secret_ref: None,
    }
}

fn events(core: &CoreBackend, s: SessionId, text: &str, opts: ExecOptions) -> Vec<QueryEvent> {
    let h = core.queries().execute(s, Script::new(text), opts);
    let mut out = Vec::new();
    block_on(async {
        while let Ok(ev) = h.events.recv().await {
            let done = matches!(ev, QueryEvent::Finished);
            out.push(ev);
            if done {
                break;
            }
        }
    });
    out
}

fn errors(evs: &[QueryEvent]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            QueryEvent::Error(q) => Some(q.to_string()),
            _ => None,
        })
        .collect()
}

fn notices(evs: &[QueryEvent]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            QueryEvent::Notice(n) => Some(n.message.clone()),
            _ => None,
        })
        .collect()
}

fn keys(evs: &[QueryEvent]) -> Vec<KeyEntry> {
    evs.iter()
        .flat_map(|e| match e {
            QueryEvent::Data {
                chunk: ResultChunk::Keys(k),
                ..
            } => k.as_ref().clone(),
            _ => Vec::new(),
        })
        .collect()
}

fn rows(evs: &[QueryEvent]) -> usize {
    evs.iter()
        .map(|e| match e {
            QueryEvent::Data {
                chunk: ResultChunk::Rows(b),
                ..
            } => b.num_rows(),
            _ => 0,
        })
        .sum()
}

fn last_result(evs: &[QueryEvent]) -> ResultId {
    evs.iter()
        .rev()
        .find_map(|e| match e {
            QueryEvent::ResultStarted { result, .. } => Some(*result),
            _ => None,
        })
        .expect("a result")
}

/// Asks for `n` more keys and reads until the result pauses again.
fn more(core: &CoreBackend, h: &QueryHandle, rid: ResultId, n: usize) -> (usize, bool) {
    block_on(core.queries().fetch_more(rid, n)).unwrap();
    let mut got = 0;
    block_on(async {
        while let Ok(ev) = h.events.recv().await {
            match ev {
                QueryEvent::Data {
                    chunk: ResultChunk::Keys(k),
                    ..
                } => got += k.len(),
                QueryEvent::ResultPaused { has_more, .. } => return (got, has_more),
                _ => {}
            }
        }
        (got, false)
    })
}

#[test]
fn redis_flows_on_a_real_server() {
    let Ok(url) = std::env::var("FERRODB_TCK_REDIS") else {
        return;
    };
    let core = CoreBackend::new(CoreOptions::in_memory()).unwrap();
    let cfg = source("redis", "redis", &url, false);
    block_on(core.data_sources().save(cfg.clone(), SecretValues::new())).unwrap();
    let ds = cfg.id.clone();
    let caps = core.data_sources().get(&ds).unwrap().capabilities;
    assert_eq!(caps.data_model, DataModel::KeyValue);
    let q = core.queries();
    let s = block_on(q.open_session(&ds)).unwrap();
    let p = format!("flows{}", std::process::id());

    // Console: one command per line, replies as tables.
    let mut script = format!("DEL {p}:h\nHSET {p}:h name Ann city Kraków\nHGETALL {p}:h\n");
    for i in 0..1200 {
        if i % 400 == 0 {
            script.push_str("MSET");
        }
        script.push_str(&format!(" {{{p}}}:user:{i} v{i}"));
        if i % 400 == 399 {
            script.push('\n');
        }
    }
    script.push_str(&format!(
        "SET {p}:s hello\nRPUSH {p}:l a b c\nEXPIRE {p}:s 300"
    ));
    let evs = events(&core, s, &script, ExecOptions::default());
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
    let affected: Vec<Option<u64>> = evs
        .iter()
        .filter_map(|e| match e {
            QueryEvent::StatementFinished { affected, .. } => Some(*affected),
            _ => None,
        })
        .collect();
    assert_eq!(affected[1], Some(2), "HSET reports the fields added");
    assert!(evs.iter().any(|e| matches!(
        e,
        QueryEvent::ResultStarted { shape: ResultShape::Table(t), .. }
            if t.fields().iter().map(|f| f.name().as_str()).collect::<Vec<_>>() == ["field", "value"]
    )));

    // SCAN → key browser result, paged.
    let h = q.execute(
        s,
        Script::new(format!("SCAN 0 MATCH {{{p}}}:user:* COUNT 500")),
        ExecOptions {
            page_size: 500,
            ..Default::default()
        },
    );
    let mut first = Vec::new();
    let mut rid = None;
    let mut has_more = false;
    block_on(async {
        while let Ok(ev) = h.events.recv().await {
            match ev {
                QueryEvent::ResultStarted {
                    result,
                    shape: ResultShape::Keys { pattern },
                    ..
                } => {
                    assert_eq!(pattern, format!("{{{p}}}:user:*"));
                    rid = Some(result);
                }
                QueryEvent::Data {
                    chunk: ResultChunk::Keys(k),
                    ..
                } => first.extend(k.iter().cloned()),
                QueryEvent::ResultPaused { has_more: m, .. } => {
                    has_more = m;
                    break;
                }
                QueryEvent::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
    });
    let rid = rid.expect("key result");
    assert_eq!(first.len(), 500);
    assert!(has_more);
    assert!(
        first
            .iter()
            .all(|k| k.kind == KeyType::String && k.size.is_some())
    );
    let (n2, more2) = more(&core, &h, rid, 500);
    let (n3, _) = more(&core, &h, rid, 500);
    assert_eq!((n2, more2, n2 + n3), (500, true, 700));

    // Key values and edits through the preview.
    match block_on(q.key_value(s, format!("{p}:h"))).unwrap() {
        KeyValue::Hash(mut v) => {
            v.sort();
            assert_eq!(v[1], ("name".into(), "Ann".into()));
        }
        other => panic!("{other:?}"),
    }
    let changes = ChangeSet {
        changes: vec![
            Change::SetKeyMember {
                key: format!("{p}:h"),
                member: "city".into(),
                value: "Gdańsk".into(),
            },
            Change::SetKeyMember {
                key: format!("{p}:l"),
                member: "1".into(),
                value: "B B".into(),
            },
            Change::ExpireKey {
                key: format!("{p}:s"),
                ttl_secs: None,
            },
            Change::SetKey {
                key: format!("{p}:s"),
                value: "new value".into(),
            },
        ],
    };
    let preview = block_on(q.preview_changes(rid, changes.clone())).unwrap();
    assert_eq!(
        preview,
        vec![
            format!("HSET {p}:h city Gdańsk"),
            format!("LSET {p}:l 1 \"B B\""),
            format!("PERSIST {p}:s"),
            format!("SET {p}:s \"new value\" KEEPTTL"),
        ]
    );
    let report = block_on(q.apply_changes(rid, changes)).unwrap();
    assert_eq!(report.applied, 4);
    assert_eq!(
        block_on(q.key_value(s, format!("{p}:l"))).unwrap(),
        KeyValue::List(vec!["a".into(), "B B".into(), "c".into()])
    );
    assert_eq!(
        block_on(q.key_value(s, format!("{p}:s"))).unwrap(),
        KeyValue::String("new value".into())
    );

    // Explorer: databases → prefixes → keys.
    let m = core.metadata();
    let root = block_on(m.children(NodeRef::root(ds.clone()))).unwrap();
    assert!(
        root.iter()
            .any(|o| o.name == "db0" && o.kind == ObjectKind::Database)
    );
    let db = root.iter().find(|o| o.name == "db0").unwrap().node.clone();
    let top = block_on(m.children(db.clone())).unwrap();
    let pfx = top
        .iter()
        .find(|o| o.name == p && o.kind == ObjectKind::KeyPrefix)
        .unwrap_or_else(|| panic!("prefix {p} in {top:?}"));
    let under = block_on(m.children(pfx.node.clone())).unwrap();
    let names: HashSet<&str> = under.iter().map(|o| o.name.as_str()).collect();
    assert!(names.contains("h") && names.contains("l"), "{names:?}");
    assert!(
        under
            .iter()
            .any(|o| o.kind == ObjectKind::Key && o.detail.as_deref() == Some("Hash"))
    );

    // Risks and read-only.
    let risks = core
        .language()
        .risks("GET a\nFLUSHALL", &QueryLanguage::RedisCommands, false);
    assert_eq!(risks.len(), 1);
    let ro = source("redis-ro", "redis", &url, true);
    block_on(core.data_sources().save(ro.clone(), SecretValues::new())).unwrap();
    let rs = block_on(q.open_session(&ro.id)).unwrap();
    let evs = events(
        &core,
        rs,
        &format!("GET {p}:s\nDEL {p}:s"),
        ExecOptions::default(),
    );
    assert_eq!(rows(&evs), 1);
    assert!(errors(&evs)[0].contains("read-only"), "{:?}", errors(&evs));
    let evs = events(
        &core,
        rs,
        &format!("SCAN 0 MATCH {p}:*"),
        ExecOptions::default(),
    );
    assert!(!keys(&evs).is_empty());
    let rid = last_result(&evs);
    let r = block_on(q.apply_changes(
        rid,
        ChangeSet {
            changes: vec![Change::DeleteKey {
                key: format!("{p}:s"),
            }],
        },
    ));
    assert_eq!(r, Err(Error::ReadOnly));

    // Cleanup.
    let evs = events(
        &core,
        s,
        &format!("SCAN 0 MATCH *{p}*"),
        ExecOptions {
            page_size: 100_000,
            ..Default::default()
        },
    );
    for k in keys(&evs) {
        events(
            &core,
            s,
            &ferrodb_drivers::kv::command_line(&["DEL", &k.key]),
            ExecOptions::default(),
        );
    }
}

#[test]
fn cassandra_flows_on_a_real_server() {
    let Ok(url) = std::env::var("FERRODB_TCK_CASSANDRA") else {
        return;
    };
    let core = CoreBackend::new(CoreOptions::in_memory()).unwrap();
    let cfg = source("cass", "cassandra", &url, false);
    block_on(core.data_sources().save(cfg.clone(), SecretValues::new())).unwrap();
    let ds = cfg.id.clone();
    let q = core.queries();
    let s = block_on(q.open_session(&ds)).unwrap();
    let ks = format!("flows{}", std::process::id());
    let evs = events(
        &core,
        s,
        &format!(
            "CREATE KEYSPACE {ks} WITH replication = {{'class': 'SimpleStrategy', 'replication_factor': 1}};\n\
             USE {ks};\n\
             CREATE TYPE address (city text, zip int);\n\
             CREATE TABLE readings (sensor int, day date, ts timestamp, v double, tags set<text>, home frozen<address>, note text static, PRIMARY KEY ((sensor, day), ts)) WITH CLUSTERING ORDER BY (ts DESC) AND comment = 'raw';\n\
             CREATE INDEX readings_v ON readings (v);\n\
             CREATE TABLE kv (k text PRIMARY KEY, n int);"
        ),
        ExecOptions::default(),
    );
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
    let mut inserts = String::new();
    for i in 0..1500 {
        inserts.push_str(&format!(
            "INSERT INTO readings (sensor, day, ts, v, tags, home) VALUES (1, '2026-11-21', {}, {i}.5, {{'a', 'b'}}, {{city: 'Kraków', zip: 30}});\n",
            1_700_000_000_000i64 + i
        ));
    }
    let evs = events(&core, s, &inserts, ExecOptions::default());
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));

    // Paging: 1500 rows, first page 500.
    let h = q.execute(
        s,
        Script::new("SELECT * FROM readings WHERE sensor = 1 AND day = '2026-11-21'"),
        ExecOptions {
            page_size: 500,
            ..Default::default()
        },
    );
    let mut got = 0;
    let mut rid = None;
    block_on(async {
        while let Ok(ev) = h.events.recv().await {
            match ev {
                QueryEvent::ResultStarted { result, .. } => rid = Some(result),
                QueryEvent::Data {
                    chunk: ResultChunk::Rows(b),
                    ..
                } => got += b.num_rows(),
                QueryEvent::ResultPaused { .. } => break,
                QueryEvent::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
    });
    assert_eq!(got, 500);
    let rid = rid.unwrap();

    // Editing by full primary key (sensor, day, ts).
    let info = block_on(q.edit_info(rid)).unwrap();
    assert!(
        matches!(&info.identity, RowIdentity::Key(k) if k.len() == 3),
        "{info:?}"
    );
    let v_col = info
        .columns
        .iter()
        .position(|c| c.as_deref() == Some("v"))
        .unwrap();
    let change = ChangeSet {
        changes: vec![Change::SetCell {
            row: 0,
            column: v_col,
            value: "42.25".into(),
        }],
    };
    let preview = block_on(q.preview_changes(rid, change.clone())).unwrap();
    assert_eq!(preview.len(), 1);
    assert!(
        preview[0].starts_with("UPDATE ")
            && preview[0]
                .contains("SET v = 42.25 WHERE sensor = 1 AND day = '2026-11-21' AND ts = "),
        "{preview:?}"
    );
    let report = block_on(q.apply_changes(rid, change)).unwrap();
    assert_eq!(report.applied, 1);
    let evs = events(
        &core,
        s,
        "SELECT v FROM readings WHERE sensor = 1 AND day = '2026-11-21' LIMIT 1",
        ExecOptions::default(),
    );
    let b = evs
        .iter()
        .find_map(|e| match e {
            QueryEvent::Data {
                chunk: ResultChunk::Rows(b),
                ..
            } => Some(b.clone()),
            _ => None,
        })
        .unwrap();
    let v = b
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::Float64Array>()
        .unwrap()
        .value(0);
    assert_eq!(v, 42.25);

    // A result without the full key is not editable.
    let evs = events(
        &core,
        s,
        "SELECT sensor, v FROM readings",
        ExecOptions::default(),
    );
    let rid = last_result(&evs);
    let e = block_on(q.edit_info(rid)).unwrap_err();
    assert!(e.to_string().contains("full primary key"), "{e}");

    // ALLOW FILTERING warns before running; consistency per console.
    let evs = events(
        &core,
        s,
        "SELECT * FROM readings WHERE v > 100 ALLOW FILTERING;\nCONSISTENCY QUORUM;\nCONSISTENCY;",
        ExecOptions::default(),
    );
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
    assert!(
        notices(&evs)
            .iter()
            .any(|n| n.starts_with("ALLOW FILTERING"))
    );
    assert!(notices(&evs).iter().any(|n| n.contains("QUORUM")));

    // Risks.
    let risks = core.language().risks(
        "DROP TABLE kv; DELETE FROM kv WHERE k = 'a'",
        &QueryLanguage::Cql,
        false,
    );
    assert_eq!(risks.len(), 1);

    // Explorer and DDL.
    let m = core.metadata();
    let root = block_on(m.children(NodeRef::root(ds.clone()))).unwrap();
    let k = root
        .iter()
        .find(|o| o.name == ks)
        .unwrap_or_else(|| panic!("{ks} in {root:?}"));
    assert_eq!(k.kind, ObjectKind::Keyspace);
    assert!(k.detail.as_deref().unwrap_or("").contains("SimpleStrategy"));
    let content = block_on(m.children(k.node.clone())).unwrap();
    let names: Vec<(&str, ObjectKind)> =
        content.iter().map(|o| (o.name.as_str(), o.kind)).collect();
    assert!(
        names.contains(&("readings", ObjectKind::Table)),
        "{names:?}"
    );
    assert!(
        names.contains(&("address", ObjectKind::Function)),
        "{names:?}"
    );
    let t = content.iter().find(|o| o.name == "readings").unwrap();
    let cols = block_on(m.children(t.node.clone())).unwrap();
    assert!(
        cols.iter()
            .any(|o| o.kind == ObjectKind::Index && o.name == "readings_v")
    );
    let ddl = block_on(m.definition(vec![t.node.clone()], DdlOptions::default())).unwrap();
    assert!(
        ddl.contains("PRIMARY KEY ((sensor, day), ts)")
            && ddl.contains("CLUSTERING ORDER BY (ts DESC)")
            && ddl.contains("note text STATIC")
            && ddl.contains("CREATE INDEX readings_v"),
        "{ddl}"
    );

    // Cleanup.
    let evs = events(
        &core,
        s,
        &format!("DROP KEYSPACE {ks}"),
        ExecOptions::default(),
    );
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
}
