# Phase 10.1 — Full TCK and integration run: status

Scope: plan, section 7, phase 10, first item ("full run of TCK and integration tests for 26 databases on three systems; fixing regressions"), with section 9 (CI: fast subset per PR, nightly full matrix of containers, emulators and server versions, cloud-only databases on budget-limited accounts, commercial databases without public image in a dedicated environment) and Appendix D.

This package builds the machinery for the full run: the test matrix as data, selectable database groups for local runs, the nightly workflow and the per-PR changes. The full run itself happens in the nightly workflow on GitHub-hosted runners; this machine (Windows 11, Docker in WSL, very little free memory) only verified the wiring.

## What was built

| Item | Where | Notes |
|---|---|---|
| Containers as data | `xtask/src/db.rs` | `Container { key, group, image, versions, ports, env, args, memory, privileged, ready, vars, tests, needs }`. Groups `native` (PostgreSQL, MySQL, MariaDB, SQL Server, CockroachDB, Greenplum, ClickHouse, SSH bastion), `nosql` (MongoDB, Redis, Cassandra, DynamoDB Local, Couchbase), `jdbc` (Oracle, Db2, Exasol, Hive, Sybase) |
| New containers | `xtask/src/db.rs` | IBM Db2 Community (`icr.io/db2_community/db2`, privileged, 4 GB), Exasol (`exasol/docker-db:2025.1.17`, privileged, 4 GB), Apache Hive 4.2.1 (HiveServer2), Sybase ASE (`datagrip/sybase:16.0`, unofficial). Oracle Free, Cassandra, Redis, Couchbase and DynamoDB Local already existed (phase 8) |
| Readiness | `xtask/src/db.rs` | `db-up` waits until each container accepts connections: a command in the container (`pg_isready`, `mysqladmin ping`, `healthcheck.sh`, `redis-cli ping`, `cqlsh`…), a log line (SQL Server, Db2, Exasol), a TCP connect, or the MongoDB / Couchbase initialisation; fails with the container name when it stops or times out. `--no-wait` skips it |
| Selection | `xtask/src/main.rs`, `db.rs` | `db-up`, `db-down`, `tck`, `env` take `--only key,key`, `--group native\|nosql\|jdbc\|all`; `db-up` takes `--image key=image` (server versions); `tck --skip-meta`. `--only ssh` brings PostgreSQL along (`needs`). The pseudo-key `embedded` selects SQLite, H2, Derby, HyperSQL. `tck --only` runs only the selected tests (`--test tck -- --exact …`) and sets only their variables (plus `FERRODB_TCK_JDBC=1` for JDBC targets) |
| `cargo xtask env` | `xtask/src/db.rs` | prints `KEY=value` lines for a shell or `$GITHUB_ENV`; `FERRODB_TCK_HOST` overrides the host |
| Test matrix | `xtask/src/matrix.rs` | 27 rows (26 + DuckDB): layer, method (container, emulator, embedded, embedded JDBC, cloud account, dedicated environment), container images and nightly versions, variables, PR yes/no, nightly job, runners, local command. Images, versions and variables come from the container data |
| `cargo xtask matrix` | `xtask/src/matrix.rs` | prints the markdown tables; `--write` / `--check` keep `docs/test-matrix.md` in sync (checked in `ci.yml` and `nightly.yml`); `--gha` prints the nightly server matrix (29 entries: every container at every version) |
| TCK entries | `ferrodb-driver-tck/tests/tck.rs`, `src/lib.rs` | `db2`, `sybase`, `exasol` (creates and opens schema `TCK`), `vertica`, `hive` (DML scenario excluded: non-ACID tables) with new profiles; `dynamodb_aws` (real DynamoDB, region from `FERRODB_TCK_DYNAMODB_AWS`, credentials from the AWS environment) |
| Nightly workflow | `.github/workflows/nightly.yml` | `plan` (matrix check, server matrix; manual runs take `only`), `servers` (one runner per container and version: `db-up --only --image`, `env`, `tck --only`, `ferrodb-drivers` tests for PostgreSQL / MySQL / MariaDB / SSH; Java 21 + JDBC host for the `jdbc` group; container log on failure), `cloud` (Redshift, Azure SQL, Snowflake, BigQuery, DynamoDB on AWS through OIDC; each target skipped with a notice when its secrets are missing; ADBC libraries from the 1.11.0 wheels; 30 min timeout), `dedicated` (Vertica, Sybase ASE, gated on secrets), `os` (Windows, macOS, Linux: whole workspace with nextest, doc tests, embedded JDBC TCK, ADBC SQLite, DuckDB driver tests), `report` (every `target/tck/*.md` in the run summary) |
| Per-PR CI | `.github/workflows/ci.yml` | push branch `main` → `master` (the default branch); `cargo xtask matrix --check` in `lint`; the `tck` job adds DynamoDB Local (service container; plan Appendix D: emulator on every PR) and H2 / Derby / HyperSQL in the JDBC host (Java 21, Maven build of the host jar; Appendix D: in-process on every PR) |
| Documentation | `docs/test-matrix.md` | generated matrix, what runs where, local commands, secrets and budget guards per cloud account, databases without a public image |

