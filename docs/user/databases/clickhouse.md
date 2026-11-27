# ClickHouse

Analytical tabular database, SQL (ClickHouse dialect), reached by a **native (N)** driver over the ClickHouse HTTP interface; language intelligence **level B**. No transactions.

## Requirements

| What | Details |
|---|---|
| Tested | `clickhouse/clickhouse-server` 26.10.4 (container `clickhouse/clickhouse-server:26.10` of `cargo xtask db-up`): driver TCK 13 passed, 1 skipped (affected rows) |
| Download | Nothing: the driver is built into FerroDb |
| Network | HTTP port **8123**, HTTPS port **8443** |

## Connecting

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `8123` | `8443` is used when TLS is on and no port is given |
| Database | `default` | Session database (container shown first in the explorer) |
| User | `default` when empty | |
| Password | — | Keyring |
| TLS (`ssl.mode`) | `disable` | Set on the SSL tab |

URL shape shown in the dialog: `clickhouse://{user}@{host}:{port}/{database}`.

**TLS**: HTTPS is used when the SSL mode is `require` or `verify-full`, or when the port is `8443` or `443`. `require` encrypts without the full check; `verify-full` verifies the certificate against the system store (or a CA bundle given as `ssl.ca` in the data source configuration). `prefer` behaves like plain HTTP.

**SSH tunnel**: supported; the real host name is kept for TLS and the `Host` header.

**Options tab**: every `key=value` pair (except `connect_timeout`, `query_timeout`) is sent as a ClickHouse **setting** with each statement, e.g. `max_execution_time=60; max_threads=4`.

## Authentication

| Method | Fields |
|---|---|
| User / password | **User**, **Password**, sent with each HTTP request |

## What works

- **Explorer**: databases (`system.databases`) → tables and views (views, materialized, live and window views) with their engine → columns with types; partition and sorting keys, TTL, `DEFAULT` / `MATERIALIZED` / `ALIAS` expressions, codecs, data-skipping indexes, dictionaries.
- **Console**: ClickHouse dialect file (keywords, types, functions with docs), splitting, completion, diagnostics, formatter (level B). `USE` and `SET` persist through the HTTP session (`session_id`).
- **Results**: grid; the server sends `FORMAT ArrowStream`, decoded as it arrives. Array, Nested, Map, Tuple, LowCardinality, Nullable, Decimal, DateTime64, UUID and Enum types are mapped. Server errors keep their code and position.
- **Cancel**: each statement has a `query_id`; cancel closes the request and runs `KILL QUERY`.
- **Editing**: edits use the sorting key as the row key and run as mutations (`ALTER TABLE … UPDATE` with `mutations_sync = 1`) and lightweight `DELETE`.
- **Definitions**: the server's own `CREATE` statement (`create_table_query`), engine settings, TTL and projections included.
- **Execution plan**: `EXPLAIN json = 1`, shown as tree / table / diagram.
- **Read-only mode**: every statement is sent with the `readonly` setting.
- **Import**: a bulk-load fast path is used for imports.
- **Export**: all result export formats.

## Limitations

- **No transactions**: Manual mode is refused ("ClickHouse has no transactions; every statement is applied at once").
- **Affected-row counts**: mutations report no counts, so `UPDATE` / `DELETE` show no number of affected rows.
- The **sorting key is not unique**: editing a row whose sorting-key values are shared by other rows changes all of them. Check the operation preview.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "ClickHouse HTTP: …" / connection refused | Use the HTTP port (8123 / 8443), not the native TCP port 9000 |
| TLS handshake errors | Set SSL mode `require` or `verify-full` and port 8443 (or the port the server exposes for HTTPS) |
| "ClickHouse: timed out" | Raise `connect_timeout` (seconds) in the Options tab |
| A setting from Options is rejected | Only real ClickHouse settings may go there; the server refuses unknown names |
| Edits apply slowly | Mutations are synchronous (`mutations_sync = 1`) and rewrite data parts |
