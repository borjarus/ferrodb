# FerroDb

A native, cross-platform database IDE for SQL and NoSQL, written in Rust on GPUI + GPUI Kit. The plan is in [`docs/plan-ferrodb.en.md`](docs/plan-ferrodb.en.md).

**Status: phases 0–9.** AI and extensibility (version 1.1): natural-language query assistant (SQL, MQL, CQL…), permission layer for agents (read-only default, per-call consent), embedded MCP server and stdio bridge, and sandboxed Rhai extension scripts for custom export formats and aggregators. See [`docs/phase-9.md`](docs/phase-9.md). For earlier phases, see [`docs/phase-8.md`](docs/phase-8.md), [`docs/phase-7.md`](docs/phase-7.md), [`docs/phase-6.md`](docs/phase-6.md) and [`docs/phase-3.md`](docs/phase-3.md).

## Quick start

```sh
cargo run                      # FerroDb (real databases, default project)
cargo run -- --mock            # FerroDb on the mock backend (demo scenarios)
cargo run -- path/to/project   # open a project folder
cargo run -p ferrodb-gallery   # component gallery (mock data, both themes, scenarios)
cargo xtask ci                 # fmt + clippy + tests (+ cargo-deny if installed)
cargo xtask spike gui --bench  # GUI performance spike (FPS report)
cargo xtask db-up              # start PostgreSQL, MySQL, MariaDB and SSH test containers
cargo xtask tck                # run the driver TCK against them (cargo xtask db-down removes them)
```

Environment variables:
- `FERRODB_HOME=<dir>` redirects the configuration and data directories, which is useful for tests and demos.
- `FERRODB_LOG=debug` changes the log level.
- `FERRODB_BACKEND=mock` is the same as `--mock`.

## Building the executable

### Prerequisites

- **Windows:** Visual Studio 2022 Build Tools (or full VS) with the *Desktop development with C++* workload, including MSVC v143 and the Windows 11 SDK. DuckDB (C++), SQLite (C) and GPUI (DirectX) need them.
  ```powershell
  winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  winget install Rustlang.Rustup
  ```
- **Linux:** `build-essential`, rustup, then `./xtask/linux-deps.sh` (GPUI libraries: X11/Wayland, Vulkan, fontconfig…).
- **macOS:** `xcode-select --install` and rustup.

rustup installs the pinned toolchain (see `rust-toolchain.toml`) on the first `cargo` call. CMake, NASM, OpenSSL and Perl are not needed: TLS is rustls + `ring`.

### Release build

```sh
cargo build --release -p ferrodb-app
```

The binary lands in `target/release/ferrodb.exe` (Windows) or `target/release/ferrodb` (Linux, macOS). It is self-contained: dialects, translations and icons are embedded, so the file can be copied anywhere and run on its own. On Windows the release build uses the GUI subsystem, so it opens no console window and log output on stderr is not visible; use a debug build (`cargo run`) to see logs (`FERRODB_LOG=debug` for more detail).

The first build takes a long time (DuckDB compiles from C++; expect 30–90 minutes) and needs plenty of memory (≥ 20 GB free commit). Running out of it does not look like an out-of-memory error: rustc crashes with `STATUS_STACK_BUFFER_OVERRUN` (0xc0000409) or reports nonsense such as ``cannot find trait `From` in this scope``. In that case close memory-heavy programs (`wsl --shutdown`, rust-analyzer), raise the page file to 16–32 GB, or limit parallelism with `cargo build --release -p ferrodb-app -j 3`. If another process holds files in `target/`, build into a separate directory with `CARGO_TARGET_DIR=<dir>`.

### Installers

```sh
cargo install cargo-packager --locked
cargo xtask package
```

`cargo xtask package` builds the release binary and creates installers in `target/packages/` (NSIS `-setup.exe` and `.msi` on Windows, `.app`/`.dmg` on macOS, AppImage and `.deb` on Linux). Signing runs only when the signing environment is set; see `xtask/src/release.rs`.

More detail (memory, troubleshooting, running the tests) is in [`docs/running-tests.md`](docs/running-tests.md) (Polish).

## Workspace

