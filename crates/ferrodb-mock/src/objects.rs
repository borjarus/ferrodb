//! Synthetic object trees per data model / family.

use ferrodb_api::{
    DataModel, DbObject, HierarchyLevel, HierarchySpec, NodeRef, ObjectKind, PlanNode,
};

use crate::{Scenario, data};

use ObjectKind as K;

/// Shape of the explorer tree for a driver.
pub fn hierarchy(driver: &str, model: DataModel) -> HierarchySpec {
    let level = |kind, children: &[ObjectKind]| HierarchyLevel {
        kind,
        children: children.to_vec(),
    };
    match (model, driver) {
        (DataModel::KeyValue, _) => HierarchySpec {
            levels: vec![
                level(K::Database, &[K::KeyPrefix, K::Key]),
                level(K::KeyPrefix, &[K::KeyPrefix, K::Key]),
            ],
            key_separator: Some(":".into()),
        },
        (DataModel::Document, "couchbase") => HierarchySpec {
            levels: vec![
                level(K::Bucket, &[K::Scope]),
                level(K::Scope, &[K::Collection]),
                level(K::Collection, &[K::Index, K::Field]),
            ],
            key_separator: None,
        },
        (DataModel::Document, "dynamodb") => HierarchySpec {
            levels: vec![level(K::Table, &[K::Index, K::Field])],
            key_separator: None,
        },
        (DataModel::Document, _) => HierarchySpec {
            levels: vec![
                level(K::Database, &[K::Collection, K::View]),
                level(K::Collection, &[K::Index, K::Field]),
            ],
            key_separator: None,
        },
        (DataModel::Tabular, "cassandra") => HierarchySpec {
            levels: vec![
                level(K::Keyspace, &[K::Table, K::MaterializedView]),
                level(K::Table, &[K::Column, K::Index]),
            ],
            key_separator: None,
        },
        (DataModel::Tabular, "bigquery") => HierarchySpec {
            levels: vec![
                level(K::Project, &[K::Dataset]),
                level(K::Dataset, &[K::Table, K::View]),
                level(K::Table, &[K::Column]),
            ],
            key_separator: None,
        },
        (DataModel::Tabular, _) => HierarchySpec {
            levels: vec![
                level(K::Database, &[K::Schema]),
                level(K::Schema, &[K::Table, K::View, K::Function, K::Sequence]),
                level(K::Table, &[K::Column, K::Index, K::ForeignKey]),
            ],
            key_separator: None,
        },
    }
}

fn obj(
    parent: &NodeRef,
    kind: ObjectKind,
    name: impl Into<String>,
    detail: Option<String>,
) -> DbObject {
    let name = name.into();
    DbObject {
        node: parent.child(kind, name.clone()),
        kind,
        name,
        detail,
        has_children: kind.is_container(),
    }
}

const TABLES: &[&str] = &[
    "customers",
    "orders",
    "order_items",
    "products",
    "categories",
    "invoices",
    "payments",
    "shipments",
    "suppliers",
    "warehouses",
    "employees",
    "departments",
];

fn table_name(i: usize) -> String {
    let base = TABLES[i % TABLES.len()];
    if i < TABLES.len() {
        base.to_owned()
    } else {
        format!("{base}_{}", i / TABLES.len())
    }
}

