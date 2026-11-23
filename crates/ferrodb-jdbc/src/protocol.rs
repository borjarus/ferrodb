//! The bridge protocol (see `java/jdbc-host/.../Wire.java`): frames
//! `[u8 kind][u64 id][u32 length][payload]`, big endian, on the host's
//! stdin / stdout. JSON-RPC requests and answers, JSON events, Arrow IPC
//! stream bytes and credits (backpressure) share one pipe; the id names the
//! request a frame belongs to.

use std::{cell::Cell, panic::AssertUnwindSafe};

use arrow_array::RecordBatch;
use arrow_buffer::Buffer;
use arrow_ipc::{MessageHeader, reader::StreamDecoder};
use arrow_schema::{ArrowError, SchemaRef};
use ferrodb_api::{Error, QueryError};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::bridge::Incoming;

pub const REQUEST: u8 = 1;
pub const RESPONSE: u8 = 2;
pub const EVENT: u8 = 3;
pub const DATA: u8 = 4;
pub const CREDIT: u8 = 5;

/// Protocol version spoken by this client (`hello` answers the host's).
pub const PROTOCOL: u64 = 1;

/// Largest frame accepted (a record batch of wide rows stays far below).
const MAX_FRAME: u32 = 256 << 20;
/// Bytes reserved up front for a frame's payload.
const PREALLOC: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub id: u64,
    pub payload: Vec<u8>,
}

pub fn encode(kind: u8, id: u64, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(13 + payload.len());
    v.push(kind);
    v.extend_from_slice(&id.to_be_bytes());
    v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    v.extend_from_slice(payload);
    v
}

pub fn request(id: u64, method: &str, params: Value) -> Vec<u8> {
    let body = serde_json::json!({ "method": method, "params": params });
    encode(REQUEST, id, body.to_string().as_bytes())
}

pub fn credit(id: u64, n: i32) -> Vec<u8> {
    encode(CREDIT, id, &n.to_be_bytes())
}

/// The next frame; `None` at end of stream.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<Frame>> {
    let mut head = [0u8; 13];
    match r.read_exact(&mut head[..1]).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    r.read_exact(&mut head[1..]).await?;
    let id = u64::from_be_bytes(head[1..9].try_into().expect("8 bytes"));
    let len = u32::from_be_bytes(head[9..13].try_into().expect("4 bytes"));
    if len > MAX_FRAME {
        return Err(std::io::Error::other(format!("frame of {len} bytes")));
    }
    // Grown as bytes arrive: a corrupt length must not allocate (and zero)
    // up to 256 MiB before the stream turns out to be shorter.
    let mut payload = Vec::with_capacity((len as usize).min(PREALLOC));
    (&mut *r)
        .take(u64::from(len))
        .read_to_end(&mut payload)
        .await?;
    if payload.len() != len as usize {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    Ok(Some(Frame {
        kind: head[0],
        id,
        payload,
    }))
}

/// A frame as the reader task routes it to its request: Arrow bytes,
/// events, answers (results or host errors). `None` for frame kinds a
/// client never receives.
pub fn incoming(f: Frame) -> Option<Incoming> {
    Some(match f.kind {
        DATA => Incoming::Data(f.payload),
        EVENT | RESPONSE => {
            let v: Value = serde_json::from_slice(&f.payload).unwrap_or_else(
                |e| serde_json::json!({ "error": {"kind": "protocol", "message": e.to_string()} }),
            );
            if f.kind == EVENT {
                Incoming::Event(v)
            } else if let Some(err) = v.get("error") {
                Incoming::Done(Err(HostError::from_json(err)))
            } else {
                Incoming::Done(Ok(v.get("result").cloned().unwrap_or(Value::Null)))
            }
        }
        _ => return None,
    })
}

/// The schema of a result set: its first data frame. An IPC stream
/// decoder only finishes a message when more bytes follow, so the schema
/// message is read on its own, closed by an end-of-stream marker (an empty
/// result has no further frames).
pub fn stream_schema(first: &[u8]) -> Result<SchemaRef, ArrowError> {
    let mut with_eos = Vec::with_capacity(first.len() + 8);
    with_eos.extend_from_slice(first);
    with_eos.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]);
    guarded(|| Ok(arrow_ipc::reader::StreamReader::try_new(&with_eos[..], None)?.schema()))
}

