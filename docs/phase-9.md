# Phase 9 — AI and extensibility: status

Scope: plan, section 7, phase 9 (version 1.1, optional): a natural-language query assistant (SQL, MQL, CQL…) aware of the schema, a permission layer for agents (read-only by default, consent for each call), an MCP server (`rmcp`), and `rhai` scripts for custom export formats and aggregates.

The work was done in four packages in parallel (assistant, agents + MCP, scripts, UI) on top of the API v7 contracts and merged on `phase-9`. The plan gives no completion criteria for this phase; the table lists each scope item with what shows it works.

## What was built

### API v7

| Item | Where | Notes |
|---|---|---|
| Assistant contract | `ferrodb-api/src/ai.rs` | `AssistantService` (settings, test, models, streaming `ask` → `AssistantEvent::{Context, Delta, Finished, Failed}`), `AiSettings`, `AiProvider` (Anthropic, OpenAI-compatible, Ollama), `AssistantTask` (Generate, Explain, Fix, Optimize, Chat), `SuggestedQuery` (modifies, risks, problems) |
| Agents contract | `ferrodb-api/src/ai.rs` | `AgentService`, `AgentPolicy` / `AgentAccess` (Hidden, MetadataOnly, ReadOnly default, ReadWrite) / `ConsentMode` (EveryCall default, QueriesOnly, WritesOnly), `McpSettings` (127.0.0.1, port 7391), consent requests and decisions, audit entries, `AgentEvent` |
| Scripts contract | `ferrodb-api/src/scripts.rs`, `transfer.rs` | `ScriptService` (list, reload, directory, create, aggregate, extract), `ExtScriptKind`, `ExtScriptInfo`, `ScriptValue`, `ScriptTable`; `DataFormat::Script` + `FormatOptions::script` |
| Backend | `ferrodb-api/src/backend.rs` | `assistant()`, `agents()`, `scripts()` with defaults `NoAssistant`, `NoAgents`, `NoScripts` (`available() == false`): older backends and tests work unchanged |
| Composition | `ferrodb-ai/src/backend.rs`, `ferrodb-app/src/main.rs` | `AiBackend` wraps any backend (core or mock) and adds the assistant and the agents; scripts come from the inner backend |

### 9.1 Natural-language assistant (`ferrodb-ai/src/assistant/`)

| Item | Where | Notes |
|---|---|---|
| Settings and key | `settings.rs` | `<config>/ai.toml` (atomic write); API key in the system keyring at `secret://ai-<provider>`, never returned; keyring and file access on blocking threads |
| Anthropic | `providers.rs`, `http.rs` | Raw HTTP Messages API (no official Rust SDK): `anthropic-version: 2026-10-21`, default model `claude-opus-5-5`, streaming SSE, `effort: medium`, no sampling parameters, prompt caching (`cache_control` on the stable system prefix and on the newest turn of a conversation), server-side refusal fallback (`fallbacks: "default"`) on the official endpoint for current models; refusals, `max_tokens` truncation, 401 / 429 / 529 reported clearly; `models()` pages `/v1/models` |
| OpenAI-compatible, Ollama | `providers.rs` | Chat Completions SSE (with usage), Ollama `/api/chat` NDJSON; model listings; local servers without keys |
| Schema context | `context.rs` | From the snapshot or a bounded walk of the explorer tree: objects ranked by overlap with the question and the context query (snake/camel split, singular/plural), FK neighbours, keys fetched for the top tables; compact one-line definitions up to `schema_budget`, the rest listed by name. Collections with known fields, Redis key prefixes (no key names). `share_schema = false` sends only the product and language. Rows are never sent |
| Prompts | `prompt.rs` | Stable per data source (cacheable): product, exact dialect / language rules (T-SQL, PL/SQL, GoogleSQL, ClickHouse, MongoDB shell, CQL partition keys, PartiQL, SQL++, Redis), schema; the task in the user turn |
| Answers | `answer.rs` | Fenced blocks → `SuggestedQuery`, checked by the language service: error diagnostics → `problems`, `risks(…, true)` → `risks` / `modifies`. Nothing is ever run by the assistant |
| Cancellation | `mod.rs` | The stream is dropped (connection closed) and the answer ends with `Failed("Cancelled.")` |

