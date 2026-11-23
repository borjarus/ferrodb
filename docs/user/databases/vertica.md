# Vertica

Analytical tabular database, SQL (Vertica dialect), reached through the **JDBC bridge (J)**; language intelligence **level B**.

## Requirements

| What | Details |
|---|---|
| Tested | Not run against a Vertica server (`docs/phase-8.md`). Covered by unit tests and a TCK entry that runs when a server is configured |
| JDBC driver | `com.vertica.jdbc:vertica-jdbc` from Maven Central, downloaded by the driver manager and checked against its SHA-256; driver class `com.vertica.jdbc.Driver` |
| Versions offered | **25.3.0-0**, **25.2.0-0** (Java 8+) |
| License | Vertica Client License Agreement (shown before download) |
| Java runtime | Java 17 or newer (installed, or Eclipse Temurin 21 downloaded with the driver when none is found) |
| Network | TCP port **5433** (default) |

## Connecting

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `5433` | |
| Database | — | |
| User / Password | — | |
| Kerberos principal | — | Shown for the Kerberos method (see Limitations) |
| URL | — | Overrides the fields above |

URL built: `jdbc:vertica://<host>:<port>/<database>`.

- **Options tab**: `key=value; …` pairs are passed to the Vertica driver as connection properties (TLS, Kerberos and other driver settings); `connect_timeout` (seconds, default 15) is FerroDb's own.
- **SSH tunnel**: supported.
- **TLS**: the SSL tab is not used for JDBC; configure the driver's TLS through Options or the URL.

## Authentication

| Method | Fields |
|---|---|
| User / password | **User**, **Password** (keyring) |
| Kerberos | **User**, **Kerberos principal** |

## What works

- **Explorer**: schemas (`v_catalog.schemata`) → tables and views → columns; keys, indexes and routines from `DatabaseMetaData`.
- **Console**: SQL with the Vertica dialect file, splitting, completion, diagnostics, formatter (level B). The schema switch runs `SET SEARCH_PATH TO "<schema>"`; current schema from `SELECT CURRENT_SCHEMA()`.
- **Results**: grid, Arrow batches streamed with paging; cancel via `Statement.cancel`.
- **Editing**: grid edits with the operation preview (inserts are written one row per `VALUES`).
- **Definitions**: generated DDL.
- **Transactions**: Auto and Manual modes; DDL is not transactional.
- **Execution plan**: text plan from `EXPLAIN …`.
- **Read-only mode**: `setReadOnly(true)` and a client-side write check.
- **Export**: all result export formats.

## Limitations

- **Kerberos principal is not forwarded**: the bridge passes only the user, the password and the Options-tab properties to the driver. For Kerberos, give the Vertica driver's own Kerberos properties in the Options tab or the URL. Kerberos was not tested.
- Never tested against a real Vertica server.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "the JDBC driver for `vertica` is not downloaded" | Download it in the driver manager |
| Authentication fails with Kerberos | Set the driver's Kerberos properties (Options tab / URL); the principal field alone has no effect |
| Unqualified table names not found | Pick the schema in the console's schema picker (sets the search path) or qualify the names |
| Connection drops for every JDBC source at once | The bridge process restarted after a crash; reconnect |
