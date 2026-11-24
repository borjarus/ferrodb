# Phase 10 — Stabilization and release 1.0: status

Scope: plan, section 7, phase 10: a full TCK and integration run for the 26 databases on three systems with regressions fixed; profiling, regression benchmarks and fuzzing (parsers of every language, JSON → BSON, the JDBC bridge protocol); crash reporting with consent, packaging and signing (Authenticode, Apple notarization), signed updates, signatures and verification of downloaded ADBC drivers and the Java runtime on macOS; user documentation with a page for each of the 26 databases; a beta program.

The work was done in four packages in parallel (matrix, quality, release, documentation) on top of the API v8 contract and merged on `phase-10`. Each package has a detailed status page in `docs/phase-10/`.

## Scope and outcome

| Plan item | Outcome | Details |
|---|---|---|
| Full TCK and integration run, 26 databases × 3 systems | **Tooling done, full run pending.** The matrix is data (`cargo xtask matrix` → `docs/test-matrix.md`), every database has a test route (container, emulator, embedded, cloud account, dedicated environment), `.github/workflows/nightly.yml` runs it. Locally only Redis (7.4, 8.10) and SQLite ran; the nightly has not run yet (nothing pushed) | [matrix.md](phase-10/matrix.md), [test-matrix.md](test-matrix.md) |
| Fixing regressions | 13 bugs fixed (below) | this page |
| Profiling, regression benchmarks | 21 `divan` benchmarks with budgets (`benches/budgets.toml`), `cargo xtask bench-check`, nightly in `quality.yml`; `docs/profiling.md` | [quality.md](phase-10/quality.md) |
| Fuzzing | `fuzz/` (cargo-fuzz, 7 targets: all 28 languages and every language-service entry point, MQL values, Extended JSON, cell values, import, JDBC protocol and errors); the same harness runs on stable as proptest smoke tests in the normal suite | [quality.md](phase-10/quality.md) |
| Crash reporting (with consent) | `ferrodb-release`: panic hook, masked report in `<data>/crashes/`, consent Ask / Always / Never, startup dialog; sending needs a crash endpoint configured at build time | [release.md](phase-10/release.md) |
| Packaging and signing | `cargo xtask package` signs with signtool (Authenticode), codesign + notarytool + stapler (macOS), optional GPG (Linux), each step driven by environment variables; `.github/workflows/release.yml` on `v*` tags | [release.md](phase-10/release.md) |
| Signed updates | Ed25519-signed manifest (key embedded at build time), stable / beta channels, no downgrades, SHA-256-verified packages, install per platform; `xtask release-keygen`, `release-manifest` | [release.md](phase-10/release.md) |
| ADBC drivers and Java runtime on macOS | After the SHA-256 check: `codesign --verify --deep --strict`; ad-hoc signature for unsigned PyPI dylibs; JRE must carry its vendor's signature; quarantine removed from verified files only | [release.md](phase-10/release.md) |
| User documentation | `docs/user/`: 17 guide pages, `docs/user/databases/` with 27 pages (26 + DuckDB) and an overview | [docs/user/README.md](user/README.md) |
| Beta program | Beta channel in the updater (Settings ▸ Updates and Privacy, Help ▸ Join Beta Program…), `docs/user/beta-program.md`, maintainers' plan `docs/beta/plan.md`, GitHub issue forms (bug, beta feedback, database support) | [beta/plan.md](beta/plan.md) |

## API v8

| Item | Where | Notes |
|---|---|---|
| Updates | `ferrodb-api/src/release.rs` | `UpdateService` (current version, settings, `check`, `check_due`, `download` → `UpdateEvent::{Progress, Ready, Failed}`, `install`), `UpdateSettings`, `UpdateChannel` (Stable, Beta), `UpdateInfo` |
| Crash reports | same | `CrashReportService` (consent, pending, all, send with comment, dismiss, clear, directory), `CrashConsent` (Ask default, Always, Never), `CrashReport` |
| Backend | `ferrodb-api/src/backend.rs` | `updates()`, `crash_reports()` with defaults `NoUpdates`, `NoCrashReports`; `AiBackend` forwards them; `ReleaseBackend` (`ferrodb-release`) provides them and wraps core or mock in `ferrodb-app` |

## Bugs fixed

| Bug | Where | Found by |
|---|---|---|
| MQL lexer looped forever (and allocated without bound) on an emoji or a BOM | `ferrodb-lang` | fuzz smoke test |
| Integer overflow on extreme dates and times (BSON dates beyond ±292k years, `format_date`, `format_time`, Date → Timestamp, Arrow timestamps) | `ferrodb-data/src/value.rs` | fuzz smoke test |
| A malformed Arrow result from the JDBC host panicked the query (arrow-ipc 60.1 slices buffers unchecked); messages are now validated and decoder panics become errors | `ferrodb-jdbc` | fuzz smoke test |
| JDBC error ranges could end inside a character; positions off by one after `İ` | `ferrodb-jdbc` | writing the harness |
| A corrupt bridge frame header made the client allocate up to 256 MiB before reading | `ferrodb-jdbc/src/protocol.rs` | writing the harness |
| A non-ASCII CSV separator was cut to one byte (import and export) | `ferrodb-data` | writing the harness |
| With 10k tables every unknown table name scanned the whole catalog: diagnostics on 1k lines 2.2 s → ~15 ms, completion at the end of a 50k-line console 2.5 min → 275 ms | `ferrodb-lang` | benchmarks |
| Results of the native SQL databases (PostgreSQL family, MySQL / MariaDB, SQLite, SQL Server / Azure SQL) could not be edited: `editable_results` was still `false` from before phase 6 | `ferrodb-drivers/src/registry.rs` | documentation review |
| Editing a saved data source reset SSH, SSL mode, driver options and the schema filter, and dropped keys without an input (`ssl.ca`) | `ferrodb-ui/src/dialogs/data_source.rs` | documentation review |
| DynamoDB Local: the pre-filled `default` profile prevented the dummy credentials | `ferrodb-drivers/src/dynamodb/conn.rs` | documentation review |
| "Remove data source" said its secrets stay in the keyring; the core deletes them | `assets/i18n/{en,pl}/main.ftl` | documentation review |

