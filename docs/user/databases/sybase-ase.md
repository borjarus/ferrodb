# Sybase ASE

Tabular · T-SQL (Sybase ASE dialect, derived from T-SQL) · access layer **J** (JDBC bridge, jTDS driver) · language intelligence level **B**.

## Requirements

| | |
|---|---|
| Target | SAP Adaptive Server Enterprise 16 (the version the dialect file is written for) |
| Tested | **Not run against an ASE server.** No public container image is used; covered by unit tests on recorded outputs |
| JDBC driver | Downloaded by the driver manager from Maven Central (not bundled): `net.sourceforge.jtds:jtds` **1.3.1**, SHA-256 verified. License: LGPL-2.1. SAP jConnect is not in the catalog |
| Java | Java 17 or newer for the JDBC host. If none is found, downloading the driver also downloads an Eclipse Temurin 21 JRE |
| Network | TCP to the server, port **5000** by default |

Download the driver from the data source dialog or File → Driver Manager…. Another driver (for example jConnect) can only be used as custom files, unverified.

## Connecting

Driver id `sybase`, family `jdbc`.

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `5000` | |
| Database | — | Optional |
| User | — | |
| Password | — | Keyring |
| URL (overrides the fields above) | — | Any URL jTDS accepts |

URL built from the fields: `jdbc:jtds:sybase://{host}:{port}/{database}`.

- Driver options (`key=value;…`) are passed to jTDS as connection properties; use them for TLS. The SSL tab has no effect for JDBC databases.
- The SSH tunnel works with the host/port fields, not with a URL in the URL field.
- Liveness is checked with `select 1` (jTDS has no `Connection.isValid`).

## Authentication

| Method | Notes |
|---|---|
| Password | The only method offered |

## What works

| Area | Support |
|---|---|
| Explorer | **Databases** (`master..sysdatabases`) → tables, views → columns. Introspection through `DatabaseMetaData` with the database as JDBC catalog: primary and foreign keys, indexes, procedures, functions |
| Console | Level B: highlighting, statement splitting, schema-aware completion, syntax diagnostics (SQL Server parser), formatter. The dialect removes SQL Server-only features and adds ASE ones |
| Results | Arrow streams from the JDBC host |
| Cancel | `Statement.cancel` |
| Database switch | JDBC `setCatalog` |
| Editing | Single-table `SELECT` results with a key; Sybase literals (`bit` booleans, `0x…` binaries) |
| Transactions | Auto and Manual modes |
| Read-only | `setReadOnly` plus a client-side check |
| Explain | `set showplan on` + `set noexec on`: the showplan messages as a text plan, without running the statement |
| Export | All export formats. Imports use `INSERT` |

## Limitations

- **Untested against a real server**; report problems with the exact error text.
- **DDL is not transactional** here (FerroDb runs DDL scripts statement by statement).
- View definitions, sequences and triggers are not introspected.
- Explain is a text plan; Explain Analyze shows the same showplan output.
- Level B: no inspections with quick fixes and no rename refactoring.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `driver \`sybase\` is not downloaded` | Download jTDS in the dialog or the Driver Manager |
| `connection failed: timed out after 15 s` | Check host and port (5000 is only the default), or use the SSH tab |
| Objects of another database missing | Expand that database in the explorer; each database is a separate container |
| `driver \`Java runtime ≥ 17 (…)\` is not downloaded` | Install Java 17+ or set `JAVA_HOME` |
