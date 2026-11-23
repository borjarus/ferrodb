# AI assistant and MCP

FerroDb has two optional AI features. Both are off by default.

- **AI Assistant**: you ask questions in natural language; FerroDb sends them to an AI provider you choose and shows the suggested queries. Queries are never run for you.
- **MCP server**: AI agents running on your machine (for example Claude Code or Claude Desktop) can list your data sources and run queries through FerroDb, within permissions you set and with your consent per call.

## AI Assistant

### Set up

1. Open **Tools ▸ AI Assistant Settings…** (or **Settings… ▸ AI Assistant**).
2. Turn on **Enable AI assistant**.
3. Under **Provider**, choose the **Provider**:

   | Provider | Default endpoint | API key |
   |---|---|---|
   | Anthropic (Claude) | `https://api.anthropic.com` | Required. Default model `claude-opus-5-5` |
   | OpenAI-compatible | `https://api.openai.com/v1` | Required for hosted services; not for local servers |
   | Ollama (local) | `http://localhost:11434` | Not needed |

4. Optionally change the **Endpoint** and the **Model** (**Load models** lists what the provider offers).
5. Enter the **API key**. It is stored in the system keyring, never in a file. Leave the field empty to keep the stored key; **Clear** removes it on save.
6. Under **What is shared**, decide whether to **Share schema (names and types)**. Set **Max tokens per answer** and **Schema budget (characters)** if needed.
7. **Test** checks the connection; **Save**.

### Use

Open the panel with **View ▸ AI Assistant** (right side).

- The data source picker follows the active console (**Follow the active console**), or pick one yourself.
- Type a question and press Enter (Shift+Enter adds a line). The answer streams in; **Stop** ends it; **New Chat** starts over.
- The line under the answer says what was sent, e.g. *Schema of 12 objects sent, 3400 chars*.
- Each suggested query has **Insert into Console**, **Open in New Console** and **Copy**. A **Modifies data** badge marks queries that change data, with the risks and any problems found by FerroDb's own language checks.

From a console (**Console** menu or the editor's context menu):

| Command | Sends |
|---|---|
| **Ask AI…** | Your question, with the selection or the statement under the cursor as context |
| **AI: Explain Query** | The statement, asking for an explanation |
| **AI: Optimize Query** | The statement, asking for a faster version |
| **Fix with AI** | The failed statement and its error message (also offered as a button after a statement fails) |

These commands have no default shortcuts; you can add them in `keymap.toml`.

### What is sent to the provider

| Sent | When |
|---|---|
| Your question and the earlier turns of the same chat | Always |
| The database product and the query language or dialect | Always |
| The selected query or the statement under the cursor | For console commands |
| The error message | For **Fix with AI** |
| Names and types of the tables, views, columns and keys most relevant to the question, up to the schema budget; other objects by name only | Only with **Share schema** on |
| Field names already known for collections; Redis key prefixes (never key names) | Only with **Share schema** on |

Never sent: rows, documents or values from your databases, query results, passwords or connection details. Schemas hidden by the data source's schema filter (**Schemas** tab) are not included. **Share sample values** is shown in the settings but not implemented in 1.0.

## Agents and the MCP server

The MCP server lets AI agents use your data sources through FerroDb. It listens only on `127.0.0.1`, requires a bearer token, and accepts only requests whose `Host` and `Origin` are local.

### Set up

1. Open **Tools ▸ Agents and MCP Settings…** (or **Settings… ▸ Agents and MCP**).
2. Under **MCP server**, turn on **Start the MCP server**. The default **Port (127.0.0.1)** is 7391. The status shows **Running** and the connected clients.
3. Connect your agent:
   - **Agents that launch a command** (Claude Code, Claude Desktop): click **Copy Client Config** and add it to the agent's configuration. It runs `ferrodb mcp`:

     ```json
     {"mcpServers": {"ferrodb": {"command": "<path to ferrodb>", "args": ["mcp"]}}}
     ```

     `ferrodb mcp` opens no window: it forwards the agent's messages to the running FerroDb, reading the port from `agents.toml` and the token from the keyring. FerroDb must be running; otherwise `ferrodb mcp` exits with an error.
   - **Agents that connect over HTTP**: use **Copy URL** (`http://127.0.0.1:7391/mcp`) and **Copy Token** (send it as `Authorization: Bearer <token>`).
4. **Regenerate Token** invalidates the old token; clients must reconnect.

The status bar shows **MCP · N clients** while agents are connected.

### Tools offered to agents

| Tool | Does |
|---|---|
| `list_data_sources` | Lists the data sources the agent may see |
| `list_objects` | Lists schemas, tables, collections… |
| `describe_object` | Returns DDL or the inferred schema |
| `run_query` | Runs a query and returns one page (up to **Max rows per query**) |
| `explain_query` | Returns the estimated plan; never runs the query |

### Permissions

Under **Permissions**:

| Setting | Values |
|---|---|
| **Default access** | **Hidden**, **Metadata only**, **Read-only** (default), **Read and write** |
| **Access per data source** | Overrides the default for single data sources |
| **Ask for consent** | **For every call** (default), **For queries only**, **For writes only** |
| **Max rows per query** | Default 200 |
| **Consent timeout (seconds)** | Default 120; an unanswered request is denied |

What each access level allows:

| Access | Agent can |
|---|---|
| Hidden | Nothing: the data source does not exist for agents |
| Metadata only | List objects and read definitions; no queries |
| Read-only | Also run queries that do not modify data. Modifying queries are denied without asking |
| Read and write | Also modifying queries, but **always** with your consent, whatever the consent mode |

A data source marked **Read-only** in FerroDb refuses writes from agents as well. FerroDb decides whether a query modifies data with the same checks the console uses for its confirmations.

### Consent

When an agent's call needs consent, FerroDb shows **An agent wants to use a data source** with the **Client**, **Tool**, **Data source** and the query. A call that changes data is marked in red: *This call changes data or schema.*

- **Allow Once**
- **Allow for This Session**: remembered for this MCP session, tool and data source. Not available for writes.
- **Deny** (also Esc)

Requests from several agents are queued. A request closes by itself when it times out or the agent gives up.

### Audit log

Every agent call is recorded with its query text (never result data) and outcome: *allowed by policy*, *allowed*, *denied by policy*, *denied*, *timed out*. The **Audit log** section of the settings lists the newest calls first; the file is `agents-audit.jsonl` in the data directory.
