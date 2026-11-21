//! Phase 8 native variants: SQL Server DDL / ALTER generation (always),
//! and introspection + DDL round trips on real servers when
//! `FERRODB_TCK_SQLSERVER` / `FERRODB_TCK_COCKROACH` are set.

use ferrodb_api::{CompareOptions, ScriptKind};
use ferrodb_drivers::{CancellationToken, Connection, Registry, ResolvedConfig, sink::collect};

use crate::{
    ddl::{Object, Target, container_ddl, object_ddl, script},
    diff::alter_table,
    introspector,
    model::*,
};

fn col(name: &str, ty: &str, nullable: bool) -> Column {
    Column {
        name: name.into(),
        data_type: ty.into(),
        nullable,
        ..Default::default()
    }
}

fn orders() -> Table {
    Table {
        name: "orders".into(),
        columns: vec![
            Column {
                identity: Some("IDENTITY(1,1)".into()),
                ..col("id", "int", false)
            },
            Column {
                default: Some("((0))".into()),
                comment: Some("Gross".into()),
                ..col("total", "decimal(10,2)", true)
            },
            Column {
                generated: Some("([total]*(2))".into()),
                generated_stored: true,
                ..col("doubled", "decimal(11,2)", true)
            },
            col("key", "nvarchar(20)", true),
        ],
        primary_key: Some(Key {
            name: Some("PK_orders".into()),
            columns: vec!["id".into()],
        }),
        indexes: vec![
            Index {
                name: "cx_orders_total".into(),
                columns: vec!["[total]".into()],
                method: Some("CLUSTERED".into()),
                ..Default::default()
            },
            Index {
                name: "ix_key".into(),
                columns: vec!["[key] DESC".into()],
                include: vec!["[total]".into()],
                predicate: Some("([key] IS NOT NULL)".into()),
                unique: true,
                ..Default::default()
            },
        ],
        comment: Some("All orders".into()),
        ..Default::default()
    }
}

#[test]
fn sqlserver_ddl() {
    let t = Target::new(Dialect::SqlServer, "dbo");
    let ddl = object_ddl(&t, Object::Table(&orders()));
    assert!(
        ddl.contains("CREATE TABLE dbo.orders (\n    id int IDENTITY(1,1) NOT NULL,"),
        "{ddl}"
    );
    assert!(
        ddl.contains("total decimal(10,2) DEFAULT ((0)) NULL,"),
        "{ddl}"
    );
    assert!(ddl.contains("doubled AS ([total]*(2)) PERSISTED,"), "{ddl}");
    assert!(ddl.contains("[key] nvarchar(20) NULL,"), "{ddl}");
    assert!(
        ddl.contains("CONSTRAINT PK_orders PRIMARY KEY NONCLUSTERED (id)"),
        "{ddl}"
    );
    assert!(
        ddl.contains("CREATE CLUSTERED INDEX cx_orders_total ON dbo.orders ([total]);"),
        "{ddl}"
    );
    assert!(
        ddl.contains(
            "CREATE UNIQUE NONCLUSTERED INDEX ix_key ON dbo.orders ([key] DESC) INCLUDE ([total]) WHERE ([key] IS NOT NULL);"
        ),
        "{ddl}"
    );
    assert!(
        ddl.contains("sp_addextendedproperty @name = N'MS_Description', @value = N'All orders'"),
        "{ddl}"
    );
    let o = orders();
    let drop_col = crate::ddl::drop_stmt(&t, Object::Column(&o, &o.columns[1]));
    assert!(drop_col.contains("sys.default_constraints"), "{drop_col}");
    assert!(
        drop_col.ends_with("ALTER TABLE dbo.orders DROP COLUMN total;\n"),
        "{drop_col}"
    );
    assert_eq!(
        crate::ddl::drop_stmt(&t, Object::Index(&o, &o.indexes[1])),
        "DROP INDEX ix_key ON dbo.orders;\n"
    );
    let ins = script(&t, Object::Table(&o), ScriptKind::Insert).unwrap();
    assert!(!ins.contains("id,") && !ins.contains("doubled"), "{ins}");
    let proc = Routine {
        name: "add_order".into(),
        kind: RoutineKind::Procedure,
        arguments: vec![
            Argument {
                name: Some("@total".into()),
                data_type: "decimal(10,2)".into(),
                mode: "IN".into(),
            },
            Argument {
                name: Some("@id".into()),
                data_type: "int".into(),
                mode: "INOUT".into(),
            },
        ],
        signature: "add_order(decimal(10,2), int)".into(),
        ..Default::default()
    };
    assert_eq!(
        script(&t, Object::Routine(&proc), ScriptKind::Call).unwrap(),
        "EXEC dbo.add_order @total = :total, @id = :id OUTPUT;\n"
    );
    let content = Content {
        tables: vec![o.clone()],
        sequences: vec![Sequence {
            name: "seq".into(),
            data_type: Some("bigint".into()),
            start: Some(1),
            increment: Some(1),
            ..Default::default()
        }],
        synonyms: vec![Synonym {
            name: "o".into(),
            target: "[dbo].[orders]".into(),
        }],
        ..Default::default()
    };
    let all = container_ddl(&t, &content);
    assert!(
        all.starts_with("IF SCHEMA_ID(N'dbo') IS NULL EXEC(N'CREATE SCHEMA dbo');\nGO\n"),
        "{all}"
    );
    assert!(
        all.contains("CREATE SEQUENCE dbo.seq AS bigint START WITH 1 INCREMENT BY 1 NO CACHE;"),
        "{all}"
    );
    assert!(
        all.contains("CREATE SYNONYM dbo.o FOR [dbo].[orders];\nGO\n"),
        "{all}"
    );
}

