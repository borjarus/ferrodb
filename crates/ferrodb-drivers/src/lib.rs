//! # ferrodb-drivers
//!
//! The access layer below `ferrodb-core` (plan, section 5.4):
//!
//! - [`Driver`] — one database product; describes itself
//!   ([`DriverDescriptor`]) and opens [`Connection`]s;
//! - [`Connection`] — one server session; executes [`Request`]s, streaming
//!   results into a [`ResultSink`] as Arrow record batches;
//! - [`FamilyVariant`] — what differs between databases sharing a protocol
//!   (PostgreSQL vs Redshift, MySQL vs MariaDB): dialect, authentication,
//!   quirks, object hierarchy (ADR-008);
//! - [`Registry`] — the drivers compiled into this build.
//!
//! Families are behind feature flags: `sqlite`, `pgwire`, `mysql`, `mongodb`.

pub mod columns;
pub mod config;
pub mod registry;
pub mod sink;

#[cfg(feature = "mongodb")]
pub mod mongodb;
#[cfg(feature = "mysql")]
pub mod mysql;
#[cfg(feature = "pgwire")]
pub mod pgwire;
#[cfg(feature = "sqlite")]
pub mod sqlite;

use std::sync::Arc;

pub use config::{Endpoint, ResolvedConfig};
use ferrodb_api::{
    AuthMethod, Capabilities, DriverDescriptor, DriverId, FamilyId, HierarchySpec, ObjectKind,
    Requirement, SqlDialectId,
};
pub use ferrodb_api::{BoxFuture, Error, Result};
pub use registry::Registry;
pub use sink::{ResultSink, SinkEvent};
pub use tokio_util::sync::CancellationToken;

/// One database product (`"postgresql"`, `"mariadb"`…).
pub trait Driver: Send + Sync {
    fn descriptor(&self) -> DriverDescriptor;
    fn variant(&self) -> Arc<dyn FamilyVariant>;
    /// Opens a connection. `cfg` already holds secrets from the keyring.
    fn connect<'a>(&'a self, cfg: &'a ResolvedConfig)
    -> BoxFuture<'a, Result<Box<dyn Connection>>>;

    fn id(&self) -> DriverId {
        self.descriptor().id
    }
    fn family(&self) -> FamilyId {
        self.descriptor().family
    }
    fn capabilities(&self) -> Capabilities {
        self.descriptor().capabilities
    }
    fn requirements(&self) -> Vec<Requirement> {
        self.descriptor().requirements
    }
}

/// What a connection is asked to run. Parameters are already substituted
/// (`ferrodb_lang::sql::substitute`), so the text is final.
// A request is built once and moved into the driver: its size is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// One statement in the source's language (SQL, CQL, SQL++, PartiQL;
    /// MongoDB parses MQL text itself).
    Text { text: String },
    /// A parsed MongoDB shell statement.
    Mql(ferrodb_lang::mql::Command),
}

impl Request {
    /// The SQL text of a text request; MQL is not SQL.
    pub fn into_text(self) -> Result<String> {
        match self {
            Self::Text { text } => Ok(text),
            Self::Mql(_) => Err(Error::Unsupported(
                "MongoDB commands on an SQL connection".into(),
            )),
        }
    }
}

/// Outcome of one request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecSummary {
    /// Rows affected by DML (`None` for queries and DDL).
    pub affected: Option<u64>,
    /// Result sets streamed into the sink.
    pub results: usize,
    /// The sink was closed by the consumer before all rows were read.
    pub truncated: bool,
}

/// Server-side cancellation, usable while the connection is busy executing
/// (PostgreSQL cancel request, MySQL `KILL QUERY`, SQLite interrupt).
pub trait RemoteCancel: Send + Sync {
    fn cancel(&self) -> BoxFuture<'static, Result<()>>;
}

/// One open server session. Methods take `&mut self`: the core serializes
/// access (one statement at a time per session).
pub trait Connection: Send {
    /// Server product and version, e.g. `PostgreSQL 18.1`.
    fn server_version(&self) -> String;

    /// Executes one statement, streaming result sets into `sink`. Must return
    /// promptly with [`Error::Cancelled`] once `cancel` fires.
    fn execute<'a>(
        &'a mut self,
        req: Request,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ExecSummary>>;

    /// A handle that cancels the running statement from another task.
    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>>;

    /// Makes the session read-only at the server where supported.
    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>>;

    /// Default schema / database for subsequent statements.
    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>>;

    /// Current schema / database.
    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>>;

    fn begin(&mut self) -> BoxFuture<'_, Result<()>>;
    fn commit(&mut self) -> BoxFuture<'_, Result<()>>;
    fn rollback(&mut self) -> BoxFuture<'_, Result<()>>;

    /// Cheap liveness check (connection pools, "Test connection").
    fn ping(&mut self) -> BoxFuture<'_, Result<()>>;

    /// `false` once the transport is known to be dead.
    fn is_alive(&self) -> bool {
        true
    }
}

/// Differences between databases of one family (ADR-008).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Quirks {
    /// DDL participates in transactions (PostgreSQL, SQLite) or commits
    /// implicitly (MySQL, Oracle).
    pub transactional_ddl: bool,
    /// The server can enforce read-only sessions.
    pub read_only_sessions: bool,
}

/// A query listing objects under a node of the explorer tree (phase-3
/// level; full introspection arrives in phase 4). The query returns one row
/// per object: `name` and optionally `detail` as the second column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListQuery {
    pub sql: String,
    pub kind: ObjectKind,
    /// Listed objects can be expanded.
    pub has_children: bool,
}

pub trait FamilyVariant: Send + Sync {
    fn driver_id(&self) -> DriverId;
    fn dialect(&self) -> SqlDialectId;
    fn auth(&self) -> Vec<AuthMethod>;
    fn quirks(&self) -> Quirks;
    fn hierarchy(&self) -> HierarchySpec;
    /// Listing queries for the children of `path` (empty = root), one per
    /// child kind.
    fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery>;
}

/// Quotes a SQL string literal (`'` doubled).
pub fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Quotes an identifier with `q` (`"` or `` ` ``), doubling embedded quotes.
pub fn quote_ident(s: &str, q: char) -> String {
    let d = format!("{q}{q}");
    format!("{q}{}{q}", s.replace(q, &d))
}
