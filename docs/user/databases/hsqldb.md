# HyperSQL (HSQLDB)

Tabular database, SQL (HSQLDB dialect), reached through the **JDBC bridge (J)**; language intelligence **level B**. Embedded file, in-memory and server modes.

## Requirements

| What | Details |
|---|---|
| Tested | Embedded HyperSQL inside the JDBC host on Java 17 (driver TCK passed, `docs/phase-8.md`). Server mode was not run against a server |
| JDBC driver | `org.hsqldb:hsqldb` from Maven Central, downloaded by the driver manager and checked against its SHA-256; driver class `org.hsqldb.jdbc.JDBCDriver` |
| Versions offered | **2.7.4**, **2.7.3** (both need Java 11+, the bridge itself needs 17+) |
| License | HSQLDB License (BSD) |
| Java runtime | Java 17 or newer, found on the system (`JAVA_HOME`, `PATH`, usual locations) or an Eclipse Temurin 21 JRE downloaded together with the driver when none is found |
| Network | Server mode only: TCP port **9001** (HSQL protocol) |

## Connecting

| Mode | Fields used | JDBC URL built |
|---|---|---|
| `embedded` (default) | Database (path or name) | `jdbc:hsqldb:file:<path>` |
| `memory` | Database (name) | `jdbc:hsqldb:mem:<name>` |
| `server` | Host (default `localhost`), Port (default `9001`), Database | `jdbc:hsqldb:hsql://<host>:<port>/<name>` |

- **URL (overrides the fields above)**: any HSQLDB URL (for example `jdbc:hsqldb:hsqls://…` or `jdbc:hsqldb:http://…`) is used as is.
- Backslashes in an embedded path are turned into `/`.
- **Options tab**: `key=value; …` pairs become JDBC connection properties (for example HSQLDB's `ifexists=true`); `connect_timeout` (seconds, default 15) is FerroDb's own.
- **SSH tunnel**: server mode only.
- **TLS**: not configured by the SSL tab; use an `hsqls://` URL and the driver's properties.

## Authentication

| Method | Fields |
|---|---|
| User / password | **User**, **Password** (keyring). A new HSQLDB database has the user `SA` with an empty password |

## What works

- **Explorer**: schemas (`information_schema.schemata`) → tables and views → columns; keys, indexes, routines, sequences and triggers through `DatabaseMetaData` and the HSQLDB catalog.
- **Console**: SQL with the HyperSQL dialect file, splitting, schema-aware completion, diagnostics, formatter (level B). The schema switch runs `SET SCHEMA "<name>"`; the current schema comes from `VALUES CURRENT_SCHEMA`.
- **Results**: grid, streamed as Arrow batches with paging; cancel through `Statement.cancel`.
- **Editing**: grid edits with the operation preview.
- **Definitions**: generated DDL to the console or clipboard.
- **Transactions**: Auto and Manual modes. DDL is **not** transactional (it commits).
- **Execution plan**: text plan from `EXPLAIN PLAN FOR …`.
- **Read-only mode**: `setReadOnly(true)` plus a client-side check refusing statements that write.
- **Export**: all result export formats (CSV, JSON, XLSX, …).

## Limitations

- An embedded database is opened by the JDBC host process; other programs cannot open the same files at the same time (HSQLDB lock file).
- Server mode and the `hsqls` / `http` variants were not part of the tested setup.
- An in-memory database lives as long as the JDBC host process; it is lost when the bridge restarts.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "the JDBC driver for `hsqldb` is not downloaded (Drivers → Download)" | Download it in the driver manager |
| "JDBC bridge: ferrodb-jdbc-host.jar not found …" | Incomplete installation; reinstall or set `FERRODB_JDBC_HOST_JAR` |
| "HyperSQL: no database given" | Fill in **Database** or give a URL |
| "Database lock acquisition failure" | Another process holds the database files; close it or use server mode |
| Objects created earlier are missing | In `memory` mode data disappears when the bridge process restarts; use `embedded` with a file path |