/// Children of `node` for a source using `driver` with data `model`.
pub fn children(
    driver: &str,
    model: DataModel,
    scenario: Scenario,
    node: &NodeRef,
) -> Vec<DbObject> {
    let p = node;
    match (model, p.kind()) {
        // ---- key-value
        (DataModel::KeyValue, None) => (0..4)
            .map(|i| obj(p, K::Database, format!("db{i}"), None))
            .collect(),
        (DataModel::KeyValue, Some(K::Database)) => key_prefixes(p, scenario),
        (DataModel::KeyValue, Some(K::KeyPrefix)) => {
            // Group keys under this prefix by the next `:`-separated segment.
            let prefix: Vec<&str> = p.path.iter().skip(1).map(|(_, n)| n.as_str()).collect();
            let prefix = format!("{}:", prefix.join(":"));
            let mut prefixes: Vec<(String, usize)> = Vec::new();
            let mut keys = Vec::new();
            for k in (0..scenario.keys().min(50_000)).map(data::key) {
                let Some(rest) = k.key.strip_prefix(&prefix) else {
                    continue;
                };
                match rest.split_once(':') {
                    Some((seg, _)) => match prefixes.iter_mut().find(|(s, _)| s == seg) {
                        Some((_, n)) => *n += 1,
                        None => prefixes.push((seg.to_owned(), 1)),
                    },
                    None if keys.len() < 500 => keys.push(obj(
                        p,
                        K::Key,
                        rest.to_owned(),
                        Some(format!("{:?}", k.kind)),
                    )),
                    None => {}
                }
            }
            prefixes.sort();
            prefixes
                .into_iter()
                .map(|(s, n)| obj(p, K::KeyPrefix, s, Some(format!("{n} keys"))))
                .chain(keys)
                .collect()
        }
        // ---- documents
        (DataModel::Document, None) if driver == "couchbase" => ["travel-sample", "beer-sample"]
            .iter()
            .map(|n| obj(p, K::Bucket, *n, None))
            .collect(),
        (DataModel::Document, Some(K::Bucket)) => ["inventory", "tenant_agent_00", "_default"]
            .iter()
            .map(|n| obj(p, K::Scope, *n, None))
            .collect(),
        (DataModel::Document, Some(K::Scope)) => ["airline", "airport", "hotel", "route"]
            .iter()
            .map(|n| obj(p, K::Collection, *n, None))
            .collect(),
        (DataModel::Document, None) if driver == "dynamodb" => ["Orders", "Customers", "Sessions"]
            .iter()
            .map(|n| obj(p, K::Table, *n, Some("PAY_PER_REQUEST".into())))
            .collect(),
        (DataModel::Document, None) => ["catalog", "analytics", "admin"]
            .iter()
            .map(|n| obj(p, K::Database, *n, None))
            .collect(),
        (DataModel::Document, Some(K::Database)) => {
            let mut v: Vec<_> = ["customers", "products", "orders", "reviews", "events"]
                .iter()
                .map(|n| {
                    obj(
                        p,
                        K::Collection,
                        *n,
                        Some(format!("~{} docs", scenario.documents())),
                    )
                })
                .collect();
            v.push(obj(p, K::View, "active_customers", None));
            v
        }
        (DataModel::Document, Some(K::Collection | K::Table)) => {
            let mut v = vec![
                obj(p, K::Index, "_id_", Some("unique".into())),
                obj(p, K::Index, "name_1", None),
            ];
            for (f, t) in [
                ("_id", "objectId"),
                ("name", "string"),
                ("age", "int"),
                ("address", "object"),
                ("tags", "array"),
                ("balance", "double"),
                ("active", "bool"),
            ] {
                let mut o = obj(p, K::Field, f, Some(t.into()));
                o.has_children = t == "object";
                v.push(o);
            }
            v
        }
        // ---- tabular
        (DataModel::Tabular, None) if driver == "cassandra" => {
            ["events", "telemetry", "system_auth"]
                .iter()
                .map(|n| obj(p, K::Keyspace, *n, None))
                .collect()
        }
        (DataModel::Tabular, Some(K::Keyspace)) => {
            let mut v: Vec<_> = ["events_by_day", "events_by_user", "devices"]
                .iter()
                .map(|n| obj(p, K::Table, *n, None))
                .collect();
            v.push(obj(p, K::MaterializedView, "events_by_type", None));
            v
        }
        (DataModel::Tabular, None) if driver == "bigquery" => {
            vec![obj(p, K::Project, "acme-analytics", None)]
        }
        (DataModel::Tabular, Some(K::Project)) => ["sales", "marketing", "raw_events"]
            .iter()
            .map(|n| obj(p, K::Dataset, *n, None))
            .collect(),
        (DataModel::Tabular, Some(K::Dataset)) => (0..8)
            .map(|i| obj(p, K::Table, table_name(i), Some("partitioned".into())))
            .collect(),
        (DataModel::Tabular, None) => ["shop", "postgres"]
            .iter()
            .map(|n| obj(p, K::Database, *n, None))
            .collect(),
        (DataModel::Tabular, Some(K::Database)) => ["public", "audit", "reporting"]
            .iter()
            .map(|n| obj(p, K::Schema, *n, None))
            .collect(),
        (DataModel::Tabular, Some(K::Schema)) => {
            let n = scenario.tables_per_schema();
            let mut v: Vec<_> = (0..n)
                .map(|i| {
                    obj(
                        p,
                        K::Table,
                        table_name(i),
                        Some(format!("~{} rows", data::mix(i as u64) % 100_000)),
                    )
                })
                .collect();
            v.push(obj(p, K::View, "v_customer_orders", None));
            v.push(obj(
                p,
                K::Function,
                "calculate_total",
                Some("(order_id bigint) → numeric".into()),
            ));
            v.push(obj(p, K::Sequence, "orders_id_seq", None));
            v
        }
        (DataModel::Tabular, Some(K::Table | K::View | K::MaterializedView)) => {
            let schema = data::table_schema(scenario.table_columns());
            let mut v: Vec<_> = schema
                .fields()
                .iter()
                .map(|f| {
                    obj(
                        p,
                        K::Column,
                        f.name().clone(),
                        Some(format!("{}", f.data_type())),
                    )
                })
                .collect();
            if p.kind() == Some(K::Table) {
                v.push(obj(
                    p,
                    K::Index,
                    format!("{}_pkey", p.name().unwrap_or("t")),
                    Some("primary key".into()),
                ));
                if driver != "cassandra" {
                    v.push(obj(
                        p,
                        K::ForeignKey,
                        "fk_customer",
                        Some("→ customers(id)".into()),
                    ));
                }
            }
            v
        }
        _ => Vec::new(),
    }
}

