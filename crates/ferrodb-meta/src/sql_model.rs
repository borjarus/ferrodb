//! The metadata catalog as the SQL engine's schema model (completion,
//! inspections, navigation in consoles).

use ferrodb_lang::sql::schema::{
    ColumnInfo, ForeignKeyInfo, RoutineInfo, SchemaInfo, SchemaModel, TableInfo,
};

use crate::model::{Catalog, RoutineKind, Table};

fn table(t: &Table) -> TableInfo {
    let pk: &[String] = t.primary_key.as_ref().map_or(&[], |k| k.columns.as_slice());
    TableInfo {
        name: t.name.clone(),
        view: t.is_view(),
        columns: t
            .columns
            .iter()
            .map(|c| ColumnInfo {
                name: c.name.clone(),
                data_type: c.data_type.clone(),
                nullable: c.nullable,
                primary_key: pk.contains(&c.name),
                generated: c.identity.is_some()
                    || c.serial
                    || c.auto_increment
                    || c.generated.is_some(),
                comment: c.comment.clone(),
            })
            .collect(),
        foreign_keys: t
            .foreign_keys
            .iter()
            .map(|f| ForeignKeyInfo {
                columns: f.columns.clone(),
                ref_schema: f.ref_container.clone(),
                ref_table: f.ref_table.clone(),
                ref_columns: f.ref_columns.clone(),
            })
            .collect(),
        comment: t.comment.clone(),
    }
}

/// Schema model of a catalog. Containers are listed in the order of the
/// introspector (the connection's schema / database first), which becomes
/// the search path for unqualified names.
pub fn sql_model(catalog: &Catalog) -> SchemaModel {
    let containers = catalog.containers.as_deref().unwrap_or_default();
    let schemas = containers
        .iter()
        .map(|c| {
            let content = c.content.as_ref();
            SchemaInfo {
                name: c.name.clone(),
                loaded: content.is_some(),
                tables: content.map_or_else(Vec::new, |x| x.tables.iter().map(table).collect()),
                routines: content.map_or_else(Vec::new, |x| {
                    x.routines
                        .iter()
                        .map(|r| RoutineInfo {
                            name: r.name.clone(),
                            procedure: r.kind == RoutineKind::Procedure,
                            arguments: r
                                .arguments
                                .iter()
                                .filter(|a| a.is_input())
                                .map(|a| (a.name.clone(), a.data_type.clone()))
                                .collect(),
                            returns: r.returns.clone(),
                            comment: r.comment.clone(),
                        })
                        .collect()
                }),
            }
        })
        .collect();
    let search_path = containers.first().map(|c| vec![c.name.clone()]).unwrap_or_default();
    SchemaModel::new(schemas, search_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, Container, Content, ForeignKey, Key};

    #[test]
    fn catalog_becomes_a_model() {
        let mut cat = Catalog::new();
        cat.containers = Some(vec![
            Container {
                name: "public".into(),
                comment: None,
                content: Some(Content {
                    tables: vec![Table {
                        name: "orders".into(),
                        columns: vec![
                            Column {
                                name: "id".into(),
                                data_type: "integer".into(),
                                serial: true,
                                ..Default::default()
                            },
                            Column {
                                name: "customer_id".into(),
                                data_type: "integer".into(),
                                nullable: true,
                                ..Default::default()
                            },
                        ],
                        primary_key: Some(Key {
                            name: None,
                            columns: vec!["id".into()],
                        }),
                        foreign_keys: vec![ForeignKey {
                            columns: vec!["customer_id".into()],
                            ref_table: "customers".into(),
                            ref_columns: vec!["id".into()],
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            Container {
                name: "other".into(),
                comment: None,
                content: None,
            },
        ]);
        let m = sql_model(&cat);
        assert_eq!(m.search_path, vec!["public".to_owned()]);
        assert!(m.schemas[0].loaded && !m.schemas[1].loaded);
        let t = &m.schemas[0].tables[0];
        assert!(t.columns[0].primary_key && t.columns[0].generated);
        assert_eq!(t.foreign_keys[0].ref_table, "customers");
    }
}
