# FerroDb

A native, cross-platform database IDE for SQL and NoSQL, written in Rust on GPUI + GPUI Kit. The plan is in [`docs/plan-ferrodb.en.md`](docs/plan-ferrodb.en.md).

**Status: phases 0–3.** The application runs on real databases: PostgreSQL, MySQL, MariaDB and SQLite, with native drivers, TLS, SSH tunnels, keyring-stored secrets, streaming, cancellation, transactions and read-only mode. See [`docs/phase-3.md`](docs/phase-3.md). The GUI components from phase 2 are described in [`docs/phase-2.md`](docs/phase-2.md).

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

## Workspace

| Crate | Role |
|---|---|
| `crates/ferrodb-api` | Contracts (v2): `Backend` and its services; the three data models; `Capabilities`; the driver catalog; query events |
| `crates/ferrodb-mock` | `MockBackend`: the 26-database catalog, 5 profiles (PostgreSQL-, BigQuery-, Cassandra-, MongoDB- and Redis-like) and 10 load or failure scenarios |
| `crates/ferrodb-core` | The real `Backend`: data sources (`data-sources.toml`), sessions, script execution with paging, transactions, read-only mode, timeouts, history |
| `crates/ferrodb-drivers` | `Driver` / `Connection` / `FamilyVariant`, Arrow mapping; families `pgwire` (PostgreSQL), `mysql` (MySQL, MariaDB), `sqlite` |
| `crates/ferrodb-driver-tck` | Driver compatibility suite run against every database (ADR-012) |
| `crates/ferrodb-lang` | SQL statement splitting, parameters, statement analysis (read-only, warnings) |
| `crates/ferrodb-net` | TLS (rustls) and SSH tunnels (russh) |
| `crates/ferrodb-secrets` | System keyring (ADR-013) |
| `crates/ferrodb-storage` | `app.db`: query history (FTS5) and query log |
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
