//! # ferrodb-api v3
//!
//! Contracts between the GUI and the logic. The GUI talks to the rest of the
//! application **exclusively** through [`Backend`]; in phases 1–2 it is
//! implemented by `ferrodb-mock`, from phase 3 by `ferrodb-core`.
//!
//! Design rules (ADR-003):
//! - three data models from day one: [`DataModel::Tabular`],
//!   [`DataModel::Document`], [`DataModel::KeyValue`];
//! - features are declared by [`Capabilities`], never inferred from the database name;
//! - every trait is dyn-compatible (futures are boxed);
//! - nothing here depends on the GUI library or on an async runtime.

mod backend;
mod capabilities;
mod datasource;
mod drivers;
mod error;
mod history;
mod ids;
mod language;
mod metadata;
mod query;
mod search;

pub use backend::*;
pub use capabilities::*;
pub use datasource::*;
pub use drivers::*;
pub use error::*;
pub use history::*;
pub use ids::*;
pub use language::*;
pub use metadata::*;
pub use query::*;
pub use search::*;

/// Re-exports so implementors and the UI use the same versions.
pub use arrow_array;
pub use arrow_schema;
pub use bson;
pub use futures::future::BoxFuture;

/// Version of the contract. Bumped on every breaking change of this crate.
pub const API_VERSION: u32 = 3;