/// The Arrow IPC stream of one result set, fed with the data frames of
/// its request.
///
/// arrow-ipc trusts the buffer layout of a message and panics (slicing
/// the message body) when a record batch names bytes outside it. A
/// misbehaving host must not take the IDE down, so every message is
/// checked before it reaches the decoder, and a panic inside the decoder
/// on input the check does not cover becomes an error too.
#[derive(Debug, Default)]
pub struct ArrowStream {
    decoder: StreamDecoder,
    check: MessageCheck,
}

impl ArrowStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks the messages in a data frame; the bytes come back as a
    /// buffer for [`Self::next_batch`]. An error ends the result.
    pub fn feed(&mut self, bytes: Vec<u8>) -> Result<Buffer, ArrowError> {
        self.check.check(&bytes)?;
        Ok(Buffer::from(bytes))
    }

    /// The next record batch in `buf`; `None` when more bytes are needed
    /// (`buf` is then consumed).
    pub fn next_batch(&mut self, buf: &mut Buffer) -> Result<Option<RecordBatch>, ArrowError> {
        guarded(|| self.decoder.decode(buf))
    }
}

thread_local! {
    static IN_ARROW: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread is inside a guarded Arrow decode, whose panics are
/// caught and reported as errors (for panic hooks that abort or report
/// crashes, e.g. libFuzzer's).
#[doc(hidden)]
pub fn in_guarded_decode() -> bool {
    IN_ARROW.with(Cell::get)
}

fn guarded<T>(f: impl FnOnce() -> Result<T, ArrowError>) -> Result<T, ArrowError> {
    let outer = IN_ARROW.with(|g| g.replace(true));
    let r = std::panic::catch_unwind(AssertUnwindSafe(f));
    IN_ARROW.with(|g| g.set(outer));
    r.unwrap_or_else(|p| {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        Err(ArrowError::IpcError(format!(
            "malformed Arrow IPC stream: {msg}"
        )))
    })
}

/// Largest IPC message metadata accepted (schemas of very wide results
/// stay far below).
const MAX_METADATA: usize = 64 << 20;

/// Framing of an IPC stream (`[0xFFFFFFFF] <i32 metadata length>
/// <metadata> <body>`, the continuation marker optional), followed across
/// data frames, checking each message's buffers against its body.
#[derive(Debug, Default)]
struct MessageCheck {
    pending: Vec<u8>,
    state: Framing,
}

#[derive(Debug, Default, Clone, Copy)]
enum Framing {
    #[default]
    Prefix,
    Metadata(usize),
    Body(u64),
    End,
}

impl MessageCheck {
    fn check(&mut self, mut bytes: &[u8]) -> Result<(), ArrowError> {
        let bad = |m: String| ArrowError::IpcError(format!("malformed Arrow IPC stream: {m}"));
        while !bytes.is_empty() {
            match self.state {
                Framing::End => return Ok(()),
                Framing::Body(rest) => {
                    let n = rest.min(bytes.len() as u64);
                    bytes = &bytes[n as usize..];
                    self.state = if n == rest {
                        Framing::Prefix
                    } else {
                        Framing::Body(rest - n)
                    };
                }
                Framing::Prefix => {
                    let continued = self.pending.len() >= 4 && self.pending[..4] == [0xff; 4];
                    let need = if continued { 8 } else { 4 };
                    let n = (need - self.pending.len()).min(bytes.len());
                    self.pending.extend_from_slice(&bytes[..n]);
                    bytes = &bytes[n..];
                    if self.pending.len() < need || self.pending[..] == [0xff; 4] {
                        continue;
                    }
                    let len = i32::from_le_bytes(
                        self.pending[need - 4..need].try_into().expect("4 bytes"),
                    );
                    self.pending.clear();
                    self.state = match usize::try_from(len) {
                        Ok(0) => Framing::End,
                        Ok(l) if l <= MAX_METADATA => Framing::Metadata(l),
                        _ => return Err(bad(format!("message metadata of {len} bytes"))),
                    };
                }
                Framing::Metadata(len) => {
                    let n = (len - self.pending.len()).min(bytes.len());
                    self.pending.extend_from_slice(&bytes[..n]);
                    bytes = &bytes[n..];
                    if self.pending.len() < len {
                        continue;
                    }
                    let body = message_body(&self.pending).map_err(bad)?;
                    self.pending.clear();
                    self.state = if body == 0 {
                        Framing::Prefix
                    } else {
                        Framing::Body(body)
                    };
                }
            }
        }
        Ok(())
    }
}

/// The body length of a message, after checking that every buffer of a
/// record or dictionary batch lies inside the body.
fn message_body(metadata: &[u8]) -> Result<u64, String> {
    let msg = arrow_ipc::root_as_message(metadata).map_err(|e| e.to_string())?;
    let body =
        u64::try_from(msg.bodyLength()).map_err(|_| format!("body of {}", msg.bodyLength()))?;
    let batch = match msg.header_type() {
        MessageHeader::RecordBatch => msg.header_as_record_batch(),
        MessageHeader::DictionaryBatch => msg.header_as_dictionary_batch().and_then(|d| d.data()),
        _ => None,
    };
    for b in batch.and_then(|b| b.buffers()).iter().flatten() {
        let end = u64::try_from(b.offset())
            .ok()
            .zip(u64::try_from(b.length()).ok())
            .and_then(|(o, l)| o.checked_add(l));
        if end.is_none_or(|e| e > body) {
            return Err(format!(
                "buffer at {} of {} bytes outside a body of {body}",
                b.offset(),
                b.length()
            ));
        }
    }
    Ok(body)
}

/// An error answered by the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError {
    /// `sql`, `connection`, `timeout`, `cancelled`, `host`, `protocol`.
    pub kind: String,
    pub message: String,
    pub sql_state: Option<String>,
    pub vendor_code: i64,
    /// 0-based character offset reported by the driver (Oracle).
    pub position: Option<usize>,
}

