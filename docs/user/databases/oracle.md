# Oracle

Tabular · SQL + PL/SQL (Oracle dialect) · access layer **J** (JDBC bridge, Oracle JDBC thin driver; no Instant Client) · language intelligence level **A**.

## Requirements

| | |
|---|---|
| Tested server | Oracle Database 23ai Free (`gvenzl/oracle-free:slim`), TCK through the JDBC bridge |
| Dialect file | Oracle 19c / 21c / 23ai (functions marked 21c+ / 23ai need that release) |
| JDBC driver | Downloaded by the driver manager from Maven Central (not bundled): `com.oracle.database.jdbc:ojdbc11`, versions **23.26.4.0.0** (default, newest) or 23.9.0.25.07, SHA-256 verified. License: Oracle Free Use Terms and Conditions (FUTC) |
| Java | Java 17 or newer for the JDBC host. If none is found (`JAVA_HOME`, `PATH`, usual install locations), downloading the driver also downloads an Eclipse Temurin 21 JRE from Adoptium |
| Network | TCP to the listener, port **1521** by default |

Download the driver from the data source dialog (Download button next to the driver status) or from File → Driver Manager…, where you can also pick a version or point to local files (offline installation is verified against the catalog; other files are used unverified).

## Connecting

Driver id `oracle`, family `jdbc`.

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Port | `1521` | |
| Service name | `FREEPDB1` when empty | A service name, not a SID |
| User | — | |
| Password | — | Password authentication only; keyring |
| URL (overrides the fields above) | — | Any URL the Oracle thin driver accepts |
| Kerberos principal | — | Shown for Kerberos; see Authentication |

URL built from the fields: `jdbc:oracle:thin:@//{host}:{port}/{service}`.

- **Driver options** (Options tab, `key=value;…`) are passed to the driver as JDBC connection properties (except `connect_timeout`, used by FerroDb). Use them for anything the fields do not cover, including TLS (`tcps`) settings.
- **SSL tab has no effect** for JDBC databases: TLS is configured through the URL and driver options.
- **SSH tunnel** works with the host/port fields. With a URL in the URL field the tunnel is not used.
- FerroDb sets `v$session.program` to `FerroDb`.

TNS aliases, wallets and `DESCRIPTION=` connect strings are not handled by FerroDb itself; they reach the driver unchanged when written in the URL field and driver options.

## Authentication

| Method | Notes |
|---|---|
| Password | User and password sent to the driver |
| Kerberos | Listed, but FerroDb only sends the user: the `principal` field is not passed to the driver and no password is sent. Kerberos must be configured through the Oracle driver's own properties (Driver options) and the system Kerberos setup. Not tested |

## What works

| Area | Support |
|---|---|
| Explorer | Schemas (all users from `all_users`) → tables, views → columns. Introspection through `DatabaseMetaData` plus `ALL_*` views: primary and foreign keys, indexes (constraint indexes `SYS_C…` folded into their constraint), view definitions, sequences, triggers, procedures, functions and packages with their source, synonyms |
| Console | Level A: PL/SQL blocks and `/` splitting, `:p` parameters, `CONNECT BY` and `MERGE` in the parser, completion inside blocks (97.5 % on the test corpus), per-statement risk warnings, live templates |
| Results | Arrow streams from the JDBC host with backpressure |
| Cancel | `Statement.cancel` |
| Schema switch | `ALTER SESSION SET CURRENT_SCHEMA` |
| Editing | Single-table `SELECT` results with a key; Oracle literals (`HEXTORAW`, typed date literals, no boolean literals before 23ai, one row per `VALUES`) |
| DDL | Generated tables with identity, generated columns, `COMMENT ON`; server source for routines and packages |
| Transactions | Auto and Manual modes |
| Read-only | `setReadOnly` on the JDBC connection plus a client-side check that refuses writes before sending |
| Explain | `EXPLAIN PLAN SET STATEMENT_ID = … FOR …`, then the `PLAN_TABLE` rows as a structured plan (tree, table, diagram); the rows are deleted afterwards |
| Export | All export formats. Imports use `INSERT` |

## Limitations

- **DDL commits implicitly** in Oracle: DDL scripts run statement by statement and cannot be rolled back.
- **Explain Analyze** gives the same estimated `PLAN_TABLE` plan; actual row counts and times are not collected.
- Synonyms are introspected but not shown in the explorer.
- The explorer lists every user as a schema, including Oracle-maintained ones; use the Schemas tab filter (e.g. `HR, APP_*`).
- No syntax diagnostics from `sqlparser` (the Oracle dialect has no parser there); completion, splitting and inspections still work.
- The language engine has rename and quick fixes, but the console does not offer them yet.
- Kerberos is not wired to the driver (see Authentication). SSL tab settings are ignored.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `driver \`oracle\` is not downloaded` | Click Download in the dialog or use File → Driver Manager… |
| `driver \`Java runtime ≥ 17 (…)\` is not downloaded` | Install Java 17+ or set `JAVA_HOME`; downloading the Oracle driver again also fetches Temurin 21 when no Java is found |
| `… needs Java N, the bridge runs on Java M` | Install a newer Java, or pick an older driver version in the Driver Manager |
| `JDBC bridge: ferrodb-jdbc-host.jar not found` | The installation is incomplete; reinstall, or set `FERRODB_JDBC_HOST_JAR` to the jar |
| `ORA-12514` (listener does not know the service) | Fix the Service name (`FREEPDB1` is only the Oracle Free default); a SID needs a URL in the URL field |
| `ORA-01017` | Wrong user or password |
| Connections drop with "restarting after a failure" in the bridge status | The Java host crashed and was restarted; open sessions were disconnected and reconnect on the next statement |
| `timed out after 15 s` | The listener is unreachable; check host, port, firewall, or use the SSH tab. Raise `connect_timeout` for slow networks |
