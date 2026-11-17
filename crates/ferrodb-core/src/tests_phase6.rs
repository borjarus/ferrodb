//! Phase 6 through the whole backend on SQLite (no server needed): editing
//! tabular results (key, no key, manual transaction), related rows, the
//! value filter, export / import / copy with type round trips, and the
//! import budget. `FERRODB_TCK_MONGODB` adds the table ↔ collection round
//! trip on a real MongoDB.

#![allow(clippy::disallowed_methods)]

use std::{path::PathBuf, time::Instant};

use ferrodb_api::*;
use futures::executor::block_on;

use crate::{CoreBackend, CoreOptions};

struct Fx {
    core: CoreBackend,
    dir: tempfile::TempDir,
}

fn sqlite(id: &str, file: PathBuf, tx: TxMode) -> DataSourceConfig {
    DataSourceConfig {
        id: id.into(),
        name: id.into(),
        driver: "sqlite".into(),
        folder: vec![],
        environment: Environment::Development,
        read_only: false,
        tx_mode: tx,
        properties: vec![("file".into(), file.to_string_lossy().into_owned())],
        secret_ref: None,
    }
}

fn fx(tx: TxMode) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let core = CoreBackend::new(CoreOptions {
        config_dir: Some(dir.path().join("config")),
        data_dir: Some(dir.path().join("data")),
        ..CoreOptions::in_memory()
    })
    .unwrap();
    for id in ["a", "b"] {
        let cfg = sqlite(id, dir.path().join(format!("{id}.sqlite")), tx);
        block_on(core.data_sources().save(cfg, SecretValues::new())).unwrap();
    }
    Fx { core, dir }
}

impl Fx {
    fn ds(&self, id: &str) -> DataSourceId {
        id.into()
    }

    fn session(&self, id: &str) -> SessionId {
        block_on(self.core.queries().open_session(&self.ds(id))).unwrap()
    }

    fn run(&self, s: SessionId, text: &str) -> Vec<QueryEvent> {
        let h = self
            .core
            .queries()
            .execute(s, Script::new(text), ExecOptions::default());
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
        let errs: Vec<_> = out
            .iter()
            .filter_map(|e| match e {
                QueryEvent::Error(q) => Some(q.to_string()),
                _ => None,
            })
            .collect();
        assert!(errs.is_empty(), "{text}: {errs:?}");
        out
    }

    /// Runs a query; returns its result id and cells as text.
    fn query(&self, s: SessionId, text: &str) -> (ResultId, Vec<Vec<Option<String>>>) {
        let evs = self.run(s, text);
        let mut rid = None;
        let mut rows = Vec::new();
        for e in &evs {
            match e {
                QueryEvent::ResultStarted { result, .. } => rid = Some(*result),
                QueryEvent::Data {
                    chunk: ResultChunk::Rows(b),
                    ..
                } => {
                    for r in 0..b.num_rows() {
                        rows.push(
                            b.columns()
                                .iter()
                                .map(|c| ferrodb_data::value::value_at(c.as_ref(), r).to_text())
                                .collect(),
                        );
                    }
                }
                _ => {}
            }
        }
        (rid.expect("a result"), rows)
    }

    fn transfer(&self, req: TransferRequest) -> Result<TransferReport, String> {
        let h = self.core.transfers().start(req);
        block_on(async {
            while let Ok(ev) = h.events.recv().await {
                match ev {
                    TransferEvent::Finished(r) => return Ok(r),
                    TransferEvent::Failed(e) => return Err(e),
                    _ => {}
                }
            }
            Err("no outcome".into())
        })
    }
}

fn s(v: &str) -> Option<String> {
    Some(v.to_owned())
}

const SCHEMA: &str = "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, city TEXT);\n\
    CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id), \
        amount NUMERIC(10,2), placed DATE, note TEXT);\n\
    INSERT INTO customers VALUES (1, 'Ala', 'Kraków'), (2, 'Ola', NULL);\n\
    INSERT INTO orders VALUES (10, 1, 19.99, '2024-01-02', NULL), (11, 1, 5, '2024-02-03', 'gift'), \
        (12, 2, 7.5, NULL, NULL);";