| Crate | Role |
|---|---|
| `crates/ferrodb-api` | Contracts (v2): `Backend` and its services; the three data models; `Capabilities`; the driver catalog; query events |
| `crates/ferrodb-mock` | `MockBackend`: the 26-database catalog, 5 profiles (PostgreSQL-, BigQuery-, Cassandra-, MongoDB- and Redis-like) and 10 load or failure scenarios |
| `crates/ferrodb-core` | The real `Backend`: data sources (`data-sources.toml`), sessions, script execution with paging, transactions, read-only mode, timeouts, history |
| `crates/ferrodb-drivers` | `Driver` / `Connection` / `FamilyVariant`, Arrow mapping; families `pgwire` (PostgreSQL, Greenplum, CockroachDB, Redshift), `mysql` (MySQL, MariaDB), `sqlite`, `mongodb`, `duckdb` (file data sources, phase 7), `tds` (SQL Server, Azure SQL), `clickhouse`, `redis`, `cql` (Cassandra), `dynamodb`, `couchbase` (phase 8) |
| `crates/ferrodb-jdbc` | JDBC bridge client: starts and watches the Java host (`java/jdbc-host`), JDBC flavors (Oracle, Db2, Sybase, H2, Derby, HyperSQL, Exasol, Vertica, Hive) (phase 8) |
| `crates/ferrodb-driver-manager` | Catalog of downloadable drivers (JDBC jars, ADBC libraries) with SHA-256 and licenses, downloads, offline / custom files, Java runtime (phase 8) |
| `crates/ferrodb-adbc` | ADBC drivers loaded at run time: Snowflake, BigQuery (dry-run cost estimate, jobs) (phase 8) |
| `crates/ferrodb-driver-tck` | Driver compatibility suite run against every database (ADR-012) |
| `crates/ferrodb-lang` | SQL statement splitting, parameters, statement analysis (read-only, warnings); dialects as data; CQL, SQL++, PartiQL and Redis command languages (phase 8) |
| `crates/ferrodb-data` | Result data (phase 6): typed values, edit generators, export / import formats, row ↔ document conversion, result comparison, charts; execution plan parsers (phase 7). See [`docs/phase-6.md`](docs/phase-6.md), [`docs/phase-7.md`](docs/phase-7.md) |
| `crates/ferrodb-net` | TLS (rustls) and SSH tunnels (russh) |
| `crates/ferrodb-secrets` | System keyring (ADR-013) |
| `crates/ferrodb-storage` | `app.db`: query history (FTS5), query log, Local History revisions (phase 7) |
| `crates/ferrodb-ai` | AI Assistant (Anthropic, OpenAI-compatible, Ollama), schema context and prompt caching; permission layer for agents; embedded MCP server (`rmcp`) and stdio bridge (phase 9) |
| `crates/ferrodb-script` | Rhai extension scripts for custom export formats and aggregators, sandboxed execution, built-in and user scripts (phase 9) |
| `crates/ferrodb-ui-kit` | The facade over GPUI Kit (`gpui`, `kit`, `theme`, `icons`); the only place GPUI Kit is named |
| `crates/ferrodb-ui` | Views: workspace, dock panels, palettes, settings, actions and keymap, i18n (PL/EN) |
| `crates/ferrodb-app` | The `ferrodb` binary: startup, window, keymap file, packaging metadata |
| `apps/ferrodb-gallery` | Component gallery binary (plan 2.9) |
| `spikes/` | Phase 0 prototypes: GUI under load, ADBC, JDBC bridge (Java host + Rust reader) |
| `xtask/` | `cargo xtask ci, fmt, lint, test, package, spike, db-up, db-down, tck` |
| `docs/adr/` | ADR-001…008, ADR-012, ADR-013 |

## What works (phase 1)

- **Main window.** The title bar is custom on Windows and Linux, with an in-window menu bar. macOS gets native menus. Below sit a toolbar and a status bar showing the backend, connections and active scenario.
- **Docking layout:**
  - Database Explorer on the left;
  - Welcome and consoles in the center;
  - Services and Output at the bottom;
  - Inspector on the right.
  The layout is saved per project to `.ferrodb/layout.json` and restored on start. Saves are debounced, and the layout is also saved on close and quit. The file carries a version.
- **Actions:**
  - Every command is an action with a translated label, a category and a default binding compatible with JetBrains IDEs (plan, Appendix B).
  - **Find Action** (Ctrl/Cmd+Shift+A) lists every action.
  - **Search Everywhere** (double Shift) ranks actions together with mock tables, collections, keys and data sources. Ranking uses fuzzy matching (`nucleo`).
  - The user can override bindings in `keymap.toml`, placed in the configuration directory:
    ```toml
    [[binding]]
    keys = "secondary-shift-enter"   # secondary = Cmd on macOS, Ctrl elsewhere
    action = "ferrodb::ExecuteStatement"
    context = "Console"
    ```
- **Themes:** light, dark, or follow the system.
- **Settings** (`settings.toml`): theme, language, UI and editor font size, editor font family, line numbers, developer mode.
- **Notifications and dialogs**, **projects** (Open Project, Recent Projects).
- **i18n:** Polish and English via Fluent (`assets/i18n/*/main.ftl`). A test checks that both locales define the same messages.
- **Contracts and mock:**
  - The console runs scripts against the mock and writes a summary to Output: rows, pages, errors with position, read-only rejection, cancellation.
  - The Developer menu switches between mock scenarios: 1M rows, 100k documents, 1M keys, 5 MB document, 10k tables, slow network, connection error, driver not downloaded, JDBC process failure.

## Tests

`cargo test --workspace` runs:
- unit tests for the API, mock, keymap, i18n, palette ranking, settings and double-Shift detection;
- **UI tests without a window** (`crates/ferrodb-ui/tests/ui.rs`): startup with the default layout, layout surviving a restart, Find Action listing every action, Search Everywhere over the mock index, and palette actions running in the workspace;
- **core** (`ferrodb-core`, on SQLite): scripts, result titles, paging, read-only mode, manual transactions, cancellation, timeouts, parameters, history, explorer;
- **drivers and TCK**: these run against real servers when `FERRODB_TEST_*` / `FERRODB_TCK_*` are set (see `cargo xtask db-up`); SQLite always runs;
- **phase-2 flows** (`crates/ferrodb-ui/tests/flows.rs`, 23 tests on `MockBackend::deterministic()`): explorer, data source dialog and driver download, console, grid editing with preview, documents, keys, Services and history, load scenarios, mockups and the gallery.

## Phase 0 spike results

See the ADRs and [`docs/spike-results.md`](docs/spike-results.md).
