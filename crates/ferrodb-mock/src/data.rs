//! Deterministic fake data generators (no RNG dependency: results must be
//! reproducible in UI snapshot tests).

use std::{sync::Arc, time::Duration};

use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampMillisecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use bson::{RawDocumentBuf, doc};
use ferrodb_api::{KeyEntry, KeyType};

const FIRST: &[&str] = &[
    "Anna", "Jan", "Maria", "Piotr", "Kasia", "Tomasz", "Ewa", "Marek",
];
const CITIES: &[&str] = &[
    "Warszawa", "Kraków", "Gdańsk", "Wrocław", "Poznań", "Łódź", "Berlin",
];

/// Cheap integer hash so every cell is a pure function of (row, col).
pub fn mix(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^ (x >> 33)
}

/// Schema of a synthetic table with `columns` columns cycling through types.
pub fn table_schema(columns: usize) -> Arc<Schema> {
    let fields: Vec<Field> = (0..columns.max(1))
        .map(|c| match c {
            0 => Field::new("id", DataType::Int64, false),
            1 => Field::new("name", DataType::Utf8, true),
            2 => Field::new("city", DataType::Utf8, true),
            _ => match c % 4 {
                0 => Field::new(format!("amount_{c}"), DataType::Float64, true),
                1 => Field::new(format!("flag_{c}"), DataType::Boolean, true),
                2 => Field::new(
                    format!("created_{c}"),
                    DataType::Timestamp(TimeUnit::Millisecond, None),
                    true,
                ),
                _ => Field::new(format!("note_{c}"), DataType::Utf8, true),
            },
        })
        .collect();
    Arc::new(Schema::new(fields))
}

/// Rows `[offset, offset + len)` of the synthetic table.
pub fn table_batch(schema: &Arc<Schema>, offset: u64, len: usize) -> RecordBatch {
    let rows = offset..offset + len as u64;
    let columns: Vec<ArrayRef> =
        schema
            .fields()
            .iter()
            .enumerate()
            .map(|(c, f)| -> ArrayRef {
                let h = |r: u64| mix(r.wrapping_mul(131).wrapping_add(c as u64));
                match (c, f.data_type()) {
                    (0, _) => Arc::new(Int64Array::from_iter_values(
                        rows.clone().map(|r| r as i64 + 1),
                    )),
                    (1, _) => Arc::new(StringArray::from_iter_values(
                        rows.clone()
                            .map(|r| format!("{} {}", FIRST[(h(r) % 8) as usize], r + 1)),
                    )),
                    (2, _) => Arc::new(StringArray::from_iter(rows.clone().map(|r| {
                        (h(r) % 10 != 0).then(|| CITIES[(h(r) % 7) as usize].to_owned())
                    }))),
                    (_, DataType::Float64) => Arc::new(Float64Array::from_iter(
                        rows.clone()
                            .map(|r| Some((h(r) % 1_000_000) as f64 / 100.0)),
                    )),
                    (_, DataType::Boolean) => Arc::new(BooleanArray::from_iter(
                        rows.clone().map(|r| Some(h(r) % 2 == 0)),
                    )),
                    (_, DataType::Timestamp(..)) => Arc::new(TimestampMillisecondArray::from_iter(
                        rows.clone()
                            .map(|r| Some(1_700_000_000_000 + (h(r) % 31_536_000_000) as i64)),
                    )),
                    _ => Arc::new(StringArray::from_iter(rows.clone().map(|r| {
                        (h(r) % 3 != 0).then(|| format!("note {:x}", h(r) % 0xffff))
                    }))),
                }
            })
            .collect();
    RecordBatch::try_new(schema.clone(), columns).expect("generated columns match schema")
}

/// Document `i` of a synthetic `customers` collection.
pub fn document(i: u64) -> RawDocumentBuf {
    let h = mix(i);
    let d = doc! {
        "_id": format!("{:024x}", h as u128 * 7 + i as u128),
        "name": format!("{} {}", FIRST[(h % 8) as usize], i + 1),
        "age": (18 + h % 60) as i32,
        "address": { "city": CITIES[(h % 7) as usize], "zip": format!("{:02}-{:03}", h % 99, h % 999) },
        "tags": (0..(h % 4)).map(|t| format!("tag{t}")).collect::<Vec<_>>(),
        "balance": (h % 100_000) as f64 / 100.0,
        "active": !h.is_multiple_of(3),
    };
    RawDocumentBuf::try_from(&d).expect("valid document")
}

/// One ~`bytes`-sized document (the "5 MB document" scenario).
pub fn big_document(bytes: usize) -> RawDocumentBuf {
    let items = bytes / 64;
    let d = doc! {
        "_id": "big-document",
        "items": (0..items).map(|i| doc! { "i": i as i64, "v": format!("value-{:032}", i) }).collect::<Vec<_>>(),
    };
    RawDocumentBuf::try_from(&d).expect("valid document")
}

/// Key `i` of a synthetic Redis keyspace, grouped by `:`.
pub fn key(i: u64) -> KeyEntry {
    let h = mix(i);
    let (prefix, kind) = match h % 6 {
        0 => ("session", KeyType::String),
        1 => ("user", KeyType::Hash),
        2 => ("queue", KeyType::List),
        3 => ("tags", KeyType::Set),
        4 => ("leaderboard", KeyType::ZSet),
        _ => ("events", KeyType::Stream),
    };
    KeyEntry {
        key: format!("app:{prefix}:{}:{i}", h % 97),
        kind,
        ttl: h.is_multiple_of(4).then(|| Duration::from_secs(h % 86_400)),
        size: Some(h % 4096),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_batches_are_deterministic_and_shaped() {
        let s = table_schema(50);
        let a = table_batch(&s, 999_000, 1000);
        let b = table_batch(&s, 999_000, 1000);
        assert_eq!(a.num_columns(), 50);
        assert_eq!(a.num_rows(), 1000);
        assert_eq!(a, b);
    }

    #[test]
    fn big_document_is_about_requested_size() {
        let d = big_document(5 * 1024 * 1024);
        assert!(d.as_bytes().len() > 4 * 1024 * 1024);
    }
}
