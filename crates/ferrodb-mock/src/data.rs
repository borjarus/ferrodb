//! Deterministic fake data generators (no RNG dependency: results must be
//! reproducible in UI snapshot tests).

use std::{sync::Arc, time::Duration};

use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampMillisecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use bson::{RawDocumentBuf, doc};
use ferrodb_api::{KeyEntry, KeyType, KeyValue, StreamEntry};

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

/// Value of a synthetic key; the type follows the key's prefix
/// (`app:<kind>:…`), so it matches what [`key`] reports.
pub fn key_value(key: &str) -> KeyValue {
    let h = mix(key.bytes().fold(7u64, |a, b| a.wrapping_mul(31) ^ b as u64));
    let n = 3 + (h % 12) as usize;
    let kind = key.split(':').nth(1).unwrap_or("");
    match kind {
        "session" => KeyValue::String(format!(
            "{{\"user\":{},\"ip\":\"10.0.{}.{}\",\"ok\":true}}",
            h % 1000,
            h % 255,
            (h >> 8) % 255
        )),
        "user" => KeyValue::Hash(vec![
            (
                "name".into(),
                format!("{} {}", FIRST[(h % 8) as usize], h % 100),
            ),
            ("city".into(), CITIES[(h % 7) as usize].into()),
            ("age".into(), (18 + h % 60).to_string()),
            ("email".into(), format!("user{}@example.com", h % 10_000)),
        ]),
        "queue" => KeyValue::List(
            (0..n)
                .map(|i| format!("job-{}", (h + i as u64) % 9999))
                .collect(),
        ),
        "tags" => KeyValue::Set((0..n).map(|i| format!("tag{}", (h >> i) % 50)).collect()),
        "leaderboard" => {
            let mut v: Vec<(String, f64)> = (0..n)
                .map(|i| {
                    (
                        format!("player{}", (h >> i) % 500),
                        (mix(h + i as u64) % 10_000) as f64,
                    )
                })
                .collect();
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            KeyValue::ZSet(v)
        }
        "events" => KeyValue::Stream(
            (0..n)
                .map(|i| StreamEntry {
                    id: format!("{}-{i}", 1_700_000_000_000 + h % 1_000_000),
                    fields: vec![
                        ("type".into(), ["click", "view", "buy"][i % 3].into()),
                        ("user".into(), ((h + i as u64) % 1000).to_string()),
                    ],
                })
                .collect(),
        ),
        _ => KeyValue::Json(format!("{{\"key\":\"{key}\",\"n\":{}}}", h % 100)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::Array;
    use bson::Document;

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

    #[test]
    fn mix_is_deterministic_and_injective_on_small_inputs() {
        assert_eq!(mix(0), 0);
        assert_eq!(mix(42), mix(42));
        let outputs: std::collections::HashSet<u64> = (0..10_000).map(mix).collect();
        assert_eq!(outputs.len(), 10_000);
    }

    #[test]
    fn table_schema_starts_with_id_name_city_then_cycles_types() {
        let s = table_schema(8);
        let names: Vec<&str> = s.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            names,
            [
                "id",
                "name",
                "city",
                "note_3",
                "amount_4",
                "flag_5",
                "created_6",
                "note_7",
            ]
        );
        assert_eq!(s.field(0).data_type(), &DataType::Int64);
        assert!(!s.field(0).is_nullable());
        assert_eq!(s.field(1).data_type(), &DataType::Utf8);
        assert_eq!(s.field(4).data_type(), &DataType::Float64);
        assert_eq!(s.field(5).data_type(), &DataType::Boolean);
        assert_eq!(
            s.field(6).data_type(),
            &DataType::Timestamp(TimeUnit::Millisecond, None)
        );
    }

    #[test]
    fn table_schema_zero_columns_yields_single_id_column() {
        let s = table_schema(0);
        assert_eq!(s.fields().len(), 1);
        assert_eq!(s.field(0).name(), "id");
    }

    #[test]
    fn table_batch_id_and_name_follow_row_index() {
        let s = table_schema(3);
        let b = table_batch(&s, 10, 5);
        let ids = b
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("id is Int64");
        assert_eq!(ids.values().to_vec(), vec![11, 12, 13, 14, 15]);

        let names = b
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("name is Utf8");
        for row in 0..5 {
            let name = names.value(row);
            let (first, num) = name.rsplit_once(' ').expect("name has a space");
            assert!(FIRST.contains(&first), "unexpected first name {first:?}");
            assert_eq!(num, (10 + row + 1).to_string());
        }
    }

    #[test]
    fn table_batch_values_are_in_expected_ranges() {
        let s = table_schema(7);
        let b = table_batch(&s, 0, 1000);

        let amounts = b
            .column(4)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("amount is Float64");
        assert_eq!(amounts.null_count(), 0);
        assert!(amounts.values().iter().all(|v| (0.0..10_000.0).contains(v)));

        let created = b
            .column(6)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .expect("created is timestamp");
        assert!(
            created
                .values()
                .iter()
                .all(|v| { (1_700_000_000_000..1_700_000_000_000 + 31_536_000_000).contains(v) })
        );
    }

    #[test]
    fn table_batch_city_has_some_nulls_and_some_values() {
        let s = table_schema(3);
        let b = table_batch(&s, 0, 1000);
        let cities = b.column(2);
        assert!(cities.null_count() > 0, "expected some null cities");
        assert!(cities.null_count() < 1000, "expected some non-null cities");
    }

    #[test]
    fn table_batch_zero_rows_is_empty_but_well_formed() {
        let s = table_schema(4);
        let b = table_batch(&s, 0, 0);
        assert_eq!(b.num_rows(), 0);
        assert_eq!(b.num_columns(), 4);
    }

    fn decode(d: &RawDocumentBuf) -> Document {
        Document::try_from(d).expect("valid bson")
    }

    #[test]
    fn document_is_deterministic_and_has_expected_shape() {
        let a = decode(&document(7));
        let b = decode(&document(7));
        assert_eq!(a, b);

        let id = a.get_str("_id").expect("_id is string");
        assert_eq!(id.len(), 24);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));

        let name = a.get_str("name").expect("name is string");
        assert!(
            name.ends_with(" 8"),
            "name should end with i+1, got {name:?}"
        );

        let age = a.get_i32("age").expect("age is i32");
        assert!((18..78).contains(&age));

        let address = a.get_document("address").expect("address is document");
        assert!(CITIES.contains(&address.get_str("city").expect("city")));
        assert!(address.get_str("zip").expect("zip").contains('-'));

        assert!(a.get_array("tags").is_ok());
        assert!(a.get_f64("balance").expect("balance is f64") >= 0.0);
        assert!(a.get_bool("active").is_ok());
    }

    #[test]
    fn documents_differ_across_indices() {
        let ids: std::collections::HashSet<String> = (0..100)
            .map(|i| decode(&document(i)).get_str("_id").expect("_id").to_owned())
            .collect();
        assert_eq!(ids.len(), 100);
    }

    #[test]
    fn big_document_item_count_matches_requested_bytes() {
        let d = decode(&big_document(640));
        assert_eq!(d.get_str("_id").expect("_id"), "big-document");
        assert_eq!(d.get_array("items").expect("items").len(), 10);

        let empty = decode(&big_document(0));
        assert!(empty.get_array("items").expect("items").is_empty());
    }

    #[test]
    fn key_prefix_matches_reported_kind() {
        for i in 0..200 {
            let e = key(i);
            let parts: Vec<&str> = e.key.split(':').collect();
            assert_eq!(parts.len(), 4, "key {:?}", e.key);
            assert_eq!(parts[0], "app");
            assert_eq!(parts[3], i.to_string());
            let expected = match parts[1] {
                "session" => KeyType::String,
                "user" => KeyType::Hash,
                "queue" => KeyType::List,
                "tags" => KeyType::Set,
                "leaderboard" => KeyType::ZSet,
                "events" => KeyType::Stream,
                other => panic!("unexpected prefix {other:?}"),
            };
            assert_eq!(e.kind, expected, "key {:?}", e.key);
        }
    }

    #[test]
    fn key_covers_all_kinds_and_bounds_ttl_and_size() {
        let entries: Vec<KeyEntry> = (0..200).map(key).collect();
        let kinds: std::collections::HashSet<String> =
            entries.iter().map(|e| format!("{:?}", e.kind)).collect();
        assert_eq!(kinds.len(), 6);

        for e in &entries {
            if let Some(ttl) = e.ttl {
                assert!(ttl < Duration::from_secs(86_400));
            }
            assert!(e.size.expect("size is always set") < 4096);
        }
        assert!(entries.iter().any(|e| e.ttl.is_some()));
        assert!(entries.iter().any(|e| e.ttl.is_none()));
    }

    #[test]
    fn key_is_deterministic() {
        assert_eq!(key(123), key(123));
    }

    #[test]
    fn key_value_kind_matches_key_kind() {
        for i in 0..200 {
            let e = key(i);
            let v = key_value(&e.key);
            assert_eq!(v.kind(), e.kind, "key {:?}", e.key);
        }
    }

    #[test]
    fn key_value_is_deterministic() {
        let k = key(5).key;
        assert_eq!(key_value(&k).kind(), key_value(&k).kind());
        let a = format!("{:?}", key_value(&k));
        let b = format!("{:?}", key_value(&k));
        assert_eq!(a, b);
    }

    #[test]
    fn key_value_unknown_prefix_falls_back_to_json() {
        match key_value("app:other:1:2") {
            KeyValue::Json(s) => {
                assert!(s.contains("\"key\":\"app:other:1:2\""));
            }
            other => panic!("expected Json fallback, got {other:?}"),
        }
    }

    #[test]
    fn key_value_hash_has_expected_fields() {
        // Find a user key deterministically.
        let k = (0..)
            .map(key)
            .find(|e| e.key.split(':').nth(1) == Some("user"))
            .expect("some user key exists")
            .key;
        match key_value(&k) {
            KeyValue::Hash(fields) => {
                let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
                assert_eq!(names, ["name", "city", "age", "email"]);
            }
            other => panic!("expected Hash, got {other:?}"),
        }
    }

    #[test]
    fn key_value_leaderboard_is_sorted_descending() {
        let k = (0..)
            .map(key)
            .find(|e| e.key.split(':').nth(1) == Some("leaderboard"))
            .expect("some leaderboard key exists")
            .key;
        match key_value(&k) {
            KeyValue::ZSet(members) => {
                assert!(!members.is_empty());
                assert!(members.windows(2).all(|w| w[0].1 >= w[1].1));
            }
            other => panic!("expected ZSet, got {other:?}"),
        }
    }

    #[test]
    fn key_value_stream_entries_have_known_types() {
        let k = (0..)
            .map(key)
            .find(|e| e.key.split(':').nth(1) == Some("events"))
            .expect("some events key exists")
            .key;
        match key_value(&k) {
            KeyValue::Stream(entries) => {
                assert!(!entries.is_empty());
                for entry in entries {
                    let ty = entry
                        .fields
                        .iter()
                        .find(|(n, _)| n == "type")
                        .map(|(_, v)| v.as_str());
                    assert!(matches!(ty, Some("click" | "view" | "buy")));
                }
            }
            other => panic!("expected Stream, got {other:?}"),
        }
    }
}