## Test runs (2026-11-23, Windows 11 x64, after merging)

Run with `CARGO_INCREMENTAL=0`, `-j 2` (memory-constrained machine; `ferrodb-ui` test binaries built with `profile.dev.package.ferrodb-ui.debug=0`).

- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` passed (0 warnings).
- `cargo test`:

| Crate | Result |
|---|---|
| `ferrodb-api` | 3 passed |
| `ferrodb-lang` | 102 unit + property and fuzz smoke tests passed |
| `ferrodb-data` | 55 + 7 fuzz smoke passed |
| `ferrodb-jdbc` | 14 + 7 fuzz smoke passed (`tests/bridge.rs` needs Java, not run) |
| `ferrodb-release` | 15 + 5 + 1 passed |
| `ferrodb-driver-manager`, `xtask` | passed |
| `ferrodb-ai` | 45 passed, 1 ignored |
| `ferrodb-mock` | 66 passed |
| `ferrodb-drivers` (lib) | 58 passed |
| `ferrodb-ui` | 88 passed (lib 36, flows 23, phase7 6, phase9 8, phase10 10, ui 5) |
| `ferrodb-app` | clippy only (no tests) |
| `ferrodb-core` | 53 passed, 2 ignored (sprawdzone 2026-11-24) |
| `ferrodb-script` | 20 passed (sprawdzone 2026-11-24) |

- Dodatkowy pełny przebieg weryfikacyjny (2026-11-24, macOS Apple Silicon):
  - `cargo fmt --all -- --check` passed.
  - `cargo clippy --workspace --all-targets --locked -- -D warnings` passed (0 warnings).
  - Wszystkie pakiety przetestowane, w tym zaległe `ferrodb-core` (53 passed, 2 ignored) i `ferrodb-script` (20 passed).
  - `cargo test --workspace --doc --locked` passed.
  - `cargo test -p ferrodb-driver-tck --test tck -- --exact sqlite` passed (14/14 scenariuszy TCK SQLite).
  - `cargo xtask tck --only embedded` passed (SQLite TCK + 56 testów `ferrodb-meta`, w tym roundtripy MariaDB, MongoDB, MySQL, PostgreSQL).
  - `cargo test -p ferrodb-drivers --features duckdb --lib duckdb` passed (3/3 testy DuckDB).
  - `cargo xtask bench-check` 21/21 w budżecie.
  - `cargo xtask fuzz-smoke 2000` (lang, data, jdbc) passed.
  - `cargo run -p ferrodb-app -- --version` passed (`ferrodb 0.1.0 (API v8, dev channel)`).

- Earlier, per package: Redis 7.4 / 8.10 TCK 13/13 and SQLite TCK in Docker (WSL); `cargo xtask bench-check` 21/21 within budget; smoke tests at 40k (lang), 100k (data), 50k (jdbc) cases.

## Not done / deviations

- **The full run on three systems has not happened**: it is the nightly workflow, which has not run (the branch is not pushed). The Db2, Sybase ASE, Exasol, Vertica and Hive TCK profiles were written from vendor documentation and never ran against a server; Vertica has no public image any more (dedicated environment only). Cloud accounts (Redshift, Azure SQL, Snowflake, BigQuery, DynamoDB) and the dedicated environment are not set up; `docs/test-matrix.md` lists the secrets and budget guards. Containers run on Linux runners only.
- **cargo-fuzz never ran here** (no nightly toolchain on this machine); the harness ran on stable as proptest smoke tests (lang 40k cases, data 100k, jdbc 50k).
- **Completion in a 50k-line console takes 275–430 ms**, not the 50 ms of the plan (the engine re-analyses the whole console per request); its budget is pinned at the current value. Completion in ordinary consoles meets 50 ms. Budgets are loose (measured on a busy machine) and should be recalibrated from the first CI runs.
- **No real certificates or keys**: Authenticode, notarization, stapling, GPG and `release.yml` never executed; the macOS codesign path is tested in its pure parts only. No crash endpoint or update server exists (build-time settings, empty by default). GitHub's `releases/latest` skips pre-releases, so the beta manifest needs its own stable URL.
- **Nobody clicked through the new UI**; no installer was exercised on a real install.
- **Known gaps found while documenting, left for 1.x** (documented as limitations in `docs/user/`): SSL settings are ignored on the JDBC path; the Kerberos principal is not passed for Oracle, Vertica and Hive; no SSH password / key passphrase / `ssl.ca` field in the dialog; SSH ignored for Snowflake, BigQuery, DynamoDB and DuckDB; Snowflake OAuth and MongoDB Kerberos are not implemented; Rename… and Insert Live Template… still show "Coming in phase 5"; logs go to standard output only; the dialog's default SSL mode is `prefer` (the plan asks for verification by default — decide before 1.0).
- **Plan vs code**: Redshift IAM uses a hand-written SigV4 signer (not `aws-config`), Azure SQL hand-written Entra ID token sources (not `azure_identity`), Couchbase its HTTP services (not the SDK).
