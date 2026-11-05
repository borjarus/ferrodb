# ADR-003: Data models and `Capabilities`

- Status: **Accepted**
- Date: 2026-11-05

## Context

Version 1.0 covers 26 databases: relational, analytical, wide-column, document and key-value. If we abstract over the lowest common denominator, the result is a poor tool for every one of them.

## Decision

- The contract defines three data models from the start: `DataModel::{Tabular, Document, KeyValue}`. Cassandra counts as *tabular*: CQL returns rows.
- Each driver declares `Capabilities`:
  - data model, query language and access layer;
  - language level (A/B/C) and transaction support;
  - schema kind (strict / inferred / keys only);
  - joins, explain support, server-side cancellation, editable results, cost estimate;
  - authentication methods.
- **The GUI enables features based on `Capabilities` and never on the database name.** For example, the cost dialog appears only when `cost_estimate` is set. JOIN suggestions appear only when `joins` is set.
- The explorer tree shape comes from `HierarchySpec`, a list of levels and child kinds for each source. Examples: database → schema → table; keyspace → table; bucket → scope → collection; key prefixes split on a separator.
- The query language belongs to the data source: `QueryLanguage::{Sql(dialect), Mql, Cql, SqlPlusPlus, PartiQl, RedisCommands}`.

## Evidence

- `ferrodb-mock/src/catalog.rs` describes all 26 databases. A test checks the layer split: 15 native, 2 ADBC, 9 JDBC.
- Five mock profiles exercise each model end to end: PostgreSQL-like, BigQuery-like, Cassandra-like, MongoDB-like and Redis-like (`each_profile_produces_its_model`).

## Consequences

- Each new capability is a contract change, and `ferrodb_api::API_VERSION` is bumped when the change breaks compatibility.
- Some views will be specific to a model (document browser, key browser). That is intentional.
