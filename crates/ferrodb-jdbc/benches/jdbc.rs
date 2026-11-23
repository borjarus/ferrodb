//! Regression benchmarks of the JDBC bridge client (plan, section 9: JDBC
//! bridge throughput): frame decoding and the Arrow IPC result stream as
//! the client reads them from the Java host. Budgets:
//! `benches/budgets.toml`; `cargo xtask bench-check`.

use std::sync::{Arc, LazyLock};

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema};
use divan::{Bencher, black_box, counter::BytesCount};
use ferrodb_jdbc::protocol::{self, ArrowStream, DATA, EVENT, encode};

fn main() {
    divan::main();
}

/// 64 MiB of data frames of 64 KiB, with an event every 16 frames.
static FRAMES: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let payload = vec![0xA5u8; 64 << 10];
    let mut v = Vec::with_capacity(65 << 20);
    for i in 0..1024u64 {
        v.extend(encode(DATA, i % 8, &payload));
        if i % 16 == 0 {
            v.extend(encode(EVENT, i % 8, br#"{"event":"notice","message":"m"}"#));
        }
    }
    v
});

/// 100k rows × 4 columns as the host streams them: an IPC stream whose
/// messages travel in data frames of 4096 rows each.
static RESULT: LazyLock<Vec<Vec<u8>>> = LazyLock::new(|| {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, true),
        Field::new("code", DataType::Utf8, true),
    ]));
    let mut out = Vec::new();
    let mut frames = Vec::new();
    {
        let mut w = StreamWriter::try_new(&mut out, &schema).unwrap();
        w.flush().unwrap();
        for start in (0..100_000i64).step_by(4096) {
            let ids = start..(start + 4096).min(100_000);
            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(Int64Array::from_iter_values(ids.clone())),
                    Arc::new(StringArray::from_iter(
                        ids.clone().map(|i| Some(format!("row {i}"))),
                    )),
                    Arc::new(Float64Array::from_iter(
                        ids.clone().map(|i| Some(i as f64 / 3.0)),
                    )),
                    Arc::new(StringArray::from_iter(
                        ids.map(|i| (i % 3 != 0).then_some("ABC")),
                    )),
                ],
            )
            .unwrap();
            w.write(&batch).unwrap();
        }
        w.finish().unwrap();
    }
    // Re-cut the stream into frames at message boundaries is not needed:
    // the client's decoder takes arbitrary chunks. 256 KiB per frame.
    for c in out.chunks(256 << 10) {
        frames.push(c.to_vec());
    }
    frames
});

/// Frame decoding (`read_frame`) of 64 MiB from the host's stdout.
#[divan::bench]
fn read_frames_64mib(b: Bencher) {
    b.counter(BytesCount::new(FRAMES.len())).bench(|| {
        let mut r = &FRAMES[..];
        let mut n = 0usize;
        while let Some(f) = futures::executor::block_on(protocol::read_frame(&mut r)).unwrap() {
            if let Some(m) = protocol::incoming(f) {
                n += matches!(m, ferrodb_jdbc::bridge::Incoming::Data(_)) as usize;
            }
        }
        n
    });
}

/// The Arrow result of 100k rows checked and decoded into record batches
/// (what a JDBC query does with the host's data frames).
#[divan::bench]
fn arrow_result_100k_rows(b: Bencher) {
    let bytes: usize = RESULT.iter().map(Vec::len).sum();
    b.counter(BytesCount::new(bytes)).bench(|| {
        let mut dec = ArrowStream::new();
        let mut rows = 0;
        let _ = protocol::stream_schema(black_box(&RESULT[0]));
        for frame in RESULT.iter() {
            let mut buf = dec.feed(frame.clone()).unwrap();
            while !buf.is_empty() {
                let before = buf.len();
                match dec.next_batch(&mut buf).unwrap() {
                    Some(batch) => rows += batch.num_rows(),
                    None if buf.len() == before => break,
                    None => {}
                }
            }
        }
        rows
    });
}