/// A plausible execution plan for the mock (shape depends on the model).
pub fn plan(model: DataModel, text: &str) -> PlanNode {
    let node = |op: &str, detail: Option<&str>, cost: f64, rows: u64, children| PlanNode {
        operation: op.into(),
        detail: detail.map(str::to_owned),
        cost,
        rows,
        children,
    };
    let lower = text.to_ascii_lowercase();
    match model {
        DataModel::Document => node(
            "FETCH",
            None,
            120.0,
            200,
            vec![node("IXSCAN", Some("{ name: 1 }"), 40.0, 200, vec![])],
        ),
        DataModel::KeyValue => node("SCAN", Some("MATCH *"), 1.0, 1000, vec![]),
        DataModel::Tabular if lower.contains("join") => node(
            "Limit",
            None,
            1834.5,
            100,
            vec![node(
                "Hash Join",
                Some("o.customer_id = c.id"),
                1830.2,
                12_400,
                vec![
                    node("Seq Scan", Some("orders o"), 1120.0, 48_000, vec![]),
                    node(
                        "Hash",
                        None,
                        410.0,
                        10_000,
                        vec![node(
                            "Index Scan",
                            Some("customers_pkey on customers c"),
                            380.0,
                            10_000,
                            vec![],
                        )],
                    ),
                ],
            )],
        ),
        DataModel::Tabular => node(
            "Sort",
            Some("id"),
            95.3,
            1000,
            vec![node("Seq Scan", Some("customers"), 22.5, 1000, vec![])],
        ),
    }
}

