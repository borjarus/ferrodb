//! Fuzzing harness (plan, phase 10.2 and section 9): JSON → BSON
//! (Extended JSON) conversion, typed cell values and the import readers on
//! arbitrary input. The cargo-fuzz targets in `fuzz/` and the stable
//! proptest smoke tests in `tests/fuzz_smoke.rs` call the same functions.
//!
//! Not part of the public API.

use bson::{Bson, Document};
use ferrodb_api::{DataFormat, FormatOptions};

use crate::{
    doc_edit,
    docs::{document_to_row, infer_columns, lookup, row_to_document},
    import::{self, Mapping},
    value::{self, Value, ValueType},
};

/// Every target type of a conversion.
pub const VALUE_TYPES: &[ValueType] = &[
    ValueType::Bool,
    ValueType::Int,
    ValueType::Float,
    ValueType::Decimal,
    ValueType::Text,
    ValueType::Bytes,
    ValueType::Date,
    ValueType::Time,
    ValueType::Timestamp { tz: false },
    ValueType::Timestamp { tz: true },
    ValueType::Json,
    ValueType::Any,
];

/// JSON text (relaxed or canonical Extended JSON) → BSON, as the document
/// view, the import and the edit dialogs do it; then every conversion of
/// the result. Canonical Extended JSON must round-trip exactly.
pub fn ext_json(data: &[u8]) {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    let Ok(b) = Bson::try_from(json) else { return };
    let canonical = b.clone().into_canonical_extjson();
    let back = Bson::try_from(canonical.clone())
        .unwrap_or_else(|e| panic!("canonical Extended JSON does not parse: {canonical}: {e}"));
    assert!(
        same(&b, &back),
        "canonical round trip changed the value: {b:?} -> {canonical} -> {back:?}"
    );
    let _ = b.clone().into_relaxed_extjson().to_string();
    bson_value(&b);
}

/// Every conversion of one BSON value: to a grid value and back, to JSON,
/// SQL++ / PartiQL literals, DynamoDB JSON, rows.
pub fn bson_value(b: &Bson) {
    let v = Value::from_bson(b);
    let _ = v.to_text();
    let _ = v.to_json();
    let _ = v.to_bson();
    let mut s = String::new();
    v.write_json(&mut s);
    for ty in VALUE_TYPES {
        let _ = v.clone().convert(*ty);
    }
    let _ = doc_edit::to_json(b);
    let _ = doc_edit::json_literal(b);
    let _ = doc_edit::partiql_literal(b);
    let _ = doc_edit::dynamo_json(b);
    let doc = match b {
        Bson::Document(d) => d.clone(),
        other => {
            let mut d = Document::new();
            d.insert("value", other.clone());
            d
        }
    };
    let cols = infer_columns([&doc]);
    let row = document_to_row(&doc, &cols);
    let names: Vec<String> = cols.iter().map(|c| c.path.clone()).collect();
    let _ = row_to_document(&names, &row);
    for c in &cols {
        assert!(
            lookup(&doc, &c.path).is_some(),
            "inferred column {c:?} not found"
        );
        let _ = doc_edit::sqlpp_path(&c.path);
        let _ = doc_edit::partiql_path(&c.path);
        let _ = doc_edit::sqlpp_set("b.s.c", &Bson::Int32(1), &c.path, b);
        let _ = doc_edit::partiql_set("t", &[("k".into(), Bson::Int32(1))], &c.path, b);
    }
    let _ = doc_edit::sqlpp_insert("b.s.c", &doc);
    let _ = doc_edit::partiql_insert("t", &doc);
    let _ = doc_edit::dynamo_key(&doc, &names);
}

/// A cell typed in the grid or read from a file: JSON cell values, text
/// conversion to every type, the date / time / hex parsers.
pub fn cell_text(text: &str) {
    for ty in VALUE_TYPES {
        let _ = Value::from_cell_json(text, *ty);
        if let Ok(v) = Value::Text(text.to_owned()).convert(*ty) {
            let _ = v.to_text();
            let _ = v.to_bson();
        }
    }
    let _ = value::parse_date(text);
    let _ = value::parse_time(text);
    let _ = value::parse_timestamp(text);
    let _ = value::unhex(text);
    let _ = value::guess_type(text.split(',').map(str::trim));
    let _ = ValueType::from_sql(text);
}

/// Raw fuzzer input for the import readers: byte 0 picks the format and
/// options, the rest is the file.
pub fn import_bytes(data: &[u8]) {
    let Some((&sel, file)) = data.split_first() else {
        return;
    };
    let format = [
        DataFormat::Csv,
        DataFormat::Tsv,
        DataFormat::Json,
        DataFormat::JsonLines,
    ][(sel & 3) as usize];
    let opts = FormatOptions {
        header: sel & 4 != 0,
        delimiter: [None, Some(';'), Some('|'), Some('é')][((sel >> 3) & 3) as usize],
        null_text: if sel & 32 != 0 {
            "NULL".into()
        } else {
            String::new()
        },
        ..FormatOptions::default()
    };
    import(file.to_vec(), format, &opts);
}

/// Reads a whole file as rows and as documents, then maps the rows to the
/// guessed types and to text.
pub fn import(file: Vec<u8>, format: DataFormat, opts: &FormatOptions) {
    let Ok(mut r) = import::from_bytes(file.clone(), format, opts) else {
        return;
    };
    let cols = r.columns().to_vec();
    let identity = Mapping::identity(&cols);
    let as_text: Vec<(String, ValueType)> = cols
        .iter()
        .map(|c| (c.name.clone(), ValueType::Text))
        .collect();
    let to_text = Mapping::new(&cols, &as_text, &[]).ok();
    loop {
        let Ok(rows) = r.next_rows(7) else { break };
        if rows.is_empty() {
            break;
        }
        for row in rows {
            assert!(row.len() <= cols.len().max(1), "row wider than the columns");
            let _ = identity.row(&row);
            if let Some(m) = &to_text {
                let _ = m.row(&row);
            }
        }
    }
    if let Ok(mut r) = import::from_bytes(file, format, opts) {
        while let Ok(docs) = r.next_documents(5) {
            if docs.is_empty() {
                break;
            }
        }
    }
}

/// BSON equality with NaN equal to itself.
fn same(a: &Bson, b: &Bson) -> bool {
    match (a, b) {
        (Bson::Double(x), Bson::Double(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Bson::Array(x), Bson::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (Bson::Document(x), Bson::Document(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|((ka, va), (kb, vb))| ka == kb && same(va, vb))
        }
        _ => a == b,
    }
}
