# SQLite

Tabular · SQL (SQLite dialect) · access layer **N** (native, `rusqlite` with the SQLite library compiled in) · language intelligence level **A**.

## Requirements

| | |
|---|---|
| Engine | SQLite is bundled with FerroDb (TCK passed on SQLite 3.54). Nothing to install or download |
| Server | None: FerroDb opens the database file directly |
| Network | None |

## Connecting

Driver id `sqlite`. The only field is **File**: the path of the database file.

- The file is created if it does not exist (unless the source is read-only).
- SQLite URI file names (`file:…?mode=…`) are accepted.
- A template "SQLite file" starts with `data.db`. SQLite files in the project files panel can also be opened as a data source.
- If another process holds a write lock, FerroDb waits up to 5 seconds (`busy_timeout`) before reporting the database as locked.

URL preview: `sqlite:{file}`.

TLS and SSH tunnel do not apply. Options: `query_timeout` (seconds).

Each connection runs on its own thread; the UI never waits for the file.

## Authentication

None. Access is controlled by file system permissions.

## What works

| Area | Support |
|---|---|
| Explorer | Schemas (`main` and attached databases, from `pragma_database_list`) → tables, views → columns, primary keys (also composite), unique and foreign keys, indexes (also partial), triggers. Read from `sqlite_master` (exact DDL) and the table-valued pragmas |
| Console | Level A: highlighting, statement splitting, schema-aware completion incl. `JOIN` on foreign keys, inspections, formatter. Parameters `:name` and `?` |
| Results | Streamed; values keep their storage class (SQLite has no date or decimal types) |
| Errors | Extended result codes (`SQLITE_…`); syntax errors point at the offending position |
| Cancel | `sqlite3_interrupt` |
| Editing | Single-table `SELECT` results with a primary or unique key; without a key, rows are matched by all values |
| DDL | `sqlite_master.sql` is the exact DDL |
| Table editor | Changes `ALTER TABLE` cannot do (type, NULL, default, keys, checks, dropping key columns) rebuild the table: copy, drop, rename, recreate indexes and triggers; `AUTOINCREMENT` is kept |
| Transactions | Auto and Manual modes; DDL is transactional |
| Read-only | The file is opened read-only and `PRAGMA query_only` is set |
| Explain | `EXPLAIN QUERY PLAN`; Explain Analyze also runs the statement and times it (refused for statements that change data). Temporary b-trees are flagged |
| Import / export | All export formats. Import uses 500-row `INSERT`s in one transaction (200,000 CSV rows in 0.58 s in a release build) |

## Limitations

- The default schema cannot be switched: unqualified names resolve in `main` (then attached databases). Choosing another schema in the console fails with "SQLite cannot change the default schema; qualify names instead".
- `CHECK` constraints are not read by the introspector; a table rebuild warns that they are not carried over.
- The language engine has rename, quick fixes and parameter info, but the console does not offer them yet.
- The bundled SQLite is built without encryption support (no SQLCipher): encrypted database files cannot be opened.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `connection failed: <path>: unable to open database file` | The directory does not exist or is not writable; for a read-only source the file must already exist |
| `database is locked` | Another program holds a write transaction for more than 5 s; finish it there, or open the source read-only |
| A new empty database appears instead of your data | The path was wrong and a new file was created; fix the File field |
| `SQLite: no database file given` | Fill the File field |
