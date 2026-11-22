# Phase 8 — Coverage of the 26 databases: status

Scope: plan, section 7, phase 8. Every database of the 1.0 list (plan 2.8) gets a real driver, an introspector, an edit generator, execution plans where the database has them, its dialect or language, and a place in the driver TCK: the pgwire variants and TDS (8.1), the JDBC bridge and driver manager (8.2), the JDBC databases (8.3), ADBC with Snowflake and BigQuery plus native ClickHouse (8.4), the NoSQL databases Redis, Cassandra, DynamoDB and Couchbase (8.5), and the dialect files and level B languages (8.6).

The work was done in six packages in parallel and merged on `phase-8`. Completion is shown by the TCK on real servers where a container exists (table at the end) and by unit tests on recorded server outputs everywhere else. `tests_catalog::every_database_of_the_catalog_is_registered` checks that the real registry has all 26 databases of the UI's catalog with the same family, data model, language, access layer, language level and authentication methods.

## What was built

### 8.1 pgwire variants and TDS

| Plan item | Where | Notes |
|---|---|---|
| Greenplum, CockroachDB, Redshift | `ferrodb-drivers/src/pgwire/{mod,conn}.rs` | Flavors of the PostgreSQL family, told apart by the `crdb_version` parameter or `version()`. CockroachDB: statements failing with 40001 in auto-commit are retried (`option.retries`, backoff, notice); inside a transaction the error asks for ROLLBACK; internal schemas and hidden columns skipped; DDL is not transactional. Redshift: listings through `svv_all_*` (external schemas), client-side read-only guard |
| Redshift IAM | `ferrodb-drivers/src/aws.rs`, `http.rs` | `GetClusterCredentials` (DbUser, AutoCreate, DbGroups), `GetClusterCredentialsWithIAM`, Serverless `GetCredentials`, signed with a hand-written SigV4 (checked against the AWS example). Credentials: properties, environment, `~/.aws` files, `credential_process`, `aws configure export-credentials` (SSO, assumed roles) |
| SQL Server and Azure SQL (TDS) | `ferrodb-drivers/src/tds/` (feature `tds`), `third_party/tiberius` | SQL login, Windows SSPI / NTLM, Microsoft Entra ID (token, client credentials, `az`). Every type to Arrow (exact `money`, `datetimeoffset` in UTC, `sql_variant` as text). Several result sets, row counts, PRINT / RAISERROR as notices, errors with code and line range. `GO [n]` batches. Cancel through TDS attention (the session survives). Read-only: ApplicationIntent plus a guard |
| Introspection | `ferrodb-meta/src/sqlserver.rs`, `pg_flavors.rs` | SQL Server from `sys.*`: tables, views, computed columns, identity, collations, `MS_Description` comments, keys, checks, clustered / columnstore / filtered indexes, routines, sequences, triggers, synonyms. Greenplum distribution and storage, CockroachDB `SHOW CREATE TABLE`, Redshift DISTSTYLE / DISTKEY / SORTKEY and external tables |
| DDL, ALTER | `ferrodb-meta/src/{ddl,diff}.rs` | T-SQL: brackets, `IDENTITY`, `PERSISTED`, `NONCLUSTERED` primary keys, `sp_rename`, defaults dropped by name; Greenplum / Redshift table options |
| Plans | `ferrodb-data/src/{plan_mssql,plan_text}.rs` | SQL Server XML showplans (estimated and actual) with missing indexes, implicit conversions, spills, key lookups; CockroachDB `EXPLAIN (VERBOSE)` trees; Redshift text plans with `DS_BCAST_INNER` / `DS_DIST_BOTH`; Greenplum Motion nodes |
| Edits | `ferrodb-data/src/sql.rs` | `Family::SqlServer`: `N'…'`, `0x…`, `UPDATE/DELETE TOP (1)`, `IDENTITY_INSERT`, `OUTPUT INSERTED.*` |

### 8.2 / 8.3 JDBC bridge, driver manager, JDBC databases

