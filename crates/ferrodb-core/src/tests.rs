//! Core behaviour on SQLite (no server needed). The same flows run against
//! PostgreSQL / MySQL / MariaDB in `ferrodb-driver-tck`.

// Plain test threads, not the UI thread: short sleeps are fine here.
#![allow(clippy::disallowed_methods)]

use std::time::{Duration, Instant};

use ferrodb_api::*;
use futures::executor::block_on;

use crate::{CoreBackend, CoreOptions};

struct Fixture {
    core: CoreBackend,
    ds: DataSourceId,
    _dir: tempfile::TempDir,
}

fn fixture(edit: impl FnOnce(&mut DataSourceConfig)) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let core = CoreBackend::new(CoreOptions {
        config_dir: Some(dir.path().join("config")),
        data_dir: Some(dir.path().join("data")),
        ..CoreOptions::in_memory()
    })
    .unwrap();
    let mut cfg = DataSourceConfig {
        id: "lite".into(),
        name: "Lite".into(),
        driver: "sqlite".into(),
        folder: vec![],
        environment: Environment::Development,
        read_only: false,
        tx_mode: TxMode::Auto,
        properties: vec![(
            "file".into(),
            dir.path().join("db.sqlite").to_string_lossy().into_owned(),
        )],
        secret_ref: None,
    };
    edit(&mut cfg);
    block_on(core.data_sources().save(cfg.clone(), SecretValues::new())).unwrap();
    Fixture {
        core,
        ds: cfg.id,
        _dir: dir,
    }
}

impl Fixture {
    fn session(&self) -> SessionId {
        block_on(self.core.queries().open_session(&self.ds)).unwrap()
    }

    /// Events until `Finished`.
    fn run(&self, s: SessionId, text: &str, opts: ExecOptions) -> (QueryHandle, Vec<QueryEvent>) {
        let h = self.core.queries().execute(s, Script::new(text), opts);
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
        (h, out)
    }

    fn exec(&self, s: SessionId, text: &str) -> Vec<QueryEvent> {
        self.run(s, text, ExecOptions::default()).1
    }
}

fn rows(evs: &[QueryEvent]) -> usize {
    evs.iter()
        .map(|e| match e {
            QueryEvent::Data { chunk, .. } => chunk.len(),
            _ => 0,
        })
        .sum()
}

fn errors(evs: &[QueryEvent]) -> Vec<&QueryError> {
    evs.iter()
        .filter_map(|e| match e {
            QueryEvent::Error(q) => Some(q),
            _ => None,
        })
        .collect()
}

fn affected(evs: &[QueryEvent]) -> Vec<Option<u64>> {
    evs.iter()
        .filter_map(|e| match e {
            QueryEvent::StatementFinished { affected, .. } => Some(*affected),
            _ => None,
        })
        .collect()
}

