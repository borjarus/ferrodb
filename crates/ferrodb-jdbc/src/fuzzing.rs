//! Fuzzing harness (plan, phase 10.2 and section 9): the bridge protocol
//! as the client reads it from the Java host's stdout — frames, their
//! routing, JSON answers and host errors, Arrow IPC result streams — on
//! arbitrary bytes. The cargo-fuzz target `jdbc_protocol` in `fuzz/` and
//! the stable proptest smoke tests in `tests/fuzz_smoke.rs` call the same
//! functions.
//!
//! Not part of the public API.

use ferrodb_api::Error;

use crate::{
    bridge::Incoming,
    errors,
    protocol::{self, ArrowStream, HostError},
};

/// A byte stream from the host: every frame is decoded and handled the
/// way the reader task and a running query handle it. The statement the
/// errors refer to is fixed (positions are mapped into it).
pub fn host_stream(data: &[u8]) {
    let sql = "select * from zażółć where \"İd\" = 1\nand x";
    let mut r = data;
    let mut decoder: Option<ArrowStream> = None;
    let mut began = false;
    while let Ok(Some(frame)) = futures::executor::block_on(protocol::read_frame(&mut r)) {
        match protocol::incoming(frame) {
            None => {}
            Some(Incoming::Event(ev)) => match ev["event"].as_str() {
                Some("begin") => {
                    decoder = Some(ArrowStream::new());
                    began = false;
                }
                Some("end") => decoder = None,
                _ => {}
            },
            Some(Incoming::Data(bytes)) => {
                // Data outside `begin` / `end` still goes through a decoder:
                // the fuzzer need not find the event first.
                let dec = decoder.get_or_insert_with(ArrowStream::new);
                if !began {
                    began = true;
                    let _ = protocol::stream_schema(&bytes);
                }
                arrow_batches(dec, bytes);
            }
            Some(Incoming::Done(Ok(_))) => {}
            Some(Incoming::Done(Err(e))) => host_error(e, sql),
        }
    }
}

/// Decodes record batches as a query does (stops on an error or when the
/// decoder makes no progress).
fn arrow_batches(dec: &mut ArrowStream, bytes: Vec<u8>) {
    let Ok(mut buf) = dec.feed(bytes) else {
        return;
    };
    while !buf.is_empty() {
        let before = buf.len();
        match dec.next_batch(&mut buf) {
            Ok(Some(batch)) => {
                let _ = batch.num_rows();
            }
            Ok(None) if buf.len() == before => break,
            Ok(None) => {}
            Err(_) => break,
        }
    }
}

/// For libFuzzer, whose panic hook aborts even on panics that are caught:
/// lets the panics of guarded Arrow decoding (turned into errors by
/// [`ArrowStream`], known arrow-ipc behaviour on malformed input) unwind,
/// and keeps aborting on every other panic.
pub fn tolerate_guarded_arrow_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !protocol::in_guarded_decode() {
            previous(info);
        }
    }));
}

/// A host error turned into the core's error, with its range checked.
pub fn host_error(e: HostError, sql: &str) {
    let _ = errors::position(&e.message, sql);
    let _ = errors::clean(&e.message);
    for stmt in [Some(sql), None] {
        if let Error::Query(q) = e.clone().into_error(stmt)
            && let Some(r) = q.range
        {
            assert!(
                r.start <= r.end && r.end <= sql.len(),
                "range {r:?} out of bounds"
            );
            assert!(
                sql.is_char_boundary(r.start) && sql.is_char_boundary(r.end),
                "range {r:?} not on char boundaries"
            );
        }
    }
}

/// Vendor error messages against a statement: byte 0 splits the input
/// into the message and the statement.
pub fn error_text(data: &[u8]) {
    let Some((&cut, rest)) = data.split_first() else {
        return;
    };
    let text = String::from_utf8_lossy(rest);
    let mut at = (cut as usize).min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let (message, sql) = text.split_at(at);
    if let Some(p) = errors::position(message, sql) {
        assert!(p <= sql.chars().count(), "position {p} past the statement");
    }
    let _ = errors::clean(message);
    host_error(
        HostError {
            kind: "sql".into(),
            message: message.to_owned(),
            sql_state: Some("42000".into()),
            vendor_code: 942,
            position: None,
        },
        sql,
    );
}