### 9.2 Permission layer for agents (`ferrodb-ai/src/agents/`)

| Item | Where | Notes |
|---|---|---|
| Decisions | `policy.rs` | Pure decision table: Hidden sources do not exist for agents; MetadataOnly allows listings and definitions; ReadOnly denies modifying queries without asking; ReadWrite always asks for modifying queries whatever the consent mode; read-only data sources refuse writes. A query modifies when `language().risks(text, lang, true)` reports anything for any statement |
| Consent | `consent.rs` | `ConsentRequested` broadcast to every subscriber, closed by a decision, a timeout (`consent_timeout_secs`) or the client's cancellation; "allow for this session" remembered per MCP session, tool and data source, never for writes |
| Audit | `audit.rs` | Every call (query text, never result data) as `AgentEvent::Audit` and in `<data>/agents-audit.jsonl`; newest first |
| Settings and token | `mod.rs` | `<config>/agents.toml` (`[policy]`, `[mcp]`); a 32-byte bearer token in the keyring (`secret://mcp`), regenerated on demand |

### 9.3 MCP server (`ferrodb-ai/src/mcp/`)

| Item | Where | Notes |
|---|---|---|
| Server | `mod.rs`, `handler.rs` | `rmcp` 3.11 streamable HTTP on `127.0.0.1:<port>/mcp` (hyper HTTP/1, no TLS); bearer token (constant-time compare, 401), loopback `Host` / `Origin` only (403); starts with the IDE when enabled, restarts on settings changes, reports a busy port at once; status with connected clients |
| Tools | `tools.rs`, `convert.rs` | `list_data_sources`, `list_objects`, `describe_object` (DDL or inferred schema), `run_query` (one session per MCP session and source, page ≤ `max_rows`, tables / Extended JSON documents / keys, notices, affected rows; cancelled with the client), `explain_query` (estimate only). Every call goes through the permission layer; refusals are tool errors |
| `ferrodb mcp` | `bridge.rs`, `ferrodb-app/src/main.rs` | Stdio ↔ HTTP bridge for clients that launch a command (Claude Code, Claude Desktop): port from `agents.toml`, token from the keyring, raw JSON-RPC forwarded both ways; clear error when the IDE does not run. Client config: `{"mcpServers":{"ferrodb":{"command":"<ferrodb>","args":["mcp"]}}}` |

### 9.4 Extension scripts (`ferrodb-script`)

| Item | Where | Notes |
|---|---|---|
| Engine and sandbox | `src/sandbox.rs` | `rhai` 1.27 (`sync`); no modules, `eval` / `import` / `export` disabled at compile time, 5 M operations per call, capped strings / arrays / maps / call depth, cancellation polled every 1024 operations |
| Script API | `src/script.rs`, `src/lib.rs` | Header comments `// name:`, `// description:`, `// extension:`. Extractors: `header(columns)`, `row(values, columns, index)`, `separator()`, `footer(count)`, state in `this`, options via `options()`. Aggregators: `aggregate(values, table)`. Helpers: `sql_literal`, `sql_ident`, `json`, `csv_field`, `html_escape`, `md_escape`, `to_number`, `numbers`, `join`, `pad`, `format_number`… |
| Built-ins | `builtin/` | Extractors: SQL IN list, SQL VALUES rows, JSON keyed by first column, Markdown (aligned), PHP array, pipe-separated. Aggregators: count, distinct, min–max, median, p90, standard deviation, concatenation |
| Library | `src/library.rs` | Built-ins + `<config>/scripts/{extractors,aggregators}/*.rhai`; compiled cache refreshed on reload, create and file changes; broken scripts listed with their error; `create()` writes a documented template, never overwrites |
| Export | `ferrodb-data/src/export.rs`, `ferrodb-core/src/transfer.rs` | `Exporter::create_script` streams Arrow batches, rows and BSON documents through the extractor; the core resolves `FormatOptions::script` and wires the transfer's cancel; report method `"<name> (script)"` |
| Backends | `ferrodb-core/src/lib.rs`, `ferrodb-mock` | The core owns a library at `<config>/scripts`; the mock serves the built-ins |

### UI (`ferrodb-ui`)

