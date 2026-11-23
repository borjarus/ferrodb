# CockroachDB

Tabular · SQL (CockroachDB dialect, derived from PostgreSQL) · access layer **N** (native, `tokio-postgres`, PostgreSQL wire protocol) · language intelligence level **B**.

## Requirements

| | |
|---|---|
| Tested server | CockroachDB v26.4.1 (`cockroachdb/cockroach:latest`, `start-single-node --insecure`), TCK 14/14 |
| Dialect file | Written for CockroachDB 24.x / 25.x |
| Download | Nothing. The driver is built into FerroDb |
| Network | TCP to any node, port **26257** by default |

## Connecting

Driver id `cockroachdb`, family `pgwire`.

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `26257` | |
| Database | `defaultdb` | |
| User | `root` when empty | |
| Password | — | Stored in the system keyring |
| Retries of a statement failing with SQLSTATE 40001 (auto-commit) | `3` | Property `option.retries` |

URL preview: `postgresql://{user}@{host}:{port}/{database}`.

Driver options (`search_path`, `options`, `connect_timeout`, `query_timeout`), TLS modes (`disable` / `prefer` / `require` / `verify-full`, optional `ssl.ca`) and the SSH tunnel work as for [PostgreSQL](postgresql.md#connecting).

The product is detected from the `crdb_version` parameter the server sends, so the server version shows as CockroachDB's own.

## Authentication

| Method | Notes |
|---|---|
| Password | The only method offered |

Client certificate authentication is not supported.

## What works

| Area | Support |
|---|---|
| Explorer | Schemas (user schemas first; `crdb_internal`, `pg_extension`, `pg_catalog`, `information_schema` last) → tables, views → columns. The hidden `rowid` column is not listed |
| DDL | Tables use the statement CockroachDB prints itself (`SHOW CREATE TABLE`), so column families, hash-sharded and inverted indexes and the primary key storage are exact |
| Console | Level B: highlighting, statement splitting, schema-aware completion, formatter. The dialect adds CockroachDB keywords and removes PostgreSQL types it does not implement |
| Results | Streamed, same type mapping as PostgreSQL |
| Cancel | PostgreSQL cancel request |
| Transaction retries | In auto-commit, a statement failing with SQLSTATE 40001 is retried up to `option.retries` times with a growing pause; each retry is reported as a notice. Statements returning rows, `BEGIN`/`COMMIT` and statements inside a transaction are not retried |
| Transactions | Auto and Manual modes. Inside a transaction a 40001 error tells you to `ROLLBACK` and run the transaction again |
| Editing | Single-table `SELECT` results with a primary or unique key |
| Read-only | Read-only session plus a client-side check |
| Explain | `EXPLAIN (VERBOSE)` and `EXPLAIN ANALYZE (VERBOSE)` shown as a plan tree |
| Import / export | All export formats; import through `COPY … FROM STDIN` |

## Limitations

- **DDL is not transactional** here: schema changes run asynchronously, so DDL scripts (table editor, schema comparison) run statement by statement. A failing statement leaves the earlier ones applied.
- **No syntax diagnostics**: the dialect has no `sqlparser` parser (`AS OF SYSTEM TIME`, `UPSERT` and similar break the PostgreSQL one). Completion and highlighting still work.
- Level B: no inspections with quick fixes and no rename refactoring.
- Secure clusters with TLS were not tested; the TCK runs an insecure single node.

## Troubleshooting

| Symptom | Fix |
|---|---|
| SQLSTATE 40001 in Manual mode or inside `BEGIN … COMMIT` | Roll back and run the whole transaction again; only auto-commit statements are retried automatically |
| 40001 still reported after retries | Raise `option.retries`, or run the statement when the conflicting load is lower |
| A DDL script stopped half-way | Earlier statements stay applied (DDL is not transactional). Fix the failing statement and run the rest |
| Login refused on a secure cluster | Use `require` or `verify-full` (with `ssl.ca` for the cluster CA) and a user with a password; certificate login is not available |