#[test]
fn data_sources_persist_without_secrets() {
    let f = fixture(|_| {});
    let mut cfg = f.core.data_sources().get(&f.ds).unwrap().config;
    cfg.name = "Renamed".into();
    block_on(
        f.core
            .data_sources()
            .save(cfg, SecretValues::new().with("password", "s3cret")),
    )
    .unwrap();
    let path = f._dir.path().join("config").join("data-sources.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("Renamed") && text.contains("secret://lite"));
    assert!(!text.contains("s3cret"), "secrets never reach the file");
    // Reloaded by a new core.
    let again = CoreBackend::new(CoreOptions {
        config_dir: Some(f._dir.path().join("config")),
        ..CoreOptions::in_memory()
    })
    .unwrap();
    assert_eq!(again.data_sources().list().len(), 1);
    assert_eq!(
        again.data_sources().list()[0].capabilities.language,
        QueryLanguage::sql("sqlite")
    );
    // Test connection with an unsaved config.
    let report = block_on(f.core.data_sources().test(
        again.data_sources().list()[0].config.clone(),
        SecretValues::new(),
    ))
    .unwrap();
    assert!(report.server_version.starts_with("SQLite"));
}

#[test]
fn script_with_titles_affected_counts_and_error_position() {
    let f = fixture(|_| {});
    let s = f.session();
    let evs = f.exec(
        s,
        "create table t (id integer primary key, name text);\n\
         insert into t (name) values ('a'), ('b');\n\
         -- All names\n\
         select * from t order by id;\n\
         select count(*) as n from t",
    );
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
    assert_eq!(affected(&evs), vec![None, Some(2), None, None]);
    let titles: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            QueryEvent::ResultStarted { title, .. } => Some(title.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(titles, vec![Some("All names".to_owned()), None]);
    assert_eq!(rows(&evs), 3);

    // Errors stop the script and point into it (base offset included).
    let text = "select 1;\nselect * from nope;\nselect 2";
    let h = f.core.queries().execute(
        s,
        Script {
            text: text.into(),
            base_offset: 100,
        },
        ExecOptions::default(),
    );
    let evs: Vec<_> = block_on(async {
        let mut v = Vec::new();
        while let Ok(e) = h.events.recv().await {
            let done = matches!(e, QueryEvent::Finished);
            v.push(e);
            if done {
                break;
            }
        }
        v
    });
    let err = errors(&evs)[0];
    assert!(err.message.contains("nope"), "{err:?}");
    let r = err.range.clone().unwrap();
    assert!(r.start >= 100 + 10 && r.end <= 100 + 28, "{r:?}");
    assert_eq!(rows(&evs), 1, "statement after the error did not run");
}

#[test]
fn results_page_and_fetch_more() {
    let f = fixture(|_| {});
    let s = f.session();
    let q = "with recursive r(i) as (select 1 union all select i + 1 from r where i < 1200) select i from r";
    let (h, evs) = f.run(s, q, ExecOptions::default());
    assert_eq!(rows(&evs), 500, "first page");
    let rid = evs
        .iter()
        .find_map(|e| match e {
            QueryEvent::ResultPaused {
                result,
                has_more: true,
            } => Some(*result),
            _ => None,
        })
        .expect("paused with more rows");
    let mut total = 500;
    let mut done = false;
    while !done {
        block_on(f.core.queries().fetch_more(rid, 500)).unwrap();
        block_on(async {
            while let Ok(e) = h.events.recv().await {
                match e {
                    QueryEvent::Data { chunk, .. } => total += chunk.len(),
                    QueryEvent::ResultPaused { has_more, .. } => {
                        done = !has_more;
                        break;
                    }
                    _ => {}
                }
            }
        });
    }
    assert_eq!(total, 1200);
    assert!(block_on(f.core.queries().fetch_more(rid, 1)).is_err());
    // The session is free again.
    assert_eq!(rows(&f.exec(s, "select 1")), 1);
}

#[test]
fn a_new_command_closes_a_paused_result() {
    let f = fixture(|_| {});
    let s = f.session();
    let q = "with recursive r(i) as (select 1 union all select i + 1 from r) select i from r";
    let (_h, evs) = f.run(
        s,
        q,
        ExecOptions {
            page_size: 10,
            ..Default::default()
        },
    );
    assert_eq!(rows(&evs), 10);
    // Infinite result paused; the next statement still runs.
    assert_eq!(rows(&f.exec(s, "select 42")), 1);
}

#[test]
fn earlier_results_of_a_script_are_truncated_to_one_page() {
    let f = fixture(|_| {});
    let s = f.session();
    let big = "with recursive r(i) as (select 1 union all select i + 1 from r where i < 50) select i from r";
    let evs = f
        .run(
            s,
            &format!("{big};\nselect 'next'"),
            ExecOptions {
                page_size: 20,
                ..Default::default()
            },
        )
        .1;
    assert_eq!(rows(&evs), 21);
    assert!(
        evs.iter()
            .any(|e| matches!(e, QueryEvent::Notice(n) if n.message.contains("truncated")))
    );
}

#[test]
fn read_only_sources_reject_modifications() {
    let f = fixture(|c| c.read_only = true);
    // The file must exist for a read-only open.
    let path = f.core.data_sources().get(&f.ds).unwrap().config.properties[0]
        .1
        .clone();
    {
        let c = rusqlite_create(&path);
        drop(c);
    }
    let s = f.session();
    let evs = f.exec(s, "select 1; insert into t values (1)");
    assert_eq!(rows(&evs), 1);
    let e = errors(&evs);
    assert_eq!(e[0].code.as_deref(), Some("FERRODB-READ-ONLY"));
}

fn rusqlite_create(path: &str) -> std::fs::File {
    std::fs::File::create(path).unwrap()
}

#[test]
fn manual_transactions_commit_and_rollback() {
    let f = fixture(|c| c.tx_mode = TxMode::Manual);
    let s = f.session();
    f.exec(s, "create table t (a int)");
    block_on(f.core.queries().commit(s)).unwrap();
    f.exec(s, "insert into t values (1)");
    assert!(f.core.queries().sessions()[0].in_transaction);
    block_on(f.core.queries().rollback(s)).unwrap();
    assert!(!f.core.queries().sessions()[0].in_transaction);
    let evs = f.exec(s, "select count(*) from t");
    let n = evs.iter().find_map(|e| match e {
        QueryEvent::Data {
            chunk: ResultChunk::Rows(b),
            ..
        } => Some(
            b.column(0)
                .as_any()
                .downcast_ref::<arrow_array::Int64Array>()
                .unwrap()
                .value(0),
        ),
        _ => None,
    });
    assert_eq!(n, Some(0), "rolled back");
    f.exec(s, "insert into t values (2)");
    block_on(f.core.queries().commit(s)).unwrap();
    // Another session sees the committed row.
    let s2 = f.session();
    assert_eq!(rows(&f.exec(s2, "select * from t")), 1);
}

#[test]
fn cancel_stops_a_running_statement_quickly() {
    let f = fixture(|_| {});
    let s = f.session();
    let h = f.core.queries().execute(
        s,
        Script::new("with recursive r(i) as (select 1 union all select i + 1 from r) select count(*) from r"),
        ExecOptions::default(),
    );
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    h.cancel.cancel();
    let evs: Vec<_> = block_on(async {
        let mut v = Vec::new();
        while let Ok(e) = h.events.recv().await {
            let done = matches!(e, QueryEvent::Finished);
            v.push(e);
            if done {
                break;
            }
        }
        v
    });
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(errors(&evs)[0].code.as_deref(), Some("FERRODB-CANCELLED"));
    assert_eq!(rows(&f.exec(s, "select 1")), 1, "session still usable");
}

#[test]
fn statements_time_out() {
    let f = fixture(|c| {
        c.properties
            .push(("option.query_timeout".into(), "1".into()))
    });
    let s = f.session();
    let started = Instant::now();
    let evs = f.exec(
        s,
        "with recursive r(i) as (select 1 union all select i + 1 from r) select count(*) from r",
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(errors(&evs)[0].code.as_deref(), Some("FERRODB-TIMEOUT"));
}

#[test]
fn parameters_are_substituted() {
    let f = fixture(|_| {});
    let s = f.session();
    let lang = QueryLanguage::sql("sqlite");
    let text = "select :name as n, :n2 + 1 as m, ? as p";
    let names: Vec<_> = f
        .core
        .language()
        .parameters(text, &lang)
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, vec!["name", "n2", "?1"]);
    let opts = ExecOptions {
        params: vec![
            ("name".into(), Some("O'Brien".into())),
            ("n2".into(), Some("41".into())),
            ("?1".into(), None),
        ],
        ..Default::default()
    };
    let evs = f.run(s, text, opts).1;
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
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
    let n = b
        .column(0)
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    assert_eq!(n.value(0), "O'Brien");
    let m = b
        .column(1)
        .as_any()
        .downcast_ref::<arrow_array::Int64Array>()
        .unwrap();
    assert_eq!(m.value(0), 42);
    // Missing values are reported, nothing runs.
    let evs = f.exec(s, "select :x");
    assert_eq!(errors(&evs)[0].code.as_deref(), Some("FERRODB-PARAMS"));
}

#[test]
fn history_and_query_log_are_recorded() {
    let f = fixture(|c| c.tx_mode = TxMode::Manual);
    let s = f.session();
    f.exec(s, "select 1; select * from nope");
    // Writes are asynchronous.
    let mut hist = Vec::new();
    for _ in 0..50 {
        hist = block_on(f.core.history().query(HistoryFilter::default())).unwrap();
        if hist.len() >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(hist.len(), 2);
    assert!(!hist[0].succeeded && hist[0].text.contains("nope"));
    assert!(hist[1].succeeded);
    let log = f.core.query_log(10);
    assert!(
        log.iter()
            .any(|l| l.source == crate::LogSource::Internal && l.text == "BEGIN")
    );
}

#[test]
fn explorer_lists_schemas_tables_and_columns() {
    let f = fixture(|_| {});
    let s = f.session();
    f.exec(
        s,
        "create table orders (id integer, total decimal(10,2)); create view v as select 1 as x",
    );
    let md = f.core.metadata();
    let root = block_on(md.children(NodeRef::root(f.ds.clone()))).unwrap();
    assert_eq!(root[0].name, "main");
    assert_eq!(root[0].kind, ObjectKind::Schema);
    let objs = block_on(md.children(root[0].node.clone())).unwrap();
    let names: Vec<_> = objs.iter().map(|o| (o.kind, o.name.as_str())).collect();
    assert_eq!(
        names,
        vec![(ObjectKind::Table, "orders"), (ObjectKind::View, "v")]
    );
    let cols = block_on(md.children(objs[0].node.clone())).unwrap();
    assert_eq!(cols[1].name, "total");
    assert_eq!(cols[1].detail.as_deref(), Some("decimal(10,2)"));
    assert!(
        f.core
            .search()
            .entries()
            .iter()
            .any(|e| e.label == "orders")
    );
    assert_eq!(
        f.core.data_sources().get(&f.ds).unwrap().state,
        ConnectionState::Connected
    );
    block_on(f.core.data_sources().disconnect(&f.ds)).unwrap();
    assert!(f.core.queries().sessions().is_empty());
}

/// Phase 4: cached catalog, incremental refresh, DDL and scripts, name
/// resolution, quick documentation, schema filter, start from the disk
/// cache without connecting.
#[test]
fn live_explorer_cache_refresh_and_ddl() {
    let dir = tempfile::tempdir().unwrap();
    let opts = || CoreOptions {
        config_dir: Some(dir.path().join("config")),
        data_dir: Some(dir.path().join("data")),
        ..CoreOptions::in_memory()
    };
    let core = CoreBackend::new(opts()).unwrap();
    let ds: DataSourceId = "lite".into();
    let cfg = DataSourceConfig {
        id: ds.clone(),
        name: "Lite".into(),
        driver: "sqlite".into(),
        folder: vec![],
        environment: Environment::None,
        read_only: false,
        tx_mode: TxMode::Auto,
        properties: vec![(
            "file".into(),
            dir.path().join("db.sqlite").to_string_lossy().into_owned(),
        )],
        secret_ref: None,
    };
    block_on(core.data_sources().save(cfg.clone(), SecretValues::new())).unwrap();
    let q = core.queries();
    let s = block_on(q.open_session(&ds)).unwrap();
    let run = |text: &str| {
        let h = q.execute(s, Script::new(text), ExecOptions::default());
        block_on(async {
            while let Ok(ev) = h.events.recv().await {
                if let QueryEvent::Error(e) = &ev {
                    panic!("{text}: {e}");
                }
                if matches!(ev, QueryEvent::Finished) {
                    break;
                }
            }
        });
    };
    run("create table customers (id integer primary key, email text not null unique)");
    run(
        "create table orders (id integer primary key, customer_id integer references customers(id), total real)",
    );
    let md = core.metadata();
    let main = NodeRef::root(ds.clone()).child(ObjectKind::Schema, "main");
    let objs = block_on(md.children(main.clone())).unwrap();
    assert_eq!(objs.len(), 2);
    let orders = main.child(ObjectKind::Table, "orders");
    let kids = block_on(md.children(orders.clone())).unwrap();
    assert!(kids.iter().any(|k| k.kind == ObjectKind::ForeignKey));

    // The cache answers until a refresh; a table refresh is incremental.
    run("alter table orders add column paid integer");
    let cols = |md: &dyn MetadataService| {
        block_on(md.children(orders.clone()))
            .unwrap()
            .into_iter()
            .filter(|o| o.kind == ObjectKind::Column)
            .count()
    };
    assert_eq!(cols(md), 3);
    block_on(md.refresh(&ds, RefreshScope::Node(orders.clone()))).unwrap();
    assert_eq!(cols(md), 4);
    run("create table extra (x integer)");
    block_on(md.refresh(&ds, RefreshScope::All)).unwrap();
    assert_eq!(block_on(md.children(main.clone())).unwrap().len(), 3);

    // Bulk DDL in dependency order, scripts, resolution, documentation.
    let ddl = block_on(md.definition(
        vec![orders.clone(), main.child(ObjectKind::Table, "customers")],
        DdlOptions::default(),
    ))
    .unwrap();
    assert!(
        ddl.find("CREATE TABLE customers").unwrap() < ddl.find("CREATE TABLE orders").unwrap(),
        "{ddl}"
    );
    let ins = block_on(md.script(orders.clone(), ScriptKind::Insert)).unwrap();
    assert!(
        ins.starts_with("INSERT INTO main.orders (id, customer_id, total, paid)"),
        "{ins}"
    );
    let hits = md.resolve(&ds, &["orders".into(), "total".into()]);
    assert_eq!(hits[0].node, orders.child(ObjectKind::Column, "total"));
    let doc = block_on(md.documentation(orders.clone())).unwrap();
    assert!(
        doc.fields
            .iter()
            .any(|(n, d)| n == "customer_id" && d.contains("customers"))
    );
    assert!(
        md.snapshot(&ds)
            .unwrap()
            .objects
            .iter()
            .any(|o| o.name == "email")
    );
    block_on(q.close_session(s)).unwrap();
    drop(core);

    // A new start shows the tree from the disk cache, without connecting.
    let core = CoreBackend::new(opts()).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    let objs = block_on(core.metadata().children(main.clone())).unwrap();
    assert_eq!(objs.len(), 3);
    assert!(started.elapsed() < Duration::from_millis(300));
    assert_eq!(
        core.data_sources().get(&ds).unwrap().state,
        ConnectionState::Disconnected
    );
    assert!(core.search().entries().iter().any(|e| e.label == "extra"));

    // The schema filter hides containers.
    let filtered = DataSourceConfig {
        properties: [
            cfg.properties.clone(),
            vec![("schema.filter".into(), "!main".into())],
        ]
        .concat(),
        ..cfg
    };
    block_on(core.data_sources().save(filtered, SecretValues::new())).unwrap();
    assert!(
        block_on(core.metadata().children(NodeRef::root(ds.clone())))
            .unwrap()
            .is_empty()
    );
    assert!(!core.search().entries().iter().any(|e| e.label == "extra"));
}

#[test]
fn schema_switch_and_unknown_drivers() {
    let f = fixture(|_| {});
    let s = f.session();
    let evs = f
        .run(
            s,
            "select 1",
            ExecOptions {
                schema: Some("main".into()),
                ..Default::default()
            },
        )
        .1;
    assert!(errors(&evs).is_empty());
    let evs = f
        .run(
            s,
            "select 1",
            ExecOptions {
                schema: Some("temp".into()),
                ..Default::default()
            },
        )
        .1;
    assert!(
        !errors(&evs).is_empty(),
        "SQLite cannot switch the default schema"
    );
    let couch = DataSourceConfig {
        id: "m".into(),
        name: "Couchbase".into(),
        driver: "couchbase".into(),
        folder: vec![],
        environment: Environment::None,
        read_only: false,
        tx_mode: TxMode::Auto,
        properties: vec![],
        secret_ref: None,
    };
    block_on(f.core.data_sources().save(couch, SecretValues::new())).unwrap();
    let err = block_on(f.core.queries().open_session(&"m".into())).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
}

/// Plan, phase 3 completion criterion: 1M rows streamed. Run with
/// `cargo test -p ferrodb-core --release -- --ignored million`.
#[test]
#[ignore]
fn million_rows_stream() {
    let f = fixture(|_| {});
    let s = f.session();
    let q = "with recursive r(i) as (select 1 union all select i + 1 from r where i < 1000000) \
             select i, 'row ' || i as label, i * 0.5 as half from r";
    let started = Instant::now();
    let (h, evs) = f.run(
        s,
        q,
        ExecOptions {
            page_size: 500,
            ..Default::default()
        },
    );
    let first_page = started.elapsed();
    let rid = evs
        .iter()
        .find_map(|e| match e {
            QueryEvent::ResultPaused { result, .. } => Some(*result),
            _ => None,
        })
        .unwrap();
    block_on(f.core.queries().fetch_more(rid, usize::MAX / 2)).unwrap();
    let mut total = rows(&evs);
    block_on(async {
        while let Ok(e) = h.events.recv().await {
            match e {
                QueryEvent::Data { chunk, .. } => total += chunk.len(),
                QueryEvent::ResultPaused {
                    has_more: false, ..
                } => break,
                _ => {}
            }
        }
    });
    eprintln!(
        "first page {first_page:?}, 1M rows in {:?}",
        started.elapsed()
    );
    assert_eq!(total, 1_000_000);
}
