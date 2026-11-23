# Microsoft SQL Server

Tabular · T-SQL · access layer **N** (native, `tiberius` over TDS, vendored with patches) · language intelligence level **A**.

## Requirements

| | |
|---|---|
| Tested server | SQL Server 2022 (`mcr.microsoft.com/mssql/server:2022-latest`, 16.0.4325.4), TCK 14/14 |
| Minimum server | SQL Server 2017: the introspector uses `STRING_AGG` |
| Download | Nothing. The driver is built into FerroDb |
| Network | TCP to the server, port **1433** by default. There is no instance-name field: give the port of a named instance |

## Connecting

Driver id `sqlserver`, family `tds`.

| Field | Default | Auth | Notes |
|---|---|---|---|
| Host | `localhost` | all | |
| Port | `1433` | all | |
| Database | `master` | all | |
| User | `sa` when empty | Password | SQL login. With Kerberos: optional Windows account |
| Password | — | Password, Kerberos | Keyring |
| Trust the server certificate | `true` | all | `trust_server_certificate` |
| Windows account (`DOMAIN\user`; empty = current user) | — | Kerberos | `principal` |

URL preview: `sqlserver://{host}:{port};database={database}` (display only).

**TLS** (SSL tab):

| Mode | Behaviour |
|---|---|
| `disable` | No encryption requested |
| `prefer` (default), `require` | Encrypted; the certificate is trusted without checking |
| `verify-full` | Encryption required; the certificate is verified against the system store or the `ssl.ca` file, unless "Trust the server certificate" is `true` |

TDS 8.0 strict encryption is not supported.

**SSH tunnel**: SSH tab, as for the other network databases (key file, `known_hosts` with trust on first use).

**Options**: `connect_timeout`, `query_timeout` (seconds).

At login FerroDb sets the options SSMS and ODBC set (`ANSI_NULLS`, `ANSI_PADDING`, `ANSI_WARNINGS`, `ARITHABORT`, `CONCAT_NULL_YIELDS_NULL`, `QUOTED_IDENTIFIER` on, `NUMERIC_ROUNDABORT` off), needed for indexed views and filtered indexes.

## Authentication

| Method (dialog label) | How it works |
|---|---|
| Password | SQL Server login (user + password) |
| Kerberos — Windows integrated | Windows SSPI as the current Windows user: Kerberos when the server has an SPN, NTLM otherwise. **Windows build only** |
| Kerberos — explicit account | Windows account (`DOMAIN\user`) and password: NTLM. **Windows build only** |

On Linux and macOS the Kerberos method fails with "Windows / Kerberos authentication is available in the Windows build only" (GSSAPI is not linked). Microsoft Entra ID is offered for [Azure SQL](azure-sql.md).

## What works

| Area | Support |
|---|---|
| Explorer | Schemas of the current database (role schemas hidden; `sys`, `INFORMATION_SCHEMA`, `guest` last) → tables, views → columns. Introspection from `sys.*`: computed columns, identity, collations, `MS_Description` comments, keys, checks, clustered / columnstore / filtered indexes, routines, sequences, triggers |
| Console | Level A: splitting by `GO [n]`, `BEGIN … END`, `TRY/CATCH` and routine bodies; `@p` / `:p` parameters; `TOP`, `APPLY`, `OUTPUT`, `MERGE` in the parser; completion inside blocks; per-statement risk warnings; live templates |
| Results | Every type mapped to Arrow (exact `money`, `datetimeoffset` in UTC, `sql_variant` as text); several result sets and row counts per batch |
| Messages and errors | `PRINT` / `RAISERROR` as notices; errors with number and the line they occurred on (inside a procedure: procedure name and line) |
| Batches | A line holding only `GO [count]` ends a batch, which runs `count` times |
| Cancel | TDS attention on the same connection; temporary tables, `USE` and `SET` options survive |
| Editing | Single-table `SELECT` results with a key; T-SQL literals (`N'…'`, `0x…`), `UPDATE/DELETE TOP (1)`, `IDENTITY_INSERT`, `OUTPUT INSERTED.*` |
| DDL / ALTER | Brackets, `IDENTITY`, `PERSISTED`, `NONCLUSTERED` primary keys; renames with `sp_rename`; defaults dropped by name. Views, routines and triggers use the server text (`sys.sql_modules`) |
| Transactions | Auto and Manual modes; DDL is transactional (except statements such as `CREATE DATABASE`) |
| Read-only | `ApplicationIntent=ReadOnly` at login (routes to readable secondaries) and a client-side check |
| Explain | XML showplans: estimated (`SHOWPLAN_XML`) and actual (`STATISTICS XML`); missing indexes, implicit conversions, spills and key lookups are flagged |
| Export | All export formats |

## Limitations

- **No bulk load**: imports use `INSERT`.
- **Default schema**: SQL Server has no session default schema. Choosing a schema in the console sets it for the explorer, but unqualified names in scripts still resolve through the login's default schema. Choosing a database name runs `USE`.
- Kerberos / Windows authentication only in the Windows build; no GSSAPI on Linux / macOS.
- No TDS 8.0 strict encryption.
- Synonyms are introspected but not shown in the explorer.
- T-SQL diagnostics take about 109 ms on a 5,000-line console in release builds (budget 100 ms).
- Read-only mode switched on later in a session is a client-side check only; `ApplicationIntent` is set at login.
- The language engine has rename and quick fixes, but the console does not offer them yet.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `connection failed: host:1433: Login failed for user … (error 18456)` | Wrong login or password, or SQL authentication is disabled on the server (Windows-only mode) |
| Certificate errors with `verify-full` | The server uses a self-signed certificate: provide `ssl.ca`, set "Trust the server certificate", or use `require` |
| `Windows / Kerberos authentication is available in the Windows build only` | Use a SQL login on Linux / macOS |
| Kerberos falls back to NTLM or fails across domains | The server needs a registered SPN for Kerberos; check with your domain administrator |
| `a user name is required` | Fill User (password authentication) |
| Unqualified table not found after picking a schema | Qualify the name (`schema.table`) or change the login's default schema |
