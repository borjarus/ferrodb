//! Schema tools of the mock (phase 7): the explorer objects of a schema as
//! a `ferrodb-meta` model, so ER diagrams, schema comparison and the table
//! editor run the real generators on mock data.
//!
//! Every table except `customers` gets a `customer_id` column referencing
//! `customers(id)`. Sources whose id contains `prod` lag one column behind
//! (the last one of each table), so comparing development with production
//! shows differences.

use ferrodb_api::{DataModel, NodeRef, ObjectKind as K};
use ferrodb_meta::{Column, Content, Dialect, ForeignKey, Index, Key, Table, TableKind};

use crate::{objects, scenario::Scenario};

/// Arrow type names of the mock columns → SQL types.
fn sql_type(arrow: &str) -> String {
    match arrow {
        "Int64" => "bigint".into(),
        "Int32" => "integer".into(),
        "Float64" => "double precision".into(),
        "Boolean" => "boolean".into(),
        "Utf8" => "text".into(),
        s if s.starts_with("Timestamp") => "timestamp".into(),
        s => s.to_ascii_lowercase(),
    }
}

pub fn dialect(driver: &str) -> Dialect {
    match driver {
        "mysql" => Dialect::MySql,
        "mariadb" => Dialect::MariaDb,
        "sqlite" => Dialect::Sqlite,
        _ => Dialect::Postgres,
    }
}

/// The schema `container` (or the schema of a table node) as a model.
pub fn content(driver: &str, scenario: Scenario, node: &NodeRef) -> (String, Content) {
    // Container = the node itself, or its parent for a table.
    let mut container = node.clone();
    while container
        .kind()
        .is_some_and(|k| !matches!(k, K::Schema | K::Database | K::Keyspace | K::Dataset))
    {
        container.path.pop();
    }
    let name = container.name().unwrap_or("public").to_owned();
    let lagging = node.data_source.as_str().contains("prod");
    let objs = objects::children(driver, DataModel::Tabular, scenario, &container);
    let mut tables = Vec::new();
    for o in objs.iter().filter(|o| matches!(o.kind, K::Table | K::View)) {
        let cols = objects::children(driver, DataModel::Tabular, scenario, &o.node);
        let mut columns: Vec<Column> = cols
            .iter()
            .filter(|c| c.kind == K::Column)
            .map(|c| Column {
                name: c.name.clone(),
                data_type: sql_type(c.detail.as_deref().unwrap_or("text")),
                nullable: c.name != "id",
                ..Default::default()
            })
            .collect();
        let view = o.kind == K::View;
        let mut t = Table {
            name: o.name.clone(),
            kind: if view {
                TableKind::View
            } else {
                TableKind::Table
            },
            ..Default::default()
        };
        if view {
            t.view_definition = Some(
                "SELECT c.id, c.name, count(o.id) AS orders FROM customers c \
                 LEFT JOIN orders o ON o.customer_id = c.id GROUP BY c.id, c.name"
                    .into(),
            );
            t.columns = columns;
            tables.push(t);
            continue;
        }
        if lagging && columns.len() > 1 {
            columns.pop();
        }
        if o.name != "customers" {
            columns.insert(
                1,
                Column {
                    name: "customer_id".into(),
                    data_type: "bigint".into(),
                    nullable: true,
                    ..Default::default()
                },
            );
            t.foreign_keys.push(ForeignKey {
                name: Some(format!("{}_customer_fk", o.name)),
                columns: vec!["customer_id".into()],
                ref_table: "customers".into(),
                ref_columns: vec!["id".into()],
                ..Default::default()
            });
            t.indexes.push(Index {
                name: format!("ix_{}_customer", o.name),
                columns: vec!["customer_id".into()],
                ..Default::default()
            });
        }
        t.columns = columns;
        t.primary_key = Some(Key {
            name: Some(format!("{}_pkey", o.name)),
            columns: vec!["id".into()],
        });
        tables.push(t);
    }
    (
        name,
        Content {
            tables,
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_schema_has_relations_and_prod_lags() {
        let dev = NodeRef::root("pg-dev".into())
            .child(K::Database, "shop")
            .child(K::Schema, "public");
        let (name, c) = content("postgresql", Scenario::Normal, &dev);
        assert_eq!(name, "public");
        let orders = c.table("orders").unwrap();
        assert_eq!(orders.foreign_keys[0].ref_table, "customers");
        assert!(c.table("v_customer_orders").unwrap().is_view());
        let prod = NodeRef::root("pg-prod".into())
            .child(K::Database, "shop")
            .child(K::Schema, "public");
        let (_, p) = content("postgresql", Scenario::Normal, &prod);
        assert_eq!(
            p.table("orders").unwrap().columns.len() + 1,
            orders.columns.len()
        );
        // A table node resolves to its schema.
        let (n, _) = content(
            "postgresql",
            Scenario::Normal,
            &dev.child(K::Table, "orders"),
        );
        assert_eq!(n, "public");
    }
}
