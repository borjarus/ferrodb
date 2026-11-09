# Phase 3 — Connections and SQL query execution: status

Scope: plan, section 7, phase 3. First databases: **PostgreSQL, SQLite, MySQL, MariaDB**. The application now runs on the real backend (`ferrodb-core`); `ferrodb --mock` (or `FERRODB_BACKEND=mock`) still starts the mock with its developer scenarios.

## Completion criteria

| Criterion (plan) | Status | Evidence |
|---|---|---|
| Four databases pass the TCK | **Done** | `cargo xtask tck`: SQLite 3.54, PostgreSQL 18.6, MySQL 8.4.13 and MariaDB 11.4.13 each pass 14/14 scenarios. PostgreSQL also passes through an SSH tunnel (table below) |
| Cancellation < 1 s | **Done** | TCK `cancel` (`pg_sleep(30)` / `sleep(30)` / endless CTE): 0–26 ms from cancel to `Cancelled`, and the connection is usable afterwards. Core: `cancel_stops_a_running_statement_quickly` (UI cancel flag → executor in < 1 s) |
| 1M rows streamed without blocking the UI | **Done (core level)** | `million_rows_stream` (`cargo test -p ferrodb-core -p ferrodb-app -- --ignored million`): first page after 4.5 ms, 1,000,000 rows in 0.76 s. Results reach the UI as 500-row pages over a bounded channel (ADR-002); the UI thread does no I/O. The grid's behaviour with 1M rows was measured in phase 2 on the mock |

### TCK v1 results (2026-11-09)

| Scenario | SQLite | PostgreSQL | MySQL | MariaDB |
|---|---|---|---|---|
| connect / descriptor / ping | pass | pass | pass | pass |
| types (int, decimal, text, bool, date, timestamp) | pass¹ | pass | pass | pass |
| dml-affected (3 / 2 / 1) | pass | pass | pass | pass |
| empty-result-shape | pass | pass | pass | pass |
| streaming-100k (≤ 1000 rows/batch) | 152 ms | 68 ms | 114 ms | 111 ms |
| backpressure-truncate | pass | pass | pass | pass |
| cancel | 0 ms | 18 ms | 17 ms | 26 ms |
| errors (message, code, position²) | pass | pass | pass | pass |
| transactions | pass | pass | pass | pass |
| schema switch | pass | pass | pass | pass |
| explorer-listing | pass | pass | pass | pass |
| read-only session | pass | pass | pass | pass |

¹ SQLite has no date or decimal types: values keep their storage class (text and real), as the profile states.
² The PostgreSQL TCK checks positions. The MySQL family reports a range for syntax errors (1064) only, and that is covered by the driver tests.

## What was built