#[test]
fn edits_by_primary_key() {
    let f = fx(TxMode::Auto);
    let s1 = f.session("a");
    f.run(s1, SCHEMA);
    let (rid, _) = f.query(
        s1,
        "SELECT id, name, city, upper(name) AS shout FROM customers ORDER BY id",
    );
    let q = f.core.queries();
    let info = block_on(q.edit_info(rid)).unwrap();
    assert_eq!(info.identity, RowIdentity::Key(vec!["id".into()]));
    assert_eq!(
        info.columns,
        vec![s("id"), s("name"), s("city"), None],
        "computed column is read-only"
    );
    let changes = ChangeSet {
        changes: vec![
            Change::SetCell {
                row: 0,
                column: 2,
                value: "\"Gdańsk\"".into(),
            },
            Change::InsertRow {
                values: vec![None, Some("\"Ewa\"".into()), Some("null".into()), None],
            },
            Change::DeleteRow { row: 1 },
        ],
    };
    let preview = block_on(q.preview_changes(rid, changes.clone())).unwrap();
    assert_eq!(
        preview,
        vec![
            "UPDATE customers SET city = 'Gdańsk' WHERE id = 1;",
            "INSERT INTO customers (name, city) VALUES ('Ewa', NULL);",
            "DELETE FROM customers WHERE id = 2;",
        ]
    );
    // Customer 2 still has orders: the DELETE fails and nothing is applied.
    let e = block_on(q.apply_changes(rid, changes.clone())).unwrap_err();
    assert!(e.to_string().contains("FOREIGN KEY"), "{e}");
    let (_, rows) = f.query(s1, "SELECT city FROM customers WHERE id = 1");
    assert_eq!(rows, vec![vec![s("Kraków")]]);
    f.run(s1, "DELETE FROM orders WHERE customer_id = 2");
    let report = block_on(q.apply_changes(rid, changes)).unwrap();
    assert_eq!((report.applied, report.pending_commit), (3, false));
    let (_, rows) = f.query(s1, "SELECT id, name, city FROM customers ORDER BY id");
    assert_eq!(
        rows,
        vec![
            vec![s("1"), s("Ala"), s("Gdańsk")],
            vec![s("3"), s("Ewa"), None]
        ]
    );
    // A computed column cannot be edited.
    let bad = ChangeSet {
        changes: vec![Change::SetCell {
            row: 0,
            column: 3,
            value: "\"X\"".into(),
        }],
    };
    assert!(block_on(q.preview_changes(rid, bad)).is_err());
}

#[test]
fn edits_without_key_refuse_ambiguous_rows() {
    let f = fx(TxMode::Auto);
    let s1 = f.session("a");
    f.run(
        s1,
        "CREATE TABLE t (a INT, b TEXT); INSERT INTO t VALUES (1, 'x'), (1, 'x'), (2, 'y');",
    );
    let (rid, _) = f.query(s1, "SELECT a, b FROM t ORDER BY a");
    let q = f.core.queries();
    let info = block_on(q.edit_info(rid)).unwrap();
    assert_eq!(info.identity, RowIdentity::AllColumns);
    assert!(info.warning.unwrap().contains("no primary or unique key"));
    let dup = ChangeSet {
        changes: vec![Change::SetCell {
            row: 0,
            column: 1,
            value: "\"z\"".into(),
        }],
    };
    let e = block_on(q.apply_changes(rid, dup)).unwrap_err().to_string();
    assert!(e.contains("would change 2 rows"), "{e}");
    let ok = ChangeSet {
        changes: vec![Change::SetCell {
            row: 2,
            column: 1,
            value: "\"z\"".into(),
        }],
    };
    assert_eq!(block_on(q.apply_changes(rid, ok)).unwrap().applied, 1);
    let (_, rows) = f.query(s1, "SELECT b FROM t ORDER BY a, b");
    assert_eq!(rows, vec![vec![s("x")], vec![s("x")], vec![s("z")]]);
}

#[test]
fn only_single_table_selects_are_editable() {
    let f = fx(TxMode::Auto);
    let s1 = f.session("a");
    f.run(s1, SCHEMA);
    let q = f.core.queries();
    for text in [
        "SELECT c.id, o.id FROM customers c JOIN orders o ON o.customer_id = c.id",
        "SELECT city, count(*) FROM customers GROUP BY city",
        "SELECT DISTINCT city FROM customers",
        "SELECT id FROM customers UNION SELECT id FROM orders",
    ] {
        let (rid, _) = f.query(s1, text);
        assert!(block_on(q.edit_info(rid)).is_err(), "{text}");
    }
    // Aliased table, qualified name and reordered columns are fine.
    let (rid, _) = f.query(s1, "SELECT o.note, o.id FROM main.orders o WHERE o.id > 10");
    let info = block_on(q.edit_info(rid)).unwrap();
    assert_eq!(info.columns, vec![s("note"), s("id")]);
    let set = ChangeSet {
        changes: vec![Change::SetCell {
            row: 0,
            column: 0,
            value: "\"edited\"".into(),
        }],
    };
    assert_eq!(
        block_on(q.preview_changes(rid, set)).unwrap(),
        vec!["UPDATE orders SET note = 'edited' WHERE id = 11;"]
    );
}

