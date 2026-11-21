//! The bridge protocol (see `java/jdbc-host/.../Wire.java`): frames
//! `[u8 kind][u64 id][u32 length][payload]`, big endian, on the host's
//! stdin / stdout. JSON-RPC requests and answers, JSON events, Arrow IPC
//! stream bytes and credits (backpressure) share one pipe; the id names the
//! request a frame belongs to.

use ferrodb_api::{Error, QueryError};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};

pub const REQUEST: u8 = 1;
pub const RESPONSE: u8 = 2;
pub const EVENT: u8 = 3;
pub const DATA: u8 = 4;
pub const CREDIT: u8 = 5;

/// Protocol version spoken by this client (`hello` answers the host's).
pub const PROTOCOL: u64 = 1;

/// Largest frame accepted (a record batch of wide rows stays far below).
const MAX_FRAME: u32 = 256 << 20;

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
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await?;
    Ok(Some(Frame {
        kind: head[0],
        id,
        payload,
    }))
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
                        let start = char_to_byte(s, p);
                        Some(start..(start + 1).min(s.len().max(start)))
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
}