#[test]
fn sqlserver_alter() {
    let t = Target::new(Dialect::SqlServer, "dbo");
    let old = orders();
    let mut new = orders();
    new.name = "purchases".into();
    new.columns[1].data_type = "decimal(12,2)".into();
    new.columns[1].nullable = false;
    new.columns[1].default = Some("((1))".into());
    new.columns[3].name = "code".into();
    new.columns.push(col("note", "nvarchar(max)", true));
    new.comment = None;
    let ch = alter_table(
        &t,
        &old,
        &new,
        &[("code".into(), "key".into())],
        &CompareOptions::default(),
    );
    let s = ch.ordered().join("\n");
    assert!(
        s.contains("EXEC sp_rename N'dbo.orders', N'purchases';"),
        "{s}"
    );
    assert!(
        s.contains("EXEC sp_rename N'dbo.purchases.[key]', N'code', N'COLUMN';"),
        "{s}"
    );
    assert!(
        s.contains("ALTER TABLE dbo.purchases ALTER COLUMN total decimal(12,2) NOT NULL;"),
        "{s}"
    );
    assert!(s.contains("ADD DEFAULT ((1)) FOR total;"), "{s}");
    assert!(
        s.contains("ALTER TABLE dbo.purchases ADD note nvarchar(max) NULL;"),
        "{s}"
    );
    assert!(s.contains("sp_dropextendedproperty"), "{s}");
}

#[test]
fn greenplum_and_redshift_table_options() {
    let t = Table {
        table_options: Some("DISTRIBUTED BY (id)".into()),
        partition_by: Some("RANGE (day)".into()),
        ..Table {
            name: "events".into(),
            columns: vec![col("id", "integer", false), col("day", "date", false)],
            ..Default::default()
        }
    };
    let ddl = object_ddl(&Target::new(Dialect::Postgres, "public"), Object::Table(&t));
    assert!(
        ddl.contains(") PARTITION BY RANGE (day) DISTRIBUTED BY (id);"),
        "{ddl}"
    );
    let crdb = Table {
        name: "t".into(),
        server_sql: Some("CREATE TABLE public.t (\n\tid INT8 NOT NULL,\n\tCONSTRAINT t_pkey PRIMARY KEY (id ASC)\n)".into()),
        ..Default::default()
    };
    let ddl = object_ddl(
        &Target::new(Dialect::Postgres, "public"),
        Object::Table(&crdb),
    );
    assert!(
        ddl.starts_with("CREATE TABLE public.t (\n\tid INT8"),
        "{ddl}"
    );
}

// ---------------------------------------------------------- live servers

async fn connect(driver: &str, url: &str) -> Box<dyn Connection> {
    let reg = Registry::builtin();
    let d = reg.get(&driver.into()).unwrap();
    let cfg = ResolvedConfig::from_url(driver, url).unwrap();
    d.connect(&cfg).await.unwrap()
}

