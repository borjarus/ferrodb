//! ADBC metadata calls as flat result sets. `GetObjects` returns one
//! deeply nested row per catalog (lists of structs of lists…); the
//! explorer and the introspectors read flat rows of text, so the nesting is
//! unrolled here: one row per catalog / schema / table / column, depending
//! on the depth asked for.

use std::sync::Arc;

use arrow_array::{
    Array, ArrayRef, BooleanArray, Int16Array, Int32Array, Int64Array, LargeStringArray, ListArray,
    RecordBatch, StringArray, StringViewArray, StructArray, UInt32Array, UnionArray,
};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};

/// Columns of the flattened `GetObjects` result.
pub const OBJECT_COLUMNS: [&str; 9] = [
    "catalog_name",
    "db_schema_name",
    "table_name",
    "table_type",
    "column_name",
    "ordinal_position",
    "type_name",
    "is_nullable",
    "remarks",
];

/// A text table: `names` and rows of optional text.
pub fn text_batch(names: &[&str], rows: &[Vec<Option<String>>]) -> RecordBatch {
    let schema: SchemaRef = Arc::new(Schema::new(
        names
            .iter()
            .map(|n| Field::new(*n, DataType::Utf8, true))
            .collect::<Vec<_>>(),
    ));
    let columns: Vec<ArrayRef> = (0..names.len())
        .map(|i| {
            Arc::new(
                rows.iter()
                    .map(|r| r.get(i).cloned().flatten())
                    .collect::<StringArray>(),
            ) as ArrayRef
        })
        .collect();
    RecordBatch::try_new(schema, columns).expect("columns match the schema")
}

/// Text of a scalar cell (strings, integers, booleans).
pub fn cell_text(a: &dyn Array, i: usize) -> Option<String> {
    if a.is_null(i) {
        return None;
    }
    let any = a.as_any();
    if let Some(x) = any.downcast_ref::<StringArray>() {
        return Some(x.value(i).to_owned());
    }
    if let Some(x) = any.downcast_ref::<LargeStringArray>() {
        return Some(x.value(i).to_owned());
    }
    if let Some(x) = any.downcast_ref::<StringViewArray>() {
        return Some(x.value(i).to_owned());
    }
    if let Some(x) = any.downcast_ref::<Int16Array>() {
        return Some(x.value(i).to_string());
    }
    if let Some(x) = any.downcast_ref::<Int32Array>() {
        return Some(x.value(i).to_string());
    }
    if let Some(x) = any.downcast_ref::<Int64Array>() {
        return Some(x.value(i).to_string());
    }
    if let Some(x) = any.downcast_ref::<UInt32Array>() {
        return Some(x.value(i).to_string());
    }
    if let Some(x) = any.downcast_ref::<BooleanArray>() {
        return Some(x.value(i).to_string());
    }
    if let Some(x) = any.downcast_ref::<ListArray>() {
        let v = x.value(i);
        let items: Vec<String> = (0..v.len()).filter_map(|j| cell_text(&v, j)).collect();
        return Some(items.join(", "));
    }
    if let Some(u) = any.downcast_ref::<UnionArray>() {
        let child = u.child(u.type_id(i));
        return cell_text(child.as_ref(), u.value_offset(i));
    }
    None
}

fn field<'a>(s: &'a StructArray, name: &str) -> Option<&'a ArrayRef> {
    s.column_by_name(name)
}

fn text(s: &StructArray, name: &str, i: usize) -> Option<String> {
    field(s, name).and_then(|a| cell_text(a.as_ref(), i))
}

/// The struct items of list `l` at row `i`: (struct array, index range).
fn items(l: &dyn Array, i: usize) -> Option<(StructArray, std::ops::Range<usize>)> {
    let l = l.as_any().downcast_ref::<ListArray>()?;
    if l.is_null(i) {
        return None;
    }
    let offsets = l.value_offsets();
    let (a, b) = (offsets[i] as usize, offsets[i + 1] as usize);
    let s = l.values().as_any().downcast_ref::<StructArray>()?.clone();
    Some((s, a..b))
}

