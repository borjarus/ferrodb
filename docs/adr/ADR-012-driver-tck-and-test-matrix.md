# ADR-012: Driver compatibility test suite (TCK) and test matrix

- Status: **Accepted**
- Date: 2026-11-09

## Context

Plan section 2.9 defines a "supported database" as one that passes the same checks as every other database. Section 9 asks for a `ferrodb-driver-tck` run per database, with expectations that depend on `Capabilities`. Appendix D asks for the container images to be confirmed at the start of phase 3.

## Decision

- **One suite, data-driven expectations.** `ferrodb-driver-tck` runs the same scenarios against every `Driver`. What differs between databases lives in a `Profile`:
  - SQL snippets: a row series, a long-running statement, the DDL of a types table;
  - the expected Arrow types;
  - whether the driver reports error positions;
  - the first level of the explorer tree.

  Scenarios never branch on the driver id.
- **v1 scenarios** (phase 3):
  - connect and ping; descriptor and variant consistency;
  - faithful types (int, decimal, text with non-ASCII, bool, date, timestamp) with no NULL loss;
  - DML affected counts; empty results keep their shape;
  - 100k rows streamed in batches of at most 1000; the consumer going away stops the driver (`truncated`) and leaves the connection usable;
  - cancellation in under 1 s, after which the connection is usable;
  - errors with message, code and (where supported) position;
  - transactions (rollback discards, commit keeps); read-only sessions reject writes;
  - schema switching; explorer listing queries (container → table → column).

  Phase 4 adds introspection and DDL, phase 6 editing and export.
- **Where databases come from.**
  - SQLite runs everywhere (temporary file).
  - Server databases run when `FERRODB_TCK_<DB>` holds a URL. `cargo xtask db-up` starts the containers and prints the variables, `cargo xtask tck` runs the suite, and `cargo xtask db-down` removes them.
  - Plain Docker CLI rather than `testcontainers`. Developers on Windows run Docker inside WSL, which `testcontainers` cannot reach without exposing the daemon over TCP; the CLI works through `wsl -e docker`. CI (Linux) uses the same xtask commands.
- **Images confirmed (Appendix D, phase-3 databases):**

  | Database | Image | Port |
  |---|---|---|
  | PostgreSQL | `postgres:18` | 55432 |
  | MySQL | `mysql:8.4` | 53306 |
  | MariaDB | `mariadb:11.4` | 53307 |
  | SSH bastion (tunnel tests) | `linuxserver/openssh-server` with the `openssh-server-ssh-tunnel` mod | 52222 |

  The containers share the network `ferrodb-tck`, so the SSH container reaches `ferrodb-tck-pg:5432`.
- Driver crates keep their own deeper integration tests behind `FERRODB_TEST_<DB>` variables: type edge cases, notices, TLS modes, multi-result procedures.

## Consequences

- Adding a database means adding a `Profile` and passing the suite. A failing scenario is a release blocker for that database.
- WSL forwards only some published ports to Windows `localhost`. The xtask therefore prints URLs with the WSL address, which always works.
- Cloud-only databases (Redshift, Azure SQL, Snowflake, BigQuery) will run the same suite nightly with secrets from CI (phase 8).