| Plan item | Where | Notes |
|---|---|---|
| Java host | `java/jdbc-host` (Maven) | `ferrodb-jdbc-host.jar`: JSON-RPC requests on stdin, Arrow IPC result streams on stdout with credits (backpressure), `Statement.cancel`, `DatabaseMetaData`, one process for every JDBC connection |
| Bridge client | `crates/ferrodb-jdbc` | Starts the host lazily, watches it, restarts it after a crash (`BridgeState` in Services); connections of a dead host report a connection error. `Request::Native` for metadata calls and plans |
| JDBC flavors | `ferrodb-jdbc/src/flavor.rs` | Oracle, Db2, Sybase ASE, H2, Derby, HyperSQL, Exasol, Vertica, Hive (and custom drivers, level C): descriptors, URLs (embedded file / in-memory URLs for H2, Derby, HyperSQL), explorer queries, schema switching |
| Driver manager | `crates/ferrodb-driver-manager`, `catalog.toml` | Catalog of artifacts with Maven Central coordinates, versions, SHA-256, licenses; downloads with progress, verified before install; offline install (verified) and custom jars (unverified); Java runtime found on the system or a Temurin JRE downloaded. Native libraries (ADBC) use the same catalog |
| Introspection, DDL | `ferrodb-meta/src/jdbc.rs`, `ddl.rs` | One introspector over `DatabaseMetaData` plus per-database catalog queries; identity, generated columns, `COMMENT ON`, sequences, Oracle synonyms |
| Plans, edits | `ferrodb-data/src/plan_jdbc.rs`, `sql.rs`, `core/src/explain.rs` | Oracle `PLAN_TABLE`, text plans of H2, HyperSQL, Derby, Db2, Vertica, Exasol, Hive, Sybase. Edit families Oracle (`HEXTORAW`, typed date literals), Sybase, Hive, Vertica |

### 8.4 ADBC, Snowflake, BigQuery, ClickHouse

| Plan item | Where | Notes |
|---|---|---|
| Generic ADBC driver | `crates/ferrodb-adbc` | `adbc_driver_manager` 0.25 on a dedicated thread; batches re-chunked with backpressure, `AdbcStatementCancel`, autocommit transactions, `Request::Native` for `get_objects`, `get_table_schema`, `get_info`, `get_table_types` |
| Snowflake, BigQuery | `ferrodb-adbc/src/{flavor,bigquery}.rs` | Snowflake: password, key pair (encrypted or not), `externalbrowser` SSO; warehouses, roles, `USE …` as native calls. BigQuery: service-account JSON or ADC; dry-run cost estimate (`QueryService::estimate_cost`), jobs and `jobs.cancel` through the REST API (RS256 JWT with `ring`) |
| Libraries on demand | `ferrodb-adbc/src/artifacts.rs`, `core/src/adbc.rs` | The PyPI wheels of the Apache Arrow ADBC drivers (1.11.0, Windows / Linux / macOS, x86_64 / aarch64) with SHA-256 become driver manager artifacts; the library is extracted from the wheel under the platform's name and loaded through the driver manager (`ManagerLocator`), an explicit `driver.library` path or `FERRODB_ADBC_<DRIVER>` |
| ClickHouse | `ferrodb-drivers/src/clickhouse/` (feature `clickhouse`) | HTTP interface with `FORMAT ArrowStream`; Array, Nested, Map, Tuple, LowCardinality, Nullable, Decimal, DateTime64, UUID, Enum mapped; `query_id` + `KILL QUERY` for cancel; `session_id` for `USE` and settings; `readonly=1`; errors with code and position; bulk load fast path; no transactions |
| Introspection, plans, edits | `ferrodb-meta/src/{clickhouse,cloud}.rs`, `ferrodb-data/src/plan/cloud.rs`, `sql.rs` | ClickHouse engines, partition / sorting keys, TTL, materialized views, DDL from `create_table_query`; Snowflake and BigQuery from `INFORMATION_SCHEMA` / `GetObjects`. Plans: ClickHouse `EXPLAIN json = 1`, Snowflake `EXPLAIN USING JSON`, BigQuery job stages. Edits: ClickHouse mutations (`mutations_sync = 1`) and lightweight `DELETE`, Snowflake, BigQuery |

### 8.5 NoSQL: Redis, Cassandra, DynamoDB, Couchbase

