# Settings and files

## The Settings dialog

Open it with **File ▸ Settings…** (Windows, Linux), **FerroDb ▸ Settings…** (macOS), Ctrl+Alt+S or Cmd+,. Changes are saved immediately.

| Page | Settings |
|---|---|
| **General** | **Theme** (**System**, **Light**, **Dark**), **Interface font size**, **Language** (**System default**, English, Polish). Some texts change only after a restart |
| **Editor** | **Editor font size**, **Editor font family** (empty = theme default), **Show line numbers** |
| **AI Assistant** | Provider, endpoint, model, API key, what is shared. See [AI assistant and MCP](ai-and-mcp.md) |
| **Agents and MCP** | MCP server, port, token, permissions, audit log. See [AI assistant and MCP](ai-and-mcp.md#agents-and-the-mcp-server) |
| **Updates and Privacy** | Update channel, automatic checks, crash reports. See [Updates](updates.md) and [Privacy](privacy.md) |
| **Developer** | **Developer mode**: shows the Developer menu (demo scenarios) |

**View ▸ Toggle Light/Dark Theme** switches the theme without opening the dialog.

## Configuration directory

| System | Path |
|---|---|
| Windows | `%APPDATA%\FerroDb\FerroDb\config` |
| macOS | `~/Library/Application Support/dev.FerroDb.FerroDb` |
| Linux | `~/.config/ferrodb` |
| Any, with `FERRODB_HOME` set | `$FERRODB_HOME/config` |

| File | Content | Written by |
|---|---|---|
| `settings.toml` | Theme, language, font sizes, line numbers, recent projects, developer mode | The Settings dialog |
| `data-sources.toml` | Data sources, without secrets. See [Data sources](data-sources.md#properties-in-data-sourcestoml) | The data source dialog |
| `keymap.toml` | Your keyboard shortcuts. See [Keyboard shortcuts](keyboard-shortcuts.md#changing-shortcuts) | You |
| `templates.toml` | Your live templates (see below) | You |
| `ai.toml` | AI assistant settings (no API key) | The AI Assistant page |
| `agents.toml` | Agent permissions and MCP server settings | The Agents and MCP page |
| `release.toml` | Update channel, automatic checks, crash report consent, time of the last update check | The Updates and Privacy page |
| `scripts/extractors/*.rhai`, `scripts/aggregators/*.rhai` | Your extension scripts. See [Extension scripts](scripts.md) | You, or **Tools ▸ New … Script…** |

FerroDb writes its files atomically (a temporary file, then a rename), so a crash never leaves half a file. A file that cannot be read is ignored, a warning is logged, and the defaults are used. Edit `data-sources.toml`, `ai.toml`, `agents.toml` and `release.toml` by hand only while FerroDb is closed; the application rewrites them from memory.

### settings.toml

```toml
appearance = "system"        # system | light | dark
locale = ""                  # en | pl; empty = follow the system
ui_font_size = 14.0
editor_font_size = 13.0
editor_font_family = ""
show_line_numbers = true
developer_mode = false
```

### templates.toml

```toml
[[template]]
abbreviation = "lim"
description = "LIMIT clause"
body = "LIMIT $count$$END$"
language = "sql"   # sql | mql | cql | sqlpp | partiql | redis | any, or a dialect id (tsql, oracle…)
```

In `body`, `$NAME$` is a placeholder, `$END$` the final cursor position, `$SELECTION$` the selected text, and `$$` a literal `$`. Templates are offered in completion.

### ai.toml

```toml
enabled = true
provider = "anthropic"       # anthropic | open-ai-compatible | ollama
endpoint = ""                # empty = the provider's default
model = ""                   # empty = the provider's default
max_tokens = 16000
share_schema = true
share_samples = false        # not implemented in 1.0
schema_budget = 24000        # characters
```

The API key is in the keyring under `secret://ai-<provider>`.

### agents.toml

```toml
[policy]
default_access = "read-only"     # hidden | metadata-only | read-only | read-write
sources = [["orders-prod", "hidden"]]
consent = "every-call"           # every-call | queries-only | writes-only
max_rows = 200
consent_timeout_secs = 120

[mcp]
enabled = false
port = 7391                      # always on 127.0.0.1
```

The MCP bearer token is in the keyring under `secret://mcp`.

### release.toml

```toml
consent = "ask"                  # crash reports: ask | always | never
last_update_check = 1795000000   # Unix seconds, written by FerroDb

[updates]
channel = "stable"               # stable | beta
check_automatically = true
```

## Data directory

| System | Path |
|---|---|
| Windows | `%APPDATA%\FerroDb\FerroDb\data` |
| macOS | `~/Library/Application Support/dev.FerroDb.FerroDb` |
| Linux | `~/.local/share/ferrodb` |
| Any, with `FERRODB_HOME` set | `$FERRODB_HOME/data` |

| Path | Content |
|---|---|
| `app.db` | Query history, query log, Local History (SQLite) |
| `metadata/<source>.json` | Metadata cache per data source. Safe to delete; it is rebuilt |
| `drivers/` | Downloaded JDBC drivers (`jdbc/`), ADBC libraries (`adbc/`), the downloaded Java runtime (`runtime/`) and their state (`state.json`) |
| `agents-audit.jsonl` | Audit log of agent calls |
| `crashes/` | Crash reports (`<id>.json`). See [Privacy](privacy.md#crash-reports) |
| `default-project/` | The project opened when no other project is chosen |

## Project files

Inside each project folder, FerroDb uses `.ferrodb/`: `layout.json` (panels), `consoles/` (consoles without a file), `diagrams/` (ER diagram layouts). The folder contains no secrets.

## Secrets

Secrets are in the system keyring under the service `FerroDb`:

| Entry | Content |
|---|---|
| `<data source id>` | All secrets of one data source |
| `ai-<provider>` | AI provider API key |
| `mcp` | MCP bearer token |

## Environment variables

| Variable | Effect |
|---|---|
| `FERRODB_HOME` | Use `<dir>/config` and `<dir>/data` instead of the system directories |
| `FERRODB_LOG` | Log level or filter: `error`, `warn`, `info` (default), `debug`, `trace` |
| `JAVA_HOME` | Java runtime used for the JDBC bridge, if it is version 17 or newer |
| `FERRODB_ADBC_<DRIVER>` | Path to an ADBC driver library, e.g. `FERRODB_ADBC_SNOWFLAKE` |

## Logs

FerroDb writes its log to standard output. To keep it, start FerroDb from a terminal and redirect the output:

```sh
FERRODB_LOG=debug ferrodb > ferrodb.log 2>&1              # macOS, Linux
```

```powershell
$env:FERRODB_LOG = "debug"; & "<install folder>\ferrodb.exe" > ferrodb.log 2>&1   # Windows, PowerShell
```

`FERRODB_LOG` accepts filters as well, e.g. `FERRODB_LOG=info,ferrodb_jdbc=debug`. Secrets are kept out of the log, but it can contain query text, object names and host names: read it before you share it. The **Output** panel inside FerroDb shows messages about your actions (query summaries, transfer results, keymap problems).
