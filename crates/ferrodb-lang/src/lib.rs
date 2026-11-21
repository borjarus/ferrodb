//! # ferrodb-lang
//!
//! Query language support.
//! - [`sql`]: statement splitting, parameters and statement analysis
//!   (phase 3); the intelligent editor of phase 5 — dialects as data,
//!   structural parser, name resolution, completion, inspections with quick
//!   fixes, formatter, rename, structure view, parameter info, live
//!   templates;
//! - [`mql`]: the MongoDB console language (phase 4b);
//! - [`cql`], [`sqlpp`], [`partiql`], [`redis`]: the level B languages of
//!   Cassandra, Couchbase, DynamoDB and Redis (phase 8);
//! - [`service`]: the [`LanguageService`](ferrodb_api::LanguageService)
//!   implementations on top of both.

pub mod cql;
pub mod mql;
pub mod partiql;
pub mod redis;
pub mod service;
pub mod sql;
pub mod sqlpp;
mod util;

pub use service::{BasicLanguageService, Engine};