| Plan item | Where | Notes |
|---|---|---|
| Redis | `ferrodb-drivers/src/redis/` (feature `redis`), `ferrodb-drivers/src/kv.rs`, `core/src/kv.rs`, `ferrodb-meta/src/redis.rs` | Standalone (TLS, ACL, db index, SSH tunnel), Cluster (slot routing, SCAN over every master, MULTI/EXEC as one pipeline), Sentinel. RESP3 replies as tables; read-only from a command table, dangerous commands (FLUSHALL, CONFIG SET…) flagged; blocking commands released with `CLIENT UNBLOCK`. Key browser: databases → prefixes → keys, paged SCAN, values and TTL, edits with preview |
| Cassandra | `ferrodb-drivers/src/cql/` (feature `cql`), `ferrodb-meta/src/cassandra.rs`, `ferrodb-data/src/cql.rs` | `scylla` driver: contact points, `local_dc`, consistency per console, paging state per batch, server warnings as notices. `system_schema` introspection (keys, clustering order, indexes, views, UDTs, functions) and `CREATE TABLE … WITH CLUSTERING ORDER`. Edits by full primary key |
| DynamoDB | `ferrodb-drivers/src/dynamodb/` (feature `dynamodb`), `ferrodb-meta/src/dynamodb.rs` | AWS SDK with a rustls + ring HTTP client; PartiQL paged with `NextToken`; console calls in the AWS CLI shape (Query, Scan, GetItem, TransactWriteItems…); scans warned about first; consumed capacity as a notice; sets kept as sets. Tables with keys, GSI / LSI, billing; schema from a sampled Scan |
| Couchbase | `ferrodb-drivers/src/couchbase/` (feature `couchbase`), `ferrodb-meta/src/couchbase.rs` | Query and cluster-manager REST services; `query_context`, streaming JSON → BSON, cancel through `active_requests`, `BEGIN WORK` transactions, errors with position. Bucket → scope → collection → index; `INFER` schema inference; EXPLAIN plans (`ferrodb-data/src/plan/couchbase.rs`) |
| Document edits | `ferrodb-data/src/doc_edit.rs`, `core/src/documents.rs` | SQL++ `UPDATE … USE KEYS`, `UPSERT`, `DELETE`; PartiQL `UPDATE "t" SET/REMOVE … WHERE pk = …`; applied in one transaction (ExecuteTransaction / `BEGIN WORK`) |
| Key-value TCK | `ferrodb-driver-tck/src/key_value.rs` | 13 scenarios: types round trip, JSON, TTL, scan paging, reply rendering, MULTI/EXEC, edits, read-only, blocking-command cancel, connection loss |

### 8.6 Dialects and languages

| Plan item | Where | Notes |
|---|---|---|
| Dialect files | `assets/dialects/*.toml` | The 14 remaining products (Greenplum, CockroachDB, Redshift, Sybase, Db2, H2, Derby, HyperSQL, Exasol, Vertica, HiveQL, Snowflake, GoogleSQL, ClickHouse): keywords, types, functions with signatures and docs, quoting, case folding. Derived dialects can remove inherited keywords, types and functions |
| T-SQL and Oracle at level A | `ferrodb-lang/src/sql/` | Splitting by `GO [n]`, BEGIN…END, TRY/CATCH and routine bodies (T-SQL), PL/SQL blocks and `/` (Oracle); `@p` / `:p` parameters; TOP, APPLY, OUTPUT, MERGE, CONNECT BY in the parser; completion inside blocks (T-SQL 100 %, Oracle 97.5 % on the corpus); risks per statement; live templates |
| CQL, SQL++, PartiQL, Redis | `ferrodb-lang/src/{cql,sqlpp,partiql,redis}/` | CQL on the SQL engine (keyspaces, consistency levels, `ALLOW FILTERING` warnings); SQL++ paths and fields; PartiQL with key-aware scan detection; 424 Redis commands with arity, flags and key positions (completion, signature help, hover, diagnostics, formatter, `is_write` / `is_dangerous`) |
| Corpus | `sql/dialect_corpus_tests.rs`, `tests/format_props.rs`, `tests/split_props.rs` | Per dialect: no false errors, splitting, formatter idempotence, function completion; property tests over all 24 SQL dialects |

### Shared changes

- `ferrodb_meta::Dialect` has a variant for every database; every dialect has an introspector.
- `ferrodb_drivers::Request::Native { method, args }`: driver calls that are not statements (DatabaseMetaData, ADBC GetObjects, DynamoDB ListTables, BigQuery dry run, Snowflake warehouses…).
- Model: `Collection.scope`, `Table.table_options`, `Content.synonyms`.
- TCK: `run_adjusted` (per-database statements), `run_except` (scenarios that do not apply), the transactions scenario checks that databases without transactions refuse `BEGIN`.
- `xtask db-up` starts SQL Server, CockroachDB, Greenplum, Oracle Free, ClickHouse, Redis, Cassandra, DynamoDB Local and Couchbase (with memory limits).
- New Cargo features of `ferrodb-drivers`: `tds`, `redis`, `cql`, `dynamodb`, `couchbase`, `clickhouse` (all enabled by the core). New crates: `ferrodb-jdbc`, `ferrodb-driver-manager`, `ferrodb-adbc`. Every TLS stack is rustls with `ring` (no aws-lc, no OpenSSL).

