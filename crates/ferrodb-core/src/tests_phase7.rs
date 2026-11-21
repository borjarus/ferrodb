//! Phase 7 through the whole backend, without servers: execution plans on
//! SQLite and DuckDB, the ER model, schema comparison and applying its
//! script, the table editor (design → script → apply → explorer), DuckDB
//! file data sources in the explorer, and Local History surviving a
//! restart. `FERRODB_TCK_POSTGRES` / `_MYSQL` / `_MONGODB` (URLs, see the
//! TCK) add plans on real servers.

#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};

use ferrodb_api::*;
use futures::executor::block_on;

use crate::{CoreBackend, CoreOptions};

fn sqlite(id: &str, file: PathBuf) -> DataSourceConfig {
    DataSourceConfig {
        id: id.into(),
        name: id.into(),
        driver: "sqlite".into(),
        folder: vec![],
        environment: Environment::Development,
        read_only: false,
        tx_mode: TxMode::Auto,
        properties: vec![("file".into(), file.to_string_lossy().into_owned())],
        secret_ref: None,
    }
}

#[cfg(feature = "duckdb")]
fn duck(id: &str, files: &[&Path]) -> DataSourceConfig {
    DataSourceConfig {
        driver: "duckdb".into(),
        properties: vec![(
            "files".into(),
            files
                .iter()
                .map(|f| f.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("\n"),
        )],
        ..sqlite(id, PathBuf::new())
    }
}

fn core(dir: &Path) -> CoreBackend {
    CoreBackend::new(CoreOptions {
        config_dir: Some(dir.join("config")),
        data_dir: Some(dir.join("data")),
        ..CoreOptions::in_memory()
    })
    .unwrap()
}

fn run(core: &CoreBackend, s: SessionId, text: &str) {
    let h = core.queries().execute(
        s,
        Script::new(text),
        ExecOptions {
            mode: ExecMode::WholeScript,
            ..Default::default()
        },
    );
    let errs: Vec<String> = block_on(async {
        let mut errs = Vec::new();
        while let Ok(ev) = h.events.recv().await {
            match ev {
                QueryEvent::Error(q) => errs.push(q.to_string()),
                QueryEvent::Finished => break,
                _ => {}
            }
        }
        errs
    });
    assert!(errs.is_empty(), "{text}: {errs:?}");
}

const SHOP: &str = "
create table customers (id integer primary key, email text not null);
create table orders (id integer primary key, customer_id integer references customers(id), total real, note text);
create index ix_orders_customer on orders (customer_id);
insert into customers values (1, 'a@x'), (2, 'b@x');
insert into orders (customer_id, total, note) values (1, 10, 'x'), (2, 20, 'y'), (1, 5, 'z');
";

fn node(ds: &str, path: &[(ObjectKind, &str)]) -> NodeRef {
    let mut n = NodeRef::root(ds.into());
    for (k, name) in path {
        n = n.child(*k, *name);
    }
    n
}

#[test]
fn sqlite_plans_estimate_and_analyze() {
    let dir = tempfile::tempdir().unwrap();
    let c = core(dir.path());
    block_on(
        c.data_sources()
            .save(sqlite("a", dir.path().join("a.db")), SecretValues::new()),
    )
    .unwrap();
    let s = block_on(c.queries().open_session(&"a".into())).unwrap();
    run(&c, s, SHOP);
    let q = "select c.email, sum(o.total) from orders o join customers c on c.id = o.customer_id \
             where o.note <> 'q' group by c.email order by 2 desc; select 1";
    let plan = block_on(
        c.queries()
            .explain(s, Script::new(q), ExplainOptions::default()),
    )
    .unwrap();
    assert!(
        plan.statement
            .starts_with("EXPLAIN QUERY PLAN select c.email"),
        "{}",
        plan.statement
    );
    assert!(!plan.analyzed);
    let ops: Vec<String> = plan
        .root
        .flatten()
        .iter()
        .map(|(_, n)| n.operation.clone())
        .collect();
    assert!(ops.iter().any(|o| o == "SCAN" || o == "SEARCH"), "{ops:?}");
    assert!(ops.iter().any(|o| o == "USE TEMP B-TREE"), "{ops:?}");
    let plan = block_on(c.queries().explain(
        s,
        Script::new(q),
        ExplainOptions {
            analyze: true,
            schema: None,
        },
    ))
    .unwrap();
    assert!(plan.analyzed);
    assert_eq!(plan.root.actual_rows, Some(2));
    assert!(plan.execution_ms.is_some());
    // Analyze would run a write: refused; the estimate is fine.
    let del = Script::new("delete from orders");
    let e = block_on(c.queries().explain(
        s,
        del.clone(),
        ExplainOptions {
            analyze: true,
            schema: None,
        },
    ));
    assert!(matches!(e, Err(Error::Unsupported(_))), "{e:?}");
    block_on(c.queries().explain(s, del, ExplainOptions::default())).unwrap();
    let rows: i64 = {
        // Nothing was deleted.
        let h = c.queries().execute(
            s,
            Script::new("select count(*) from orders"),
            ExecOptions::default(),
        );
        block_on(async {
            let mut n = 0;
            while let Ok(ev) = h.events.recv().await {
                match ev {
                    QueryEvent::Data {
                        chunk: ResultChunk::Rows(b),
                        ..
                    } => {
                        n = b
                            .column(0)
                            .as_any()
                            .downcast_ref::<arrow_array::Int64Array>()
                            .unwrap()
                            .value(0);
                    }
                    QueryEvent::Finished => break,
                    _ => {}
                }
            }
            n
        })
    };
    assert_eq!(rows, 3);
    // Errors of the database come back as errors.
    assert!(
        block_on(c.queries().explain(
            s,
            Script::new("select * from nope"),
            ExplainOptions::default()
        ))
        .is_err()
    );
}

#[test]
fn schema_tools_on_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let c = core(dir.path());
    for id in ["dev", "prod"] {
        block_on(c.data_sources().save(
            sqlite(id, dir.path().join(format!("{id}.db"))),
            SecretValues::new(),
        ))
        .unwrap();
    }
    let dev = block_on(c.queries().open_session(&"dev".into())).unwrap();
    let prod = block_on(c.queries().open_session(&"prod".into())).unwrap();
    run(&c, dev, SHOP);
    run(
        &c,
        dev,
        "alter table customers add column city text; create table tags (id integer primary key, label text)",
    );
    run(&c, prod, SHOP);
    let dev_main = node("dev", &[(ObjectKind::Schema, "main")]);
    let prod_main = node("prod", &[(ObjectKind::Schema, "main")]);

    // ER model: two tables, one relation.
    let er = block_on(
        c.metadata()
            .er_model(dev_main.child(ObjectKind::Table, "orders")),
    )
    .unwrap();
    assert_eq!(er.container, "main");
    assert_eq!(er.entities.len(), 3);
    assert_eq!(er.relations.len(), 1);
    assert_eq!(
        (er.relations[0].from.as_str(), er.relations[0].to.as_str()),
        ("orders", "customers")
    );

    // dev → prod: a new table and a new column.
    let cmp = block_on(c.metadata().compare_schemas(
        dev_main.clone(),
        prod_main.clone(),
        CompareOptions::default(),
    ))
    .unwrap();
    let names: Vec<(DiffKind, &str)> = cmp
        .items
        .iter()
        .map(|i| (i.kind, i.name.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![(DiffKind::Added, "tags"), (DiffKind::Changed, "customers")],
        "{names:?}"
    );
    assert!(
        cmp.script
            .contains("ALTER TABLE main.customers ADD COLUMN city TEXT;"),
        "{}",
        cmp.script
    );
    let statements: Vec<String> = cmp
        .items
        .iter()
        .flat_map(|i| i.statements.clone())
        .collect();
    let n = block_on(c.metadata().apply_script(prod_main.clone(), statements)).unwrap();
    assert_eq!(n, 2);
    let again = block_on(c.metadata().compare_schemas(
        dev_main.clone(),
        prod_main.clone(),
        CompareOptions::default(),
    ))
    .unwrap();
    assert!(again.items.is_empty(), "{:?}", again.items);
    // The explorer sees the new table after the refresh.
    let kids = block_on(c.metadata().children(prod_main.clone())).unwrap();
    assert!(kids.iter().any(|o| o.name == "tags"), "{kids:?}");

    // Table editor: rename a column, add one, make total NOT NULL.
    let orders = prod_main.child(ObjectKind::Table, "orders");
    let mut d = block_on(c.metadata().table_design(orders.clone())).unwrap();
    assert_eq!(d.original.as_deref(), Some("orders"));
    assert_eq!(d.primary_key, vec!["id"]);
    d.columns
        .iter_mut()
        .find(|c| c.name == "note")
        .unwrap()
        .name = "comment".into();
    let total = d.columns.iter_mut().find(|c| c.name == "total").unwrap();
    total.nullable = false;
    total.default = Some("0".into());
    d.columns.push(ColumnDesign {
        name: "status".into(),
        data_type: "text".into(),
        nullable: true,
        default: Some("'new'".into()),
        ..Default::default()
    });
    let script = block_on(c.metadata().design_script(prod_main.clone(), d.clone())).unwrap();
    assert!(
        script.warnings.iter().any(|w| w.contains("rebuilt")),
        "{:?}",
        script.warnings
    );
    block_on(
        c.metadata()
            .apply_script(prod_main.clone(), script.statements),
    )
    .unwrap();
    let cols: Vec<String> = block_on(c.metadata().children(orders.clone()))
        .unwrap()
        .into_iter()
        .filter(|o| o.kind == ObjectKind::Column)
        .map(|o| o.name)
        .collect();
    assert_eq!(
        cols,
        vec!["id", "customer_id", "total", "comment", "status"]
    );
    let d2 = block_on(c.metadata().table_design(orders.clone())).unwrap();
    assert!(
        !d2.columns
            .iter()
            .find(|c| c.name == "total")
            .unwrap()
            .nullable
    );
    // A failing script is rolled back as a whole.
    let bad = vec![
        "alter table main.orders add column x integer".to_owned(),
        "alter table main.nope add column y integer".to_owned(),
    ];
    let e = block_on(c.metadata().apply_script(prod_main.clone(), bad));
    assert!(e.unwrap_err().to_string().contains("statement 2"));
    let d3 = block_on(c.metadata().table_design(orders)).unwrap();
    assert!(!d3.columns.iter().any(|c| c.name == "x"));
    // A new table from the editor.
    let new = TableDesign {
        name: "notes".into(),
        columns: vec![
            ColumnDesign {
                name: "id".into(),
                data_type: "integer".into(),
                auto_increment: true,
                ..Default::default()
            },
            ColumnDesign {
                name: "body".into(),
                data_type: "text".into(),
                nullable: true,
                ..Default::default()
            },
        ],
        primary_key: vec!["id".into()],
        ..Default::default()
    };
    let s = block_on(c.metadata().design_script(prod_main.clone(), new)).unwrap();
    assert!(
        s.text().contains("id INTEGER PRIMARY KEY AUTOINCREMENT"),
        "{}",
        s.text()
    );
    block_on(c.metadata().apply_script(prod_main.clone(), s.statements)).unwrap();
    // Read-only sources refuse DDL.
    let mut ro = sqlite("ro", dir.path().join("ro.db"));
    ro.read_only = true;
    block_on(c.data_sources().save(ro, SecretValues::new())).unwrap();
    let e = block_on(c.metadata().apply_script(
        node("ro", &[(ObjectKind::Schema, "main")]),
        vec!["create table t (a int)".into()],
    ));
    assert_eq!(e, Err(Error::ReadOnly));
}

#[test]
#[cfg(feature = "duckdb")]
fn duckdb_file_data_source() {
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("orders.csv");
    std::fs::write(&csv, "id,customer,total\n1,ala,10.5\n2,ola,20\n3,ala,7\n").unwrap();
    let json = dir.path().join("customers.json");
    std::fs::write(
        &json,
        r#"[{"name":"ala","city":"Kraków"},{"name":"ola","city":"Gdańsk"}]"#,
    )
    .unwrap();
    let c = core(dir.path());
    block_on(
        c.data_sources()
            .save(duck("files", &[&csv, &json]), SecretValues::new()),
    )
    .unwrap();
    let info = c
        .data_sources()
        .list()
        .into_iter()
        .find(|d| d.config.id == "files".into())
        .unwrap();
    assert_eq!(info.capabilities.explain, ExplainSupport::Structured);
    // Explorer: schema main with the two files as views and their columns.
    let root = block_on(c.metadata().children(NodeRef::root("files".into()))).unwrap();
    assert_eq!(root[0].name, "main", "{root:?}");
    let objs = block_on(c.metadata().children(root[0].node.clone())).unwrap();
    let names: Vec<&str> = objs.iter().map(|o| o.name.as_str()).collect();
    assert!(
        names.contains(&"orders") && names.contains(&"customers"),
        "{names:?}"
    );
    let orders = objs.iter().find(|o| o.name == "orders").unwrap();
    let cols = block_on(c.metadata().children(orders.node.clone())).unwrap();
    let cols: Vec<&str> = cols.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(cols, vec!["id", "customer", "total"]);
    // Queries and plans.
    let s = block_on(c.queries().open_session(&"files".into())).unwrap();
    run(
        &c,
        s,
        "select * from orders o join customers c on c.name = o.customer",
    );
    let q = Script::new(
        "select c.city, sum(o.total) from orders o join customers c on c.name = o.customer group by 1",
    );
    let plan = block_on(c.queries().explain(s, q.clone(), ExplainOptions::default())).unwrap();
    assert!(plan.root.len() > 2, "{:#?}", plan.root);
    assert!(plan.statement.starts_with("EXPLAIN (FORMAT JSON)"));
    let plan = block_on(c.queries().explain(
        s,
        q,
        ExplainOptions {
            analyze: true,
            schema: None,
        },
    ))
    .unwrap();
    assert!(plan.analyzed, "{}", plan.raw);
    // Export of a file view works like a table (phase 6 transfer).
    let out = dir.path().join("out.csv");
    let h = c.transfers().start(TransferRequest {
        source: TransferSource::Object(orders.node.clone()),
        target: TransferTarget::File {
            path: out.clone(),
            format: DataFormat::Csv,
            options: FormatOptions::default(),
        },
    });
    let ok = block_on(async {
        while let Ok(ev) = h.events.recv().await {
            match ev {
                TransferEvent::Finished(r) => return Ok(r.items),
                TransferEvent::Failed(e) => return Err(e),
                _ => {}
            }
        }
        Err("no outcome".into())
    });
    assert_eq!(ok, Ok(3));
}

#[test]
fn local_history_persists() {
    let dir = tempfile::tempdir().unwrap();
    {
        let c = core(dir.path());
        let h = c.local_history();
        let r = block_on(h.record(
            "console:a".into(),
            "select 1".into(),
            RevisionReason::Execute,
            None,
        ))
        .unwrap();
        assert!(r.is_some());
        assert!(
            block_on(h.record(
                "console:a".into(),
                "select 1".into(),
                RevisionReason::Autosave,
                None
            ))
            .unwrap()
            .is_none()
        );
        block_on(h.record(
            "console:a".into(),
            "select 2".into(),
            RevisionReason::Label,
            Some("before refactor".into()),
        ))
        .unwrap();
    }
    let c = core(dir.path());
    let h = c.local_history();
    let revs = block_on(h.revisions("console:a".into())).unwrap();
    assert_eq!(revs.len(), 2);
    assert_eq!(revs[0].label.as_deref(), Some("before refactor"));
    assert_eq!(block_on(h.text(revs[1].id)).unwrap(), "select 1");
    let docs = block_on(h.documents()).unwrap();
    assert_eq!(docs[0].document, "console:a");
    block_on(h.delete(revs[0].id)).unwrap();
    assert_eq!(block_on(h.revisions("console:a".into())).unwrap().len(), 1);
}

/// Plans on real servers, when the TCK variables point at them.
#[test]
fn server_plans_when_available() {
    let cases = [
        (
            "FERRODB_TCK_POSTGRES",
            "postgresql",
            "select * from pg_class c join pg_namespace n on n.oid = c.relnamespace",
        ),
        (
            "FERRODB_TCK_MYSQL",
            "mysql",
            "select * from information_schema.tables",
        ),
        (
            "FERRODB_TCK_MONGODB",
            "mongodb",
            "db.system.version.find({})",
        ),
    ];
    for (var, driver, q) in cases {
        let Ok(url) = std::env::var(var) else {
            eprintln!("{var} not set: {driver} plans skipped");
            continue;
        };
        let dir = tempfile::tempdir().unwrap();
        let c = core(dir.path());
        let resolved = ferrodb_drivers::ResolvedConfig::from_url(driver, &url).unwrap();
        let cfg = DataSourceConfig {
            driver: driver.into(),
            properties: resolved.properties.clone().into_iter().collect(),
            ..sqlite("srv", PathBuf::new())
        };
        block_on(c.data_sources().save(cfg, resolved.secrets.clone())).unwrap();
        let s = block_on(c.queries().open_session(&"srv".into())).unwrap();
        for analyze in [false, true] {
            let p = block_on(c.queries().explain(
                s,
                Script::new(q),
                ExplainOptions {
                    analyze,
                    schema: None,
                },
            ))
            .unwrap_or_else(|e| panic!("{driver} analyze={analyze}: {e}"));
            assert_eq!(p.analyzed, analyze, "{driver}: {}", p.raw);
            assert!(!p.root.operation.is_empty());
        }
    }
}
