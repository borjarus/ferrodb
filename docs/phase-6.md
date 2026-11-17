# Phase 6 — Data editor, import and export: status

Scope: plan, section 7, phase 6. Results of SQL queries can be edited on the real backend, rows can be followed along foreign keys and filtered by a value, and data moves between files, tables and collections through one streaming transfer service.

## Completion criteria

| Criterion (plan) | Status | Evidence |
|---|---|---|
| Import of 200k rows / documents < 5 s locally | **Done (SQLite)** | `ferrodb-core` `tests_phase6::import_200k_rows`: a 200,000-row CSV (4 columns) into an existing SQLite table in one transaction. Release build: **0.58 s**; debug build: 1.26 s. PostgreSQL (`COPY`) and MySQL (`LOAD DATA`) fast paths are implemented but were not timed in this change (no containers were run) |
| Export of 10M rows at constant memory | **Done** | `tests_phase6::export_10m_rows` (ignored by default; run with `--release -- --ignored`): 10,000,000 generated rows → 285 MB CSV in 5.3 s. Peak working set of the whole test process: **12 MB** (Windows, measured while the test ran) |
| Type round trip table ↔ collection | **Done for the value layer; real server test written, not run** | `ferrodb-data` `value::tests::bson_round_trip` (bool, int32/int64, double, decimal, UTF-8 text, binary, UTC timestamp; dates come back through the target column type). `tests_phase6::table_collection_round_trip_on_mongodb` copies a SQLite table into a MongoDB collection and back into a table, comparing values and SQLite storage types; it runs when `FERRODB_TCK_MONGODB` is set and was skipped here. Table → table copies keep the declared types (`NUMERIC(10,2)`, `DATE`…): `tests_phase6::export_import_and_copy_keep_types` |

## What was built

| Plan item | Where | Notes |
|---|---|---|
| `ferrodb-data` crate | `crates/ferrodb-data/` | New (plan 5.3). No database access; pure data code shared by core, mock and UI |
| Editing via ChangeSet with generators per language | `ferrodb-data/src/edit.rs`, `ferrodb-core/src/tables.rs`, `documents.rs` | SQL: one `UPDATE` per edited row with `WHERE` on the original key values, `INSERT` (empty cells take defaults; `DEFAULT VALUES` / `() VALUES ()`), `DELETE`. MongoDB: `$set`, `$unset`, `insertOne`, `replaceOne` (keeps `_id`), `deleteOne` |
| Which results are editable | `tables.rs` (`edit_target`) | A `SELECT` from one table or alias, without `GROUP BY` / `HAVING` / `DISTINCT` / set operations. Computed columns are read-only. The table and its key come from the metadata catalog (re-read once if the table is newer than the cache). Views are refused |
| Rows without a key | `tables.rs` | Primary key, else unique key. Without one (or when the result lacks the key columns) rows are matched by all their comparable values, the preview starts with a warning comment, and MySQL statements get `LIMIT 1` |
| Preview → commit / rollback | `tables.rs`, `session.rs` (`Cmd::Apply`) | Every statement must change exactly one row, otherwise everything is undone ("statement 1 would change 2 rows instead of 1"). Auto-commit sources: one transaction on a pooled connection, so a paused result keeps its cursor. Manual-commit sources: inside the session's transaction under `SAVEPOINT ferrodb_edit`; the report says `pending_commit` and the user commits or rolls back |
| Pasting, bulk changes | existing grid | The grid already collects many pending cell edits, inserted and deleted rows into one change set; they are now applied for real |
| Filtering and sorting server-side and locally | existing grid + `QueryService::value_filter` | Local search/sort and the server `WHERE` / `ORDER BY` fields existed (phase 2). New: "Filter by This Value" builds a dialect-correct condition (`city = 'O''Hara'`, `note IS NULL`; `{ "city": "Kraków" }` for MongoDB) and re-runs the statement |
| Foreign key navigation | `QueryService::navigations` / `related_script`, grid context menu | Outgoing keys (`customer_id → customers(id)`) and tables referencing this one (`orders.customer_id → customers`), from every loaded catalog container. "Related: …" runs `SELECT * FROM <table> WHERE <columns> = <row values>` |
| Aggregates | `ferrodb-data/src/chart.rs` (`Aggregates`) | count, nulls, distinct, sum, avg, min, max (numbers) or min/max as text. The grid footer keeps its phase-2 numeric aggregates; switching it to the new function is not done |
| Value editors | — | Not changed in this phase: the phase-2 inline editors (text, boolean toggle, validated numbers / dates) are used |
| Streaming export | `ferrodb-data/src/export.rs` | CSV, TSV, JSON (array, optional pretty), JSON Lines, Extended JSON (canonical, types kept), Markdown, HTML, XLSX (constant-memory worksheets, a new sheet after 1,048,576 rows, real numbers and dates), SQL `INSERT` (dialect quoting, N rows per statement), Parquet (Snappy). Documents written to tabular formats take the columns of the first batch; later fields are counted in a warning |
| Import with mapping | `ferrodb-data/src/import.rs` | CSV/TSV (header, delimiter, NULL text, BOM, type guessing on 1000 rows), XLSX (first or named sheet; Excel dates), Parquet, JSON array / JSON Lines / Extended JSON (`$oid`, `$date`… kept). `Mapping`: by name ignoring case, or explicit source → target pairs; values converted to the target column types with the column named in errors |
| Fast paths | `ferrodb-drivers` `Connection::bulk_load` | PostgreSQL `COPY … FROM STDIN` (text format), MySQL / MariaDB `LOAD DATA LOCAL INFILE` streamed from memory, MongoDB `insertMany` (1000 per batch). If the server refuses `LOAD DATA` (`local_infile` off, MySQL 8 default) the import continues with multi-row `INSERT` and says so in a warning. SQLite uses 500-row `INSERT`s in one transaction |
| Copying between sources and models; materializing a result | `ferrodb-core/src/transfer.rs` | One `TransferService`: source = read-only query, table / collection, or file; target = file, table (optionally created from the source column types and precision, optionally truncated) or collection. Producer and consumer run concurrently over a 4-chunk channel. Writes as export sources are refused. The target table appears in the explorer afterwards (catalog refresh) |
| Comparing results | `ferrodb-data/src/compare.rs` | By key columns or position, numeric tolerance, excluded columns, case-insensitive option; reports changed columns per row and rows / columns on one side only |
| Charts | `ferrodb-data/src/chart.rs` | Bar / line / pie series from a category column and value columns with sum / count / avg / min / max, largest N categories plus "other" |
| Extensible per driver | `Connection::bulk_load` (default: unsupported), `sql::Family`, `DataFormat` | A new driver gets the `INSERT` path for free and can add a fast path; a new dialect family adds literals / types in `sql.rs` |