#[test]
fn manual_transaction_keeps_edits_until_commit() {
    let f = fx(TxMode::Manual);
    let s1 = f.session("a");
    f.run(s1, SCHEMA);
    f.run(s1, "COMMIT");
    let q = f.core.queries();
    let (rid, _) = f.query(s1, "SELECT id, city FROM customers ORDER BY id");
    let set = |v: &str| ChangeSet {
        changes: vec![Change::SetCell {
            row: 1,
            column: 1,
            value: format!("\"{v}\""),
        }],
    };
    let r = block_on(q.apply_changes(rid, set("Lublin"))).unwrap();
    assert!(r.pending_commit);
    assert!(q.sessions().iter().any(|x| x.id == s1 && x.in_transaction));
    block_on(q.rollback(s1)).unwrap();
    let (_, rows) = f.query(s1, "SELECT city FROM customers WHERE id = 2");
    assert_eq!(rows, vec![vec![None]]);
    block_on(q.apply_changes(rid, set("Lublin"))).unwrap();
    block_on(q.commit(s1)).unwrap();
    let s2 = f.session("a");
    let (_, rows) = f.query(s2, "SELECT city FROM customers WHERE id = 2");
    assert_eq!(rows, vec![vec![s("Lublin")]]);
}

#[test]
fn related_rows_and_value_filter() {
    let f = fx(TxMode::Auto);
    let s1 = f.session("a");
    f.run(s1, SCHEMA);
    let q = f.core.queries();
    let (rid, _) = f.query(s1, "SELECT * FROM orders ORDER BY id");
    let navs = block_on(q.navigations(rid)).unwrap();
    let out = navs
        .iter()
        .find(|n| n.outgoing)
        .expect("orders → customers");
    assert_eq!(out.label, "customer_id → customers(id)");
    assert_eq!(out.columns, vec![1]);
    let script = block_on(q.related_script(rid, out.clone(), vec!["1".into()])).unwrap();
    assert_eq!(script.text, "SELECT * FROM customers WHERE id = 1");
    let (_, rows) = f.query(s1, &script.text);
    assert_eq!(rows[0][1], s("Ala"));

    let (crid, _) = f.query(s1, "SELECT * FROM customers");
    let navs = block_on(q.navigations(crid)).unwrap();
    let back = navs
        .iter()
        .find(|n| !n.outgoing)
        .expect("customers ← orders");
    assert_eq!(back.label, "orders.customer_id → customers");
    let script = block_on(q.related_script(crid, back.clone(), vec!["2".into()])).unwrap();
    assert_eq!(script.text, "SELECT * FROM orders WHERE customer_id = 2");

    assert_eq!(
        q.value_filter(crid, "city".into(), "\"O'Hara\"".into())
            .unwrap(),
        "city = 'O''Hara'"
    );
    assert_eq!(
        q.value_filter(crid, "city".into(), "null".into()).unwrap(),
        "city IS NULL"
    );
}

