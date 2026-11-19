# Phase 7 — Advanced tools: status

Scope: plan, section 7, phase 7. Execution plans in one model with three views, ER diagrams on real foreign keys, schema comparison with a migration script, the visual table editor generating `ALTER`, a project files panel, DuckDB as a file data source, and Local History kept in `app.db`.

The plan gives no completion criteria for this phase; the table below lists each scope item with what shows it works.

## What was built

| Plan item | Where | Notes |
|---|---|---|
| Execution plan: shared model | `ferrodb-api` `PlanNode`, `ExecutionPlan`, `ExplainOptions` (API v6) | A node has estimated cost and rows, the object read, actual rows / time / loops (with analyze), every other property the database reports (in its order) and warnings. Helpers: self cost, self time, depth-first rows for the table view |
| Plans of PostgreSQL, MySQL, MariaDB, SQLite, MongoDB (+ DuckDB) | `ferrodb-data/src/plan.rs`, `ferrodb-core/src/explain.rs` | PostgreSQL `EXPLAIN (FORMAT JSON)` / `(ANALYZE, BUFFERS, FORMAT JSON)`; MySQL `EXPLAIN FORMAT=JSON` and `EXPLAIN ANALYZE` (tree text); MariaDB `EXPLAIN` / `ANALYZE FORMAT=JSON`; SQLite `EXPLAIN QUERY PLAN` (analyze = the statement is also run and timed); MongoDB `explain` of `find`, `aggregate`, `countDocuments`, `distinct` with `queryPlanner` / `executionStats` (pipelines that are not pushed down become a chain of stages, sharded output a merge node); DuckDB `EXPLAIN (FORMAT JSON)` / `(ANALYZE, FORMAT JSON)` |
| Plan warnings | `plan.rs` | The same rules for every database: a full scan of ≥ 1000 rows, a sort spilled to disk, filesort / temporary tables (MySQL), temporary b-trees (SQLite), `COLLSCAN` and in-memory sorts (MongoDB), estimates off by 10× or more, filters removing most rows, documents examined ≫ returned |
| Explain runs in the session | `ferrodb-core/src/session.rs` (`Cmd::Explain`) | The session actor runs the explain statement on its connection: the plan sees the session's schema, temporary tables and open transaction. `analyze` of a statement that changes data is refused (it would run); the estimate is allowed |
| Plan views: tree, table, diagram | `ferrodb-ui/src/views/plan.rs` | Tree (as before, with time / rows), table (one row per operation: cost, self-cost bar, estimated and actual rows, time, loops), diagram (boxes with inputs below, shaded by their share of the cost — or of the time when analyzed). Warnings above, properties of the selected operation on the right, "Copy Raw Plan". Console: Explain (Ctrl+Shift+E) and Explain Analyze (Ctrl+Alt+Shift+E) |
| ER diagrams | `ferrodb-meta` `diff::er_model`, `MetadataService::er_model`, `ferrodb-ui/src/views/er_diagram.rs` | From the catalog: tables and views, PK / FK columns marked, every foreign key drawn from its column to the referenced column (optional ones dimmed). Layout in layers: referenced tables left of the tables referencing them. Boxes are dragged by their header; positions are saved in `.ferrodb/diagrams/<source>-<schema>.json` (plan 5.10) and restored; "Reset Layout" |
| Schema comparison and migration script | `ferrodb-meta/src/diff.rs` (`compare`), `MetadataService::compare_schemas`, `ferrodb-ui/src/views/schema_compare.rs` | Any two schemas (also of different sources): tables, columns (type / NULL / default / identity / generated / comment), primary, unique, foreign keys, checks, indexes, views, routines, sequences, triggers. Types and defaults are compared normalized (`INT` = `integer`, `int(11)` = `int`, `'a'::text` = `'a'`). The script is in the target's dialect and ordered to run top to bottom (drops of foreign keys / views / indexes first, creates of foreign keys / views / triggers last); removed objects are dropped only when asked. The view lists the differences, shows the DDL of both sides as a diff, and opens the script in a console or applies it |
| Visual table editor with `ALTER` | `ferrodb-meta` `diff::alter_table`, `design_script`; `MetadataService::table_design` / `design_script` / `apply_script`; `ferrodb-ui/src/views/table_editor.rs` | Columns (name, type, default, NULL, PK, auto-increment), indexes and foreign keys of a new or existing table. Renamed columns are renamed (not dropped and re-added). Every edit asks the backend for the script and shows its warnings (dropped columns lose data, type changes convert values, NOT NULL without a default on a table with rows). Apply runs it and refreshes the explorer |
| SQLite: changes `ALTER TABLE` cannot do | `diff::sqlite_rebuild` | Type / NULL / default changes, keys, checks, dropping key columns: the table is rebuilt (create a copy, copy the rows, drop, rename, recreate indexes and triggers), `AUTOINCREMENT` kept. `ADD COLUMN`, `RENAME COLUMN`, `DROP COLUMN` and `RENAME TO` stay in place |
| Running DDL | `ferrodb-core/src/schema_tools.rs` (`run_ddl`) | One transaction where DDL is transactional (PostgreSQL, SQLite, DuckDB): a failing statement undoes the whole script ("statement 2 failed: …"); statement by statement elsewhere (MySQL). Refused on read-only sources. Statements go to the query log; the container's metadata is refreshed |
| Project file panel | `ferrodb-ui/src/panels/files.rs` | Files of the project directory (hidden folders, `target`, `node_modules` skipped). Scripts open in a console bound to the file: the tab shows the file name and `•` when changed, Save File (Ctrl+S) writes it. A console without a file saves to `.ferrodb/consoles/` (plan 5.10). Data files and DuckDB / SQLite files open as a data source |
| DuckDB as a "file data source" | `ferrodb-drivers/src/duckdb.rs` (feature `duckdb`), `ferrodb-meta/src/duckdb.rs`, `assets/dialects/duckdb.toml` | `duckdb` 1.x compiled from source with the Parquet and JSON readers. Properties: `file` (database file, empty = in memory) and `files` (CSV, TSV, Parquet, JSON, JSON Lines, also `.gz`): each data file becomes a view named after it (`Sales 2025.csv` → `sales_2025`), so it can be queried, joined, exported, compared. Streaming in DuckDB's own Arrow batches cut to the sink size, `duckdb_interrupt` for cancellation. Introspection from `duckdb_tables()` / `duckdb_columns()` / `duckdb_constraints()` / `duckdb_indexes()`, including temporary views. SQL dialect inherits PostgreSQL (sqlparser `DuckDbDialect` for diagnostics) |
| Full Local History | `ferrodb-api` `LocalHistoryService`, `ferrodb-storage` (`local_history` table, schema version 2), `ferrodb-core/src/local_history.rs`, `ferrodb-ui/src/panels/local_history.rs` | Revisions per document (a file path or `console:<source>`), taken when a console runs, is saved, after 30 s without typing (autosave) and before a restore; labels on demand. Identical consecutive texts are stored once. Retention: 30 days and 200 revisions per document, labels and the newest revision kept. The panel lists documents and revisions; compare with the current text, restore (into the open console, else a new one), delete, clear. Persistent with the core; in memory on the mock |