| Item | Where | Notes |
|---|---|---|
| AI Assistant tool window | `panels/assistant.rs` | Right dock (View ▸ AI Assistant): data source picker following the active console, chat with history, streamed answer, context line ("Schema of N objects sent"), per query: Insert into Console, Open in New Console, Copy, "Modifies data" badge with risks and problems; Stop, New Chat; empty state linking to the settings |
| Console | `panels/console.rs`, `menus.rs` | Ask AI…, AI: Explain Query, AI: Optimize Query, Fix with AI (button after a failed statement); editor context menu, Console menu, palette. No default shortcuts |
| Settings | `ai_settings.rs`, `settings_view.rs` | "AI Assistant" (provider, endpoint, model with Load Models, masked key with keep / clear, sharing options, budgets, Test) and "Agents and MCP" (server, port, status, copy URL / token / client config, regenerate token, default and per-source access, consent mode, limits, audit log) |
| Consent | `agents.rs` | Queued modal dialogs: client, tool, source, query, red warning for writes; Allow Once / Allow for This Session (disabled for writes) / Deny, Esc = Deny; closed on timeout; status-bar "MCP · N clients" |
| Scripts | `scripts.rs`, `views/wizard.rs`, `results/grid.rs` | Extractors in the export wizard; Copy As ▸ extractor in the grid; aggregator picker (Σ) in the grid footer; Open Scripts Folder, New Extractor / Aggregator Script…, Reload Scripts |
| Translations | `assets/i18n/{en,pl}` | All new strings in both languages |

## Tests

| Crate | What |
|---|---|
| `ferrodb-ai` (45 + 1 ignored) | Local HTTP servers per provider (stream order, split SSE events, usage, request bodies, refusal, errors, cancellation, models); context ranking and budget on the mock; extraction and risks; settings round trip; policy table; consent flows (once, session, deny, timeout, several subscribers, cancellation); audit file; end to end MCP with an `rmcp` client on the mock (tools, annotations, queries, denials, 401 / 403, token regeneration, busy port); the stdio bridge in process. The ignored test calls the real Anthropic API when `ANTHROPIC_API_KEY` is set |
| `ferrodb-script` (20) | Every built-in with exact output, headers, streaming writer, helpers, runaway loops and recursion stopped, `eval` / `import` unavailable, caps, cancellation, compile errors, templates, reload |
| `ferrodb-data`, `ferrodb-core` (`tests_phase9`), `ferrodb-mock` | Export through scripts (Arrow, BSON, empty, failing); a SQLite query exported through a built-in and a user script via the transfer service; unknown / missing / broken scripts; aggregate, extract, create, reload through `Backend` |
| `ferrodb-ui` (`tests/phase9.rs`, 8) | On the mock with fake services: ask → stream → insert / new console / copy, history; disabled state; Fix and Explain from a console; consent queue, session → once downgrade for writes, `ConsentClosed`, Esc, status bar; both settings pages; export wizard request; Copy As; aggregator picker |

## Not done / deviations

- **Not clicked through in the GUI by a person**, and **no real LLM was called** (no API key here); providers are tested against recorded protocol shapes.
- **`share_samples` is not implemented**: sample values would require running queries. Document fields are not inferred for the prompt (`infer_schema` samples documents) — only fields already known to the explorer are sent.
- **MCP protocol versions up to 2025-11-25** are advertised: the newest revision served by `rmcp` (2026-07-28) has no sessions, which per-session consent and query sessions depend on.
- **The bridge installs the `ring` rustls provider** process-wide (the app's `reqwest` is built without a default provider).
- **Audit outcomes**: invalid arguments and unknown / hidden sources are recorded as policy denials; client cancellations as user denials with "cancelled by the client" (there is no `Cancelled` outcome).
- **The `ferrodb mcp` process itself is not tested end to end**; the forwarding logic is.
- **Copy As uses the right-clicked row** (the grid has no multi-row selection yet); script aggregators work on the selected column over the loaded rows, like the built-in summary.
- **Script options are a function** (`options()`), not a constant: rhai functions cannot see the caller's constants. The Markdown (aligned) extractor buffers rows to compute widths.
- **The mock does not watch a scripts directory**: it serves built-ins only.
