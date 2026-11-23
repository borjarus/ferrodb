# FerroDb 1.0 user guide

FerroDb is a database IDE for SQL and NoSQL databases. It runs on Windows, macOS and Linux and works with 26 databases plus DuckDB for local files.

## Start here

| Page | What it covers |
|---|---|
| [Installation](installation.md) | System requirements, installers, on-demand drivers, where files are kept, uninstalling |
| [Getting started](getting-started.md) | Projects, your first data source, running a query, reading results |
| [Keyboard shortcuts](keyboard-shortcuts.md) | Default shortcuts and how to change them |

## Working with FerroDb

| Page | What it covers |
|---|---|
| [Data sources](data-sources.md) | The data source dialog, SSH and TLS, secrets, environments, read-only mode, drivers and the Driver Manager |
| [Database Explorer](explorer.md) | The object tree, filtering, generating scripts and DDL, Search Everywhere |
| [Consoles](consoles.md) | Editor, completion, diagnostics, formatting, run modes, parameters, transactions, cancellation |
| [Results and editing](results-and-editing.md) | Result grid, document browser, key browser, editing with the change preview |
| [Import and export](import-export.md) | Export and import wizards, formats, extractor scripts |
| [Tools](tools.md) | Execution plans, ER diagrams, schema comparison, table editor, project files and DuckDB, Local History, query history |
| [AI assistant and MCP](ai-and-mcp.md) | The AI Assistant, agent permissions, the MCP server and `ferrodb mcp` |
| [Extension scripts](scripts.md) | Rhai extractors and aggregators: format and API reference |
| [Settings and files](settings-and-files.md) | Settings pages and configuration files |

## Updates, privacy and the beta

| Page | What it covers |
|---|---|
| [Updates](updates.md) | Automatic checks, Check for Updates…, stable and beta channels |
| [Privacy](privacy.md) | What FerroDb sends over the network and when |
| [Beta program](beta-program.md) | Joining and leaving the beta, reporting problems |
| [Troubleshooting](troubleshooting.md) | Java and JDBC, ADBC, TLS, SSH, keyring, Linux display, logs, crash reports |

## Databases

[Supported databases](databases/README.md) lists every database with its driver layer and language support level. Each database has its own page with requirements, authentication methods and limitations.

| Relational | Analytical and cloud | NoSQL |
|---|---|---|
| [PostgreSQL](databases/postgresql.md) | [Amazon Redshift](databases/redshift.md) | [MongoDB](databases/mongodb.md) |
| [Greenplum](databases/greenplum.md) | [Snowflake](databases/snowflake.md) | [Couchbase](databases/couchbase.md) |
| [CockroachDB](databases/cockroachdb.md) | [Google BigQuery](databases/bigquery.md) | [Amazon DynamoDB](databases/dynamodb.md) |
| [MySQL](databases/mysql.md) | [ClickHouse](databases/clickhouse.md) | [Redis](databases/redis.md) |
| [MariaDB](databases/mariadb.md) | [Exasol](databases/exasol.md) | [Apache Cassandra](databases/cassandra.md) |
| [SQLite](databases/sqlite.md) | [Vertica](databases/vertica.md) | |
| [Microsoft SQL Server](databases/sqlserver.md) | [Apache Hive](databases/hive.md) | |
| [Azure SQL](databases/azure-sql.md) | [DuckDB](databases/duckdb.md) (files) | |
| [Oracle](databases/oracle.md) | | |
| [IBM Db2](databases/db2.md) | | |
| [Sybase ASE](databases/sybase-ase.md) | | |
| [H2](databases/h2.md) | | |
| [Apache Derby](databases/derby.md) | | |
| [HyperSQL](databases/hsqldb.md) | | |

## For maintainers

- [Beta program plan](../beta/plan.md): timeline, cohorts, triage and release cadence.
