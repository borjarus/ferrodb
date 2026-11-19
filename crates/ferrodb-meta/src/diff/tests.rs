//! Schema comparison, migration scripts and the table editor's `ALTER`:
//! generated text per dialect, and on SQLite the full loop (introspect →
//! compare → run the script → compare again: no differences left).

use ferrodb_api::{
    ColumnDesign, CompareOptions, DiffKind, ForeignKeyDesign, IndexDesign, ObjectKind, TableDesign,
};
use ferrodb_drivers::{CancellationToken, Connection, Registry, ResolvedConfig, sink::collect};

use super::*;
use crate::{ddl::Target, introspector};

fn col(name: &str, ty: &str, nullable: bool) -> Column {
    Column {
        name: name.into(),
        data_type: ty.into(),
        nullable,
        ..Default::default()
    }
}

fn orders_v1() -> Table {
    Table {
        name: "orders".into(),
        columns: vec![
            col("id", "integer", false),
            col("customer_id", "integer", true),
            col("total", "integer", true),
            col("note", "text", true),
        ],
        primary_key: Some(Key {
            name: Some("orders_pkey".into()),
            columns: vec!["id".into()],
        }),
        foreign_keys: vec![ForeignKey {
            name: Some("orders_customer_fk".into()),
            columns: vec!["customer_id".into()],
            ref_table: "customers".into(),
            ref_columns: vec!["id".into()],
            ..Default::default()
        }],
        indexes: vec![Index {
            name: "ix_note".into(),
            columns: vec!["note".into()],
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn orders_v2() -> Table {
    let mut t = orders_v1();
    t.columns[2] = Column {
        default: Some("0".into()),
        ..col("total", "numeric(10,2)", false)
    };
    t.columns.remove(3); // note
    t.columns.push(col("status", "varchar(20)", true));
    t.indexes = vec![Index {
        name: "ix_status".into(),
        columns: vec!["status".into()],
        ..Default::default()
    }];
    t.foreign_keys[0].on_delete = Some("CASCADE".into());
    t
}

#[test]
fn types_and_defaults_normalize() {
    assert_eq!(normalize_type("INT"), normalize_type("integer"));
    assert_eq!(normalize_type("int(11)"), "integer");
    assert_eq!(normalize_type("VARCHAR( 20 )"), "character varying(20)");
    assert_eq!(normalize_type("timestamptz"), "timestamp with time zone");
    assert_eq!(normalize_type("int unsigned"), "integer unsigned");
    assert_ne!(normalize_type("bigint"), normalize_type("integer"));
    assert_eq!(normalize_default("'new'::character varying"), "'new'");
    assert_eq!(normalize_default("((0))"), "0");
}

#[test]
fn postgres_alter_table() {
    let t = Target::new(Dialect::Postgres, "public");
    let ch = alter_table(
        &t,
        &orders_v1(),
        &orders_v2(),
        &[],
        &CompareOptions::default(),
    );
    let s = ch.ordered();
    let text = s.join("\n");
    // Foreign key re-created with the new action: dropped first, added last.
    assert!(
        s[0].starts_with("ALTER TABLE public.orders DROP CONSTRAINT orders_customer_fk"),
        "{text}"
    );
    assert!(s.last().unwrap().contains("ADD CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES public.customers (id) ON DELETE CASCADE"), "{text}");
    assert!(text.contains("DROP INDEX public.ix_note;"), "{text}");
    assert!(
        text.contains("ALTER TABLE public.orders DROP COLUMN note;"),
        "{text}"
    );
    assert!(
        text.contains("ALTER TABLE public.orders ADD COLUMN status varchar(20);"),
        "{text}"
    );
    assert!(
        text.contains("ALTER COLUMN total TYPE numeric(10,2) USING total::numeric(10,2);"),
        "{text}"
    );
    assert!(text.contains("ALTER COLUMN total SET NOT NULL;"), "{text}");
    assert!(text.contains("ALTER COLUMN total SET DEFAULT 0;"), "{text}");
    assert!(
        text.contains("CREATE INDEX ix_status ON public.orders (status);"),
        "{text}"
    );
    assert!(
        ch.warnings
            .iter()
            .any(|w| w.contains("dropping column orders.note"))
    );
    assert!(
        ch.details
            .iter()
            .any(|d| d.starts_with("column total: type integer → numeric(10,2)"))
    );
    // Nothing to do between equal tables.
    let same = alter_table(
        &t,
        &orders_v1(),
        &orders_v1(),
        &[],
        &CompareOptions::default(),
    );
    assert!(same.is_empty(), "{:?}", same);
}

#[test]
fn mysql_alter_and_rename() {
    let t = Target::new(Dialect::MySql, "shop");
    let mut new = orders_v2();
    new.name = "purchase".into();
    new.columns[1].name = "client_id".into();
    new.foreign_keys[0].columns = vec!["client_id".into()];
    let ch = alter_table(
        &t,
        &orders_v1(),
        &new,
        &[("client_id".into(), "customer_id".into())],
        &CompareOptions::default(),
    );
    let text = ch.ordered().join("\n");
    assert!(
        text.contains("RENAME TABLE `shop`.`orders` TO `shop`.`purchase`;"),
        "{text}"
    );
    assert!(
        text.contains("ALTER TABLE `shop`.`purchase` RENAME COLUMN `customer_id` TO `client_id`;"),
        "{text}"
    );
    assert!(
        text.contains(
            "ALTER TABLE `shop`.`purchase` MODIFY COLUMN `total` numeric(10,2) NOT NULL DEFAULT 0;"
        ),
        "{text}"
    );
    assert!(
        text.contains("DROP FOREIGN KEY `orders_customer_fk`"),
        "{text}"
    );
    assert!(
        text.contains("DROP INDEX `ix_note` ON `shop`.`orders`"),
        "{text}"
    );
    // The renamed column is not dropped and re-added.
    assert!(!text.contains("DROP COLUMN `customer_id`"), "{text}");
    assert!(!text.contains("ADD COLUMN `client_id`"), "{text}");
}

fn content(tables: Vec<Table>) -> Content {
    Content {
        tables,
        ..Default::default()
    }
}

fn customers() -> Table {
    Table {
        name: "customers".into(),
        columns: vec![col("id", "integer", false), col("email", "text", false)],
        primary_key: Some(Key {
            name: Some("customers_pkey".into()),
            columns: vec!["id".into()],
        }),
        ..Default::default()
    }
}

#[test]
fn compare_orders_script() {
    let mut payments = Table {
        name: "payments".into(),
        columns: vec![
            col("id", "integer", false),
            col("order_id", "integer", false),
        ],
        primary_key: Some(Key {
            name: None,
            columns: vec!["id".into()],
        }),
        foreign_keys: vec![ForeignKey {
            name: Some("payments_order_fk".into()),
            columns: vec!["order_id".into()],
            ref_table: "orders".into(),
            ref_columns: vec!["id".into()],
            ..Default::default()
        }],
        ..Default::default()
    };
    payments.indexes.push(Index {
        name: "ix_payments_order".into(),
        columns: vec!["order_id".into()],
        ..Default::default()
    });
    let view = |def: &str| Table {
        name: "big".into(),
        kind: TableKind::View,
        view_definition: Some(def.into()),
        ..Default::default()
    };
    let src = content(vec![
        customers(),
        orders_v2(),
        payments,
        view("SELECT * FROM orders WHERE total > 100"),
    ]);
    let legacy = Table {
        name: "legacy".into(),
        columns: vec![col("x", "integer", true)],
        ..Default::default()
    };
    let tgt = content(vec![
        customers(),
        orders_v1(),
        legacy,
        view("select *   from orders where total > 50"),
    ]);
    let side = |c| Side {
        dialect: Dialect::Postgres,
        container: "public",
        content: c,
    };
    let r = compare(side(&src), side(&tgt), &CompareOptions::default());
    let kinds: Vec<(DiffKind, &str)> = r.items.iter().map(|i| (i.kind, i.name.as_str())).collect();
    assert!(kinds.contains(&(DiffKind::Added, "payments")), "{kinds:?}");
    assert!(kinds.contains(&(DiffKind::Removed, "legacy")), "{kinds:?}");
    assert!(kinds.contains(&(DiffKind::Changed, "orders")), "{kinds:?}");
    assert!(kinds.contains(&(DiffKind::Changed, "big")), "{kinds:?}");
    assert!(!kinds.iter().any(|(_, n)| *n == "customers"), "{kinds:?}");
    let s = &r.script;
    let pos = |needle: &str| {
        s.find(needle)
            .unwrap_or_else(|| panic!("{needle} not in\n{s}"))
    };
    // Order: drop FK / view before altering, create table before its FK.
    assert!(pos("DROP VIEW public.big") < pos("ALTER TABLE public.orders DROP COLUMN note"));
    assert!(pos("CREATE TABLE public.payments") < pos("ADD CONSTRAINT payments_order_fk"));
    assert!(pos("CREATE INDEX ix_payments_order") > pos("CREATE TABLE public.payments"));
    assert!(pos("CREATE VIEW public.big AS") > pos("ALTER TABLE public.orders DROP COLUMN note"));
    // Removed objects are kept unless dropping is enabled.
    assert!(!s.contains("DROP TABLE public.legacy"));
    let r = compare(
        side(&src),
        side(&tgt),
        &CompareOptions {
            drop_removed: true,
            ..Default::default()
        },
    );
    assert!(r.script.contains("DROP TABLE public.legacy;"));
    assert!(r.warnings.iter().any(|w| w.contains("legacy")));
    // Comparing a schema with itself finds nothing.
    let same = compare(side(&src), side(&src), &CompareOptions::default());
    assert!(
        same.items.is_empty() && same.script.is_empty(),
        "{:?}",
        same.items
    );
}

#[test]
fn compare_into_another_schema_renames() {
    let src = content(vec![customers()]);
    let tgt = content(vec![]);
    let r = compare(
        Side {
            dialect: Dialect::Postgres,
            container: "dev",
            content: &src,
        },
        Side {
            dialect: Dialect::Postgres,
            container: "prod",
            content: &tgt,
        },
        &CompareOptions::default(),
    );
    assert!(
        r.script.contains("CREATE TABLE prod.customers"),
        "{}",
        r.script
    );
    assert!(!r.script.contains("dev."), "{}", r.script);
}

#[test]
fn er_model_relations() {
    let mut orders = orders_v1();
    orders.columns[1].nullable = false;
    let m = er_model("public", &content(vec![customers(), orders]));
    assert_eq!(m.entities.len(), 2);
    let o = m.entities.iter().find(|e| e.name == "orders").unwrap();
    assert!(o.columns[0].primary && o.columns[1].foreign);
    assert_eq!(m.relations.len(), 1);
    let r = &m.relations[0];
    assert_eq!((r.from.as_str(), r.to.as_str()), ("orders", "customers"));
    assert!(!r.optional && !r.one_to_one);
}

#[test]
fn design_round_trip_and_validation() {
    let d = table_to_design(&orders_v1());
    assert_eq!(d.original.as_deref(), Some("orders"));
    assert_eq!(d.primary_key, vec!["id"]);
    let back = design_to_table(Dialect::Postgres, &d, Some(&orders_v1()));
    assert_eq!(back, orders_v1());
    let c = content(vec![customers(), orders_v1()]);
    // A new table named like an existing one is refused.
    let mut dup = d.clone();
    dup.original = None;
    assert!(
        design_script(Dialect::Postgres, "public", &c, &dup)
            .unwrap_err()
            .contains("already exists")
    );
    let mut bad = d.clone();
    bad.columns[0].data_type.clear();
    assert!(design_script(Dialect::Postgres, "public", &c, &bad).is_err());
    let mut fresh = TableDesign {
        name: "tags".into(),
        columns: vec![
            ColumnDesign {
                name: "id".into(),
                data_type: "integer".into(),
                auto_increment: true,
                ..Default::default()
            },
            ColumnDesign {
                name: "label".into(),
                data_type: "text".into(),
                nullable: true,
                ..Default::default()
            },
        ],
        primary_key: vec!["id".into()],
        indexes: vec![IndexDesign {
            name: "ix_tags_label".into(),
            columns: vec!["label".into()],
            unique: true,
        }],
        ..Default::default()
    };
    let s = design_script(Dialect::Postgres, "public", &c, &fresh)
        .unwrap()
        .text();
    assert!(
        s.contains("id integer GENERATED BY DEFAULT AS IDENTITY NOT NULL"),
        "{s}"
    );
    assert!(
        s.contains("CREATE UNIQUE INDEX ix_tags_label ON public.tags (label);"),
        "{s}"
    );
    fresh.foreign_keys.push(ForeignKeyDesign {
        columns: vec!["id".into()],
        ref_table: "customers".into(),
        ref_columns: vec!["id".into()],
        ..Default::default()
    });
    let s = design_script(Dialect::MySql, "shop", &c, &fresh)
        .unwrap()
        .text();
    assert!(s.contains("`id` integer NOT NULL AUTO_INCREMENT"), "{s}");
    assert!(s.contains("REFERENCES `shop`.`customers` (`id`)"), "{s}");
}

// ------------------------------------------------------------ SQLite, live

async fn sqlite(path: &std::path::Path) -> Box<dyn Connection> {
    let reg = Registry::builtin();
    let d = reg.get(&"sqlite".into()).unwrap();
    let cfg = ResolvedConfig::new("sqlite").with("file", path.to_string_lossy());
    d.connect(&cfg).await.unwrap()
}

async fn run(conn: &mut dyn Connection, statements: &[String]) {
    for st in statements {
        collect(conn, st.clone(), CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{st}: {e}"));
    }
}

fn split(script: &str) -> Vec<String> {
    split_statements(script)
}

const V1: &str = "
CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL);
CREATE TABLE orders (id INTEGER PRIMARY KEY AUTOINCREMENT, customer_id INTEGER, total INTEGER, note TEXT);
CREATE INDEX ix_note ON orders (note);
CREATE VIEW big AS SELECT * FROM orders WHERE total > 50;
INSERT INTO customers VALUES (1, 'a@x'), (2, 'b@x');
INSERT INTO orders (customer_id, total, note) VALUES (1, 10, 'x'), (2, 200, 'y');
";

const V2: &str = "
CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL, name TEXT DEFAULT 'anon');
CREATE TABLE orders (id INTEGER PRIMARY KEY AUTOINCREMENT, customer_id INTEGER REFERENCES customers(id) ON DELETE CASCADE, total REAL NOT NULL DEFAULT 0, status TEXT);
CREATE INDEX ix_status ON orders (status);
CREATE TABLE payments (id INTEGER PRIMARY KEY, order_id INTEGER NOT NULL REFERENCES orders(id), amount REAL);
CREATE VIEW big AS SELECT * FROM orders WHERE total > 100;
";

#[tokio::test]
async fn sqlite_migration_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = sqlite(&dir.path().join("target.db")).await;
    let mut b = sqlite(&dir.path().join("source.db")).await;
    run(a.as_mut(), &split(V1)).await;
    run(b.as_mut(), &split(V2)).await;
    let i = introspector(Dialect::Sqlite);
    let tgt = i.content(a.as_mut(), "main", None).await.unwrap();
    let src = i.content(b.as_mut(), "main", None).await.unwrap();
    let side = |c| Side {
        dialect: Dialect::Sqlite,
        container: "main",
        content: c,
    };
    let r = compare(side(&src), side(&tgt), &CompareOptions::default());
    assert!(
        r.warnings.iter().any(|w| w.contains("rebuilt")),
        "{:?}",
        r.warnings
    );
    let orders = r.items.iter().find(|i| i.name == "orders").unwrap();
    assert_eq!(orders.kind, DiffKind::Changed);
    assert!(
        orders.detail.contains("column note dropped"),
        "{}",
        orders.detail
    );
    run(a.as_mut(), &split(&r.script)).await;

    // The target now matches the source.
    let tgt2 = i.content(a.as_mut(), "main", None).await.unwrap();
    let again = compare(side(&src), side(&tgt2), &CompareOptions::default());
    assert!(
        again.items.is_empty(),
        "left after migration: {:#?}\nscript was:\n{}",
        again
            .items
            .iter()
            .map(|i| (&i.name, &i.detail))
            .collect::<Vec<_>>(),
        r.script
    );
    // Rows survived the rebuild; the new column got its default.
    let out = collect(
        a.as_mut(),
        "select count(*) as n, sum(total) as s from orders",
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let rows = crate::rows::rows(&out.results);
    assert_eq!(rows[0].int("n"), Some(2));
    assert_eq!(rows[0].int("s"), Some(210));
    // AUTOINCREMENT is kept.
    let orders = tgt2.table("orders").unwrap();
    assert!(
        orders
            .server_sql
            .as_deref()
            .unwrap()
            .to_uppercase()
            .contains("AUTOINCREMENT")
    );
}

#[tokio::test]
async fn sqlite_table_editor_rename_and_add() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = sqlite(&dir.path().join("t.db")).await;
    run(a.as_mut(), &split(V1)).await;
    let i = introspector(Dialect::Sqlite);
    let c = i.content(a.as_mut(), "main", None).await.unwrap();
    // Rename a column and add a nullable one: done in place.
    let mut d = table_to_design(c.table("customers").unwrap());
    d.columns[1].name = "mail".into();
    d.columns.push(ColumnDesign {
        name: "city".into(),
        data_type: "TEXT".into(),
        nullable: true,
        ..Default::default()
    });
    let s = design_script(Dialect::Sqlite, "main", &c, &d).unwrap();
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    assert_eq!(
        s.statements,
        vec![
            "ALTER TABLE main.customers RENAME COLUMN email TO mail;".to_owned(),
            "ALTER TABLE main.customers ADD COLUMN city TEXT;".to_owned(),
        ]
    );
    run(a.as_mut(), &s.statements).await;
    // Making a column NOT NULL needs a rebuild; data stays.
    let c = i.content(a.as_mut(), "main", None).await.unwrap();
    let mut d = table_to_design(c.table("orders").unwrap());
    let total = d.columns.iter_mut().find(|c| c.name == "total").unwrap();
    total.nullable = false;
    total.default = Some("0".into());
    let s = design_script(Dialect::Sqlite, "main", &c, &d).unwrap();
    assert!(s.warnings.iter().any(|w| w.contains("rebuilt")));
    run(a.as_mut(), &s.statements).await;
    let c = i.content(a.as_mut(), "main", None).await.unwrap();
    let orders = c.table("orders").unwrap();
    assert!(!orders.column("total").unwrap().nullable);
    assert_eq!(orders.indexes[0].name, "ix_note");
    assert_eq!(c.table("customers").unwrap().columns[1].name, "mail");
    let _ = ObjectKind::Table;
}