impl HostError {
    pub fn from_json(v: &Value) -> Self {
        Self {
            kind: v["kind"].as_str().unwrap_or("host").to_owned(),
            message: v["message"].as_str().unwrap_or("JDBC error").to_owned(),
            sql_state: v["sql_state"].as_str().map(str::to_owned),
            vendor_code: v["vendor_code"].as_i64().unwrap_or(0),
            position: v["position"].as_u64().map(|p| p as usize),
        }
    }

    /// The error as the core sees it. `sql` is the statement that failed,
    /// used to turn positions into byte ranges.
    pub fn into_error(self, sql: Option<&str>) -> Error {
        match self.kind.as_str() {
            "cancelled" => Error::Cancelled,
            "timeout" => Error::Timeout,
            "connection" => Error::Connection(self.message),
            "sql" => {
                let position = self
                    .position
                    .or_else(|| sql.and_then(|s| crate::errors::position(&self.message, s)));
                let range = match (position, sql) {
                    (Some(p), Some(s)) => {
                        // The char at the position (a byte range on char
                        // boundaries: the editor slices the text with it).
                        let start = char_to_byte(s, p);
                        let end = s[start..]
                            .chars()
                            .next()
                            .map_or(start, |c| start + c.len_utf8());
                        Some(start..end)
                    }
                    _ => None,
                };
                let code = match (&self.sql_state, self.vendor_code) {
                    (Some(s), 0) => Some(s.clone()),
                    (Some(s), v) => Some(format!("{s}/{v}")),
                    (None, 0) => None,
                    (None, v) => Some(v.to_string()),
                };
                Error::Query(QueryError {
                    message: crate::errors::clean(&self.message),
                    code,
                    range,
                })
            }
            _ => Error::Other(format!("JDBC bridge: {}", self.message)),
        }
    }
}

