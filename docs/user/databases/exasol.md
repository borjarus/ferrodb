# Exasol

Analytical tabular database, SQL (Exasol dialect), reached through the **JDBC bridge (J)**; language intelligence **level B**.

## Requirements

| What | Details |
|---|---|
| Tested | Not run against an Exasol server (`docs/phase-8.md`: no container used). Covered by unit tests and a TCK entry that runs when a server is configured |
| JDBC driver | `com.exasol:exasol-jdbc` from Maven Central, downloaded by the driver manager and checked against its SHA-256; driver class `com.exasol.jdbc.EXADriver` |
| Versions offered | **26.2.10** (needs Java 11+), **25.2.6** (Java 8+) |
| License | EXAClient License (shown before download) |
| Java runtime | Java 17 or newer (installed, or Eclipse Temurin 21 downloaded automatically with the driver when none is found) |
| Network | TCP port **8563** (default) |

## Connecting

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `8563` | |
| Schema | — | Optional; opened at connect time |
| User / Password | — | |
| URL | — | Overrides the fields above |

URL built: `jdbc:exa:<host>:<port>;schema=<schema>` (without `;schema=…` when the field is empty).

- **Options tab**: `key=value; …` pairs become JDBC connection properties of the Exasol driver (use them for the driver's encryption / certificate settings); `connect_timeout` (seconds, default 15) is FerroDb's own.
- **SSH tunnel**: supported (the bridge connects to the tunnel's local end).
- **TLS**: the SSL tab is not used for JDBC; TLS is whatever the Exasol driver negotiates with its own properties / URL parameters.

## Authentication

| Method | Fields |
|---|---|
| User / password | **User**, **Password** (keyring) |

## What works

- **Explorer**: schemas (`EXA_SCHEMAS`) → tables (`EXA_ALL_TABLES`) and views (`EXA_ALL_VIEWS`) → columns (`EXA_ALL_COLUMNS`); further objects through `DatabaseMetaData`.
- **Console**: SQL with the Exasol dialect file (keywords, types, functions with signatures), splitting, completion, diagnostics, formatter (level B). The schema switch runs `OPEN SCHEMA "<name>"`; current schema from `SELECT CURRENT_SCHEMA`.
- **Results**: grid, Arrow batches streamed with paging; cancel via `Statement.cancel`.
- **Editing**: grid edits with the operation preview.
- **Definitions**: generated DDL.
- **Transactions**: Auto and Manual modes; DDL is transactional.
- **Execution plan**: text, from Exasol profiling (see Limitations).
- **Read-only mode**: `setReadOnly(true)` and a client-side write check.
- **Export**: all result export formats.

## Limitations

- **Explain runs the query**: Exasol has no estimated plan. FerroDb turns profiling on (`ALTER SESSION SET PROFILE = 'ON'`), runs the statement, then reads `EXA_USER_PROFILE_LAST_DAY` after `FLUSH STATISTICS`. Explain is refused for statements that are not queries.
- Never tested against a real Exasol server; report problems with the server version and driver version.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "the JDBC driver for `exasol` is not downloaded" | Download it in the driver manager |
| "JDBC bridge: ferrodb-jdbc-host.jar not found …" | Incomplete installation; reinstall or set `FERRODB_JDBC_HOST_JAR` |
| Certificate / fingerprint errors | Set the Exasol driver's TLS properties in the Options tab or the URL |
| Connection times out | Check the port (8563) and firewall; raise `connect_timeout` in the Options tab |
