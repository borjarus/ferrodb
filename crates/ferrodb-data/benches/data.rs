//! Regression benchmarks of result data (plan, sections 3 and 9): Extended
//! JSON → BSON of a 5 MB document, export of 100k rows from Arrow batches,
//! CSV import, grid cell text. Budgets: `benches/budgets.toml`;
//! `cargo xtask bench-check`.

use std::sync::{Arc, LazyLock};

use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use bson::Bson;
use divan::{Bencher, black_box, counter::BytesCount};
use ferrodb_api::{DataFormat, FormatOptions};
use ferrodb_data::{export::Exporter, import, value::value_at};

fn main() {
    divan::main();
}

const ROWS: usize = 100_000;

/// 100k rows × 6 columns in batches of 4096 rows (the size drivers send).
static BATCHES: LazyLock<Vec<RecordBatch>> = LazyLock::new(|| {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, true),
        Field::new("active", DataType::Boolean, true),
        Field::new(
            "created",
            DataType::Timestamp(TimeUnit::Microsecond, None),
            true,
        ),
        Field::new("note", DataType::Utf8, true),
    ]));
    (0..ROWS)
        .step_by(4096)
        .map(|start| {
            let end = (start + 4096).min(ROWS);
            let ids = start as i64..end as i64;
            let cols: Vec<ArrayRef> = vec![
                Arc::new(Int64Array::from_iter_values(ids.clone())),
                Arc::new(StringArray::from_iter(
                    ids.clone()
                        .map(|i| (i % 7 != 0).then(|| format!("name, \"{i}\""))),
                )),
                Arc::new(Float64Array::from_iter(
                    ids.clone().map(|i| Some(i as f64 * 1.25)),
                )),
                Arc::new(BooleanArray::from_iter(
                    ids.clone().map(|i| Some(i % 2 == 0)),
                )),
                Arc::new(TimestampMicrosecondArray::from_iter(
                    ids.clone()
                        .map(|i| Some(1_700_000_000_000_000 + i * 1_000_000)),
                )),
                Arc::new(StringArray::from_iter(
                    ids.map(|i| Some(format!("zażółć gęślą jaźń {i}\nline 2"))),
                )),
            ];
            RecordBatch::try_new(schema.clone(), cols).unwrap()
        })
        .collect()
});

/// A ~5 MB document in relaxed Extended JSON (the document view's edit
/// dialog and import send this).
static DOC_5MB: LazyLock<String> = LazyLock::new(|| {
    let mut items = Vec::new();
    let mut size = 0;
    let mut i = 0u64;
    while size < 5 << 20 {
        let item = format!(
            r#"{{"_id":{{"$oid":"64b7f0c2a1b2c3d4{i:08x}"}},"n":{i},"big":{{"$numberLong":"{}"}},"price":{{"$numberDecimal":"{i}.25"}},"when":{{"$date":"2024-02-29T12:00:00Z"}},"name":"item {i}","tags":["a","b","c"],"nested":{{"x":1.5,"y":null,"z":true}}}}"#,
            i * 1_000_003
        );
        size += item.len() + 1;
        items.push(item);
        i += 1;
    }
    format!(r#"{{"_id":1,"items":[{}]}}"#, items.join(","))
});

/// Extended JSON text → BSON document.
#[divan::bench]
fn ext_json_to_bson_5mb(b: Bencher) {
    b.counter(BytesCount::of_str(&*DOC_5MB)).bench(|| {
        let v: serde_json::Value = serde_json::from_str(black_box(&DOC_5MB)).unwrap();
        Bson::try_from(v).unwrap()
    });
}

/// BSON → relaxed Extended JSON text (the JSON view of a 5 MB document).
#[divan::bench]
fn bson_to_ext_json_5mb(b: Bencher) {
    let v: serde_json::Value = serde_json::from_str(&DOC_5MB).unwrap();
    let doc = Bson::try_from(v).unwrap();
    b.counter(BytesCount::of_str(&*DOC_5MB))
        .bench(|| black_box(&doc).clone().into_relaxed_extjson().to_string());
}

fn export(b: Bencher, format: DataFormat) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out");
    let opts = FormatOptions::default();
    b.bench(|| {
        let mut e = Exporter::create(&path, format, &opts, "postgresql").unwrap();
        e.begin_table(&BATCHES[0].schema()).unwrap();
        for batch in BATCHES.iter() {
            e.write_batch(batch).unwrap();
        }
        e.finish().unwrap()
    });
}

/// Export of 100k rows from Arrow batches to a file.
mod export {
    use super::*;

    #[divan::bench]
    fn csv_100k_rows(b: Bencher) {
        export(b, DataFormat::Csv);
    }

    #[divan::bench]
    fn json_100k_rows(b: Bencher) {
        export(b, DataFormat::Json);
    }

    #[divan::bench]
    fn json_lines_100k_rows(b: Bencher) {
        export(b, DataFormat::JsonLines);
    }
}

/// CSV import of 100k rows (type guessing and conversion).
#[divan::bench]
fn import_csv_100k_rows(b: Bencher) {
    let mut csv = String::from("id,name,amount,active,created\n");
    for i in 0..ROWS {
        csv.push_str(&format!(
            "{i},\"name, {i}\",{}.5,{},2024-01-01 12:00:{:02}\n",
            i * 3,
            i % 2 == 0,
            i % 60
        ));
    }
    let bytes = csv.into_bytes();
    let opts = FormatOptions::default();
    b.counter(BytesCount::new(bytes.len())).bench(|| {
        let mut r = import::from_bytes(bytes.clone(), DataFormat::Csv, &opts).unwrap();
        let mut n = 0;
        loop {
            let rows = r.next_rows(4096).unwrap();
            if rows.is_empty() {
                break n;
            }
            n += rows.len();
        }
    });
}

/// Text of every cell of 100k rows (what the grid and copy display).
#[divan::bench]
fn grid_cell_text_100k_rows(b: Bencher) {
    b.bench(|| {
        let mut bytes = 0;
        for batch in BATCHES.iter() {
            for col in batch.columns() {
                for row in 0..batch.num_rows() {
                    bytes += value_at(col.as_ref(), row).to_text().map_or(0, |s| s.len());
                }
            }
        }
        bytes
    });
}
