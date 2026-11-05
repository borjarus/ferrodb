//! Synthetic object trees per data model / family.

use ferrodb_api::{DataModel, DbObject, HierarchyLevel, HierarchySpec, NodeRef, ObjectKind};

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
            let prefix: Vec<&str> = p.path.iter().skip(1).map(|(_, n)| n.as_str()).collect();
            let prefix = prefix.join(":");
            (0..scenario.keys().min(50_000))
                .map(data::key)
                .filter(|k| k.key.starts_with(&prefix))
                .take(500)
                .map(|k| obj(p, K::Key, k.key.clone(), Some(format!("{:?}", k.kind))))
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
}