### API (v5)

- `Change`: `UnsetField`, `InsertDocument`, `ReplaceDocument`.
- `ApplyReport::pending_commit`.
- `QueryService` (default-implemented, so other backends keep compiling): `edit_info`, `navigations`, `related_script`, `value_filter`, `release_result`.
- `TransferService` with `DataFormat`, `FormatOptions`, `TransferSource`, `TransferTarget`, `ColumnMapping`, `TransferEvent`, `TransferReport`, `FilePreview`; `Backend::transfers()` (default: `NoTransfers`).

## UI changes

- The export / import wizard runs real transfers: export of the shown console result (its statement runs again on its own connection) or of the table / collection chosen in the explorer; import of a file into the selected table or a new one (create / truncate options), with a preview of the first rows, guessed types and the by-name column matching. Progress and the outcome go to a notification and the Output panel.
- Grid context menu: "Filter by This Value" and "Related: …" (one item per foreign key).
- Applying edits in manual-commit mode says that the changes wait for a commit.
- The mock backend implements the new API: its exports write real files of generated rows, its imports read real files.

## Not done / deviations

- **Not clicked through in the GUI by a person.** Backend flows are covered on SQLite; the UI compiles and its existing tests pass (see below).
- **Charts and result comparison have no views yet.** The data layer (`chart`, `compare`) is done and tested; drawing the charts and a diff view remain.
- **Explicit column mapping in the wizard**: the API and core accept explicit mappings, the wizard only shows the automatic by-name matching.
- **PostgreSQL, MySQL, MariaDB and MongoDB were not run** in this change: no containers. The `COPY` / `LOAD DATA` / `insertMany` paths and the table ↔ collection round trip need `cargo xtask db-up` and the TCK variables. TCK scenarios for editing and export (ADR-012) are not added yet.
- **BSON dates have millisecond precision**: microseconds of SQL timestamps are lost on the way through a collection.
- **JSON arrays and XLSX sheets are read whole** on import (JSON Lines, CSV and Parquet stream).
- **Editing other change kinds of documents in the document view** (insert / replace / unset are in the API and core; the view still offers set field and delete document).
- The grid footer aggregates still use the phase-2 code.

## Test runs (2026-11-17, Windows 11, no database containers)

See the final section of the change description for the commands and counts.