#[test]
fn export_import_and_copy_keep_types() {
    let f = fx(TxMode::Auto);
    let sa = f.session("a");
    f.run(sa, SCHEMA);
    let (_, want) = f.query(
        sa,
        "SELECT id, customer_id, amount, placed, note FROM orders ORDER BY id",
    );
    let sb = f.session("b");
    for format in [
        DataFormat::Csv,
        DataFormat::JsonLines,
        DataFormat::ExtendedJson,
        DataFormat::Parquet,
        DataFormat::Xlsx,
    ] {
        let path = f.dir.path().join(format!("orders.{}", format.extension()));
        let r = f
            .transfer(TransferRequest {
                source: TransferSource::Query {
                    data_source: f.ds("a"),
                    text: "SELECT id, customer_id, amount, placed, note FROM orders ORDER BY id;"
                        .into(),
                    schema: None,
                },
                target: TransferTarget::File {
                    path: path.clone(),
                    format,
                    options: FormatOptions::default(),
                },
            })
            .unwrap();
        assert_eq!(r.items, 3, "{format:?}");
        let table = format!("orders_{}", format.extension());
        let r = f
            .transfer(TransferRequest {
                source: TransferSource::File {
                    path,
                    format,
                    options: FormatOptions::default(),
                },
                target: TransferTarget::Object {
                    data_source: f.ds("b"),
                    container: None,
                    name: table.clone(),
                    create: true,
                    truncate: false,
                    mapping: vec![],
                },
            })
            .unwrap_or_else(|e| panic!("{format:?}: {e}"));
        assert_eq!((r.items, r.method.as_str()), (3, "INSERT"), "{format:?}");
        assert!(
            r.statements[0].starts_with("CREATE TABLE"),
            "{:?}",
            r.statements
        );
        let (_, got) = f.query(
            sb,
            &format!("SELECT id, customer_id, amount, placed, note FROM {table} ORDER BY id"),
        );
        assert_eq!(got, want, "{format:?}");
    }

    // Copy table → table between sources, then append with truncate.
    let node = NodeRef::root(f.ds("a"))
        .child(ObjectKind::Schema, "main")
        .child(ObjectKind::Table, "orders");
    for truncate in [false, true] {
        let r = f
            .transfer(TransferRequest {
                source: TransferSource::Object(node.clone()),
                target: TransferTarget::Object {
                    data_source: f.ds("b"),
                    container: Some("main".into()),
                    name: "orders_copy".into(),
                    create: true,
                    truncate,
                    mapping: vec![],
                },
            })
            .unwrap();
        assert_eq!(r.items, 3);
    }
    let (_, got) = f.query(
        sb,
        "SELECT id, customer_id, amount, placed, note FROM orders_copy ORDER BY id",
    );
    assert_eq!(got, want);
    let (_, types) = f.query(
        sb,
        "SELECT type FROM pragma_table_info('orders_copy') ORDER BY cid",
    );
    assert_eq!(
        types,
        vec![
            vec![s("INTEGER")],
            vec![s("INTEGER")],
            vec![s("NUMERIC(10,2)")],
            vec![s("DATE")],
            vec![s("TEXT")]
        ]
    );

    // Explicit mapping into an existing table; missing table without create.
    let r = f
        .transfer(TransferRequest {
            source: TransferSource::Query {
                data_source: f.ds("a"),
                text: "SELECT name AS who, id FROM customers".into(),
                schema: None,
            },
            target: TransferTarget::Object {
                data_source: f.ds("a"),
                container: None,
                name: "orders".into(),
                create: false,
                truncate: false,
                mapping: vec![
                    ColumnMapping {
                        source: Some("who".into()),
                        target: "note".into(),
                    },
                    ColumnMapping {
                        source: Some("id".into()),
                        target: "customer_id".into(),
                    },
                ],
            },
        })
        .unwrap();
    assert_eq!(r.items, 2);
    let e = f
        .transfer(TransferRequest {
            source: TransferSource::Object(node),
            target: TransferTarget::Object {
                data_source: f.ds("b"),
                container: None,
                name: "nowhere".into(),
                create: false,
                truncate: false,
                mapping: vec![],
            },
        })
        .unwrap_err();
    assert!(e.contains("nowhere"), "{e}");
    // Writes are refused as export sources.
    let e = f
        .transfer(TransferRequest {
            source: TransferSource::Query {
                data_source: f.ds("a"),
                text: "DELETE FROM orders".into(),
                schema: None,
            },
            target: TransferTarget::File {
                path: f.dir.path().join("x.csv"),
                format: DataFormat::Csv,
                options: FormatOptions::default(),
            },
        })
        .unwrap_err();
    assert!(e.contains("only reading"), "{e}");
}

