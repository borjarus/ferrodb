# Phase 7 — handoff (to be finished on another machine)

State on 2026-11-19. Nothing is committed yet: all phase-7 changes are in the
working tree (some were staged automatically by the IDE). Commit them to a
branch and push before switching machines, e.g.

```sh
git switch -c phase-7
git add -A
git commit -m "wip: phase 7 advanced tools"
git push -u origin phase-7
```

Delete this file once phase 7 is committed for real.

## What is done and tested

Commands below use `CARGO_TARGET_DIR=target/p7`, because on the first machine
`target/` had a locked build-script file (`LNK1104`); on a fresh machine plain
`target/` is fine.

| Area | Code | Tests (passing) |
|---|---|---|
| API v6: `PlanNode` (extended), `ExecutionPlan`, `ExplainOptions`, `explain(…, opts)`; `ErModel`, `SchemaComparison`, `TableDesign`, `DesignScript`; `MetadataService::{er_model, compare_schemas, table_design, design_script, apply_script}`; `LocalHistoryService` + `MemoryLocalHistory`, `Backend::local_history()` | `crates/ferrodb-api/src/{query,schema,local_history,metadata,backend}.rs` | `cargo test -p ferrodb-api` |
| Plan parsers: PostgreSQL JSON, MySQL JSON + `EXPLAIN ANALYZE` tree, MariaDB JSON/ANALYZE, SQLite `EXPLAIN QUERY PLAN`, MongoDB `explain`, DuckDB JSON (1.x and 2.x profile) | `crates/ferrodb-data/src/plan.rs` | `cargo test -p ferrodb-data --lib plan` (12) |
| Schema diff, migration script, `ALTER TABLE`, SQLite rebuild, design ↔ table, ER model | `crates/ferrodb-meta/src/diff.rs`, `diff/tests.rs`; small changes in `ddl.rs` (SQLite index syntax, `AUTOINCREMENT`, unqualified `REFERENCES`), `Dialect::DuckDb` | `cargo test -p ferrodb-meta --lib diff` (9, incl. live SQLite migration) |
| DuckDB introspector | `crates/ferrodb-meta/src/duckdb.rs` | covered by core test below |
| DuckDB driver (feature `duckdb`, on by default in `ferrodb-core`) | `crates/ferrodb-drivers/src/duckdb.rs`, `Cargo.toml` (`duckdb = "=1.20000.2"`, bundled + parquet + json) | `cargo test -p ferrodb-drivers --features duckdb --lib duckdb` (3) |
| DuckDB SQL dialect | `assets/dialects/duckdb.toml`, `ferrodb-lang` (`dialect.rs`, `analyze.rs`, `inspect.rs`, `lexer.rs`) | — |
| Explain in the session actor (`Cmd::Explain`), schema tools, DDL runner, Local History on storage | `crates/ferrodb-core/src/{explain,schema_tools,local_history,session,metadata,lib}.rs` | `cargo test -p ferrodb-core --lib -- tests_phase7 explain` (7) |
| Local History table in `app.db` (schema v2) | `crates/ferrodb-storage/src/lib.rs` | `cargo test -p ferrodb-storage` (3) |
| Mock: explain with analyze, schema tools on generated schema | `crates/ferrodb-mock/src/{lib,objects,schema}.rs` | `cargo test -p ferrodb-mock` (65) |
| Native drivers declare `ExplainSupport::Structured` (PG, MySQL, MariaDB, SQLite, MongoDB, DuckDB) | `ferrodb-drivers/src/registry.rs`, `mongodb/mod.rs` | — (changed after the last driver test run; rerun) |

## What is NOT done

### 1. The UI has never been compiled (main remaining work)

All phase-7 UI code is written but no build of `ferrodb-ui` finished yet (the
first full build failed on the first machine from lack of memory; a retry with
`-j 4` was running). Expect compile errors to fix. Files:

