# Databases

FerroDb 1.0 supports the 26 databases of plan section 2.8, plus the embedded **DuckDB** file query engine as a 27th. Every one of them meets the same definition of a "supported database" (plan 2.9): connection with its usual authentication methods, explorer, console with highlighting / splitting / schema-aware suggestions, streamed results with paging and cancel, a result view suited to the data model, editing where the database allows it, object definitions, transactions where available, read-only mode and warnings, and export.

## Overview

Access layer: **N** = native Rust driver, **J** = JDBC bridge, **A** = ADBC library. Language level (plan 2.10): **A** = full, **B** = standard.

| Database | Model | Language | Layer | Level | Authentication | Page |
|---|---|---|---|---|---|---|
| PostgreSQL | tabular | SQL (PostgreSQL) | N | A | password | [postgresql](postgresql.md) |
| Greenplum | tabular | SQL (Greenplum) | N | B | password | [greenplum](greenplum.md) |
| CockroachDB | tabular | SQL (CockroachDB) | N | B | password | [cockroachdb](cockroachdb.md) |
| Amazon Redshift | tabular | SQL (Redshift) | N | B | password, AWS IAM | [redshift](redshift.md) |
| MySQL | tabular | SQL (MySQL) | N | A | password | [mysql](mysql.md) |
| MariaDB | tabular | SQL (MariaDB) | N | A | password | [mariadb](mariadb.md) |
| SQLite | tabular | SQL (SQLite) | N | A | none | [sqlite](sqlite.md) |
| Microsoft SQL Server | tabular | T-SQL | N | A | password (SQL login), Kerberos / Windows | [sqlserver](sqlserver.md) |
| Azure SQL | tabular | T-SQL | N | A | password, Microsoft Entra ID | [azure-sql](azure-sql.md) |
| Oracle | tabular | SQL + PL/SQL | J | A | password, Kerberos | [oracle](oracle.md) |
| IBM Db2 | tabular | SQL (Db2) | J | B | password | [db2](db2.md) |
| Sybase ASE | tabular | T-SQL (Sybase) | J | B | password | [sybase-ase](sybase-ase.md) |
| H2 | tabular | SQL (H2) | J | B | password | [h2](h2.md) |
| Apache Derby | tabular | SQL (Derby) | J | B | password | [derby](derby.md) |
| HyperSQL | tabular | SQL (HSQLDB) | J | B | password | [hsqldb](hsqldb.md) |
| Exasol | tabular | SQL (Exasol) | J | B | password | [exasol](exasol.md) |
| Vertica | tabular | SQL (Vertica) | J | B | password, Kerberos | [vertica](vertica.md) |
| Apache Hive | tabular | HiveQL | J | B | password, Kerberos | [hive](hive.md) |
| Snowflake | tabular | SQL (Snowflake) | A | B | password, key pair, SSO (external browser) | [snowflake](snowflake.md) |
| Google BigQuery | tabular | GoogleSQL | A | B | service account key / application default credentials | [bigquery](bigquery.md) |
| ClickHouse | tabular | SQL (ClickHouse) | N | B | password | [clickhouse](clickhouse.md) |
| Apache Cassandra | tabular (wide-column) | CQL | N | B | none, password | [cassandra](cassandra.md) |
| MongoDB | document | MQL (shell syntax) | N | A | none, password (SCRAM), X.509 | [mongodb](mongodb.md) |
| Couchbase | document | SQL++ | N | B | password, X.509 | [couchbase](couchbase.md) |
| Amazon DynamoDB | document (key-value) | PartiQL + API operations | N | B | AWS IAM (profiles, SSO, credential process, static keys) | [dynamodb](dynamodb.md) |
| Redis | key-value | Redis commands | N | B | none, password / ACL user | [redis](redis.md) |
| DuckDB (27th) | tabular | SQL (DuckDB) | N | A | none | [duckdb](duckdb.md) |

Kerberos for the JDBC databases is configured through the driver's own URL / properties (see each page). Custom JDBC drivers are level **C** (below).