async fn run(c: &mut dyn Connection, sql: &str) {
    collect(c, sql, CancellationToken::new())
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

const MSSQL_CORPUS: &str = "
CREATE TABLE dbo.customers (id int NOT NULL CONSTRAINT PK_customers PRIMARY KEY, email nvarchar(200) NOT NULL CONSTRAINT UQ_customers_email UNIQUE, name nvarchar(100) NULL DEFAULT (N'anon'));
CREATE TABLE dbo.orders (id int IDENTITY(10,5) NOT NULL CONSTRAINT PK_orders PRIMARY KEY NONCLUSTERED, customer_id int NOT NULL, total decimal(10,2) NULL DEFAULT ((0)), doubled AS ([total]*(2)) PERSISTED, placed datetime2(3) NOT NULL DEFAULT (sysutcdatetime()), CONSTRAINT FK_orders_customers FOREIGN KEY (customer_id) REFERENCES dbo.customers (id) ON DELETE CASCADE, CONSTRAINT CK_orders_total CHECK ([total]>=(0)));
CREATE CLUSTERED INDEX cx_orders_placed ON dbo.orders (placed);
CREATE NONCLUSTERED INDEX ix_orders_customer ON dbo.orders (customer_id, total DESC) INCLUDE (doubled) WHERE (total > 0);
CREATE SEQUENCE dbo.order_numbers AS bigint START WITH 100 INCREMENT BY 10;
CREATE SYNONYM dbo.purchases FOR dbo.orders;
EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'Customers of the shop', @level0type = N'SCHEMA', @level0name = N'dbo', @level1type = N'TABLE', @level1name = N'customers';
GO
CREATE VIEW dbo.big_orders AS SELECT id, total FROM dbo.orders WHERE total > 100
GO
CREATE PROCEDURE dbo.add_order @customer int, @total decimal(10,2), @id int OUTPUT AS BEGIN INSERT dbo.orders (customer_id, total) VALUES (@customer, @total); SET @id = SCOPE_IDENTITY(); END
GO
CREATE FUNCTION dbo.order_count (@customer int) RETURNS int AS BEGIN RETURN (SELECT count(*) FROM dbo.orders WHERE customer_id = @customer) END
GO
CREATE TRIGGER dbo.tr_orders ON dbo.orders AFTER INSERT, UPDATE AS SET NOCOUNT ON
";

#[tokio::test]
async fn sqlserver_introspection_round_trip() {
    let Ok(url) = std::env::var("FERRODB_TCK_SQLSERVER") else {
        return;
    };
    let mut c = connect("sqlserver", &url).await;
    let n = std::process::id();
    let (a, b) = (format!("ferrodb_rt_a{n}"), format!("ferrodb_rt_b{n}"));
    for db in [&a, &b] {
        run(c.as_mut(), &format!("CREATE DATABASE {db}")).await;
    }
    c.set_schema(&a).await.unwrap();
    run(c.as_mut(), MSSQL_CORPUS).await;
    let i = introspector(Dialect::SqlServer);
    let containers = i.containers(c.as_mut()).await.unwrap();
    assert_eq!(containers[0].name, "dbo");
    let content = i.content(c.as_mut(), "dbo", None).await.unwrap();
    let names: Vec<&str> = content.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["big_orders", "customers", "orders"]);
    let orders = content.table("orders").unwrap();
    assert_eq!(
        orders.columns[0].identity.as_deref(),
        Some("IDENTITY(10,5)")
    );
    assert_eq!(
        orders.columns[3].generated.as_deref(),
        Some("([total]*(2))")
    );
    assert!(orders.columns[3].generated_stored);
    assert_eq!(orders.columns[4].data_type, "datetime2(3)");
    assert_eq!(orders.foreign_keys[0].on_delete.as_deref(), Some("CASCADE"));
    assert_eq!(orders.checks[0].name.as_deref(), Some("CK_orders_total"));
    let ix: Vec<(&str, Option<&str>)> = orders
        .indexes
        .iter()
        .map(|i| (i.name.as_str(), i.method.as_deref()))
        .collect();
    assert_eq!(
        ix,
        [
            ("cx_orders_placed", Some("CLUSTERED")),
            ("ix_orders_customer", None)
        ]
    );
    assert_eq!(orders.indexes[1].include, ["[doubled]"]);
    assert_eq!(
        orders.triggers[0].event.as_deref(),
        Some("AFTER INSERT, UPDATE")
    );
    assert_eq!(
        content.table("customers").unwrap().comment.as_deref(),
        Some("Customers of the shop")
    );
    let proc = content
        .routines
        .iter()
        .find(|r| r.name == "add_order")
        .unwrap();
    assert_eq!(proc.kind, RoutineKind::Procedure);
    assert_eq!(proc.arguments[2].mode, "INOUT");
    let f = content
        .routines
        .iter()
        .find(|r| r.name == "order_count")
        .unwrap();
    assert_eq!(f.returns.as_deref(), Some("int"));
    assert_eq!(content.sequences[0].start, Some(100));
    assert_eq!(content.synonyms[0].target, "[dbo].[orders]");

    // The generated DDL recreates the same schema in another database.
    let ddl = container_ddl(&Target::new(Dialect::SqlServer, "dbo"), &content);
    c.set_schema(&b).await.unwrap();
    run(c.as_mut(), &ddl).await;
    let again = i.content(c.as_mut(), "dbo", None).await.unwrap();
    assert_eq!(again.tables, content.tables, "DDL:\n{ddl}");
    assert_eq!(again.sequences, content.sequences);
    assert_eq!(again.synonyms, content.synonyms);
    assert_eq!(again.routines.len(), content.routines.len());

    // ALTER from the table editor runs.
    let mut changed = again.table("customers").unwrap().clone();
    changed.columns[2].data_type = "nvarchar(150)".into();
    changed.columns[2].default = Some("(N'nobody')".into());
    changed.columns.push(col("phone", "varchar(30)", true));
    let ch = alter_table(
        &Target::new(Dialect::SqlServer, "dbo"),
        again.table("customers").unwrap(),
        &changed,
        &[],
        &CompareOptions::default(),
    );
    for st in &ch.ordered() {
        run(c.as_mut(), st).await;
    }
    let after = i
        .content(c.as_mut(), "dbo", Some("customers"))
        .await
        .unwrap();
    let t = &after.tables[0];
    assert_eq!(t.columns[2].data_type, "nvarchar(150)");
    assert_eq!(t.columns[2].default.as_deref(), Some("(N'nobody')"));
    assert_eq!(t.columns[3].name, "phone");

    c.set_schema("master").await.unwrap();
    for db in [&a, &b] {
        run(
            c.as_mut(),
            &format!(
                "ALTER DATABASE {db} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {db}"
            ),
        )
        .await;
    }
}