- `crates/ferrodb-ui/src/views/plan.rs` — rewritten: tree / table / diagram modes, warnings, details pane
- `crates/ferrodb-ui/src/views/er_diagram.rs` — rewritten: `er_model`, layered layout, drag, saved layout (`.ferrodb/diagrams/`)
- `crates/ferrodb-ui/src/views/schema_compare.rs` — new
- `crates/ferrodb-ui/src/views/table_editor.rs` — rewritten on `table_design` / `design_script` / `apply_script`
- `crates/ferrodb-ui/src/panels/files.rs` — new project files panel
- `crates/ferrodb-ui/src/panels/local_history.rs` — rewritten on the backend
- `crates/ferrodb-ui/src/local_history.rs` — new helper (record revisions)
- `crates/ferrodb-ui/src/panels/console.rs` — `explain_analyze`, file binding, `save_file`, autosave revisions, `set_text`
- `crates/ferrodb-ui/src/workspace.rs` — `compare_schemas` → new view, `show_files`, `project_state_dir`, `project_root`, `refresh_explorer_node`, `open_file_console`, `open_data_query`
- `crates/ferrodb-ui/src/actions.rs` — `ExplainAnalyze` (Ctrl+Alt+Shift+E), `SaveFile` (Ctrl+S), `ShowProjectFiles`
- `crates/ferrodb-ui/src/panels/{mod,explorer,gallery}.rs`, `activity.rs`, `lib.rs`, `views/mod.rs`
- `assets/i18n/{en,pl}/main.ftl` — new keys (phase-7 section at the end)

API spots worth checking first when it does not compile (written from the
gpui-component 0.8.1 sources, not compiled): `TreeState::set_items`,
`TreeEntry::is_folder`, `ListItem::on_click`, `Button::danger`,
`Checkbox::label`, `window.open_dialog(cx, |dialog, window, cx| …)` (third
argument used as `&App` in `table_editor::open`), `icons::Lucide::{TriangleAlert,
FileCode, Sheet, RefreshCw, File, ArrowRight, GitCompare, Undo2, Tag, Trash,
Copy}` (all exist in `gpui-kit-assets` 0.8.1), `f32::from(Pixels)`.

### 2. Run all tests

```sh
cargo test -j 4 -p ferrodb-api -p ferrodb-mock -p ferrodb-ui-kit -p ferrodb-ui -p ferrodb-app \
  -p ferrodb-lang -p ferrodb-drivers -p ferrodb-driver-tck -p ferrodb-secrets -p ferrodb-net \
  -p ferrodb-storage -p ferrodb-meta -p ferrodb-core -p ferrodb-data -p ferrodb-gallery
```

New UI tests: `crates/ferrodb-ui/tests/phase7.rs` (plan modes + analyze, ER
diagram + saved layout, schema comparison dev → prod, table editor ALTER,
project files + save, Local History restore). Existing ones that touch changed
code: `tests/flows.rs` (`explain_opens_a_plan_tab`, `mockup_views_open`,
`services_history_and_local_history_follow_queries`, gallery), `tests/ui.rs`
(every action in Find Action, labels translated, PL ⊇ EN messages).

Note: the mock's Local History is a process-wide static (`Backend::local_history`
default), shared by tests running in one process — the phase-7 UI test uses a
unique file document for that reason.

### 3. Then

- `cargo xtask ci` (fmt + clippy); fix warnings.
- Click through in the app once (`cargo run`, and `cargo run -- --mock`): Explain /
  Explain Analyze, ER diagram drag, Compare Schemas, New Table / Modify Table,
  Project Files (open `.sql`, open `.csv` as data source), Local History.
- Fill in "Test runs" in `docs/phase-7.md` with commands and counts, update the
  README status line, delete this file, commit (`feat: phase 7 advanced tools`).
- Optional, with containers (`cargo xtask db-up`): `FERRODB_TCK_POSTGRES/MYSQL/MONGODB`
  make `tests_phase7::server_plans_when_available` run plans on real servers.

## Things to know

- **DuckDB build cost:** `libduckdb-sys` compiles DuckDB C++ sources (~30 min,
  several GB RAM). It is rebuilt for every different feature combination of a
  cargo invocation, so always use the same `-p …` set (the command above) to
  avoid repeated rebuilds. Use `-j 4` (or less) if the machine has ≤ 16 GB RAM —
  a full parallel build ran out of memory. `ferrodb-core` without the
  `duckdb` default feature skips it.
- **Line endings:** files must stay LF. On Windows, patching with Python must use
  `open(..., 'w', newline='\n')`; text mode writes CRLF.
- `docs/phase-7.md` is written (scope table, API changes, not-done list) except
  the test results.
