# IBM Db2

Tabular · SQL (Db2 dialect) · access layer **J** (JDBC bridge, IBM Data Server Driver for JDBC, "jcc") · language intelligence level **B**.

## Requirements

| | |
|---|---|
| Target | Db2 for Linux, UNIX and Windows (LUW) 11.5 / 12.1 (the versions the dialect file is written for) |
| Tested | **Not run against a Db2 server.** Covered by unit tests on recorded outputs; no Db2 container is part of the test setup |
| JDBC driver | Downloaded by the driver manager from Maven Central (not bundled): `com.ibm.db2:jcc`, versions **12.1.5.0** (default, newest) or 12.1.4.0, SHA-256 verified. License: IBM International Program License Agreement (IPLA) |
| Java | Java 17 or newer for the JDBC host (the driver itself needs Java 8). If none is found, downloading the driver also downloads an Eclipse Temurin 21 JRE |
| Network | TCP to the Db2 instance, port **50000** by default |

Download the driver from the data source dialog or File → Driver Manager… (version choice, offline installation from a verified local file, or custom files).

## Connecting

Driver id `db2`, family `jdbc`.

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `50000` | |
| Database | — | **Required** |
| User | — | |
| Password | — | Keyring |
| URL (overrides the fields above) | — | Any URL the jcc driver accepts |

URL built from the fields: `jdbc:db2://{host}:{port}/{database}`.

- Driver options (`key=value;…`) are passed to jcc as JDBC connection properties; use them (or the URL) for TLS. The SSL tab has no effect for JDBC databases.
- The SSH tunnel works with the host/port fields, not with a URL in the URL field.
- `connect_timeout` (seconds, default 15) is used by FerroDb and not passed on.

## Authentication

| Method | Notes |
|---|---|
| Password | The only method offered |

## What works

| Area | Support |
|---|---|
| Explorer | Schemas (`syscat.schemata`) → tables, views → columns (`syscat.*`). Introspection through `DatabaseMetaData`: primary and foreign keys, indexes, procedures, functions; view definitions from `syscat.views`; sequences from `syscat.sequences` |
| Console | Level B: highlighting, statement splitting, schema-aware completion (objects, columns, keywords, built-in functions), formatter |
| Results | Arrow streams from the JDBC host |
| Cancel | `Statement.cancel` |
| Schema switch | `SET SCHEMA`; current schema from `VALUES CURRENT SCHEMA` |
| Editing | Single-table `SELECT` results with a key |
| DDL | Generated from the model (identity, generated columns, `COMMENT ON`, sequences) |
| Transactions | Auto and Manual modes; DDL is transactional |
| Read-only | `setReadOnly` plus a client-side check |
| Explain | `EXPLAIN PLAN FOR …`, then the operators of the latest explain instance as an indented text plan with costs. The explain tables are created once per user with `SYSPROC.SYSINSTALLOBJECTS` |
| Export | All export formats. Imports use `INSERT` |

## Limitations

- **Untested against a real server**; report problems with the exact error text.
- Triggers are not introspected.
- Explain gives a text plan, not the structured plan views; Explain Analyze shows the same estimated plan.
- No syntax diagnostics from `sqlparser` (the dialect has no parser there).
- Level B: no inspections with quick fixes and no rename refactoring.
- Only password authentication is offered.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `IBM Db2: no database given` | Fill Database |
| `driver \`db2\` is not downloaded` | Download it in the dialog or the Driver Manager |
| Explain fails with a missing `EXPLAIN_…` table | The user cannot create the explain tables; ask the DBA to run `SYSPROC.SYSINSTALLOBJECTS('EXPLAIN', 'C', …)` for that user |
| `SQLCODE=-30082` | Authentication failed: check user and password |
| `connection failed: timed out after 15 s` | Check host, port (50000 is only the default) and firewall, or use the SSH tab |
| `driver \`Java runtime ≥ 17 (…)\` is not downloaded` | Install Java 17+ or set `JAVA_HOME` |