## Access layers

### Native drivers (N)

Built into FerroDb: nothing to download, no Java. PostgreSQL, Greenplum, CockroachDB and Redshift share one PostgreSQL-protocol driver; MySQL and MariaDB the MySQL-protocol driver; SQL Server and Azure SQL the TDS driver. Every TLS stack is rustls with `ring`. The **SSL** tab (`disable` / `prefer` / `require` / `verify-full`) and the **SSH** tab apply to network drivers unless a page says otherwise.

### JDBC bridge (J)

Oracle, Db2, Sybase ASE, H2, Derby, HyperSQL, Exasol, Vertica, Hive and custom drivers run in **`ferrodb-jdbc-host`**, a small Java process shipped with FerroDb (`ferrodb-jdbc-host.jar` next to the executable; `FERRODB_JDBC_HOST_JAR` overrides it).

- **Started on demand** by the first JDBC connection; one process serves every JDBC connection. If it crashes it is restarted, and its connections are reported as lost. Its state (not started / starting / running with PID and connection count / restarting / failed) is shown in the driver manager and the Services panel.
- **Java runtime 17+**: FerroDb looks at `JAVA_HOME`, runtimes it downloaded earlier, `java` on `PATH` and the usual install locations. If none qualifies, downloading a JDBC driver also downloads an **Eclipse Temurin 21 JRE** from the Adoptium API, verified with the published checksum and stored under `<data dir>/drivers/runtime/`. You never need to install Java yourself.
- **Drivers are not bundled.** The driver manager (**Drivers** dialog) lists each driver with its Maven Central coordinates, versions (newest first) and license; the license is shown before download. Every file is checked against the SHA-256 pinned in the catalog before installation and again before it is loaded. Files live under `<data dir>/drivers/jdbc/<id>/<version>/`.
- **Offline installation**: choosing local jar files for a catalog driver installs them if they match a catalog version (still verified); other files are used as custom files (unverified).
- If a driver version needs a newer Java than the runtime in use (Derby 10.17 needs Java 21), the connection fails with a message naming both versions.
- JDBC connections take driver properties from the **Options** tab (`key=value; …`), use the SSH tunnel in server modes, and ignore the SSL tab (TLS is a driver setting). Passwords travel to the bridge in memory, never on a command line.

### ADBC libraries (A)

Snowflake and BigQuery use the Apache Arrow **ADBC** drivers (version 1.11.0), loaded as native libraries; results arrive directly as Arrow.

- The libraries are **downloaded on demand** by the same driver manager: the Apache Arrow ADBC project's PyPI wheels for Windows x86_64, Linux x86_64 / aarch64 and macOS x86_64 / aarch64, each **verified with its SHA-256**, then the library is extracted under the platform's file name (`.dll`, `.so`, `.dylib`). License: Apache-2.0.
- A library can also be supplied by hand: a `driver.library` path in the data source configuration, the environment variable `FERRODB_ADBC_<DRIVER>` (e.g. `FERRODB_ADBC_SNOWFLAKE`), or a file in `<data dir>/drivers/adbc/`.
- Raw driver options can be given in the **Options** tab as `adbc.…=value`.

## Custom JDBC drivers (level C)

Any JDBC driver can be used with the **Custom JDBC** data source:

1. Choose the driver's `.jar` files in the driver manager ("custom files"). They are copied under `<data dir>/drivers/jdbc/jdbc/custom/`; they are **not verified** (no catalog checksum).
2. In the data source, give the **URL** (required) and optionally the **Driver class**; without a class, the first driver in the jars that registers itself and accepts the URL is used.
3. User / password and Options-tab properties are passed to the driver.

Language support is **level C**: generic SQL highlighting, statement splitting and object-name suggestions from JDBC metadata. Results, paging, cancel (`Statement.cancel`), editing, Auto / Manual transactions, read-only mode and export work as for the other JDBC databases; there are no execution plans and no dialect-specific features.
