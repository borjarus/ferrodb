# Apache Derby

Tabular database, SQL (Derby dialect), reached through the **JDBC bridge (J)**; language intelligence **level B**. Embedded file, in-memory and network-server modes.

## Requirements

| What | Details |
|---|---|
| Tested | Embedded Derby inside the JDBC host on Java 17 (driver TCK passed, `docs/phase-8.md`). The network server mode was not run against a server |
| JDBC driver | Downloaded by the driver manager from Maven Central: `org.apache.derby:derby` + `derbyshared`, `derbytools`, `derbyclient` (all four jars are installed together), each checked against its SHA-256 |
| Versions offered | **10.17.1.0** (needs Java 21) and **10.16.1.1** (needs Java 17) |
| License | Apache-2.0 |
| Java runtime | Java 17 or newer. FerroDb uses `JAVA_HOME`, a runtime it downloaded earlier, `java` on `PATH` or the usual install locations; if none is found, downloading the driver also downloads an Eclipse Temurin 21 JRE |
| Network | Server mode only: TCP port **1527** (Derby network server) |

## Connecting

The **Mode** field selects how the database is opened:

| Mode | Fields used | JDBC URL built |
|---|---|---|
| `embedded` (default) | Database (path or name) | `jdbc:derby:<path>;create=true` |
| `memory` | Database (name) | `jdbc:derby:memory:<name>;create=true` |
| `server` | Host (default `localhost`), Port (default `1527`), Database | `jdbc:derby://<host>:<port>/<name>;create=true` |

- **URL (overrides the fields above)**: a full JDBC URL is used as is.
- `;create=true` is always added: a database that does not exist is created.
- Backslashes in an embedded path are turned into `/`.
- **Options tab**: `key=value; …` pairs are passed to the JDBC driver as connection properties (`connect_timeout` is FerroDb's own: seconds, default 15).
- **SSH tunnel**: available in server mode only (the bridge connects to the local end of the tunnel). Ignored for embedded and in-memory databases.
- **TLS**: the SSL tab is not used by JDBC drivers; configure encryption through the URL or driver properties.

## Authentication

| Method | Fields |
|---|---|
| User / password | **User**, **Password** (stored in the system keyring) |

An embedded database without authentication accepts an empty user and password.

## What works

- **Explorer**: schemas → tables and views → columns; keys, indexes, routines and other objects come from `DatabaseMetaData` plus Derby's `SYS` catalog.
- **Console**: SQL with the Derby dialect file (keywords, types, functions), statement splitting, schema-aware completion, syntax diagnostics, formatter (level B). The schema switch runs `SET SCHEMA "<name>"`; the current schema is read with `VALUES CURRENT SCHEMA`.
- **Results**: grid; results stream from the bridge as Arrow batches with paging; cancel uses JDBC `Statement.cancel`.
- **Editing**: grid edits generate `UPDATE` / `INSERT` / `DELETE` shown in the operation preview.
- **Definitions**: generated DDL to the console or clipboard.
- **Transactions**: Auto and Manual modes; Derby DDL is transactional (rolled back with the transaction).
- **Execution plan**: text plan from Derby runtime statistics (see Limitations).
- **Read-only mode**: `Connection.setReadOnly(true)` plus a check in the connection that refuses writes.
- **Export**: every export format of the result view (CSV, JSON, XLSX, …).

## Limitations

- **Plans are measured runs**: Derby reports a plan only after running the statement (`SYSCS_SET_RUNTIMESTATISTICS`), so Explain *executes* the query; it is refused for statements that are not queries.
- **Embedded databases live in the JDBC host process**: while FerroDb has an embedded Derby database open, other programs cannot open the same database directory.
- Network server mode, server-side authentication and the 10.17 driver were not part of the tested setup.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "the JDBC driver for `derby` is not downloaded (Drivers → Download)" | Open the driver manager, accept the license and download the driver |
| "Apache Derby 10.17.1.0 needs Java 21, the bridge runs on Java 17 …" | Install Java 21 (or set `JAVA_HOME` to it), or pick version 10.16.1.1 in the driver manager |
| "JDBC bridge: ferrodb-jdbc-host.jar not found …" | The installation is incomplete; reinstall, or point `FERRODB_JDBC_HOST_JAR` at the jar |
| "Apache Derby: no database given" | Fill in **Database** (or a full URL) |
| Database is locked / cannot be booted | Another process (another FerroDb, `ij`, an application) has the embedded database open; close it or use server mode |
| Connections drop at once | The bridge process crashed and was restarted (Services panel shows the bridge state); reconnect |
