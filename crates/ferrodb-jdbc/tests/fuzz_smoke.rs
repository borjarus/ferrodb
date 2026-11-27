//! Fuzz smoke tests (plan, section 9): the harness of the cargo-fuzz
//! target `jdbc_protocol` (see `fuzz/`) on random and structured host
//! output with stable Rust, plus a replay of the seed corpus.
//! `PROPTEST_CASES` scales the run.
//!
//! The binary seed `fuzz/corpus/jdbc_protocol/arrow_result` is written by
//! `cargo test -p ferrodb-jdbc --test fuzz_smoke -- --ignored write_seed`.

use std::{path::Path, sync::Arc};

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema};
use ferrodb_jdbc::{
    fuzzing::{error_text, host_stream},
    protocol::{CREDIT, DATA, EVENT, REQUEST, RESPONSE, encode},
};
use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256)
}

/// An Arrow IPC stream of two small batches (schema, batches, end marker).
fn arrow_stream() -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
    ]));
    let mut out = Vec::new();
    let mut w = StreamWriter::try_new(&mut out, &schema).unwrap();
    for n in [3, 2] {
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from_iter_values(0..n)),
                Arc::new(StringArray::from_iter(
                    (0..n).map(|i| (i % 2 == 0).then(|| format!("ż{i}"))),
                )),
            ],
        )
        .unwrap();
        w.write(&batch).unwrap();
    }
    w.finish().unwrap();
    drop(w);
    out
}

/// A query's frames as the host sends them: `begin`, the Arrow stream in
/// chunks, `end`, the answer.
fn result_frames(id: u64, chunk: usize) -> Vec<u8> {
    let mut v = encode(EVENT, id, br#"{"event":"begin"}"#);
    for c in arrow_stream().chunks(chunk.max(1)) {
        v.extend(encode(DATA, id, c));
    }
    v.extend(encode(EVENT, id, br#"{"event":"end"}"#));
    v.extend(encode(
        RESPONSE,
        id,
        br#"{"result":{"results":1,"affected":null}}"#,
    ));
    v
}

fn payload() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        prop::sample::select(vec![
            r#"{"event":"begin"}"#,
            r#"{"event":"end"}"#,
            r#"{"event":"notice","message":"careful"}"#,
            r#"{"result":{"protocol":1}}"#,
            r#"{"result":null}"#,
            r#"{"error":{"kind":"sql","message":"ORA-00942: table or view does not exist","sql_state":"42000","vendor_code":942,"position":16}}"#,
            r#"{"error":{"kind":"sql","message":"Table/View 'ZAŻÓŁĆ' does not exist.","position":null}}"#,
            r#"{"error":{"kind":"sql","message":"Syntax error; SQL statement:\nselect * from [*]zażółć [42000-224]"}}"#,
            r#"{"error":{"kind":"sql","message":"line 2:3 cannot recognize input"}}"#,
            r#"{"error":{"kind":"sql","message":"x [line 1, column 99999999999999999999]"}}"#,
            r#"{"error":{"kind":"cancelled"}}"#,
            r#"{"error":{"kind":"connection","message":"closed"}}"#,
            r#"{"error":42}"#,
            "not json",
        ])
        .prop_map(|s| s.as_bytes().to_vec()),
        prop::collection::vec(any::<u8>(), 0..64),
        (0..arrow_stream().len(), 1..64usize).prop_map(|(at, n)| {
            let s = arrow_stream();
            s[at..(at + n).min(s.len())].to_vec()
        }),
    ]
}

fn frame() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::sample::select(vec![REQUEST, RESPONSE, EVENT, DATA, CREDIT, 0, 9]),
        0u64..4,
        payload(),
    )
        .prop_map(|(k, id, p)| encode(k, id, &p))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn random_bytes(data in prop::collection::vec(any::<u8>(), 0..300)) {
        host_stream(&data);
    }

    #[test]
    fn frame_sequences(frames in prop::collection::vec(frame(), 0..12)) {
        host_stream(&frames.concat());
    }

    #[test]
    fn arrow_results_mutated(chunk in 1usize..400, at in any::<prop::sample::Index>(), byte in any::<u8>(), cut in any::<prop::sample::Index>()) {
        let mut data = result_frames(1, chunk);
        let i = at.index(data.len());
        data[i] = byte;
        host_stream(&data);
        let c = cut.index(data.len());
        host_stream(&data[..c]);
    }

    #[test]
    fn error_texts(data in prop::collection::vec(any::<u8>(), 0..200)) {
        error_text(&data);
    }

    #[test]
    fn error_texts_structured(
        cut in any::<u8>(),
        parts in prop::collection::vec(prop::sample::select(vec![
            "'", "\"", "not found: ", "line ", ":", "[line ", ", column ", "1", "2", "99",
            "SQL statement:", "\n", "[*]", "; SQL statement:", " [", "]", "tab", "İ", "ß", "ż",
            "select ", "from ", "x.", "_",
        ]), 0..30),
    ) {
        let mut data = vec![cut];
        data.extend(parts.concat().into_bytes());
        error_text(&data);
    }
}

#[test]
fn whole_results_decode() {
    for chunk in [1, 7, 64, 100_000] {
        host_stream(&result_frames(3, chunk));
    }
}

#[test]
fn seed_corpus() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/jdbc_protocol");
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("missing corpus {}: {e}", dir.display()));
    for e in entries.flatten() {
        host_stream(&std::fs::read(e.path()).unwrap());
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/jdbc_errors");
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        error_text(&std::fs::read(e.path()).unwrap());
    }
}

/// Writes the binary seed of the `jdbc_protocol` target.
#[test]
#[ignore = "writes the seed corpus"]
fn write_seed() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/jdbc_protocol");
    std::fs::create_dir_all(&dir).unwrap();
    let mut v = encode(RESPONSE, 0, br#"{"result":{"protocol":1}}"#);
    v.extend(result_frames(1, 4096));
    std::fs::write(dir.join("arrow_result"), v).unwrap();
    let mut e = encode(EVENT, 2, br#"{"event":"notice","message":"m"}"#);
    e.extend(encode(
        RESPONSE,
        2,
        r#"{"error":{"kind":"sql","message":"Table/View 'ZAŻÓŁĆ' does not exist.","sql_state":"42X05","vendor_code":0}}"#
            .as_bytes(),
    ));
    std::fs::write(dir.join("host_error"), e).unwrap();
}