/// Unrolls `GetObjects` batches into rows of [`OBJECT_COLUMNS`].
pub fn flatten_objects(
    batches: impl Iterator<Item = Result<RecordBatch, ArrowError>>,
) -> Result<Vec<Vec<Option<String>>>, ArrowError> {
    let mut rows = Vec::new();
    for b in batches {
        let b = b?;
        let s = StructArray::from(b);
        for ci in 0..s.len() {
            let catalog = text(&s, "catalog_name", ci);
            let schemas = field(&s, "catalog_db_schemas").and_then(|a| items(a.as_ref(), ci));
            let Some((ss, range)) = schemas else {
                rows.push(vec![
                    catalog.clone(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ]);
                continue;
            };
            for si in range {
                let schema = text(&ss, "db_schema_name", si);
                let tables = field(&ss, "db_schema_tables").and_then(|a| items(a.as_ref(), si));
                let Some((ts, trange)) = tables else {
                    rows.push(vec![
                        catalog.clone(),
                        schema.clone(),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    ]);
                    continue;
                };
                for ti in trange {
                    let table = text(&ts, "table_name", ti);
                    let ttype = text(&ts, "table_type", ti);
                    let cols = field(&ts, "table_columns").and_then(|a| items(a.as_ref(), ti));
                    let Some((cs, crange)) = cols else {
                        rows.push(vec![
                            catalog.clone(),
                            schema.clone(),
                            table.clone(),
                            ttype.clone(),
                            None,
                            None,
                            None,
                            None,
                            None,
                        ]);
                        continue;
                    };
                    for k in crange {
                        rows.push(vec![
                            catalog.clone(),
                            schema.clone(),
                            table.clone(),
                            ttype.clone(),
                            text(&cs, "column_name", k),
                            text(&cs, "ordinal_position", k),
                            text(&cs, "xdbc_type_name", k),
                            text(&cs, "xdbc_is_nullable", k),
                            text(&cs, "remarks", k),
                        ]);
                    }
                }
            }
        }
    }
    Ok(rows)
}

/// Names of the standard `GetInfo` codes.
pub fn info_name(code: u32) -> String {
    match code {
        0 => "vendor_name".into(),
        1 => "vendor_version".into(),
        2 => "vendor_arrow_version".into(),
        3 => "vendor_sql".into(),
        4 => "vendor_substrait".into(),
        100 => "driver_name".into(),
        101 => "driver_version".into(),
        102 => "driver_arrow_version".into(),
        103 => "driver_adbc_version".into(),
        other => other.to_string(),
    }
}

/// `GetInfo` batches → (name, value) rows.
pub fn flatten_info(
    batches: impl Iterator<Item = Result<RecordBatch, ArrowError>>,
) -> Result<Vec<Vec<Option<String>>>, ArrowError> {
    let mut rows = Vec::new();
    for b in batches {
        let b = b?;
        let (Some(names), Some(values)) = (
            b.column_by_name("info_name"),
            b.column_by_name("info_value"),
        ) else {
            continue;
        };
        for i in 0..b.num_rows() {
            let code = names
                .as_any()
                .downcast_ref::<UInt32Array>()
                .map(|a| a.value(i));
            rows.push(vec![code.map(info_name), cell_text(values.as_ref(), i)]);
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::builder::{ListBuilder, StringBuilder, StructBuilder};
    use arrow_schema::Fields;

    #[test]
    fn flattens_schema_level_objects() {
        // catalog_name, catalog_db_schemas: list<struct<db_schema_name>>
        // (a driver may leave the deeper levels out at `schemas` depth).
        let schema_fields = Fields::from(vec![Field::new("db_schema_name", DataType::Utf8, true)]);
        let mut schemas = ListBuilder::new(StructBuilder::from_fields(schema_fields.clone(), 2));
        {
            let sb = schemas.values();
            sb.field_builder::<StringBuilder>(0)
                .unwrap()
                .append_value("ds1");
            sb.append(true);
            sb.field_builder::<StringBuilder>(0)
                .unwrap()
                .append_value("ds2");
            sb.append(true);
        }
        schemas.append(true);
        let schemas = schemas.finish();
        let batch = RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Field::new("catalog_name", DataType::Utf8, true),
                Field::new("catalog_db_schemas", schemas.data_type().clone(), true),
            ])),
            vec![
                Arc::new(StringArray::from(vec!["project"])),
                Arc::new(schemas),
            ],
        )
        .unwrap();
        let rows = flatten_objects(std::iter::once(Ok(batch))).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0].as_deref(), Some("project"));
        assert_eq!(rows[1][1].as_deref(), Some("ds2"));
        assert_eq!(rows[1][2], None);
        let b = text_batch(&OBJECT_COLUMNS, &rows);
        assert_eq!((b.num_rows(), b.num_columns()), (2, 9));
    }
}