/// Byte offset of character `chars` (the host counts UTF-16 units; the
/// difference only matters outside the BMP).
fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map_or(s.len(), |(b, _)| b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip() {
        let mut bytes = encode(EVENT, 7, b"{\"event\":\"begin\"}");
        bytes.extend(credit(9, 3));
        let mut r = &bytes[..];
        let f = read_frame(&mut r).await.unwrap().unwrap();
        assert_eq!((f.kind, f.id), (EVENT, 7));
        let f = read_frame(&mut r).await.unwrap().unwrap();
        assert_eq!(
            (f.kind, f.id, f.payload),
            (CREDIT, 9, 3i32.to_be_bytes().to_vec())
        );
        assert!(read_frame(&mut r).await.unwrap().is_none());
    }

    #[test]
    fn errors_map_to_api_errors() {
        let e = HostError::from_json(&serde_json::json!({
            "kind": "sql", "message": "ORA-00942: table or view does not exist",
            "sql_state": "42000", "vendor_code": 942, "position": 14
        }));
        let sql = "select * from zażółć";
        match e.into_error(Some(sql)) {
            Error::Query(q) => {
                assert_eq!(q.code.as_deref(), Some("42000/942"));
                assert_eq!(&sql[q.range.unwrap().start..], "zażółć");
            }
            other => panic!("{other:?}"),
        }
        let c = HostError::from_json(&serde_json::json!({"kind": "cancelled", "message": "x"}));
        assert_eq!(c.into_error(None), Error::Cancelled);
    }

    #[test]
    fn error_range_covers_a_whole_char() {
        // Fuzzing review (phase 10.2): the range ended inside `ż` (one byte of two).
        let sql = "select * from zażółć";
        let e = HostError::from_json(&serde_json::json!({
            "kind": "sql", "message": "x", "position": 16
        }));
        match e.into_error(Some(sql)) {
            Error::Query(q) => assert_eq!(&sql[q.range.unwrap()], "ż"),
            other => panic!("{other:?}"),
        }
        // Past the end: an empty range at the end.
        let e = HostError::from_json(&serde_json::json!({
            "kind": "sql", "message": "x", "position": 99
        }));
        match e.into_error(Some(sql)) {
            Error::Query(q) => assert_eq!(q.range, Some(sql.len()..sql.len())),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn truncated_frame_is_an_error_without_a_large_allocation() {
        // Fuzzing review (phase 10.2): a header announcing 256 MiB allocated (and
        // zeroed) the whole payload before reading it.
        let mut bytes = vec![DATA];
        bytes.extend_from_slice(&1u64.to_be_bytes());
        bytes.extend_from_slice(&(MAX_FRAME).to_be_bytes());
        bytes.extend_from_slice(b"abc");
        let mut r = &bytes[..];
        let e = read_frame(&mut r).await.unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    fn ipc_stream() -> Vec<u8> {
        use std::sync::Arc;

        use arrow_array::{Int64Array, StringArray};
        use arrow_schema::{DataType, Field, Schema};

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
        ]));
        let mut out = Vec::new();
        let mut w = arrow_ipc::writer::StreamWriter::try_new(&mut out, &schema).unwrap();
        for n in [3, 5] {
            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(Int64Array::from_iter_values(0..n)),
                    Arc::new(StringArray::from_iter(
                        (0..n).map(|i| Some(format!("ż{i}"))),
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

    /// Rows decoded from `bytes` cut into frames of `chunk` bytes.
    fn decode_all(bytes: &[u8], chunk: usize) -> Result<usize, ArrowError> {
        let mut s = ArrowStream::new();
        let mut rows = 0;
        for c in bytes.chunks(chunk) {
            let mut buf = s.feed(c.to_vec())?;
            while !buf.is_empty() {
                let before = buf.len();
                match s.next_batch(&mut buf)? {
                    Some(b) => rows += b.num_rows(),
                    None if buf.len() == before => break,
                    None => {}
                }
            }
        }
        Ok(rows)
    }

    #[test]
    fn arrow_stream_decodes_in_any_chunks() {
        let bytes = ipc_stream();
        for chunk in [1, 3, 8, 100, bytes.len()] {
            assert_eq!(decode_all(&bytes, chunk).unwrap(), 8, "chunk {chunk}");
        }
    }

    #[test]
    fn arrow_stream_refuses_buffers_outside_the_body() {
        // Fuzzing (phase 10.2): arrow-ipc panicked slicing the message body
        // when a record batch named bytes outside it. Every single-byte
        // corruption is an error or a result now, never a panic, and the
        // message check catches the buffer layout itself.
        let bytes = ipc_stream();
        let mut caught = 0;
        for i in 0..bytes.len() {
            for v in [0x00, 0x7f, 0xff] {
                let mut b = bytes.clone();
                b[i] = v;
                if let Err(e) = decode_all(&b, 64)
                    && e.to_string().contains("outside a body")
                {
                    caught += 1;
                }
            }
        }
        assert!(caught > 0);
    }
}
