//! Incremental reader of the query service response: top-level fields are
//! collected, elements of the `results` array come out one by one as soon
//! as their bytes arrived, so a large result never sits in memory whole.

use bson::{Bson, Decimal128, Document};
use serde_json::Value as Json;

/// What the reader found in the bytes fed so far.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A top-level field other than `results` (`requestID`, `status`,
    /// `errors`, `metrics`…), with its value.
    Field(String, Json),
    /// `"results": [` was read.
    ResultsStart,
    /// One element of `results`, as raw JSON bytes.
    Result(Vec<u8>),
    /// The closing `]` of `results`.
    ResultsEnd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    /// Before the opening `{`.
    Start,
    /// Expecting a key (or `}`).
    Key,
    /// After a key, expecting `:` and its value.
    Value(String),
    /// Inside `results`: expecting an element (or `]`).
    Results {
        first: bool,
    },
    /// After a top-level value: `,` or `}`.
    AfterValue,
    Done,
}

pub struct Reader {
    buf: Vec<u8>,
    pos: usize,
    state: State,
}

impl Default for Reader {
    fn default() -> Self {
        Self::new()
    }
}

/// End (exclusive) of the JSON value starting at `start`, `None` when the
/// buffer ends first. `eof`: no more bytes will come (a number at the very
/// end is complete).
fn value_end(b: &[u8], start: usize, eof: bool) -> Option<usize> {
    let first = *b.get(start)?;
    match first {
        b'"' => string_end(b, start),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = start;
            while i < b.len() {
                match b[i] {
                    b'"' => {
                        i = string_end(b, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        _ => {
            let mut i = start;
            while i < b.len() {
                if matches!(b[i], b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t') {
                    return Some(i);
                }
                i += 1;
            }
            eof.then_some(i)
        }
    }
}

fn string_end(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

impl Reader {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            pos: 0,
            state: State::Start,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        // Drop what was consumed before growing the buffer.
        if self.pos > 0 && self.pos * 2 >= self.buf.len() {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    fn skip_ws(&mut self) {
        while self
            .buf
            .get(self.pos)
            .is_some_and(|c| c.is_ascii_whitespace())
        {
            self.pos += 1;
        }
    }

    fn bad(&self, what: &str) -> String {
        let near =
            String::from_utf8_lossy(&self.buf[self.pos..(self.pos + 40).min(self.buf.len())])
                .into_owned();
        format!("query service response: expected {what} near `{near}`")
    }

    /// The next event, `Ok(None)` when more bytes are needed.
    pub fn next(&mut self, eof: bool) -> Result<Option<Event>, String> {
        loop {
            self.skip_ws();
            if self.pos >= self.buf.len() {
                return if eof && self.state != State::Done {
                    Err("query service response ended early".into())
                } else {
                    Ok(None)
                };
            }
            let c = self.buf[self.pos];
            match self.state.clone() {
                State::Start => {
                    if c != b'{' {
                        return Err(self.bad("`{`"));
                    }
                    self.pos += 1;
                    self.state = State::Key;
                }
                State::Key => {
                    if c == b'}' {
                        self.pos += 1;
                        self.state = State::Done;
                        return Ok(None);
                    }
                    let Some(end) = value_end(&self.buf, self.pos, eof) else {
                        return Ok(None);
                    };
                    let key: String = serde_json::from_slice(&self.buf[self.pos..end])
                        .map_err(|_| self.bad("a field name"))?;
                    self.pos = end;
                    self.state = State::Value(key);
                }
                State::Value(key) => {
                    if c == b':' {
                        self.pos += 1;
                        continue;
                    }
                    if key == "results" && c == b'[' {
                        self.pos += 1;
                        self.state = State::Results { first: true };
                        return Ok(Some(Event::ResultsStart));
                    }
                    let Some(end) = value_end(&self.buf, self.pos, eof) else {
                        return Ok(None);
                    };
                    let v: Json = serde_json::from_slice(&self.buf[self.pos..end])
                        .map_err(|e| format!("query service response: {key}: {e}"))?;
                    self.pos = end;
                    self.state = State::AfterValue;
                    return Ok(Some(Event::Field(key, v)));
                }
                State::Results { first } => {
                    if c == b']' {
                        self.pos += 1;
                        self.state = State::AfterValue;
                        return Ok(Some(Event::ResultsEnd));
                    }
                    if c == b',' && !first {
                        self.pos += 1;
                        continue;
                    }
                    let Some(end) = value_end(&self.buf, self.pos, eof) else {
                        return Ok(None);
                    };
                    let item = self.buf[self.pos..end].to_vec();
                    self.pos = end;
                    self.state = State::Results { first: false };
                    return Ok(Some(Event::Result(item)));
                }
                State::AfterValue => {
                    match c {
                        b',' => self.state = State::Key,
                        b'}' => self.state = State::Done,
                        _ => return Err(self.bad("`,` or `}`")),
                    }
                    self.pos += 1;
                }
                State::Done => return Ok(None),
            }
        }
    }
}

/// JSON → BSON: integers as int32 / int64 (larger ones as decimal128),
/// other numbers as doubles (the query service computes in float64).
pub fn to_bson(v: Json) -> Bson {
    match v {
        Json::Null => Bson::Null,
        Json::Bool(b) => Bson::Boolean(b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                match i32::try_from(i) {
                    Ok(s) => Bson::Int32(s),
                    Err(_) => Bson::Int64(i),
                }
            } else if n.is_u64() {
                n.to_string()
                    .parse::<Decimal128>()
                    .map(Bson::Decimal128)
                    .unwrap_or_else(|_| Bson::Double(n.as_f64().unwrap_or(f64::NAN)))
            } else {
                Bson::Double(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        Json::String(s) => Bson::String(s),
        Json::Array(a) => Bson::Array(a.into_iter().map(to_bson).collect()),
        Json::Object(o) => Bson::Document(o.into_iter().map(|(k, x)| (k, to_bson(x))).collect()),
    }
}

/// A result element as a document; scalars (`SELECT RAW …`) and arrays
/// (`INFER`) are wrapped as `{ value: … }`.
pub fn to_document(bytes: &[u8]) -> Result<Document, String> {
    let v: Json = serde_json::from_slice(bytes).map_err(|e| format!("result: {e}"))?;
    Ok(match to_bson(v) {
        Bson::Document(d) => d,
        other => {
            let mut d = Document::new();
            d.insert("value", other);
            d
        }
    })
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    /// A response recorded from Couchbase Server 8.0 (community).
    const RECORDED: &str = r#"{
"requestID": "c02ece95-5c73-4203-92c4-47dcabd2f920",
"clientContextID": "ferrodb-1",
"signature": {"*":"*"},
"results": [
{"docs":{"a":1,"b":{"c":"x, ]}"},"big":12345678901234567890,"f":1.5}},
{"docs":{"a":-2,"s":"quote \" and \\ backslash"}}
],
"status": "success",
"metrics": {"elapsedTime": "7.16468ms","executionTime": "6.949709ms","resultCount": 2,"resultSize": 30,"serviceLoad": 6}
}
"#;

    fn events(chunks: &[&[u8]]) -> Vec<Event> {
        let mut r = Reader::new();
        let mut out = Vec::new();
        for (i, c) in chunks.iter().enumerate() {
            r.feed(c);
            let eof = i + 1 == chunks.len();
            while let Some(e) = r.next(eof).unwrap() {
                out.push(e);
            }
        }
        assert!(r.is_done());
        out
    }

    #[test]
    fn results_stream_out_whatever_the_chunking() {
        let whole = events(&[RECORDED.as_bytes()]);
        assert_eq!(whole.len(), 9, "{whole:?}");
        assert_eq!(whole[3], Event::ResultsStart);
        // One byte at a time gives the same events.
        let bytes: Vec<&[u8]> = RECORDED.as_bytes().chunks(1).collect();
        assert_eq!(events(&bytes), whole);
        let Event::Result(first) = &whole[4] else {
            panic!()
        };
        let d = to_document(first).unwrap();
        let inner = d.get_document("docs").unwrap();
        assert_eq!(inner.get("a"), Some(&Bson::Int32(1)));
        assert_eq!(inner.get_document("b").unwrap(), &doc! { "c": "x, ]}" });
        assert!(matches!(inner.get("big"), Some(Bson::Decimal128(_))));
        assert_eq!(inner.get("f"), Some(&Bson::Double(1.5)));
        let Event::Field(k, v) = &whole[7] else {
            panic!()
        };
        assert_eq!(k, "status");
        assert_eq!(v, "success");
    }

    #[test]
    fn errors_and_scalars() {
        let e = events(&[br#"{"requestID": "x", "errors": [{"code":3000,"msg":"syntax error","line":1,"column":30}], "status": "fatal"}"#]);
        assert!(matches!(&e[1], Event::Field(k, _) if k == "errors"));
        assert_eq!(to_document(b"42").unwrap(), doc! { "value": 42 });
        let mut r = Reader::new();
        r.feed(b"{\"results\": [1, 2");
        assert_eq!(r.next(false).unwrap(), Some(Event::ResultsStart));
        assert_eq!(r.next(false).unwrap(), Some(Event::Result(b"1".to_vec())));
        // `2` may continue in the next chunk.
        assert_eq!(r.next(false).unwrap(), None);
        assert_eq!(r.next(true).unwrap(), Some(Event::Result(b"2".to_vec())));
        assert!(r.next(true).is_err());
        assert!(Reader::new().next(true).is_err());
    }
}
