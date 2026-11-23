# MySQL

Tabular · SQL (MySQL dialect) · access layer **N** (native, `mysql_async`, MySQL protocol) · language intelligence level **A**.

## Requirements

| | |
|---|---|
| Tested server | MySQL 8.4 (`mysql:8.4` container, TCK 14/14 on 8.4.13) |
| Download | Nothing. The driver is built into FerroDb |
| Network | TCP to the server, port **3306** by default (Unix sockets are not used) |

## Connecting

Driver id `mysql`, family `mysql`.

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `3306` | |
| Database | — | Optional default database |
| User | — | |
| Password | — | Stored in the system keyring |

URL preview: `mysql://{user}@{host}:{port}/{database}`.

**Driver options** (Options tab): `connect_timeout` (seconds, default 15) and `query_timeout` (seconds; the statement is stopped with `KILL QUERY`). Other keys are ignored by this driver.

**TLS** (SSL tab):

| Mode | Behaviour |
|---|---|
| `disable` | No TLS |
| `prefer` (default) | TLS if the server offers it, certificate not verified; falls back to plain TCP otherwise |
| `require` | TLS required; certificate not verified unless a CA file is given (then the chain is verified, not the host name) |
| `verify-full` | TLS required; chain and host name verified against the system store or the CA file |

The CA file is the `ssl.ca` property (no dialog field; set it in `data-sources.toml`).

**SSH tunnel** (SSH tab): SSH host (`host[:port]`), user, private key file; `known_hosts` with trust on first use; ssh-agent not supported.

The client reports `program_name = FerroDb` as a connection attribute.

## Authentication

| Method | Notes |
|---|---|
| Password | The only method offered; the authentication plugin is negotiated by `mysql_async` |

## What works

| Area | Support |
|---|---|
| Explorer | Databases → tables, views, routines (procedures with `IN`/`OUT`, functions) → columns (`AUTO_INCREMENT`, `ON UPDATE CURRENT_TIMESTAMP`, stored generated, comments), primary/unique/foreign keys, checks, indexes (composite with `DESC`, prefix, `FULLTEXT`, functional key parts), triggers. From `information_schema`, cached on disk |
| Console | Level A: highlighting, statement splitting incl. `DELIMITER` and `BEGIN … END` bodies, schema-aware completion incl. `JOIN` on foreign keys, inspections, formatter. Parameters `:name` and `?` |
| Results | Text protocol, streamed; several result sets (`CALL`); warnings shown as notices; native `JSON` type |
| Errors | Message with code and SQLSTATE; syntax errors (1064) are mapped to a range in the editor |
| Cancel | `KILL QUERY` on a second connection; the session survives |
| Editing | Single-table `SELECT` results with a primary or unique key. Without a key, rows are matched by all values and statements get `LIMIT 1` (the preview warns) |
| DDL | Tables generated from the model; views, routines and triggers from the server definition |
| Transactions | Auto and Manual modes |
| Read-only | `SET SESSION TRANSACTION READ ONLY` plus a client-side check |
| Explain | `EXPLAIN FORMAT=JSON`; Explain Analyze uses `EXPLAIN ANALYZE` (tree) and is refused for statements that change data. Filesort and temporary tables are flagged |
| Import / export | All export formats. Import uses `LOAD DATA LOCAL INFILE` streamed from memory; if the server refuses it, multi-row `INSERT` is used with a warning |

## Limitations

- **DDL is not transactional**: a failing statement in a DDL script leaves the earlier ones applied.
- Not modelled: events, partitioning, the table `AUTO_INCREMENT` counter. They are missing from generated DDL.
- The language engine has rename, quick fixes and parameter info, but the console does not offer them yet.
- Table names are case-sensitive on most Linux servers: name lookups try the exact spelling first, then ignore case.
- Error ranges are reported for syntax errors (1064) only.
- TLS `verify-full` was not tested against a server with a real certificate (the test container uses a self-signed one).

## Troubleshooting

| Symptom | Fix |
|---|---|
| `connection failed: Access denied for user … (1045)` | Wrong user or password, or the user is not allowed from your host |
| `the server does not support TLS` | The server has TLS off: use `prefer` or `disable` |
| Import warns that `LOAD DATA` was refused | `local_infile` is off (MySQL 8 default). The import still finishes with `INSERT`; enable `local_infile` on the server for speed |
| `connection lost` | FerroDb reconnects on the next statement; an open transaction is reported as lost |
| Statement refused as read-only | The source is read-only |