#[test]
fn import_200k_rows() {
    let f = fx(TxMode::Auto);
    let s1 = f.session("a");
    f.run(
        s1,
        "CREATE TABLE big (id INTEGER PRIMARY KEY, name TEXT, amount REAL, day DATE)",
    );
    let path = f.dir.path().join("big.csv");
    {
        use std::io::Write;
        let mut w = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        writeln!(w, "id,name,amount,day").unwrap();
        for i in 0..200_000 {
            writeln!(w, "{i},name {i},{}.5,2024-01-{:02}", i % 1000, i % 28 + 1).unwrap();
        }
    }
    let preview = block_on(f.core.transfers().preview_file(
        path.clone(),
        DataFormat::Csv,
        FormatOptions::default(),
        3,
    ))
    .unwrap();
    assert_eq!(preview.columns, vec!["id", "name", "amount", "day"]);
    assert_eq!(preview.types, vec!["integer", "text", "float", "date"]);
    let started = Instant::now();
    let r = f
        .transfer(TransferRequest {
            source: TransferSource::File {
                path,
                format: DataFormat::Csv,
                options: FormatOptions::default(),
            },
            target: TransferTarget::Object {
                data_source: f.ds("a"),
                container: None,
                name: "big".into(),
                create: false,
                truncate: false,
                mapping: vec![],
            },
        })
        .unwrap();
    let took = started.elapsed();
    eprintln!("import of 200k rows into SQLite: {took:?}");
    assert_eq!(r.items, 200_000);
    let (_, rows) = f.query(s1, "SELECT count(*), sum(amount), max(day) FROM big");
    // sum = 200 × (0 + … + 999) + 200 000 × 0.5
    assert_eq!(
        rows,
        vec![vec![s("200000"), s("100000000"), s("2024-01-28")]]
    );
}

/// Export of 10M generated rows to CSV through the streaming pipeline.
/// Slow in debug builds: `cargo test --release -- --ignored export_10m`.
#[test]
#[ignore]
fn export_10m_rows() {
    let f = fx(TxMode::Auto);
    let path = f.dir.path().join("ten.csv");
    let started = Instant::now();
    let r = f
        .transfer(TransferRequest {
            source: TransferSource::Query {
                data_source: f.ds("a"),
                text: "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 10000000) \
                       SELECT x, 'row ' || x AS label, x * 0.5 AS half FROM c"
                    .into(),
                schema: None,
            },
            target: TransferTarget::File {
                path,
                format: DataFormat::Csv,
                options: FormatOptions::default(),
            },
        })
        .unwrap();
    eprintln!(
        "10M rows → CSV: {:?}, {} MB",
        started.elapsed(),
        r.bytes / 1_000_000
    );
    assert_eq!(r.items, 10_000_000);
}

#[test]
fn table_collection_round_trip_on_mongodb() {
    let Ok(url) = std::env::var("FERRODB_TCK_MONGODB") else {
        return;
    };
    let f = fx(TxMode::Auto);
    let mongo = DataSourceConfig {
        id: "m".into(),
        name: "m".into(),
        driver: "mongodb".into(),
        folder: vec![],
        environment: Environment::None,
        read_only: false,
        tx_mode: TxMode::Auto,
        properties: vec![("uri".into(), url), ("database".into(), "phase6".into())],
        secret_ref: None,
    };
    block_on(f.core.data_sources().save(mongo, SecretValues::new())).unwrap();
    let sa = f.session("a");
    f.run(sa, SCHEMA);
    let node = NodeRef::root(f.ds("a"))
        .child(ObjectKind::Schema, "main")
        .child(ObjectKind::Table, "orders");
    let r = f
        .transfer(TransferRequest {
            source: TransferSource::Object(node),
            target: TransferTarget::Object {
                data_source: "m".into(),
                container: Some("phase6".into()),
                name: "orders".into(),
                create: true,
                truncate: true,
                mapping: vec![],
            },
        })
        .unwrap();
    assert_eq!((r.items, r.method.as_str()), (3, "insertMany"));
    // And back into a new table: same values and types.
    let back = NodeRef::root("m".into())
        .child(ObjectKind::Database, "phase6")
        .child(ObjectKind::Collection, "orders");
    f.run(
        sa,
        "CREATE TABLE orders_back (id INTEGER PRIMARY KEY, customer_id INTEGER, amount NUMERIC(10,2), placed DATE, note TEXT)",
    );
    let r = f
        .transfer(TransferRequest {
            source: TransferSource::Object(back),
            target: TransferTarget::Object {
                data_source: f.ds("a"),
                container: None,
                name: "orders_back".into(),
                create: false,
                truncate: false,
                mapping: vec![],
            },
        })
        .unwrap();
    assert_eq!(r.items, 3);
    let q = "SELECT id, customer_id, amount, placed, note, typeof(amount), typeof(placed) FROM {} ORDER BY id";
    let (_, a) = f.query(sa, &q.replace("{}", "orders"));
    let (_, b) = f.query(sa, &q.replace("{}", "orders_back"));
    assert_eq!(a, b);
}