#[tokio::test]
async fn cockroach_introspection_round_trip() {
    let Ok(url) = std::env::var("FERRODB_TCK_COCKROACH") else {
        return;
    };
    let mut c = connect("cockroachdb", &url).await;
    let n = std::process::id();
    let (a, b) = (format!("rt_a{n}"), format!("rt_b{n}"));
    for db in [&a, &b] {
        run(c.as_mut(), &format!("CREATE DATABASE {db}")).await;
    }
    run(c.as_mut(), &format!("SET database = {a}")).await;
    for st in [
        "CREATE TABLE customers (id INT8 PRIMARY KEY, email STRING NOT NULL UNIQUE, name STRING DEFAULT 'anon')",
        "CREATE TABLE orders (id UUID PRIMARY KEY DEFAULT gen_random_uuid(), customer_id INT8 NOT NULL REFERENCES customers (id) ON DELETE CASCADE, total DECIMAL(10,2) CHECK (total >= 0), tags STRING[], INDEX orders_customer (customer_id, total DESC))",
        "CREATE TABLE events (payload JSONB, INVERTED INDEX events_payload (payload))",
        "CREATE VIEW big_orders AS SELECT id, total FROM orders WHERE total > 100",
        "CREATE SEQUENCE order_numbers START 100 INCREMENT 10",
    ] {
        run(c.as_mut(), st).await;
    }
    let i = introspector(Dialect::Postgres);
    let containers = i.containers(c.as_mut()).await.unwrap();
    assert!(containers.iter().any(|x| x.name == "public"));
    let content = i.content(c.as_mut(), "public", None).await.unwrap();
    let names: Vec<&str> = content.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["big_orders", "customers", "events", "orders"]);
    let events = content.table("events").unwrap();
    assert!(
        events.columns.iter().all(|c| c.name != "rowid"),
        "{:?}",
        events.columns
    );
    assert!(
        events
            .server_sql
            .as_deref()
            .unwrap()
            .contains("INVERTED INDEX")
    );
    let orders = content.table("orders").unwrap();
    assert_eq!(orders.foreign_keys[0].ref_table, "customers");

    let ddl = container_ddl(&Target::new(Dialect::Postgres, "public"), &content);
    run(c.as_mut(), &format!("SET database = {b}")).await;
    for st in ddl.split(";\n").map(str::trim).filter(|s| !s.is_empty()) {
        run(c.as_mut(), st).await;
    }
    let again = i.content(c.as_mut(), "public", None).await.unwrap();
    let cols = |c: &Content| -> Vec<(String, Vec<String>)> {
        c.tables
            .iter()
            .map(|t| {
                (
                    t.name.clone(),
                    t.columns
                        .iter()
                        .map(|c| format!("{} {}", c.name, c.data_type))
                        .collect(),
                )
            })
            .collect()
    };
    assert_eq!(cols(&again), cols(&content), "DDL:\n{ddl}");
    run(c.as_mut(), "SET database = defaultdb").await;
    for db in [&a, &b] {
        run(c.as_mut(), &format!("DROP DATABASE {db} CASCADE")).await;
    }
}

