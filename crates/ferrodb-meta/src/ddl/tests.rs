//! DDL generated from the model (golden strings). Round trips against real
//! servers are in `tests/roundtrip.rs`.

use super::*;

fn col(name: &str, ty: &str, nullable: bool) -> Column {
    Column {
        name: name.into(),
        data_type: ty.into(),
        nullable,
        ..Default::default()
    }
}

fn customers() -> Table {
    Table {
        name: "customers".into(),
        comment: Some("People who buy".into()),
        columns: vec![
            Column {
                serial: true,
                ..col("id", "integer", false)
            },
            Column {
                comment: Some("it's unique".into()),
                ..col("email", "character varying(200)", false)
            },
            Column {
                default: Some("now()".into()),
                ..col("created", "timestamp with time zone", true)
            },
            col("Order", "text", true),
        ],
        primary_key: Some(Key {
            name: Some("customers_pkey".into()),
            columns: vec!["id".into()],
        }),
        uniques: vec![Key {
            name: Some("customers_email_key".into()),
            columns: vec!["email".into()],
        }],
        indexes: vec![Index {
            name: "customers_created_idx".into(),
            columns: vec!["created DESC".into()],
            predicate: Some("(created IS NOT NULL)".into()),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn orders() -> Table {
    Table {
        name: "orders".into(),
        columns: vec![
            Column {
                identity: Some("ALWAYS".into()),
                ..col("id", "bigint", false)
            },
            col("customer_id", "integer", false),
            Column {
                generated: Some("(total * 1.23)".into()),
                generated_stored: true,
                ..col("gross", "numeric", true)
            },
            col("total", "numeric(10,2)", true),
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
            on_delete: Some("CASCADE".into()),
            ..Default::default()
        }],
        checks: vec![Check {
            name: Some("orders_total_check".into()),
            expression: "((total >= (0)::numeric))".into(),
        }],
        ..Default::default()
    }
}

#[test]
fn postgres_table() {
    let t = Target::new(Dialect::Postgres, "sales");
    assert_eq!(
        object_ddl(&t, Object::Table(&customers())),
        "CREATE TABLE sales.customers (\n    \
         id serial NOT NULL,\n    \
         email character varying(200) NOT NULL,\n    \
         created timestamp with time zone DEFAULT now(),\n    \
         \"Order\" text,\n    \
         CONSTRAINT customers_pkey PRIMARY KEY (id),\n    \
         CONSTRAINT customers_email_key UNIQUE (email)\n);\n\
         CREATE INDEX customers_created_idx ON sales.customers (created DESC) WHERE (created IS NOT NULL);\n\
         COMMENT ON TABLE sales.customers IS 'People who buy';\n\
         COMMENT ON COLUMN sales.customers.email IS 'it''s unique';\n"
    );
    let o = object_ddl(&t, Object::Table(&orders()));
    assert!(
        o.contains("id bigint GENERATED ALWAYS AS IDENTITY NOT NULL"),
        "{o}"
    );
    assert!(
        o.contains("gross numeric GENERATED ALWAYS AS ((total * 1.23)) STORED"),
        "{o}"
    );
    assert!(o.contains("CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES sales.customers (id) ON DELETE CASCADE"), "{o}");
    assert!(
        o.contains("CONSTRAINT orders_total_check CHECK ((total >= (0)::numeric))"),
        "{o}"
    );
}

#[test]
fn bulk_ddl_is_in_dependency_order_and_breaks_cycles() {
    let t = Target {
        rename: Some("copy"),
        ..Target::new(Dialect::Postgres, "sales")
    };
    let (c, o) = (customers(), orders());
    let ddl = tables_ddl(&t, &[&o, &c]);
    let ci = ddl.find("CREATE TABLE copy.customers").unwrap();
    let oi = ddl.find("CREATE TABLE copy.orders").unwrap();
    assert!(ci < oi, "{ddl}");
    assert!(ddl.contains("REFERENCES copy.customers"), "{ddl}");
    // A cycle: customers → orders as well.
    let mut c2 = customers();
    c2.foreign_keys.push(ForeignKey {
        name: Some("customers_last_order_fk".into()),
        columns: vec!["id".into()],
        ref_table: "orders".into(),
        ref_columns: vec!["id".into()],
        ..Default::default()
    });
    let ddl = tables_ddl(&t, &[&c2, &o]);
    assert!(
        ddl.trim_end().ends_with(
            "ALTER TABLE copy.customers ADD CONSTRAINT customers_last_order_fk FOREIGN KEY (id) REFERENCES copy.orders (id);"
        ),
        "{ddl}"
    );
    assert_eq!(ddl.matches("customers_last_order_fk").count(), 1);
}

#[test]
fn mysql_table() {
    let t = Target::new(Dialect::MySql, "shop");
    let table = Table {
        name: "items".into(),
        engine: Some("InnoDB".into()),
        collation: Some("utf8mb4_0900_ai_ci".into()),
        comment: Some("Line items".into()),
        columns: vec![
            Column {
                auto_increment: true,
                ..col("id", "int unsigned", false)
            },
            Column {
                default: Some("'new'".into()),
                ..col("state", "varchar(10)", true)
            },
            Column {
                default: Some("CURRENT_TIMESTAMP".into()),
                on_update: Some("CURRENT_TIMESTAMP".into()),
                ..col("updated", "timestamp", true)
            },
        ],
        primary_key: Some(Key {
            name: None,
            columns: vec!["id".into()],
        }),
        indexes: vec![Index {
            name: "state_idx".into(),
            columns: vec!["`state`(5)".into()],
            ..Default::default()
        }],
        ..Default::default()
    };
    assert_eq!(
        object_ddl(&t, Object::Table(&table)),
        "CREATE TABLE `shop`.`items` (\n    \
         `id` int unsigned NOT NULL AUTO_INCREMENT,\n    \
         `state` varchar(10) DEFAULT 'new',\n    \
         `updated` timestamp DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,\n    \
         PRIMARY KEY (`id`),\n    \
         KEY `state_idx` (`state`(5))\n) ENGINE=InnoDB COLLATE=utf8mb4_0900_ai_ci COMMENT='Line items';\n"
    );
}

#[test]
fn scripts() {
    let t = Target::new(Dialect::Postgres, "sales");
    let o = orders();
    assert_eq!(
        script(&t, Object::Table(&o), ScriptKind::Insert).unwrap(),
        "INSERT INTO sales.orders (customer_id, total)\nVALUES (:customer_id, :total);\n"
    );
    assert_eq!(
        script(&t, Object::Table(&o), ScriptKind::Update).unwrap(),
        "UPDATE sales.orders\nSET customer_id = :customer_id,\n    total = :total\nWHERE id = :id;\n"
    );
    assert_eq!(
        script(&t, Object::Table(&o), ScriptKind::Delete).unwrap(),
        "DELETE FROM sales.orders\nWHERE id = :id;\n"
    );
    let f = Routine {
        name: "order_total".into(),
        signature: "order_total(integer, text)".into(),
        kind: RoutineKind::Function,
        arguments: vec![
            Argument {
                name: Some("id".into()),
                data_type: "integer".into(),
                mode: "IN".into(),
            },
            Argument {
                name: None,
                data_type: "text".into(),
                mode: "IN".into(),
            },
            Argument {
                name: Some("result".into()),
                data_type: "numeric".into(),
                mode: "OUT".into(),
            },
        ],
        returns: Some("numeric".into()),
        ..Default::default()
    };
    assert_eq!(
        script(&t, Object::Routine(&f), ScriptKind::Call).unwrap(),
        "SELECT sales.order_total(:id, :arg2);\n"
    );
    assert_eq!(
        script(&t, Object::Routine(&f), ScriptKind::Drop).unwrap(),
        "DROP FUNCTION sales.order_total(integer, text);\n"
    );
    let m = Target::new(Dialect::MySql, "shop");
    let p = Routine {
        name: "close_month".into(),
        kind: RoutineKind::Procedure,
        arguments: vec![
            Argument {
                name: Some("m".into()),
                data_type: "int".into(),
                mode: "IN".into(),
            },
            Argument {
                name: Some("closed".into()),
                data_type: "int".into(),
                mode: "OUT".into(),
            },
        ],
        ..Default::default()
    };
    assert_eq!(
        script(&m, Object::Routine(&p), ScriptKind::Call).unwrap(),
        "CALL `shop`.`close_month`(:m, @closed);\n"
    );
    assert_eq!(
        script(
            &m,
            Object::Index(
                &o,
                &Index {
                    name: "i".into(),
                    ..Default::default()
                }
            ),
            ScriptKind::Drop
        )
        .unwrap(),
        "DROP INDEX `i` ON `shop`.`orders`;\n"
    );
}

#[test]
fn quoting() {
    assert_eq!(quote(Dialect::Postgres, "orders"), "orders");
    assert_eq!(quote(Dialect::Postgres, "Orders"), "\"Orders\"");
    assert_eq!(quote(Dialect::Postgres, "user"), "\"user\"");
    assert_eq!(quote(Dialect::Postgres, "a\"b"), "\"a\"\"b\"");
    assert_eq!(quote(Dialect::MySql, "a`b"), "`a``b`");
}