### API (v6)

- `PlanNode` gained `object`, `actual_rows`, `actual_ms`, `loops`, `properties`, `warnings` (and `Default`); `QueryService::explain` takes `ExplainOptions` and returns `ExecutionPlan`.
- `MetadataService` (default-implemented): `er_model`, `compare_schemas`, `table_design`, `design_script`, `apply_script`; types `ErModel`, `SchemaComparison`, `SchemaDiffItem`, `CompareOptions`, `TableDesign`, `DesignScript`…
- `Backend::local_history()` → `LocalHistoryService` (default: `MemoryLocalHistory`).

## Not done / deviations

- **Not clicked through in the GUI by a person.** UI flows run headless on the mock (`crates/ferrodb-ui/tests/phase7.rs`); backend flows run on SQLite and DuckDB.
- **PostgreSQL, MySQL, MariaDB and MongoDB plans were not run against servers** (no containers here). Their parsers are tested on recorded outputs; `tests_phase7::server_plans_when_available` runs them when the `FERRODB_TCK_*` variables are set.
- **MySQL DDL is not transactional**: a failing statement leaves the earlier ones applied (the explorer is refreshed anyway).
- **SQLite CHECK constraints** are not read by the introspector; a rebuild of a table with checks warns that they are not carried over.
- **Renames are only detected in the table editor**; the schema comparison matches by name (a renamed table is a drop + create).
- **Routines are compared by definition text**; a changed routine is dropped and recreated.
- **The ER diagram** has no zoom and no export to an image; it shows the first 14 columns of a table.
- **DuckDB is not in the mock catalog** (the 26 databases of 1.0 stay as they are): "Open as Data Source" needs the core backend.
- **DuckDB build cost**: the C++ sources take about 30 minutes to compile once per profile and feature set (`--no-default-features` on `ferrodb-core` leaves it out).

## Test runs (2026-11-19, macOS, mock and local SQLite/DuckDB tests)

- `cargo xtask ci` passed (fmt, clippy with `-D warnings`, test workspace).
- `cargo test --workspace`: 449 passed, 0 failed, 2 ignored (`million_rows_stream`, `export_10m_rows`).
- UI tests in `crates/ferrodb-ui/tests/phase7.rs`: 6 passed (plan modes + analyze, ER diagram + saved layout, schema comparison dev → prod, table editor ALTER, project files + save, Local History restore).
- `crates/ferrodb-ui/tests/flows.rs`: 23 passed.
- `crates/ferrodb-ui/tests/ui.rs`: 5 passed.
