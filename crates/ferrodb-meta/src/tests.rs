//! SQLite introspection end to end, the DDL round trip on SQLite, the tree
//! mapping and the disk cache (start from cache for 10k objects).

use std::time::Instant;

use ferrodb_api::{DataSourceId, NodeRef, ObjectKind};
use ferrodb_drivers::{CancellationToken, Connection, Registry, ResolvedConfig, sink::collect};

use crate::{
    cache::Catalogs,
    ddl::{Target, container_ddl},
    introspector,
    model::*,
    tree::{self, Filter},
};

async fn sqlite(path: &std::path::Path) -> Box<dyn Connection> {
    let reg = Registry::builtin();
    let d = reg.get(&"sqlite".into()).unwrap();
    let cfg = ResolvedConfig::new("sqlite").with("file", path.to_string_lossy());
    d.connect(&cfg).await.unwrap()
}

async fn run(conn: &mut dyn Connection, script: &str) {
    for st in script.split(";\n").map(str::trim).filter(|s| !s.is_empty()) {
        collect(conn, st, CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{st}: {e}"));
    }
}

const CORPUS: &str = "
CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, name TEXT DEFAULT 'anon');
CREATE TABLE orders (id INTEGER PRIMARY KEY AUTOINCREMENT, customer_id INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE, total REAL CHECK (total >= 0), note TEXT);
CREATE TABLE order_items (order_id INTEGER, line INTEGER, sku TEXT, PRIMARY KEY (order_id, line), FOREIGN KEY (order_id) REFERENCES orders(id));
CREATE INDEX orders_customer ON orders (customer_id, total DESC);
CREATE UNIQUE INDEX items_sku ON order_items (sku) WHERE sku IS NOT NULL;
CREATE VIEW big_orders AS SELECT * FROM orders WHERE total > 100;
CREATE TRIGGER orders_note AFTER INSERT ON orders BEGIN UPDATE orders SET note = 'new' WHERE id = NEW.id; END;
";

#[tokio::test]
async fn sqlite_introspection_and_ddl_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = sqlite(&dir.path().join("a.db")).await;
    run(a.as_mut(), CORPUS).await;
    let i = introspector(Dialect::Sqlite);
    let containers = i.containers(a.as_mut()).await.unwrap();
    assert_eq!(containers[0].name, "main");
    let content = i.content(a.as_mut(), "main", None).await.unwrap();
    let names: Vec<&str> = content.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["customers", "order_items", "orders", "big_orders"]
    );
    let orders = content.table("orders").unwrap();
    assert_eq!(orders.primary_key.as_ref().unwrap().columns, vec!["id"]);
    assert_eq!(orders.foreign_keys[0].ref_table, "customers");
    assert_eq!(orders.foreign_keys[0].on_delete.as_deref(), Some("CASCADE"));
    assert_eq!(orders.indexes[0].name, "orders_customer");
    assert_eq!(orders.indexes[0].columns, vec!["customer_id", "total"]);
    assert_eq!(orders.triggers[0].event.as_deref(), Some("AFTER INSERT"));
    let items = content.table("order_items").unwrap();
    assert_eq!(
        items.primary_key.as_ref().unwrap().columns,
        vec!["order_id", "line"]
    );
    assert!(items.indexes[0].unique && items.indexes[0].predicate.is_some());
    assert_eq!(content.table("customers").unwrap().uniques.len(), 1);
    assert_eq!(
        content
            .table("big_orders")
            .unwrap()
            .view_definition
            .as_deref(),
        Some("SELECT * FROM orders WHERE total > 100")
    );

    // Generated DDL recreates the same schema in an empty database.
    let ddl = container_ddl(&Target::new(Dialect::Sqlite, "main"), &content);
    let mut b = sqlite(&dir.path().join("b.db")).await;
    run(b.as_mut(), &ddl).await;
    let again = i.content(b.as_mut(), "main", None).await.unwrap();
    assert_eq!(again.tables, content.tables, "DDL:\n{ddl}");

    // Incremental refresh of one table.
    run(
        a.as_mut(),
        "ALTER TABLE orders ADD COLUMN paid INTEGER DEFAULT 0",
    )
    .await;
    let one = i.content(a.as_mut(), "main", Some("orders")).await.unwrap();
    assert_eq!(one.tables.len(), 1);
    let mut merged = content.clone();
    crate::merge(&mut merged, one, Some("orders"));
    assert_eq!(merged.tables.len(), 4);
    assert!(merged.table("orders").unwrap().column("paid").is_some());
}

