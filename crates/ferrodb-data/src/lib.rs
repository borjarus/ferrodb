//! # ferrodb-data
//!
//! Result data outside the drivers (plan, section 5.6 and phase 6):
//!
//! - [`value`] — a typed scalar read from Arrow cells and BSON values and
//!   converted to the type of a target column;
//! - [`sql`] — literals, DML, `CREATE TABLE` and `COPY` / `LOAD DATA` text
//!   per SQL dialect;
//! - [`edit`] — tabular change sets → `UPDATE` / `INSERT` / `DELETE`;
//! - [`export`] — streaming writers for ten file formats;
//! - [`import`] — readers for CSV/TSV, XLSX, Parquet and JSON, and the
//!   source → target column [`import::Mapping`];
//! - [`docs`] — rows ↔ documents (copying between data models);
//! - [`doc_edit`] — document edits as SQL++ (Couchbase) and PartiQL
//!   (DynamoDB) statements;
//! - [`compare`] — comparing two results;
//! - [`chart`] — chart series and selection aggregates;
//! - [`plan`] — execution plans of each database → the shared plan model
//!   ([`plan_jdbc`]: Oracle `PLAN_TABLE` and text plans of the JDBC databases).
//!
//! No I/O besides the files being exported or imported; nothing here
//! talks to a database.

pub mod chart;
pub mod compare;
pub mod cql;
pub mod doc_edit;
pub mod docs;
pub mod edit;
pub mod export;
pub mod import;
pub mod plan;
pub mod plan_mssql;
pub mod plan_text;
pub mod plan_jdbc;
pub mod sql;
pub mod value;

pub use value::{Value, ValueType};

#[cfg(test)]
mod tests;
