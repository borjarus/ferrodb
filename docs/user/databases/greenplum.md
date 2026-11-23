# Greenplum

Tabular · SQL (Greenplum dialect, derived from PostgreSQL) · access layer **N** (native, `tokio-postgres`, PostgreSQL wire protocol) · language intelligence level **B**.

## Requirements

| | |
|---|---|
| Tested server | Greenplum 7.0.0 (`andruche/greenplum:7`, single host, 2 segments), TCK 14/14 |
| Supported | Greenplum 7 (based on PostgreSQL 12) |
| Not supported | Greenplum 6: the catalog queries need PostgreSQL 12+ |
| Download | Nothing. The driver is built into FerroDb |
| Network | TCP to the coordinator, port **5432** by default |

## Connecting

Driver id `greenplum`, family `pgwire`. Listed under analytical / cloud databases.

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | The coordinator (master) host |
| Port | `5432` | |
| Database | `postgres` | |
| User | `gpadmin` when empty | |
| Password | — | Stored in the system keyring |

URL preview: `postgresql://{user}@{host}:{port}/{database}`.

Driver options, TLS modes and the SSH tunnel are the same as for [PostgreSQL](postgresql.md#connecting): `search_path`, `options`, `connect_timeout`, `query_timeout`; SSL modes `disable` / `prefer` (default) / `require` / `verify-full` with an optional `ssl.ca` file; SSH with a private key file.

The server version shown is Greenplum's own (`Greenplum 7.0.0 (PostgreSQL 12.12)`), read from `version()`.

## Authentication

| Method | Notes |
|---|---|
| Password | The only method offered |

## What works

| Area | Support |
|---|---|
| Explorer | As PostgreSQL (schemas, tables, views, materialized views, routines, sequences, keys, indexes, triggers), plus the distribution policy and append-optimized storage options |
| Console | Level B: highlighting, statement splitting, schema-aware completion (objects, columns, keywords, built-in functions), syntax diagnostics (PostgreSQL parser), formatter. The dialect adds Greenplum keywords (distribution, external tables, append-optimized storage, resource management) |
| Results | Streamed, same type mapping as PostgreSQL |
| Cancel | PostgreSQL cancel request |
| Editing | Single-table `SELECT` results with a primary or unique key |
| DDL | Generated tables include `DISTRIBUTED BY (…)` and storage options |
| Transactions | Auto and Manual modes; DDL is transactional |
| Read-only | Read-only session plus a client-side check |
| Explain | `EXPLAIN (FORMAT JSON)` and Explain Analyze; Motion nodes are shown in the plan |
| Import / export | All export formats; import through `COPY … FROM STDIN` |

## Limitations

- Greenplum 6 and older are not supported.
- Introspection gaps of PostgreSQL apply (enum/domain/composite types, policies, rules, grants… are not modelled).
- Level B: no inspections with quick fixes and no rename refactoring.
- Not run against Greenplum clusters with more than one host; the TCK uses a single-host container.

## Troubleshooting

| Symptom | Fix |
|---|---|
| Introspection errors on an older server | Check the version: only Greenplum 7 is supported |
| Login refused for `gpadmin` | Set the user explicitly; `gpadmin` is only the default when the field is empty |
| `connection failed: …: timed out` | Check that the coordinator host and port are reachable, or use the SSH tab |