fn key_prefixes(p: &NodeRef, _scenario: Scenario) -> Vec<DbObject> {
    let mut v = vec![obj(p, K::KeyPrefix, "app", Some("prefix".into()))];
    v.push(obj(p, K::Key, "config", Some("String".into())));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrodb_api::DataSourceId;

    #[test]
    fn postgres_tree_reaches_columns() {
        let root = NodeRef::root(DataSourceId::new("x"));
        let dbs = children("postgresql", DataModel::Tabular, Scenario::Normal, &root);
        let schemas = children(
            "postgresql",
            DataModel::Tabular,
            Scenario::Normal,
            &dbs[0].node,
        );
        let tables = children(
            "postgresql",
            DataModel::Tabular,
            Scenario::Normal,
            &schemas[0].node,
        );
        let cols = children(
            "postgresql",
            DataModel::Tabular,
            Scenario::Normal,
            &tables[0].node,
        );
        assert!(cols.iter().any(|c| c.kind == K::Column));
    }

    #[test]
    fn ten_thousand_tables_scenario() {
        let root = NodeRef::root(DataSourceId::new("x"));
        let schema = root.child(K::Database, "shop").child(K::Schema, "public");
        let tables = children(
            "postgresql",
            DataModel::Tabular,
            Scenario::TenThousandTables,
            &schema,
        );
        assert!(tables.iter().filter(|t| t.kind == K::Table).count() == 10_000);
    }

    fn root() -> NodeRef {
        NodeRef::root(DataSourceId::new("x"))
    }

    fn names(v: &[DbObject]) -> Vec<&str> {
        v.iter().map(|o| o.name.as_str()).collect()
    }

    fn kinds(levels: &HierarchySpec) -> Vec<ObjectKind> {
        levels.levels.iter().map(|l| l.kind).collect()
    }

    // ---- hierarchy

    #[test]
    fn hierarchy_key_value_uses_colon_separator_for_any_driver() {
        for driver in ["redis", "valkey"] {
            let spec = hierarchy(driver, DataModel::KeyValue);
            assert_eq!(spec.key_separator.as_deref(), Some(":"));
            assert_eq!(kinds(&spec), [K::Database, K::KeyPrefix]);
            assert_eq!(spec.levels[0].children, [K::KeyPrefix, K::Key]);
        }
    }

    #[test]
    fn hierarchy_document_variants() {
        let cb = hierarchy("couchbase", DataModel::Document);
        assert_eq!(kinds(&cb), [K::Bucket, K::Scope, K::Collection]);
        assert_eq!(cb.key_separator, None);
        assert_eq!(cb.levels[2].children, [K::Index, K::Field]);

        let ddb = hierarchy("dynamodb", DataModel::Document);
        assert_eq!(kinds(&ddb), [K::Table]);

        let mongo = hierarchy("mongodb", DataModel::Document);
        assert_eq!(kinds(&mongo), [K::Database, K::Collection]);
        assert_eq!(mongo.levels[0].children, [K::Collection, K::View]);
        assert_eq!(mongo.levels[1].children, [K::Index, K::Field]);
    }

    #[test]
    fn hierarchy_tabular_variants() {
        let cass = hierarchy("cassandra", DataModel::Tabular);
        assert_eq!(kinds(&cass), [K::Keyspace, K::Table]);
        assert_eq!(cass.levels[0].children, [K::Table, K::MaterializedView]);
        assert_eq!(cass.levels[1].children, [K::Column, K::Index]);

        let bq = hierarchy("bigquery", DataModel::Tabular);
        assert_eq!(kinds(&bq), [K::Project, K::Dataset, K::Table]);

        let pg = hierarchy("postgresql", DataModel::Tabular);
        assert_eq!(kinds(&pg), [K::Database, K::Schema, K::Table]);
        assert_eq!(pg.levels[2].children, [K::Column, K::Index, K::ForeignKey]);
        assert_eq!(pg.key_separator, None);
    }

    // ---- children: key-value

    #[test]
    fn key_value_root_has_four_databases() {
        let dbs = children("redis", DataModel::KeyValue, Scenario::Normal, &root());
        assert_eq!(names(&dbs), ["db0", "db1", "db2", "db3"]);
        assert!(dbs.iter().all(|d| d.kind == K::Database && d.has_children));
    }

    #[test]
    fn key_value_database_lists_prefix_and_config_key() {
        let db = root().child(K::Database, "db0");
        let v = children("redis", DataModel::KeyValue, Scenario::Normal, &db);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].kind, K::KeyPrefix);
        assert_eq!(v[0].name, "app");
        assert_eq!(v[0].detail.as_deref(), Some("prefix"));
        assert_eq!(v[1].kind, K::Key);
        assert_eq!(v[1].name, "config");
        assert_eq!(v[1].detail.as_deref(), Some("String"));
        assert!(!v[1].has_children);
    }

    #[test]
    fn key_prefix_groups_all_keys_by_next_segment() {
        let app = root().child(K::Database, "db0").child(K::KeyPrefix, "app");
        let v = children("redis", DataModel::KeyValue, Scenario::Normal, &app);

        // Every generated key starts with `app:`, so every key is counted once.
        assert!(v.iter().all(|o| o.kind == K::KeyPrefix));
        let total: u64 = v
            .iter()
            .map(|o| {
                o.detail
                    .as_deref()
                    .and_then(|d| d.strip_suffix(" keys"))
                    .and_then(|n| n.parse::<u64>().ok())
                    .expect("detail is `<n> keys`")
            })
            .sum();
        assert_eq!(total, Scenario::Normal.keys());

        assert_eq!(
            names(&v),
            ["events", "leaderboard", "queue", "session", "tags", "user"]
        );
    }

    #[test]
    fn key_prefix_drills_down_to_leaf_keys() {
        let session_key = (0..)
            .map(data::key)
            .find(|e| e.key.starts_with("app:session:"))
            .expect("a session key exists");
        let bucket = session_key.key.split(':').nth(2).expect("bucket segment");

        let node = root()
            .child(K::Database, "db0")
            .child(K::KeyPrefix, "app")
            .child(K::KeyPrefix, "session")
            .child(K::KeyPrefix, bucket);
        let leaves = children("redis", DataModel::KeyValue, Scenario::Normal, &node);

        assert!(!leaves.is_empty());
        assert!(leaves.len() <= 500);
        for leaf in &leaves {
            assert_eq!(leaf.kind, K::Key);
            assert!(!leaf.has_children);
            assert_eq!(leaf.detail.as_deref(), Some("String"));
            assert!(leaf.node.name().expect("named").parse::<u64>().is_ok());
        }
    }

    #[test]
    fn key_prefix_with_no_matches_is_empty() {
        let node = root().child(K::Database, "db0").child(K::KeyPrefix, "nope");
        assert!(children("redis", DataModel::KeyValue, Scenario::Normal, &node).is_empty());
    }

    // ---- children: documents

    #[test]
    fn couchbase_walks_bucket_scope_collection() {
        let buckets = children("couchbase", DataModel::Document, Scenario::Normal, &root());
        assert_eq!(names(&buckets), ["travel-sample", "beer-sample"]);
        assert!(buckets.iter().all(|b| b.kind == K::Bucket));

        let scopes = children(
            "couchbase",
            DataModel::Document,
            Scenario::Normal,
            &buckets[0].node,
        );
        assert_eq!(names(&scopes), ["inventory", "tenant_agent_00", "_default"]);
        assert!(scopes.iter().all(|s| s.kind == K::Scope));

        let colls = children(
            "couchbase",
            DataModel::Document,
            Scenario::Normal,
            &scopes[0].node,
        );
        assert_eq!(names(&colls), ["airline", "airport", "hotel", "route"]);
        assert!(colls.iter().all(|c| c.kind == K::Collection));

        let members = children(
            "couchbase",
            DataModel::Document,
            Scenario::Normal,
            &colls[0].node,
        );
        assert!(
            members
                .iter()
                .any(|o| o.kind == K::Index && o.name == "_id_")
        );
        assert!(members.iter().any(|o| o.kind == K::Field));
    }

    #[test]
    fn dynamodb_root_lists_tables_with_billing_mode() {
        let tables = children("dynamodb", DataModel::Document, Scenario::Normal, &root());
        assert_eq!(names(&tables), ["Orders", "Customers", "Sessions"]);
        assert!(
            tables
                .iter()
                .all(|t| { t.kind == K::Table && t.detail.as_deref() == Some("PAY_PER_REQUEST") })
        );
    }

    #[test]
    fn document_root_lists_databases() {
        let dbs = children("mongodb", DataModel::Document, Scenario::Normal, &root());
        assert_eq!(names(&dbs), ["catalog", "analytics", "admin"]);
        assert!(dbs.iter().all(|d| d.kind == K::Database));
    }

    #[test]
    fn document_database_lists_collections_and_view_with_doc_count() {
        let db = root().child(K::Database, "catalog");

        let normal = children("mongodb", DataModel::Document, Scenario::Normal, &db);
        assert_eq!(
            names(&normal),
            [
                "customers",
                "products",
                "orders",
                "reviews",
                "events",
                "active_customers"
            ]
        );
        assert_eq!(normal[0].kind, K::Collection);
        assert_eq!(normal[0].detail.as_deref(), Some("~200 docs"));
        assert_eq!(normal[5].kind, K::View);
        assert_eq!(normal[5].detail, None);

        let big = children(
            "mongodb",
            DataModel::Document,
            Scenario::HundredThousandDocuments,
            &db,
        );
        assert_eq!(big[0].detail.as_deref(), Some("~100000 docs"));
    }

    #[test]
    fn document_collection_exposes_indexes_and_typed_fields() {
        let coll = root()
            .child(K::Database, "catalog")
            .child(K::Collection, "customers");
        let v = children("mongodb", DataModel::Document, Scenario::Normal, &coll);

        let indexes: Vec<&DbObject> = v.iter().filter(|o| o.kind == K::Index).collect();
        assert_eq!(indexes.len(), 2);
        assert_eq!(indexes[0].name, "_id_");
        assert_eq!(indexes[0].detail.as_deref(), Some("unique"));
        assert!(
            indexes[0].has_children,
            "indexes are containers per is_container"
        );

        let fields: Vec<&DbObject> = v.iter().filter(|o| o.kind == K::Field).collect();
        assert_eq!(
            fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["_id", "name", "age", "address", "tags", "balance", "active"]
        );
        assert_eq!(fields[2].detail.as_deref(), Some("int"));
        assert_eq!(fields[3].detail.as_deref(), Some("object"));
        // Only the nested `address` object can be expanded.
        for f in &fields {
            assert_eq!(f.has_children, f.name == "address", "field {}", f.name);
        }
    }

    // ---- children: tabular

    #[test]
    fn cassandra_keyspace_tables_and_materialized_view() {
        let keyspaces = children("cassandra", DataModel::Tabular, Scenario::Normal, &root());
        assert_eq!(names(&keyspaces), ["events", "telemetry", "system_auth"]);

        let ks = children(
            "cassandra",
            DataModel::Tabular,
            Scenario::Normal,
            &keyspaces[0].node,
        );
        assert_eq!(
            names(&ks),
            [
                "events_by_day",
                "events_by_user",
                "devices",
                "events_by_type"
            ]
        );
        assert_eq!(ks[3].kind, K::MaterializedView);

        let cols = children(
            "cassandra",
            DataModel::Tabular,
            Scenario::Normal,
            &ks[0].node,
        );
        assert_eq!(cols.iter().filter(|o| o.kind == K::Column).count(), 12);
        assert!(
            cols.iter()
                .any(|o| o.kind == K::Index && o.name == "events_by_day_pkey")
        );
        // Cassandra has no foreign keys.
        assert!(cols.iter().all(|o| o.kind != K::ForeignKey));
    }

    #[test]
    fn bigquery_walks_project_dataset_table() {
        let projects = children("bigquery", DataModel::Tabular, Scenario::Normal, &root());
        assert_eq!(names(&projects), ["acme-analytics"]);

        let datasets = children(
            "bigquery",
            DataModel::Tabular,
            Scenario::Normal,
            &projects[0].node,
        );
        assert_eq!(names(&datasets), ["sales", "marketing", "raw_events"]);

        let tables = children(
            "bigquery",
            DataModel::Tabular,
            Scenario::Normal,
            &datasets[0].node,
        );
        assert_eq!(tables.len(), 8);
        assert!(
            tables
                .iter()
                .all(|t| t.detail.as_deref() == Some("partitioned"))
        );

        let cols = children(
            "bigquery",
            DataModel::Tabular,
            Scenario::Normal,
            &tables[0].node,
        );
        assert_eq!(cols.iter().filter(|o| o.kind == K::Column).count(), 12);
    }

    #[test]
    fn postgres_root_and_database_levels() {
        let dbs = children("postgresql", DataModel::Tabular, Scenario::Normal, &root());
        assert_eq!(names(&dbs), ["shop", "postgres"]);

        let schemas = children(
            "postgresql",
            DataModel::Tabular,
            Scenario::Normal,
            &dbs[0].node,
        );
        assert_eq!(names(&schemas), ["public", "audit", "reporting"]);
        assert!(schemas.iter().all(|s| s.kind == K::Schema));
    }

    #[test]
    fn schema_lists_cycled_table_names_and_routines() {
        let schema = root().child(K::Database, "shop").child(K::Schema, "public");
        let v = children("postgresql", DataModel::Tabular, Scenario::Normal, &schema);

        let tables: Vec<&str> = v
            .iter()
            .filter(|o| o.kind == K::Table)
            .map(|o| o.name.as_str())
            .collect();
        assert_eq!(tables.len(), 24);
        assert_eq!(tables[0], "customers");
        assert_eq!(tables[11], "departments");
        assert_eq!(tables[12], "customers_1");
        assert_eq!(tables[23], "departments_1");
        for t in &v {
            if t.kind == K::Table {
                let d = t.detail.as_deref().expect("row count detail");
                assert!(d.starts_with('~') && d.ends_with(" rows"), "{d}");
                assert!(t.has_children);
            }
        }

        let view = v.iter().find(|o| o.kind == K::View).expect("view");
        assert_eq!(view.name, "v_customer_orders");

        let func = v.iter().find(|o| o.kind == K::Function).expect("function");
        assert_eq!(func.detail.as_deref(), Some("(order_id bigint) → numeric"));

        let seq = v.iter().find(|o| o.kind == K::Sequence).expect("sequence");
        assert_eq!(seq.name, "orders_id_seq");
        assert!(!seq.has_children);
    }

    #[test]
    fn schema_with_ten_thousand_tables_still_cycles_names() {
        let schema = root().child(K::Database, "shop").child(K::Schema, "public");
        let v = children(
            "postgresql",
            DataModel::Tabular,
            Scenario::TenThousandTables,
            &schema,
        );
        let tables: Vec<&DbObject> = v.iter().filter(|o| o.kind == K::Table).collect();
        assert_eq!(tables[9_999].name, table_name(9_999));
        assert_eq!(tables[12].name, "customers_1");
    }

    #[test]
    fn table_children_include_pk_and_fk_unless_cassandra() {
        let table = root()
            .child(K::Database, "shop")
            .child(K::Schema, "public")
            .child(K::Table, "orders");
        let pg = children("postgresql", DataModel::Tabular, Scenario::Normal, &table);
        assert!(
            pg.iter()
                .any(|o| o.kind == K::Index && o.name == "orders_pkey")
        );
        assert!(
            pg.iter()
                .any(|o| o.kind == K::ForeignKey && o.name == "fk_customer")
        );
        assert_eq!(pg.iter().filter(|o| o.kind == K::Column).count(), 12);

        let cass = children("cassandra", DataModel::Tabular, Scenario::Normal, &table);
        assert!(cass.iter().all(|o| o.kind != K::ForeignKey));
    }

    #[test]
    fn view_children_are_columns_only() {
        let view = root()
            .child(K::Database, "shop")
            .child(K::Schema, "public")
            .child(K::View, "v_customer_orders");
        let v = children("postgresql", DataModel::Tabular, Scenario::Normal, &view);
        assert!(!v.is_empty());
        assert!(v.iter().all(|o| o.kind == K::Column && !o.has_children));
    }

    #[test]
    fn million_rows_scenario_widens_tables_to_fifty_columns() {
        let table = root()
            .child(K::Database, "shop")
            .child(K::Schema, "public")
            .child(K::Table, "orders");
        let v = children(
            "postgresql",
            DataModel::Tabular,
            Scenario::MillionRows,
            &table,
        );
        assert_eq!(v.iter().filter(|o| o.kind == K::Column).count(), 50);
    }

    #[test]
    fn column_details_show_arrow_types() {
        let table = root()
            .child(K::Database, "shop")
            .child(K::Schema, "public")
            .child(K::Table, "orders");
        let v = children("postgresql", DataModel::Tabular, Scenario::Normal, &table);
        let cols: Vec<&DbObject> = v.iter().filter(|o| o.kind == K::Column).collect();
        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[0].detail.as_deref(), Some("Int64"));
        assert_eq!(cols[1].name, "name");
        assert_eq!(cols[1].detail.as_deref(), Some("Utf8"));
    }

    // ---- children: leaves and object construction

    #[test]
    fn leaf_nodes_have_no_children() {
        let column = root()
            .child(K::Database, "shop")
            .child(K::Schema, "public")
            .child(K::Table, "customers")
            .child(K::Column, "id");
        assert!(children("postgresql", DataModel::Tabular, Scenario::Normal, &column).is_empty());

        let key = root().child(K::Database, "db0").child(K::Key, "config");
        assert!(children("redis", DataModel::KeyValue, Scenario::Normal, &key).is_empty());
    }

    #[test]
    fn child_objects_extend_parent_path_and_follow_container_rule() {
        let db = root().child(K::Database, "db0");
        let v = children("redis", DataModel::KeyValue, Scenario::Normal, &db);
        for o in &v {
            assert_eq!(
                o.node.path.last().map(|(_, n)| n.as_str()),
                Some(o.name.as_str())
            );
            assert_eq!(o.node.path.len(), db.path.len() + 1);
            assert_eq!(o.node.kind(), Some(o.kind));
            assert_eq!(o.has_children, o.kind.is_container());
        }
    }

    // ---- plan

    #[test]
    fn plan_document_is_fetch_over_index_scan() {
        let p = plan(DataModel::Document, "");
        assert_eq!(p.operation, "FETCH");
        assert_eq!(p.rows, 200);
        assert_eq!(p.children.len(), 1);
        let ix = &p.children[0];
        assert_eq!(ix.operation, "IXSCAN");
        assert_eq!(ix.detail.as_deref(), Some("{ name: 1 }"));
        assert!(ix.children.is_empty());
    }

    #[test]
    fn plan_key_value_is_single_scan() {
        let p = plan(DataModel::KeyValue, "anything");
        assert_eq!(p.operation, "SCAN");
        assert_eq!(p.detail.as_deref(), Some("MATCH *"));
        assert!(p.children.is_empty());
    }

    #[test]
    fn plan_tabular_join_is_detected_case_insensitively() {
        let p = plan(
            DataModel::Tabular,
            "SELECT * FROM orders o JOIN customers c ON o.customer_id = c.id",
        );
        assert_eq!(p.operation, "Limit");
        let join = &p.children[0];
        assert_eq!(join.operation, "Hash Join");
        assert_eq!(join.children.len(), 2);
        assert_eq!(join.children[0].operation, "Seq Scan");
        // Costs accumulate towards the root.
        assert!(p.cost >= join.cost && join.cost >= join.children[0].cost);
    }

    #[test]
    fn plan_tabular_without_join_is_sort_over_seq_scan() {
        let p = plan(DataModel::Tabular, "select 1");
        assert_eq!(p.operation, "Sort");
        assert_eq!(p.detail.as_deref(), Some("id"));
        assert_eq!(p.rows, 1000);
        assert_eq!(p.children.len(), 1);
        assert_eq!(p.children[0].operation, "Seq Scan");
        assert_eq!(p.children[0].detail.as_deref(), Some("customers"));
    }
}