| Plan item | Where | Notes |
|---|---|---|
| `Driver`, `Connection`, `FamilyVariant`, registry | `ferrodb-drivers/src/lib.rs`, `registry.rs` | Families behind features `sqlite`, `pgwire`, `mysql`. Variants carry dialect, auth, quirks, hierarchy and listing queries (ADR-008) |
| Type mapping → Arrow | `columns.rs`, `pgwire/decode.rs`, `mysql/types.rs`, `sqlite.rs` | Exact decimals (`Decimal128` up to 38 digits, exact text beyond). The database type name is kept in field metadata and shown in grid headers. PostgreSQL arrays, ranges, intervals, json/jsonb, uuid, inet… are rendered as psql would. Values that cannot be represented become NULL with a warning, never silently |
| PostgreSQL | `ferrodb-drivers/src/pgwire/` | tokio-postgres; binary decoding; notices; cancel request; error positions mapped to byte ranges |
| MySQL, MariaDB | `ferrodb-drivers/src/mysql/` | mysql_async text protocol; multiple result sets (`CALL`); `KILL QUERY` cancel; warnings as notices; 1064 error ranges |
| SQLite | `ferrodb-drivers/src/sqlite.rs` | rusqlite on a dedicated thread; `sqlite3_interrupt` cancel; affinity-based types |
| TCK v1 | `ferrodb-driver-tck` | Profiles are data (ADR-012). `cargo xtask db-up` / `tck` / `db-down` |
| Sessions, pools | `ferrodb-core/src/session.rs`, `sources.rs` | One actor task per session. A separate explorer connection per source. Idle connection pool (2 per source). Transparent reconnect with a warning; an open transaction is reported as lost |
| Statement splitting, execution modes, result names | `ferrodb-lang/src/sql/split.rs` | Quotes, dollar strings, nested comments, MySQL `DELIMITER`, routine bodies (`BEGIN … END`, `BEGIN ATOMIC`). The comment on the line above a statement names its result tab. Property tests |
| Parameters | `ferrodb-lang/src/sql/params.rs`, console dialog | `:name`, `$1` (PostgreSQL), `?` (MySQL, SQLite); values become escaped literals; error positions are mapped back through the substitution |
| Streaming, paging | `session.rs` (`forward`) | 500-row first page, `fetch_more`, backpressure to the socket. Only the last result of a script stays open; earlier ones are truncated with a notice. A new command closes a paused result |
| Auto/Manual transactions | `session.rs` | Manual mode begins a transaction before the first statement; commit/rollback from the console; `BEGIN`/`COMMIT` typed in the script are tracked |
| Read-only mode | driver session setting + `ferrodb-lang` `classify` | Enforced twice: by the server (read-only session) and by the core before sending |
| Warnings for dangerous operations | `ferrodb-lang/src/sql/analyze.rs`, console dialog | `sqlparser` AST with a lexical fallback: DELETE/UPDATE without WHERE, DROP, TRUNCATE, ALTER … DROP. On production sources every modifying statement asks for confirmation |
| Timeouts | `option.query_timeout` (seconds) or `ExecOptions::timeout` | The server-side cancel is reused; reported as `FERRODB-TIMEOUT` |
| Source configuration (TOML), keyring | `ferrodb-core/src/config.rs`, `ferrodb-secrets` | `data-sources.toml` without secrets; the keyring holds one credential per source (ADR-013) |
| TLS | `ferrodb-net/src/tls.rs` | libpq modes for every driver; rustls + `ring` + platform verifier; optional CA file |
| SSH tunnel | `ferrodb-net/src/ssh.rs` | russh. Password or key (also encrypted), known_hosts with trust on first use, refuses changed keys. Several forwarded connections per tunnel (needed for cancel requests) |
| Query history and query log | `ferrodb-storage` | `app.db`: history with FTS5 trigram search; a query log of every statement including the IDE's own (`BEGIN`, schema switches) |
| Integration test infrastructure | `xtask/src/db.rs`, CI job `tck` | Docker containers (via `wsl -e docker` on Windows). CI uses service containers |

## Contract changes (`ferrodb-api` v1 → v2)

- `DataSourceService::save(config, SecretValues)` and `test(config, SecretValues)`. Previously the dialog dropped typed passwords.
- `ExecOptions`: `schema` (console schema picker), `params`, `confirmed`.
- `LanguageService`: `parameters`, `risks` (`Parameter`, `Risk`, `RiskKind`).
- `SessionInfo`: `schema`, `busy`.

## UI changes

- The console asks for parameter values and confirms dangerous statements before running; it passes the selected schema.
- Stop now reaches a statement blocked on the server: the console holds the executor's cancel handle, not its own flag.
- Grid: decimals and times are displayed; headers show the database type (`varchar(20)`, `numeric(10,2)`).
- The data source dialog sends typed secrets to the keyring.

## Not done / deviations

- **Explorer depth is minimal by design:** schemas → tables/views → columns from listing queries, with no cache. Full introspection is phase 4.
- **Not in phase 3, and disabled by `Capabilities`:** editing results (phase 6), `EXPLAIN` (phase 7), formatting and completion beyond keywords (phase 5).
- **Not run:**
  - TLS `verify-full` against a server with a real certificate (the containers run without TLS, or with self-signed TLS for MySQL/MariaDB).
  - The SSH tunnel with an ssh-agent (not implemented).
  - The CI `tck` job has never run on GitHub.
- **Interactive flows on real databases:** not yet clicked through by a person. The app was started on the core backend; the flows are covered by core and TCK tests, and by the UI tests on the mock.
- **Project-level `.ferrodb/data-sources.toml`:** not wired yet; sources are global (ADR-013).
- **`testcontainers` replaced by Docker CLI xtask commands:** Docker runs inside WSL on the development machine (ADR-012).

## Environment notes (this development machine)

- **Blocked build scripts:** security software blocks a few freshly built build-script executables (`os error 5` / `32`, for example `serde_core` with one particular feature set).
  - Building or testing a crate on its own can select that set: `cargo test -p ferrodb-core`.
  - Build it together with the app instead: `cargo test -p ferrodb-core -p ferrodb-app`.
  - `cargo build --workspace` and the app build are not affected.
- **WSL port forwarding:** WSL forwards only some published ports to Windows `localhost`, so `cargo xtask db-up` prints URLs using the WSL address.