## Not done / deviations

- **Cloud-only databases never ran against real services**: Redshift, Azure SQL, Snowflake, BigQuery and real AWS DynamoDB. They are covered by unit tests on recorded responses and by TCK entries that run when their `FERRODB_TCK_*` variables are set. Db2, Sybase ASE, Exasol, Vertica and Hive were not run against servers either (no container used); H2, Derby, HyperSQL (embedded) and Oracle Free were.
- **Cassandra has no TLS**: the `scylla` rustls feature pulls aws-lc; encrypted modes are refused with a message.
- **SQL Server**: no bulk load (imports use INSERT); Kerberos only through Windows SSPI; no TDS 8.0 strict encryption (tiberius 0.12, vendored with patches because 0.13 hard-wires aws-lc); `set_schema` cannot change the session's default schema.
- **Greenplum 6** is not supported (catalog queries need PostgreSQL 12+).
- **Couchbase** profiling needs Enterprise Edition; copying into Couchbase / DynamoDB targets in transfers is not done.
- **ADBC SQLite has no statement cancel**, so the TCK cancel scenario is skipped for it; ClickHouse skips the affected-rows scenario (mutations report no counts).
- **UI wiring left for later**: no warehouse / role picker (Snowflake), no jobs panel (BigQuery), no cost estimate display, no "copy script" menus for DynamoDB tables and Couchbase scopes, synonyms not shown in the explorer (no `ObjectKind::Synonym`).
- **Duplicated logic**: the core splits Redis scripts and classifies CQL itself (`core/src/kv.rs`) while `ferrodb-lang` now has equivalents; to be unified.
- **T-SQL diagnostics** take about 109 ms on a full 5 000-line console in release (budget 100 ms); about 77 ms after an edit.
- **Redis Cluster and Sentinel containers are not in `xtask`** (they need host networking); the commands used are in the 8.5 package notes.
- **Not clicked through in the GUI by a person.** The UI builds against the new backend; phase 8 changed no views.
- `ferrodb-meta` `tests::start_from_cache_is_fast` (300 ms budget in debug) can fail when the machine is loaded; it passes when run alone.

## Test runs (2026-11-21 / 22, Windows 11, Docker in WSL)

Servers used by the TCK and the core end-to-end tests:

| Database | Image / version | Result |
|---|---|---|
| SQL Server | `mcr.microsoft.com/mssql/server:2022-latest` (16.0.4325.4) | TCK 14/14 |
| CockroachDB | `cockroachdb/cockroach:latest` (v26.4.1) | TCK 14/14 |
| Greenplum | `andruche/greenplum:7` (7.0.0) | TCK 14/14 |
| Oracle | `gvenzl/oracle-free:slim` (23ai Free) | TCK through the JDBC bridge |
| H2, Derby, HyperSQL | embedded in the JDBC host (Java 17) | TCK passed |
| ClickHouse | `clickhouse/clickhouse-server` 26.10.4 | TCK 13 passed, 1 skipped (affected rows) |
| ADBC SQLite | `adbc_driver_sqlite` 1.13.0 wheel | tabular TCK passed |
| Redis | `redis:8` (8.10.3), 3-master Cluster, Sentinel | key-value TCK 13/13 on each |
| Cassandra | `cassandra:5` (5.0.9) | TCK passed (transactions, listing, affected rows skipped by capability) |
| DynamoDB | `amazon/dynamodb-local:latest` | document TCK 13/13 |
| Couchbase | `couchbase/server:community` 8.0.2 | document TCK 13/13 |

Workspace after the merges (`CARGO_INCREMENTAL=0`, `--no-default-features` with the six new driver features, UI and GUI spike excluded): every test target passed — `ferrodb-core` 50 (2 ignored), `ferrodb-data` 53, `ferrodb-drivers` 108, `ferrodb-lang` 101 + 6 property tests, `ferrodb-meta` 52 + 4, TCK 22, `ferrodb-adbc` 19, `ferrodb-jdbc` 12 (1 ignored), `ferrodb-driver-manager` 11, `ferrodb-mock` 65, plus `ferrodb-api`, `-net`, `-secrets`, `-storage`. Clippy with `-D warnings` is clean on the same set and on `ferrodb-ui`, `ferrodb-app`, `ferrodb-gallery` (default features).

UI tests (`cargo test -p ferrodb-ui`, headless on the mock): unit 35, `flows.rs` 23, `phase7.rs` 6, `ui.rs` 5 — all passed. Without the `FERRODB_TCK_*` variables the server TCK entries are skipped.
