//! Fuzz smoke tests (plan, section 9): the harness of the cargo-fuzz
//! targets `ext_json`, `cell_value` and `import` (see `fuzz/`) on random
//! and structured input with stable Rust, plus a replay of the seed corpus
//! and of the crashes found so far. `PROPTEST_CASES` scales the run.

use std::path::Path;

use ferrodb_data::fuzzing::{cell_text, ext_json, import_bytes};
use proptest::prelude::*;
use serde_json::{Value as Json, json};

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256)
}

/// Strings that make Extended JSON interesting.
fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec![
            "",
            "0",
            "-1",
            "1.5",
            "1e400",
            "-0",
            "NaN",
            "Infinity",
            "-Infinity",
            "9223372036854775807",
            "9223372036854775808",
            "-9223372036854775809",
            "2024-02-29T12:00:00Z",
            "2024-13-01",
            "1969-12-31T23:59:59.999Z",
            "+275760-09-13T00:00:00Z",
            "64b7f0c2a1b2c3d4e5f60718",
            "zz",
            "AAAA",
            "AQID",
            "=",
            "x",
            "i",
            "imsux",
            "é",
            "😀",
            "00",
            "0x1F",
            "12:30:00",
            "24:00:00",
            "1.10",
            "1E+6143",
            "-1E-6176",
        ])
        .prop_map(str::to_owned),
        ".{0,12}",
    ]
}

fn scalar() -> impl Strategy<Value = Json> {
    prop_oneof![
        Just(Json::Null),
        any::<bool>().prop_map(Json::Bool),
        any::<i64>().prop_map(|i| json!(i)),
        any::<u64>().prop_map(|i| json!(i)),
        any::<f64>().prop_map(|f| json!(f)),
        text().prop_map(Json::String),
    ]
}

/// Extended JSON wrappers around arbitrary contents (valid and invalid).
fn ext(inner: impl Strategy<Value = Json> + Clone) -> impl Strategy<Value = Json> {
    let key = prop::sample::select(vec![
        "$oid",
        "$date",
        "$numberLong",
        "$numberInt",
        "$numberDouble",
        "$numberDecimal",
        "$binary",
        "$regularExpression",
        "$regex",
        "$options",
        "$timestamp",
        "$minKey",
        "$maxKey",
        "$undefined",
        "$symbol",
        "$code",
        "$scope",
        "$dbPointer",
        "$ref",
        "$id",
        "$uuid",
        "base64",
        "subType",
        "pattern",
        "t",
        "i",
        "a",
    ]);
    prop_oneof![
        (key.clone(), inner.clone()).prop_map(|(k, v)| json!({ k: v })),
        (key.clone(), inner.clone(), key, inner)
            .prop_map(|(k1, v1, k2, v2)| json!({ k1: v1, k2: v2 })),
    ]
}

fn json_value() -> impl Strategy<Value = Json> {
    scalar().prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Json::Array),
            prop::collection::btree_map(text(), inner.clone(), 0..5)
                .prop_map(|m| Json::Object(m.into_iter().collect())),
            ext(inner),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn ext_json_structured(v in json_value()) {
        ext_json(v.to_string().as_bytes());
    }

    #[test]
    fn ext_json_random(data in prop::collection::vec(any::<u8>(), 0..200)) {
        ext_json(&data);
    }

    #[test]
    fn cell_values(t in text()) {
        cell_text(&t);
    }

    #[test]
    fn cell_values_json(v in json_value()) {
        cell_text(&v.to_string());
    }

    #[test]
    fn import_random(data in prop::collection::vec(any::<u8>(), 0..300)) {
        import_bytes(&data);
    }

    #[test]
    fn import_structured(
        sel in any::<u8>(),
        parts in prop::collection::vec(prop::sample::select(vec![
            "a", "b", ",", ";", "|", "\t", "\n", "\r\n", "\"", "\"\"", "NULL", "1", "-2.5",
            "2024-01-01", "true", "{", "}", "[", "]", ":", "\"k\"", "{\"$oid\":\"64b7f0c2a1b2c3d4e5f60718\"}",
            "\u{feff}", "é", " ",
        ]), 0..60),
    ) {
        let mut data = vec![sel];
        data.extend(parts.concat().into_bytes());
        import_bytes(&data);
    }
}

fn replay(dir: &str, f: impl Fn(&[u8])) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz")
        .join(dir);
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("missing corpus {}: {e}", dir.display()));
    for e in entries.flatten() {
        f(&std::fs::read(e.path()).unwrap());
    }
}

#[test]
fn seed_corpus() {
    replay("corpus/ext_json", ext_json);
    replay("corpus/import", import_bytes);
    replay("corpus/ext_json", |d| {
        cell_text(&String::from_utf8_lossy(d))
    });
}
