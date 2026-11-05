use std::ops::Range;

use serde::{Deserialize, Serialize};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors crossing the contract boundary. They must be displayable to the user
/// as-is, so they never carry secrets.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("not supported by this data source: {0}")]
    Unsupported(String),
    #[error("connection failed: {0}")]
    Connection(String),
    #[error("connection lost")]
    ConnectionLost,
    #[error("driver `{0}` is not downloaded")]
    DriverMissing(String),
    #[error("cancelled")]
    Cancelled,
    #[error("timeout")]
    Timeout,
    #[error("{0}")]
    Query(QueryError),
    #[error("data source is read-only")]
    ReadOnly,
    #[error("{0}")]
    Other(String),
}

/// A server or parse error, with an optional position in the script.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryError {
    pub message: String,
    /// Vendor code: SQLSTATE, MongoDB code, Redis error prefix…
    pub code: Option<String>,
    /// Byte range in the submitted script.
    pub range: Option<Range<usize>>,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.code {
            Some(code) => write!(f, "[{code}] {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}
