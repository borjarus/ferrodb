# ADR-005: Access layers — native, ADBC, JDBC

- Status: **Accepted**
- Date: 2026-11-05

## Decision

Each database uses exactly one primary access layer (`AccessLayer`). Choose it by these rules, in order:

1. **Native Rust** when a mature async or embedded Rust driver exists that gives us server-side cancellation, raw types and server messages. **15 databases:** the PostgreSQL family (4), MySQL family (2), SQLite, the TDS family (2), ClickHouse, Cassandra, MongoDB, Couchbase, DynamoDB, Redis.
2. **ADBC** when the vendor maintains an ADBC driver that returns Arrow directly, typically cloud warehouses. **2 databases:** Snowflake, BigQuery.
3. **JDBC bridge** when no good Rust driver exists, or a native client library would be needed (for example, Oracle without Instant Client). **9 databases:** Oracle, Db2, Sybase ASE, H2, Derby, HyperSQL, Exasol, Vertica, Hive. Custom JDBC drivers also go here, at language level C.
4. **ODBC** (`odbc-api`) remains an optional, level-C fallback only.

All three layers return the same result types (ADR-004), so the GUI cannot tell which layer it is talking to.

## Evidence (phase 0)

- **ADBC spike** (`spikes/adbc`): `adbc_driver_manager 0.25` (Arrow 58–60, compatible with our `arrow 60.1`) loads a driver by file name or library name and streams `RecordBatchReader`. Running without a driver gives a clean error (`LoadLibraryExW failed`). The end-to-end measurement with `adbc_driver_sqlite` must be run by a developer who has the driver library. Command: `cargo xtask spike adbc -- <path/to/adbc_driver_sqlite> :memory: 1000000`. ADBC libraries are not bundled; the driver manager downloads them (ADR-007).
- **JDBC spike:** see ADR-006.

## Consequences

- There are three integration paths to maintain. Every driver must pass the same TCK (`ferrodb-driver-tck`, phase 3).
- No JVM is needed until the first connection to a JDBC database.
