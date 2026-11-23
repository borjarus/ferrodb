# Privacy

FerroDb has no telemetry. It does not collect usage statistics, does not send identifiers, and does not report which databases you use.

This page lists every case in which FerroDb connects to something other than your own databases.

| Connection | When | What is sent |
|---|---|---|
| Update check | Once a day at startup if automatic checks are on, or when you choose **Help ▸ Check for Updates…** | One HTTPS request for the update manifest. No identifiers, no settings, no usage data |
| Update download | When you accept an update | A download of the package |
| Driver downloads | When you download a driver or a Java runtime | Downloads from Maven Central, the Apache Arrow ADBC packages, or the Adoptium API |
| Crash reports | Only with your consent (see below) | The crash report |
| AI assistant | Only when you turn it on and ask something | Your question and the context described below, to the provider you chose |
| MCP server | Only when you turn it on | Nothing leaves your machine: the server listens on `127.0.0.1` only |

## Crash reports

When FerroDb crashes, it writes a report to `crashes/<id>.json` in the data directory. Nothing is sent at that moment.

A report contains:

| Field | Example |
|---|---|
| Report id and time | `20271108-101500-1a2b`, RFC 3339 time |
| FerroDb version | `1.0.0` |
| Operating system | `windows x86_64 10.0.26100` |
| Thread name, if known | `main` |
| The error message and its location in FerroDb's source | `file:line:column` |
| A backtrace | Function names inside FerroDb |
| The last lines of the log | Kept in memory only for this purpose |
| Your comment, if you add one | |

Before a report is written, secrets are masked in the message and the log lines:

- passwords in connection URLs (`postgres://user:***@host`);
- values of `key=value`, `key: value` and JSON pairs whose key names a secret (`password`, `pwd`, `secret`, `token`, `api_key`, `access_key`, `client_secret`, `private_key`…);
- HTTP `Bearer` and `Basic` credentials;
- AWS access key ids;
- API keys of the form `sk-…`.

The masking is deliberately greedy. Reports can still contain object names, host names or fragments of query text from the log lines; you can read every report before it is sent.

### Consent

**Settings ▸ Updates and Privacy** has three choices for crash reports:

| Choice | What happens after a crash |
|---|---|
| **Ask** (default) | At the next start, FerroDb shows **FerroDb closed unexpectedly last time**. You can add a comment and choose **Send**, **Don't Send** or **Always Send** |
| **Always** | Reports are sent automatically at the next start |
| **Never** | Reports stay on disk and are never sent |

**Don't Send** keeps the report on disk but stops offering it. **Always Send** sends this report and switches the setting to **Always**.

On the same page:

- **View Reports** lists stored reports, newest first, and whether they were sent.
- **Open Reports Folder** opens `crashes/` in the file manager.
- **Clear** deletes every stored report.

## Updates

- The update manifest is accepted only if it carries a valid Ed25519 signature made with the FerroDb release key. The public key is built into the application.
- A downloaded package is installed only if its SHA-256 checksum matches the value in the signed manifest.
- Only HTTPS is used.

See [Updates](updates.md).

## Driver downloads

Drivers are listed in a catalog built into FerroDb with their source, version, license and SHA-256 checksum. A download is verified before it is installed. The Java runtime from Adoptium is verified with the checksum Adoptium publishes. See [Installation](installation.md#drivers-downloaded-on-demand).

## AI assistant

The assistant is off by default. When you turn it on, FerroDb sends to the provider you configured:

- your question and the earlier turns of the same chat;
- the database product and query language;
- for console commands: the selected query or the statement under the cursor, and for **Fix with AI** the error message;
- with **Share schema (names and types)** on (the default once the assistant is enabled): names and types of the tables, views, columns and keys most relevant to your question, up to the schema budget; field names already known for collections; Redis key prefixes, never key names.

It never sends rows, documents, key values, query results, passwords or connection details. Your API key is stored in the system keyring. With **Ollama (local)** or another local server, nothing leaves your machine.

## Agents (MCP)

Agents connect to FerroDb on your machine. What they receive from your databases is controlled by the permissions in **Settings ▸ Agents and MCP**: by default they get read-only access and every call asks for your consent. An agent may send the data it reads to its own AI provider; that is governed by the agent, not by FerroDb. Every call is recorded in `agents-audit.jsonl` (query text and outcome, never result data). See [AI assistant and MCP](ai-and-mcp.md#agents-and-the-mcp-server).

## Data on your machine

| Data | Where |
|---|---|
| Passwords, tokens, API keys | System keyring only |
| Data source definitions | `data-sources.toml` (no secrets) |
| Query history, query log, Local History | `app.db` in the data directory |
| Metadata cache | `metadata/` in the data directory |

[Settings and files](settings-and-files.md) lists every file.
