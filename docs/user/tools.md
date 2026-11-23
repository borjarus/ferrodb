# Tools

## Execution plans

Put the cursor on a statement and choose:

- **Explain Plan** (Ctrl+Shift+E / Cmd+Shift+E): the plan the database estimates. The statement does not run.
- **Explain Analyze** (Ctrl+Alt+Shift+E / Cmd+Alt+Shift+E): runs the statement and shows actual rows, times and loops. FerroDb refuses to analyze a statement that changes data, because it would really run.

The plan is produced in the console's session, so it sees the current schema, temporary tables and the open transaction. The plan opens in a **Plan** tab (**Plan (analyzed)** for Explain Analyze) with three views:

| View | Shows |
|---|---|
| **Tree** | Operations nested by their inputs, with time and rows |
| **Table** | One row per operation: **Operation**, **Object**, **Cost**, **Self** (with a bar), **Est. rows**, **Actual rows**, **Time**, **Loops** |
| **Diagram** | Boxes with their inputs below, shaded by their share of the cost, or of the time when analyzed |

Warnings above the plan point out common problems with the same rules for every database: a full scan of 1000 rows or more, a sort spilled to disk, temporary tables and filesort (MySQL), temporary B-trees (SQLite), `COLLSCAN` and in-memory sorts (MongoDB), row estimates off by 10× or more, filters that remove most rows, and many more documents examined than returned. Selecting an operation shows all properties the database reported. **Copy Raw Plan** copies the database's own output.

Plans are available for most databases; each database page says which form it uses. When a data source has none, the console reports *This data source has no execution plans*.

## ER diagrams

**Tools ▸ ER Diagram**, or **ER Diagram** on a schema or table in the explorer, draws the tables and views of a schema from the cached metadata:

- primary and foreign key columns are marked; each foreign key is a line from its column to the referenced column, optional ones dimmed;
- referenced tables are placed left of the tables that reference them;
- drag a box by its header to move it. Positions are saved in `.ferrodb/diagrams/<source>-<schema>.json` in the project; **Reset Layout** drops them.

Limitations: no zoom, no export to an image, and only the first 14 columns of a table are shown (*… N more*).

## Schema comparison

**Tools ▸ Compare Schemas** compares any two schemas, also of different data sources.

1. Choose the **Source** and the **Target** schema, then **Compare**.
2. Options: **Drop objects missing in the source**, **Ignore comments**, **Routines and triggers**.
3. The list shows each difference; selecting one shows the DDL of both sides as a diff.
4. **Migration script (makes the target like the source)** is written in the target's dialect and ordered to run top to bottom. **Open in Console** opens it for review; **Apply to Target** runs it.

Compared: tables, columns (type, `NULL`, default, identity, generated, comment), primary, unique and foreign keys, checks, indexes, views, routines, sequences and triggers. Types and defaults are normalized before comparing (`INT` equals `integer`). Objects are matched by name, so a renamed table shows as a drop and a create. Routines are compared by their definition text.

## Table editor

**Tools ▸ New Table…** (with a schema selected) or **Modify Table…** on a table opens the visual table editor:

- **Columns**: name, type, default, `NULL`, primary key, **Auto** increment;
- **Indexes**: name, columns, **Unique**;
- **Foreign Keys**: columns, **References table**, **Referenced columns**, **On delete**.

The **DDL preview** shows the `CREATE TABLE` or `ALTER TABLE` script for your changes, with warnings: dropped columns lose data, type changes convert values, `NOT NULL` without a default fails on a table that has rows. Renamed columns are renamed, not dropped and added again. **Apply** runs the script and refreshes the explorer.

Running DDL:

- PostgreSQL, SQLite and DuckDB run the whole script in one transaction: if a statement fails, nothing is changed (*statement 2 failed: …*).
- MySQL and other databases without transactional DDL run statement by statement; a failure leaves the earlier statements applied.
- SQLite changes that `ALTER TABLE` cannot do (type, `NULL`, default, keys) rebuild the table: copy, drop, rename, recreate indexes and triggers. `CHECK` constraints are not carried over in a rebuild; FerroDb warns about it.
- DDL is refused on read-only data sources.

## Project files

**Project Files** (find it with **Find Action…**) opens the **Files** panel next to the explorer. It lists the files of the project folder, without hidden folders, `target` and `node_modules`.

- **Open in Console**: opens a script in a console bound to the file. The tab shows the file name and `•` when it has unsaved changes; **Save File** (Ctrl+S / Cmd+S) writes it.
- **Open as Data Source**: opens data files and database files as a data source (*Data source "…" ready*):
  - CSV, TSV, Parquet, JSON and JSON Lines files (also `.gz`) through DuckDB;
  - DuckDB and SQLite database files.

### DuckDB and data files

DuckDB is built into FerroDb. A DuckDB data source has a database file (empty for in memory) and a list of data files. Each data file becomes a view named after the file (`Sales 2025.csv` becomes `sales_2025`), so you can query, join, export and compare files with SQL. See [DuckDB](databases/duckdb.md).

## Local History

**View ▸ Local History** keeps revisions of every console and script file, independent of version control. A revision is taken when a console runs, is saved, after 30 seconds without typing, before a restore, and when the file changed on disk.

- The panel lists documents and their revisions (*executed*, *saved*, *autosaved*, *before restore*, *changed on disk*, *label*).
- **Compare with current console** opens a diff.
- **Restore into a new console** restores a revision (into the open console if there is one).
- **Put Label** names a revision; **Delete revision**; **Clear History of Document**.

Revisions are kept for 30 days, up to 200 per document. Labeled revisions and the newest one are always kept. Local History is stored in `app.db` in the data directory.

## Query history

**View ▸ Query History** lists every statement you ran, newest first.

- **Filter history…** searches the text of all statements (full-text search) and can be limited to a data source.
- Double-click an entry to open it in a console.

FerroDb also keeps a query log of every statement it sends, including its own (`BEGIN`, schema switches), in `app.db`.

## Services

The **Services** panel (bottom) shows sessions per data source, whether a transaction is open, running queries with **Stop Query**, and the state of the JDBC bridge.
