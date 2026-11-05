# FerroDb

A native, cross-platform database IDE for SQL and NoSQL, written in Rust on GPUI + GPUI Kit. The plan is in [`docs/plan-ferrodb.en.md`](docs/plan-ferrodb.en.md).

**Status: phases 0 and 1.** The application skeleton runs on a mock backend. Real database drivers start in phase 3.

## Quick start

```sh
cargo run                      # FerroDb (mock backend, default project)
cargo run -- path/to/project   # open a project folder
cargo xtask ci                 # fmt + clippy + tests (+ cargo-deny if installed)
cargo xtask spike gui --bench  # GUI performance spike (FPS report)
```

Environment variables:
- `FERRODB_HOME=<dir>` redirects the configuration and data directories, which is useful for tests and demos.
- `FERRODB_LOG=debug` changes the log level.

## Workspace

| Crate | Role |
|---|---|
| `crates/ferrodb-api` | Contracts (v0): `Backend` and its services; the three data models; `Capabilities`; the driver catalog; query events |
| `crates/ferrodb-mock` | `MockBackend`: the 26-database catalog, 5 profiles (PostgreSQL-, BigQuery-, Cassandra-, MongoDB- and Redis-like) and 10 load or failure scenarios |
| `crates/ferrodb-ui-kit` | The facade over GPUI Kit (`gpui`, `kit`, `theme`, `icons`); the only place GPUI Kit is named |
| `crates/ferrodb-ui` | Views: workspace, dock panels, palettes, settings, actions and keymap, i18n (PL/EN) |
| `crates/ferrodb-app` | The `ferrodb` binary: startup, window, keymap file, packaging metadata |
| `spikes/` | Phase 0 prototypes: GUI under load, ADBC, JDBC bridge (Java host + Rust reader) |
| `xtask/` | `cargo xtask ci, fmt, lint, test, package, spike` |
| `docs/adr/` | ADR-001…008 |

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
- **UI tests without a window** (`crates/ferrodb-ui/tests/ui.rs`): startup with the default layout, layout surviving a restart, Find Action listing every action, Search Everywhere over the mock index, and palette actions running in the workspace.

## Phase 0 spike results

See the ADRs and [`docs/spike-results.md`](docs/spike-results.md).
