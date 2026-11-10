# ADR-008: Driver families and database variants

- Status: **Accepted**
- Date: 2026-11-05

## Decision

- A **family** is shared protocol code (`FamilyId`):
  - `pgwire`: PostgreSQL, Greenplum, CockroachDB, Redshift
  - `mysql`: MySQL, MariaDB
  - `tds`: SQL Server, Azure SQL
  - `jdbc`: all bridge databases
  - `adbc`: Snowflake, BigQuery
  - plus single-database families
- A **variant** (`FamilyVariant`) overrides only what differs: introspector, dialect id, authentication methods and `Quirks`. Examples: missing catalog functions in Redshift, transaction retries in CockroachDB.
- Dialects are data: `assets/dialects/<id>.toml` (ADR-009). Derived dialects inherit from a base dialect: Greenplum and CockroachDB from PostgreSQL, Azure SQL and Sybase from T-SQL.
- Every variant passes the same TCK. Expectations depend on `Capabilities`.

## Consequences

- Adding a PostgreSQL-compatible database means adding a variant, not a driver.
- A protocol bug fixed in a family is fixed for every variant at once.
- Each family has a single owner on the team.

## Amendment (2026-11-10, phase 4)

- **Introspectors live in `ferrodb-meta`, not in the variants.** An introspector only needs a `Connection` that runs queries. If it lived in the driver crate, the metadata model would also have to live there, or `ferrodb-meta` would depend on drivers that depend on it.
- `ferrodb_meta::introspector(Dialect)` selects one from the source's language: one for the PostgreSQL family, one for MySQL/MariaDB with a product flag, one for SQLite and one for MongoDB.
- `FamilyVariant::list_queries` stays as the fallback for sources without an introspector. The TCK `explorer-listing` scenario still checks it.
- MongoDB is a single-database family (`mongodb`). Its requests are parsed shell statements (`Request::Mql`, ADR-010), not SQL text.
