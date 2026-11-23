# Snowflake

Cloud analytical tabular database, SQL (Snowflake dialect), reached through **ADBC (A)** with the Apache Arrow ADBC Snowflake driver; language intelligence **level B**.

## Requirements

| What | Details |
|---|---|
| Tested | Not run against a real Snowflake account (`docs/phase-8.md`). Covered by unit tests on recorded responses and a cloud test that runs when `FERRODB_TCK_SNOWFLAKE_*` variables are set |
| Driver library | `adbc_driver_snowflake` **1.11.0** (Apache Arrow ADBC), downloaded on demand by the driver manager from the project's PyPI wheel, verified with its SHA-256, and extracted under the platform's name (`adbc_driver_snowflake.dll`, `libadbc_driver_snowflake.so`, `libadbc_driver_snowflake.dylib`) |
| Platforms | Windows x86_64, Linux x86_64 / aarch64, macOS x86_64 / aarch64 |
| License | Apache-2.0 |
| Java | Not needed |
| Network | HTTPS to your Snowflake account; for SSO, a local browser |

The library can also be supplied by hand: a `driver.library` path property in the data source configuration (not shown in the form), the environment variable `FERRODB_ADBC_SNOWFLAKE`, or a file dropped into `<data dir>/drivers/adbc`.

## Connecting

| Field | Required | Notes |
|---|---|---|
| Account | yes | Snowflake account identifier |
| Warehouse | no | Compute warehouse used by the session |
| Database | no | The explorer lists the schemas of this database |
| Schema | no | Default schema |
| Role | no | Session role |
| User | yes | |

- No host / port: the driver reaches the account over HTTPS. The **SSL** and **SSH** tabs are not used.
- **Options tab**: `adbc.…=value` pairs are passed to the ADBC driver as raw database options; FerroDb also sets the application name and a login timeout from `connect_timeout` (seconds, default 15).

## Authentication

| Method (dialog) | ADBC auth type | Fields |
|---|---|---|
| Password | `auth_snowflake` | User, Password |
| Key pair | `auth_jwt` | User, **Private key (PEM)** pasted into a secret field, or **Private key file** (path); optional **Private key passphrase** for an encrypted key |
| SSO | `auth_ext_browser` | User; the driver opens the browser for the identity provider |

Secrets (password, private key, passphrase) are stored in the system keyring. When the method is not chosen explicitly, a configured key selects key pair, a password selects password, otherwise SSO is used. OAuth tokens are not offered.

## What works

- **Explorer**: schemas of the connection's database → tables and views → columns, from `INFORMATION_SCHEMA`, with primary and foreign keys from `SHOW PRIMARY KEYS` / `SHOW IMPORTED KEYS`.
- **Console**: Snowflake dialect file (keywords, types, functions with docs), splitting, completion, diagnostics, formatter (level B). The schema picker runs `use schema "<name>"`; `USE WAREHOUSE`, `USE ROLE`, `USE DATABASE` can be typed in the console.
- **Results**: grid; Arrow batches from the driver are re-chunked and streamed with backpressure; cancel uses `AdbcStatementCancel`.
- **Editing**: grid edits with the operation preview (Snowflake quoting rules).
- **Definitions**: generated DDL.
- **Transactions**: Auto and Manual modes (ADBC autocommit off). DDL commits implicitly in Snowflake.
- **Execution plan**: `EXPLAIN USING JSON`, shown as tree / table / diagram.
- **Read-only mode**: the ADBC read-only option when the driver accepts it, otherwise the connection refuses statements that change data.
- **Export**: all result export formats.

## Limitations

- **No warehouse / role picker in the UI** yet: the backend can list and switch warehouses, roles and databases (`show warehouses`, `show roles`, `use …`), but the data source form and toolbar do not offer them. Use the fields or `USE …` in the console.
- **No OAuth** and no MFA-specific options beyond what the external browser flow handles.
- The explorer shows **one database** (the one in the form or the session's current one).
- Never tested against a real account.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "Snowflake: the account is required" | Fill in **Account** |
| "Snowflake key pair: a private key is required" | Paste the PEM key or give the key file path |
| Driver missing | Download the Snowflake driver in the driver manager (or set `FERRODB_ADBC_SNOWFLAKE`) |
| "ADBC driver library not found: …" | The `driver.library` path does not point at a file |
| "No active warehouse" errors | Fill in **Warehouse** or run `USE WAREHOUSE …` |
| Explorer is empty | Set **Database** (the explorer lists the schemas of the current database only) |
