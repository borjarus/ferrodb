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
