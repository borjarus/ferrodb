# MariaDB

Tabular · SQL (MariaDB dialect, derived from MySQL) · access layer **N** (native, `mysql_async`, MySQL protocol) · language intelligence level **A**.

## Requirements

| | |
|---|---|
| Tested server | MariaDB 11.4 (`mariadb:11.4` container, TCK 14/14 on 11.4.13) |
| Download | Nothing. The driver is built into FerroDb (shared with MySQL) |
| Network | TCP to the server, port **3306** by default |

## Connecting

Driver id `mariadb`, family `mysql`.

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `3306` | |
| Database | — | Optional default database |
| User | — | |
| Password | — | Stored in the system keyring |

URL preview: `mariadb://{user}@{host}:{port}/{database}`.

Driver options (`connect_timeout`, `query_timeout`), TLS modes (`disable` / `prefer` / `require` / `verify-full`, optional `ssl.ca`) and the SSH tunnel work as for [MySQL](mysql.md#connecting).

The product is detected from `SELECT VERSION()`: a MariaDB server reached through the MySQL driver is still recognized as MariaDB (the old `5.5.5-` prefix is removed from the version).

## Authentication

| Method | Notes |
|---|---|
| Password | The only method offered |

## What works

Everything listed for [MySQL](mysql.md#what-works), with these MariaDB differences:

| Area | MariaDB specifics |
|---|---|
| Explorer | System-versioned tables are listed with the base tables; sequences are introspected |
| Column defaults | Read as SQL expressions (`'abc'`, `current_timestamp()`), as MariaDB reports them |
| Types | `JSON` is an alias of `LONGTEXT` and is shown as text. The dialect adds `inet4`, `inet6`, `uuid` and keywords such as `RETURNING`, `SYSTEM_TIME`, `VERSIONING` |
| Explain | `EXPLAIN FORMAT=JSON`; Explain Analyze uses `ANALYZE FORMAT=JSON` and is refused for statements that change data |
| Recursive CTE limit | `max_recursive_iterations` (MySQL: `cte_max_recursion_depth`) |
| Read-only | `SET SESSION TRANSACTION READ ONLY` plus a client-side check |
| Import | `LOAD DATA LOCAL INFILE`, falling back to multi-row `INSERT` |

## Limitations

- **DDL is not transactional**: a failing statement in a DDL script leaves the earlier ones applied.
- Not modelled: events, partitioning, the table `AUTO_INCREMENT` counter.
- Functional index key parts (`STATISTICS.EXPRESSION`) exist in MySQL only and are not read for MariaDB.
- The language engine has rename, quick fixes and parameter info, but the console does not offer them yet.
- Error ranges are reported for syntax errors (1064) only.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `connection failed: Access denied for user … (1045)` | Wrong user or password, or the user is not allowed from your host |
| `the server does not support TLS` | The server has TLS off: use `prefer` or `disable` |
| Import warns that `LOAD DATA` was refused | `local_infile` is disabled on the server; the import continues with `INSERT` |
| JSON columns shown as text | Expected: MariaDB stores `JSON` as `LONGTEXT` |
