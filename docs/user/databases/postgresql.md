# PostgreSQL

Tabular · SQL (PostgreSQL dialect) · access layer **N** (native, `tokio-postgres`) · language intelligence level **A**.

## Requirements

| | |
|---|---|
| Tested server | PostgreSQL 18 (`postgres:18` container, TCK 14/14 on 18.6) |
| Minimum server | PostgreSQL 12: the catalog queries of the introspector need it |
| Download | Nothing. The driver is built into FerroDb |
| Network | TCP to the server, port **5432** by default |

## Connecting

Driver id `postgresql`, family `pgwire`.

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `5432` | |
| Database | `postgres` | |
| User | `postgres` when empty | |
| Password | — | Stored in the system keyring, not in `data-sources.toml` |

The dialog shows a URL preview: `postgresql://{user}@{host}:{port}/{database}`. You cannot type a URL; use the fields.

A template "PostgreSQL (local Docker)" fills `localhost:5432`, database and user `postgres`.

**Driver options** (Options tab, `key=value;key=value`):

| Option | Effect |
|---|---|
| `search_path` | Sent as `-c search_path=…` at connect |
| `options` | Raw libpq-style startup options |
| `connect_timeout` | Seconds (default 15) |
| `query_timeout` | Seconds; a statement running longer is cancelled on the server and reported as `FERRODB-TIMEOUT` |

**TLS** (SSL tab, libpq semantics):

| Mode | Encrypted | Certificate verified |
|---|---|---|
| `disable` | no | — |
| `prefer` (default) | if the server offers it | no |
| `require` | yes | no (unless a CA file is given) |
| `verify-full` | yes | chain and host name, against the system store or the CA file |

A CA file (PEM) is read from the `ssl.ca` property. The dialog has no field for it: add it to the source in `data-sources.toml`. TLS is rustls with `ring`; no OpenSSL is used.

**SSH tunnel** (SSH tab): SSH host (`host[:port]`, port 22 by default), user and private key file. Host keys are checked against `~/.ssh/known_hosts` with trust on first use; a changed key is refused. ssh-agent is not supported. The TCK passes through an SSH tunnel.

The application name reported to the server (`pg_stat_activity`) is `FerroDb`.

## Authentication

| Method | Notes |
|---|---|
| Password | The only method offered. Whatever the server negotiates for a password login (`md5`, SCRAM) is handled by `tokio-postgres` |

Client certificates, Kerberos/GSSAPI and IAM tokens are not supported.

## What works

| Area | Support |
|---|---|
| Explorer | Schemas → tables (incl. partitioned and foreign), views, materialized views, routines (overloads by signature), sequences → columns, primary/foreign/unique keys, checks, indexes (partial, expression, `INCLUDE`, GIN…), triggers. Read from `pg_catalog`, cached on disk, filtered by the Schemas tab patterns |
| Console | Level A: highlighting, statement splitting (dollar quotes, nested comments, `BEGIN ATOMIC`), schema-aware completion incl. `JOIN` on foreign keys, inspections, formatter. Parameters `:name` and `$1` |
| Results | Streamed in 500-row pages; arrays, ranges, intervals, json/jsonb, uuid, inet rendered as psql does; exact decimals. Server notices go to the Output tab. Error positions are mapped to the editor |
| Cancel | PostgreSQL cancel request (also through an SSH tunnel) |
| Editing | Results of a single-table `SELECT` with a primary or unique key; preview, then one transaction |
| DDL | Tables generated from the model; views, functions, procedures and triggers use the server's definition. Bulk DDL in foreign-key order, cycles closed with `ALTER TABLE` |
| Transactions | Auto and Manual modes; DDL is transactional, so a failing DDL script is rolled back as a whole |
| Read-only | `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY` plus a client-side check |
| Explain | `EXPLAIN (FORMAT JSON)`; Explain Analyze uses `ANALYZE, BUFFERS` and is refused for statements that change data. Tree, table and diagram views |
| Import / export | Export to CSV, TSV, JSON, JSON Lines, Extended JSON, Markdown, HTML, XLSX, SQL `INSERT`, Parquet. Import uses `COPY … FROM STDIN` |
| Other | ER diagram, schema comparison with migration script, visual table editor (`ALTER`), Go to declaration (Ctrl+B), quick documentation (Ctrl+Q) |

## Limitations

- Not modelled by the introspector, so missing from generated DDL: enum, domain and composite types, exclusion constraints, policies, rules, grants, non-default operator classes, foreign-table servers, `OWNED BY` of manually created sequences.
- The language engine has rename, quick fixes and parameter info, but the console does not offer them yet; it uses completion, diagnostics and formatting.
- Navigation does not resolve table aliases (`o.id` with `FROM orders o`).
- Refreshing a routine or sequence re-reads its whole schema; there is no automatic change detection.
- TLS `verify-full` has not been tested against a server with a real certificate.
- Editing is refused for views and for results with `GROUP BY`, `HAVING`, `DISTINCT` or set operations.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `connection failed: host:5432: timed out` | Check host, port and firewall; on a remote network use the SSH tab |
| `… (SQLSTATE 28P01)` | Wrong password. Retype it in the dialog (an empty password field keeps the stored one) |
| `… (SQLSTATE 3D000)` | The database does not exist; fix the Database field |
| TLS errors with `require` / `verify-full` | The server has no TLS: use `prefer` or `disable`. Certificate errors with `verify-full`: the server certificate is not trusted by the system store; set `ssl.ca` to your CA file |
| `connection lost` after idle time | FerroDb reconnects on the next statement and warns; an open transaction is reported as lost |
| Statement refused with "data source is read-only" | The source is marked read-only; clear the flag in the dialog |