## Test runs (2026-11-23, Windows 11, Docker in WSL)

Free commit memory on the machine was about 1.5 GB throughout (other builds and WSL running), so only `xtask` and the TCK test binary were built, and one container at a time was started.

| What | Result |
|---|---|
| `cargo clippy -p xtask --all-targets -- -D warnings`, `cargo fmt --check` (xtask, TCK) | clean |
| `cargo test -p xtask` | 7 passed (unique keys and ports, default image among the versions, selection by key / group / needs, `--image`, JDBC variable, every database in the matrix once, every container in a matrix row, `--gha` entries) |
| `cargo xtask matrix`, `--write`, `--check` | table generated into `docs/test-matrix.md`; check passes |
| `cargo xtask matrix --gha` | 29 entries (container × version) |
| `cargo test -p ferrodb-driver-tck --test tck --no-run` (`-j 1`, dependencies at opt-level 0 and without debug info to fit in memory; earlier attempts died with "only metadata stub found" / `STATUS_STACK_BUFFER_OVERRUN`) | builds without warnings with the new profiles and entries |
| `xtask db-up --only redis` → `env --only redis` → TCK `--exact redis` → `db-down --only redis` | ready after 0 s; key-value TCK 13/13 on Redis 8.10.3 |
| same with `--image redis=redis:7.4` | 13/13 on Redis 7.4.12 (JSON scenario skipped: no RedisJSON) |
| TCK `--exact sqlite` | passed |

No TCK failure was found, so no driver code changed. The JDBC and heavier containers were not started (Java cannot even allocate its heap on this machine at the moment).

## Not done / deviations

- **The full run has not happened yet.** It needs the nightly workflow on GitHub (no remote is pushed from this package) and the cloud / dedicated secrets. This machine cannot hold more than one light container next to the builds.
- **Profiles written without a server**: Db2, Sybase ASE, Exasol, Vertica and Hive TCK profiles follow the documentation (types, row generators, sleeps); the first nightly run will show the corrections they need (most likely the Arrow types of Exasol / Vertica integers and Sybase `bit` / `datetime`, Hive read-only and cancel).
- **Vertica has no container**: the Vertica CE image is no longer on Docker Hub; it runs only on the dedicated environment. Sybase ASE uses an unofficial image nightly; the plan's dedicated environment remains the release reference.
- **Cloud accounts and the dedicated environment are not provisioned**; `docs/test-matrix.md` lists the secrets and the budget limits to configure (resource monitor, auto-suspend, query quotas, AWS / Azure budget alerts).
- **Readiness checks of the new containers are unverified** (Db2 / Exasol log lines, Hive and Sybase TCP); the old containers' checks follow their images' documented health commands.
- **Redis Cluster and Sentinel** stay manual (host networking), as in phase 8.
- **Containers run on Linux runners only**: GitHub's Windows and macOS runners have no Linux containers, so the three-system job covers the embedded databases and the full unit / UI suite.
- **DuckDB has no TCK entry**: it is covered by its driver tests (`--features duckdb`), nightly on three systems (the C++ build is too slow for every PR).
