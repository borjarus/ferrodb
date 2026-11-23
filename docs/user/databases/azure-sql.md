# Azure SQL Server

Tabular · T-SQL · access layer **N** (native, `tiberius` over TDS, with Microsoft Entra ID tokens) · language intelligence level **A**.

## Requirements

| | |
|---|---|
| Supported | Azure SQL Database, Azure SQL Managed Instance (the product is recognized from `SERVERPROPERTY('EngineEdition')`) |
| Tested | **Not run against a real Azure service.** Covered by unit tests and the SQL Server TCK; the Azure TCK entry runs only when `FERRODB_TCK_*` variables are set |
| Download | Nothing. The driver is built into FerroDb |
| Optional | Azure CLI (`az`) on `PATH` for the `azure_cli` token source |
| Network | TCP to the server, port **1433**; HTTPS to `login.microsoftonline.com` for the client-secret flow |

## Connecting

Driver id `azuresql`, family `tds`. Listed under analytical / cloud databases.

| Field | Default | Auth | Notes |
|---|---|---|---|
| Host | `localhost` | all | e.g. `myserver.database.windows.net` |
| Port | `1433` | all | |
| Database | — | all | Set it: Azure SQL logins usually work per database |
| User | — | Password | Required for password authentication |
| Password | — | Password | Keyring |
| Trust the server certificate | `false` | all | `trust_server_certificate` |
| Token source: `azure_cli`, `client_secret` or `token` | `azure_cli` | Entra ID | `entra.method` |
| Tenant ID | — | Entra ID | `tenant` |
| Application (client) ID | — | Entra ID | `entra.client_id` |
| Client secret | — | Entra ID | Keyring |
| Access token | — | Entra ID | Keyring |

URL preview: `sqlserver://{host}:{port};database={database}` (display only).

**TLS**: Azure always encrypts. With `disable`, `prefer` or `require` the connection is still encrypted and the certificate is verified against the system store unless "Trust the server certificate" is `true`. `verify-full` requires encryption and verifies against the system store or the `ssl.ca` file.

**Redirects**: Azure SQL may redirect the login to the node holding the database; FerroDb follows up to 3 redirects. Through an SSH tunnel redirects are not followed.

**SSH tunnel**, **options** (`connect_timeout`, `query_timeout`) and the session `SET` options are as for [SQL Server](sqlserver.md#connecting).

## Authentication

| Method | How it works |
|---|---|
| Password | SQL authentication (contained or server login) |
| Entra ID, `azure_cli` | `az account get-access-token --resource https://database.windows.net/` (with `--tenant` when Tenant ID is set). Covers interactive and MFA sign-ins done once with `az login` |
| Entra ID, `client_secret` | Service principal: OAuth 2.0 client-credentials flow against `login.microsoftonline.com/<tenant>`; needs Tenant ID, Application (client) ID and Client secret |
| Entra ID, `token` | An access token for `https://database.windows.net/` pasted into Access token |

When Token source is left empty, the first that applies is used: a stored access token, then a stored client secret, then the Azure CLI. The token is sent in the TDS login (`FEDAUTH`).

Windows integrated authentication is not offered for Azure SQL.

## What works

The same as [SQL Server](sqlserver.md#what-works): explorer and introspection from `sys.*`, T-SQL level A console with `GO` batches, all types, `PRINT` / `RAISERROR` notices, attention-based cancel, editing with T-SQL literals, generated DDL, XML showplans (estimated and actual), Auto / Manual transactions, `ApplicationIntent=ReadOnly` plus a client-side read-only check, all export formats.

## Limitations

- **Never run against a real Azure SQL service**; report problems with the exact error text.
- No bulk load (imports use `INSERT`), no TDS 8.0 strict encryption.
- The session default schema cannot be changed; choosing a schema in the console only affects the explorer and FerroDb, not how the server resolves unqualified names.
- A token is requested at every connect (nothing is cached between connections).
- Synonyms are introspected but not shown in the explorer.
- No managed identity or device-code flow; use `az login` or a service principal.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `Azure CLI (az) is not available` | Install the Azure CLI or choose another token source |
| `Azure CLI: … (sign in with \`az login\`)` | Run `az login` (with `--tenant` if needed) and connect again |
| `Microsoft Entra ID: tenant ID is required` / `client ID is required` / `client secret is required` | Fill the fields of the `client_secret` source |
| `Microsoft Entra ID: AADSTS…` | The token endpoint refused the request; check tenant, client ID, secret and its expiry |
| `Login failed for user '<token-identified principal>'` | The Entra identity has no user in the database: create it with `CREATE USER … FROM EXTERNAL PROVIDER` |
| `… too many redirects` or timeouts after the login is redirected | Allow outbound connections to the gateway (1433) and to the nodes Azure redirects to |
| Certificate errors | Keep "Trust the server certificate" `false` and make sure the system store has the Azure root CAs |
