# DuckDB

Embedded analytical tabular database and **file query engine** (the 27th database, besides the 26 of the 1.0 list), SQL (DuckDB dialect, derived from PostgreSQL), reached by a **native (N)** driver; the driver reports language intelligence **level A**.

## Requirements

| What | Details |
|---|---|
| Engine | DuckDB compiled into FerroDb (Rust crate `duckdb` `=1.20000.2`, bundled build with the Parquet and JSON readers). `select version()` shows the engine version |
| Tested | Backend flows and plans on local files (`docs/phase-7.md`) |
| Download | Nothing; no server, no network |
| Build note | Builds of FerroDb without the `duckdb` feature do not offer this source |

## Connecting

| Field | Notes |
|---|---|
| Database file | A DuckDB database file. Empty = in-memory database |
| Data files | CSV, TSV, Parquet, JSON / JSON Lines files (also `.gz` / `.zst` compressed CSV / JSON), one per line or separated by `;` |

At least one of the two is required. Each data file becomes a **temporary view** named after the file: lower-case, other characters replaced by `_`, a `t_` prefix when the name would start with a digit (`Sales 2025.csv` → `sales_2025`). Readers used: `read_csv`, `read_csv(…, delim = '\t')` for `.tsv` / `.tab`, `read_parquet`, `read_json`.

Shortcut: in the project file panel, data files and DuckDB files open as a data source directly.

No host, port, TLS or SSH settings apply.

## Authentication

None.

## What works

- **Explorer**: schemas of the current database → tables and views (the file views included) → columns, from `duckdb_tables()`, `duckdb_columns()`, `duckdb_constraints()`, `duckdb_indexes()`.
- **Console**: DuckDB SQL dialect (PostgreSQL-based, sqlparser `DuckDbDialect` for diagnostics), splitting, completion, formatter. File views can be queried, joined, compared and exported like tables.
- **Results**: grid, streamed as DuckDB's own Arrow batches cut to the result page size; cancel with `duckdb_interrupt`.
- **Editing**: grid edits with the operation preview (tables of a database file; file views are read-only views).
- **Definitions / schema tools**: generated DDL, ER diagram, schema comparison, visual table editor; DDL scripts run in one transaction (a failing statement undoes the whole script).
- **Transactions**: Auto and Manual modes; DDL is transactional.
- **Execution plan**: `EXPLAIN (FORMAT JSON)` / `EXPLAIN (ANALYZE, FORMAT JSON)`, shown as tree / table / diagram.
- **Read-only mode**: a database file is opened in DuckDB's read-only access mode.
- **Export**: all result export formats (CSV, JSON, XLSX, Parquet…).

## Limitations

- **Read-only mode applies to database files only**: an in-memory source with data files has nothing to protect on disk (files are only read).
- **Two data files with the same view name** (e.g. `a/sales.csv` and `b/sales.csv`) are refused; rename one.
- The views read the files at query time; a deleted or moved file makes its view fail.
- **Not in the mock catalog**: "Open as Data Source" needs the real backend.
- Only one process can open a DuckDB database file for writing at a time.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "DuckDB: give a database file or at least one data file" | Fill in one of the two fields |
| "<path>: file not found" | Check the data file path |
| "<path>: unsupported data file (CSV, TSV, Parquet or JSON expected)" | Only these extensions are recognised; query other formats with DuckDB functions in the console |
| Database file is locked | Another program (DuckDB CLI, another FerroDb window) holds it open for writing |
| CSV columns typed wrongly | Query the file in the console with `read_csv('…', types = {…})` or other `read_csv` options |