#[tokio::test]
async fn tree_resolve_and_documentation() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = sqlite(&dir.path().join("a.db")).await;
    run(a.as_mut(), CORPUS).await;
    let i = introspector(Dialect::Sqlite);
    let mut containers = i.containers(a.as_mut()).await.unwrap();
    containers[0].content = Some(i.content(a.as_mut(), "main", None).await.unwrap());
    let catalog = Catalog {
        containers: Some(containers),
        ..Catalog::new()
    };
    let ds = DataSourceId::new("s");
    let root = tree::containers(
        &ds,
        Dialect::Sqlite,
        catalog.containers.as_ref().unwrap(),
        &Filter::default(),
    );
    assert_eq!(
        root[0].node,
        NodeRef::root(ds.clone()).child(ObjectKind::Schema, "main")
    );
    let (c, content) = catalog.loaded().next().unwrap();
    let kids = tree::container_children(&root[0].node, content);
    assert!(
        kids.iter()
            .any(|k| k.kind == ObjectKind::View && k.name == "big_orders")
    );
    let orders = kids.iter().find(|k| k.name == "orders").unwrap();
    let t = tree::table_children(&orders.node, content.table("orders").unwrap());
    let kinds: Vec<ObjectKind> = t.iter().map(|o| o.kind).collect();
    assert!(kinds.contains(&ObjectKind::PrimaryKey) && kinds.contains(&ObjectKind::ForeignKey));
    assert!(kinds.contains(&ObjectKind::Index) && kinds.contains(&ObjectKind::Trigger));
    // Every child path resolves back to an object.
    for o in &t {
        assert!(
            tree::find(c, content, &o.node.path[1..]).is_some(),
            "{:?}",
            o.node
        );
    }

    let hits = tree::resolve(&ds, Dialect::Sqlite, &catalog, &["orders".into()]);
    assert_eq!(hits[0].name, "orders");
    let hits = tree::resolve(
        &ds,
        Dialect::Sqlite,
        &catalog,
        &["ORDERS".into(), "total".into()],
    );
    assert_eq!(hits[0].kind, ObjectKind::Column);
    let hits = tree::resolve(
        &ds,
        Dialect::Sqlite,
        &catalog,
        &["main".into(), "customers".into()],
    );
    assert_eq!(hits[0].kind, ObjectKind::Table);

    let found = tree::find(c, content, &orders.node.path[1..]).unwrap();
    let doc = tree::documentation(Dialect::Sqlite, "main", found);
    assert_eq!(doc.title, "orders");
    assert!(
        doc.fields
            .iter()
            .any(|(n, d)| n == "customer_id" && d.contains("→ customers")),
        "{doc:?}"
    );

    let entries = tree::search_entries(&ds, "local", Dialect::Sqlite, &catalog, &Filter::default());
    assert!(
        entries
            .iter()
            .any(|e| e.label == "order_items" && e.location == "local / main")
    );
    assert!(
        tree::search_entries(
            &ds,
            "local",
            Dialect::Sqlite,
            &catalog,
            &Filter::parse("other")
        )
        .is_empty()
    );
}

/// A catalog with `tables` tables of 10 columns, 2 indexes and a key.
fn big_catalog(tables: usize) -> Catalog {
    let tables = (0..tables)
        .map(|i| Table {
            name: format!("table_{i:05}"),
            columns: (0..10)
                .map(|c| Column {
                    name: format!("column_{c}"),
                    data_type: "character varying(100)".into(),
                    nullable: c > 0,
                    ..Default::default()
                })
                .collect(),
            primary_key: Some(Key {
                name: Some(format!("table_{i:05}_pkey")),
                columns: vec!["column_0".into()],
            }),
            indexes: (0..2)
                .map(|x| Index {
                    name: format!("table_{i:05}_idx{x}"),
                    columns: vec![format!("column_{}", x + 1)],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
        .collect();
    Catalog {
        containers: Some(vec![Container {
            name: "public".into(),
            comment: None,
            content: Some(Content {
                tables,
                ..Default::default()
            }),
        }]),
        ..Catalog::new()
    }
}

/// Plan, phase 4: start from cache < 300 ms for 10k objects. Measured
/// with 10,000 tables (≈ 140,000 objects with columns and indexes): read
/// the file, decode, and build the explorer's first two levels.
#[test]
fn start_from_cache_is_fast() {
    let dir = tempfile::tempdir().unwrap();
    let ds = DataSourceId::new("big");
    let cat = big_catalog(10_000);
    let objects = cat.object_count();
    Catalogs::new(Some(dir.path().to_owned())).put(&ds, cat);
    let size = std::fs::metadata(dir.path().join("big.json"))
        .unwrap()
        .len();

    let started = Instant::now();
    let fresh = Catalogs::new(Some(dir.path().to_owned()));
    let loaded = fresh.get(&ds).expect("cache file is read");
    let root = tree::containers(
        &ds,
        Dialect::Postgres,
        loaded.containers.as_ref().unwrap(),
        &Filter::default(),
    );
    let (_, content) = loaded.loaded().next().unwrap();
    let kids = tree::container_children(&root[0].node, content);
    let elapsed = started.elapsed();
    assert_eq!(kids.len(), 10_000);
    println!(
        "start from cache: {objects} objects ({:.1} MB) in {:.1} ms",
        size as f64 / 1e6,
        elapsed.as_secs_f64() * 1000.0
    );
    // Debug builds are several times slower than release; the budget
    // applies to both.
    assert!(elapsed.as_millis() < 300, "{elapsed:?}");
}

#[test]
fn outdated_or_broken_cache_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.json"), b"{not json").unwrap();
    std::fs::write(
        dir.path().join("b.json"),
        br#"{"version":0,"containers":null,"refreshed_at":0}"#,
    )
    .unwrap();
    let c = Catalogs::new(Some(dir.path().to_owned()));
    assert!(c.get(&"a".into()).is_none());
    assert!(c.get(&"b".into()).is_none());
    c.put(&"a".into(), Catalog::new());
    assert!(
        Catalogs::new(Some(dir.path().to_owned()))
            .get(&"a".into())
            .is_some()
    );
    c.remove(&"a".into());
    assert!(!dir.path().join("a.json").exists());
}
