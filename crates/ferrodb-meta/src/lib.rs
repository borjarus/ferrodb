//! # ferrodb-meta
//!
//! Metadata for the Database Explorer, the console and Search Everywhere
//! (plan, phase 4 and 4b):
//!
//! - [`model`] — catalogs, containers (schemas / databases), tables,
//!   columns, keys, indexes, routines, sequences, MongoDB collections;
//! - [`Introspector`] — reads a container through a driver
//!   [`Connection`] (PostgreSQL, MySQL/MariaDB, SQLite from their system
//!   catalogs; MongoDB from `listCollections` / `listIndexes`);
//! - [`cache`] — catalogs in memory and on disk, so the explorer starts
//!   from the cache without connecting;
//! - [`ddl`] — DDL (single and bulk, in dependency order), object
//!   snippets and procedure calls;
//! - [`tree`] — hierarchy per database, explorer children, name
//!   resolution, quick documentation, search entries, schema filter;
//! - [`diff`] — schema comparison, migration scripts, `ALTER TABLE` for
//!   the table editor, the ER model;
//! - [`infer`] — schema inference for document collections.
//!
//! Introspectors live here, not in the driver crate: they only need a
//! connection that runs queries, and keeping them here avoids a dependency
//! cycle (ADR-008 amendment in `docs/phase-4.md`).

pub mod cache;
pub mod cassandra;
mod clickhouse;
mod cloud;
pub mod couchbase;
pub mod ddl;
pub mod diff;
mod duckdb;
pub mod dynamodb;
pub mod infer;
mod jdbc;
pub mod model;
pub mod mongo;
mod mysql;
mod pg;
mod pg_flavors;
pub mod redis;
pub mod rows;
pub mod sql_model;
mod sqlite;
mod sqlserver;
pub mod tree;

pub use cache::Catalogs;
use ferrodb_api::Result;
use ferrodb_drivers::{BoxFuture, Connection};
pub use model::*;

/// Reads metadata of one database product.
pub trait Introspector: Send + Sync {
    fn dialect(&self) -> Dialect;

    /// Top-level containers (schemas / databases), without content.
    fn containers<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
    ) -> BoxFuture<'a, Result<Vec<Container>>>;

    /// Content of one container. With `only`, just that table / collection
    /// (incremental refresh); routines and sequences are then left empty.
    fn content<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
        container: &'a str,
        only: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Content>>;
}

/// The introspector for a dialect.
pub fn introspector(d: Dialect) -> &'static dyn Introspector {
    static MYSQL: mysql::MySqlIntrospector = mysql::MySqlIntrospector { mariadb: false };
    static MARIADB: mysql::MySqlIntrospector = mysql::MySqlIntrospector { mariadb: true };
    match d {
        Dialect::Postgres => &pg::PgIntrospector,
        Dialect::MySql => &MYSQL,
        Dialect::MariaDb => &MARIADB,
        Dialect::Sqlite => &sqlite::SqliteIntrospector,
        Dialect::Mongo => &mongo::MongoIntrospector,
        Dialect::DuckDb => &duckdb::DuckDbIntrospector,
        // Phase 8 introspectors are added per database.
        Dialect::SqlServer => &sqlserver::SqlServerIntrospector,
        Dialect::Sybase => &jdbc::JdbcIntrospector(Dialect::Sybase),
        Dialect::Oracle => &jdbc::JdbcIntrospector(Dialect::Oracle),
        Dialect::Db2 => &jdbc::JdbcIntrospector(Dialect::Db2),
        Dialect::H2 => &jdbc::JdbcIntrospector(Dialect::H2),
        Dialect::Derby => &jdbc::JdbcIntrospector(Dialect::Derby),
        Dialect::Hsqldb => &jdbc::JdbcIntrospector(Dialect::Hsqldb),
        Dialect::Exasol => &jdbc::JdbcIntrospector(Dialect::Exasol),
        Dialect::Vertica => &jdbc::JdbcIntrospector(Dialect::Vertica),
        Dialect::Hive => &jdbc::JdbcIntrospector(Dialect::Hive),
        Dialect::Snowflake => &cloud::SnowflakeIntrospector,
        Dialect::BigQuery => &cloud::BigQueryIntrospector,
        Dialect::ClickHouse => &clickhouse::ClickHouseIntrospector,
        Dialect::Cassandra => &cassandra::CassandraIntrospector,
        Dialect::Couchbase => &couchbase::CouchbaseIntrospector,
        Dialect::DynamoDb => &dynamodb::DynamoIntrospector,
        Dialect::Redis => &redis::RedisIntrospector,
    }
}

/// Merges a refreshed container (or one table of it) into `content`.
pub fn merge(content: &mut Content, fresh: Content, only: Option<&str>) {
    match only {
        None => *content = fresh,
        Some(name) => {
            content.tables.retain(|t| t.name != name);
            content.collections.retain(|c| c.name != name);
            content.tables.extend(fresh.tables);
            content.tables.sort_by(|a, b| a.name.cmp(&b.name));
            content.collections.extend(fresh.collections);
            content.collections.sort_by(|a, b| a.name.cmp(&b.name));
            content.refreshed_at = fresh.refreshed_at;
        }
    }
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_native;