#[tokio::test]
async fn greenplum_distribution_round_trip() {
    let Ok(url) = std::env::var("FERRODB_TCK_GREENPLUM") else {
        return;
    };
    let mut c = connect("greenplum", &url).await;
    assert!(
        c.server_version().starts_with("Greenplum 7"),
        "{}",
        c.server_version()
    );
    let n = std::process::id();
    let (a, b) = (format!("rt_gp_a{n}"), format!("rt_gp_b{n}"));
    run(c.as_mut(), &format!("CREATE SCHEMA {a}")).await;
    for st in [
        format!("CREATE TABLE {a}.facts (id int PRIMARY KEY, v text) DISTRIBUTED BY (id)"),
        format!("CREATE TABLE {a}.dims (code text, name text) DISTRIBUTED REPLICATED"),
        format!(
            "CREATE TABLE {a}.events (id int, day date) USING ao_column WITH (compresstype=zlib) DISTRIBUTED BY (id)"
        ),
        format!(
            "CREATE TABLE {a}.logs (id int, day date) PARTITION BY RANGE (day) DISTRIBUTED RANDOMLY"
        ),
        format!(
            "CREATE TABLE {a}.logs_2026 PARTITION OF {a}.logs FOR VALUES FROM ('2026-01-01') TO ('2027-01-01')"
        ),
    ] {
        run(c.as_mut(), &st).await;
    }
    let i = introspector(Dialect::Postgres);
    let content = i.content(c.as_mut(), &a, None).await.unwrap();
    let opts = |t: &str| content.table(t).and_then(|t| t.table_options.clone());
    assert_eq!(opts("facts").as_deref(), Some("DISTRIBUTED BY (id)"));
    assert_eq!(opts("dims").as_deref(), Some("DISTRIBUTED REPLICATED"));
    let ev = opts("events").unwrap();
    assert!(
        ev.starts_with("USING ao_column WITH (") && ev.ends_with("DISTRIBUTED BY (id)"),
        "{ev}"
    );
    assert_eq!(opts("logs").as_deref(), Some("DISTRIBUTED RANDOMLY"));
    let ddl = container_ddl(
        &Target {
            rename: Some(&b),
            ..Target::new(Dialect::Postgres, &a)
        },
        &content,
    );
    for st in ddl.split(";\n").map(str::trim).filter(|s| !s.is_empty()) {
        run(c.as_mut(), st).await;
    }
    let again = i.content(c.as_mut(), &b, None).await.unwrap();
    assert_eq!(again.tables, content.tables, "DDL:\n{ddl}");
    for s in [&a, &b] {
        run(c.as_mut(), &format!("DROP SCHEMA {s} CASCADE")).await;
    }
}
