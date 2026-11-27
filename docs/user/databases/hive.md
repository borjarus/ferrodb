# Apache Hive

Analytical tabular database, HiveQL, reached through the **JDBC bridge (J)** to HiveServer2 (Thrift); language intelligence **level B**. No transactions.

## Requirements

| What | Details |
|---|---|
| Tested | Not run against a HiveServer2 (`docs/phase-8.md`). Covered by unit tests and a TCK entry that runs when a server is configured |
| JDBC driver | `org.apache.hive:hive-jdbc` (standalone jar) from Maven Central, downloaded by the driver manager and checked against its SHA-256; driver class `org.apache.hive.jdbc.HiveDriver` |
| Versions offered | **4.2.1** (needs Java 17, ~48 MB), **4.0.1** (Java 8+, ~44 MB) |
| License | Apache-2.0 |
| Java runtime | Java 17 or newer (installed, or Eclipse Temurin 21 downloaded with the driver when none is found) |
| Network | HiveServer2 TCP port **10000** (default) |

## Connecting

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `10000` | |
| Database | `default` | |
| User / Password | — | |
| Kerberos principal | — | Shown for the Kerberos method (see Limitations) |
| URL | — | Overrides the fields above |

URL built: `jdbc:hive2://<host>:<port>/<database>` (`default` when empty). Session variables, transport mode, TLS or ZooKeeper discovery are written into a full URL in the **URL** field.

- **Options tab**: `key=value; …` pairs become JDBC connection properties; `connect_timeout` (seconds, default 15) is FerroDb's own.
- **SSH tunnel**: supported for a host / port connection.
- **TLS**: not taken from the SSL tab; use the Hive driver's URL parameters.

## Authentication

| Method | Fields |
|---|---|
| User / password | **User**, **Password** (keyring) |
| Kerberos | **User**, **Kerberos principal** |

## What works

- **Explorer**: databases (`SHOW DATABASES`) → tables (`SHOW TABLES IN`) and views (`SHOW VIEWS IN`) → columns (`DESCRIBE`); partition columns from `DESCRIBE`.
- **Console**: HiveQL dialect file (keywords, types, functions), splitting, completion, diagnostics, formatter (level B). Identifiers are quoted with backticks. The database switch runs `` USE `<db>` ``; current database from `SELECT current_database()`.
- **Results**: grid, Arrow batches with paging; cancel via `Statement.cancel`.
- **Editing**: grid edits generate HiveQL (`STRING` types, `unhex(…)` binaries, typed date literals) shown in the operation preview.
- **Definitions**: generated DDL.
- **Execution plan**: text plan from `EXPLAIN …`.
- **Read-only mode**: `setReadOnly(true)` plus a client-side write check.
- **Export**: all result export formats.

## Limitations

- **No transactions**: the source runs in auto-commit only; Manual mode and `BEGIN` are refused ("transactions (Apache Hive has none)").
- **Edits need tables that accept them**: Hive runs `UPDATE` / `DELETE` only on transactional (ACID) tables; on other tables the server rejects the generated statements.
- **Kerberos principal is not forwarded** to the driver: put `principal=…` (and the other Kerberos settings) into the JDBC URL. Kerberos was not tested.
- Never tested against a real HiveServer2.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "JDBC bridge: ferrodb-jdbc-host.jar not found …" | Incomplete installation; reinstall or set `FERRODB_JDBC_HOST_JAR` |
| "the JDBC driver for `hive` is not downloaded" | Download it in the driver manager |
| Edits fail with a "not ACID" style error | The table is not transactional; edit through `INSERT OVERWRITE` in the console instead |
| Connection hangs | Wrong transport (binary vs HTTP) or TLS mismatch: give the full `jdbc:hive2://…` URL with the server's parameters |
