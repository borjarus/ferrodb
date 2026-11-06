# Phase 2 — GUI components on mock data: status

Scope: plan, section 7, phase 2 (2.1–2.9). Everything below runs on `MockBackend`; no real driver is involved.

## Completion criteria

| Criterion (plan) | Status | Evidence |
|---|---|---|
| Three full "clickable" flows: tabular, document, key-value | Done | `tests/flows.rs`: `console_streams_results_and_pages`, `documents_tree_json_table_schema_and_edit`, `keys_are_grouped_and_values_shown_per_type` |
| Flow "add a database requiring a driver → download → connect" | Done | `driver_flow_download_then_connect` |
| Load scenarios handled | Done | `million_rows_scenario_…`, `hundred_thousand_documents_and_million_keys_page`, `five_mb_document_is_browsable`, `ten_thousand_tables_in_the_explorer`, `jdbc_process_failure_is_reported` |
| UI tests for 14 key flows | Done: 23 flows | `crates/ferrodb-ui/tests/flows.rs`, plus the 5 phase-1 tests in `tests/ui.rs` |
| Demo with 3–5 users and UX fixes | **Open** | Needs people; use `cargo run -p ferrodb-gallery` or the app with Developer → Mock Scenarios |

## What was built

| Plan item | Where | Notes |
|---|---|---|
| 2.1 Database Explorer | `ferrodb-ui/src/panels/explorer.rs` | Lazy tree from `HierarchySpec` (expanding a source connects it). Kind groups ("tables 24"), pattern filter that keeps matching branches, folders, environment markers, read-only lock, type icons, context menu by data model and capabilities, double-click opens data |
| 2.2 Data source dialog | `dialogs/data_source.rs` | 26 databases grouped and searchable, templates, form generated from `DriverDescriptor` (fields per auth method), General/SSH/SSL/Options/Schemas tabs, environment, read-only, manual commit, URL preview, driver status with in-place download, Test Connection. Secrets are never written to the config (`secret://<id>`) |
| 2.2 Driver manager | `dialogs/driver_manager.rs` | Status per driver, download with progress, version choice, licenses, custom files, JDBC bridge state |
| 2.3 Console | `panels/console.rs`, `completion.rs` | Highlighting per language, completions and diagnostics from `LanguageService` (150 ms debounce), current-statement highlight, statement under cursor / selection / whole script, source and schema pickers, cost estimate dialog before running (`cost_estimate`), Explain, manual commit/rollback, error positions as editor diagnostics, result tabs |
| 2.4 Result grid | `results/grid.rs` | Virtualized `DataTable` over Arrow batches, typed headers, local sort and search, WHERE / ORDER BY that re-run the statement, cell selection → value panel, per-type in-place editing with validation, NULL, delete/add row, copy as TSV/JSON/INSERT, aggregates, paging (load more / all, page size), record view, named result tabs |
| 2.5 Document browser | `results/documents.rs` | Tree / JSON / table, filter / sort / projection (MQL) or WHERE (SQL++/PartiQL), type-faithful values (`NumberLong`, `ObjectId`, `ISODate`…), field editing and document deletion through the preview, inferred schema and index panel; arrays over 1000 items are capped in the tree |
| 2.6 Key browser | `results/keys.rs` | Tree by `:` separator, type/TTL/size columns, pattern filter (`SCAN … MATCH`), value pane per type (string/JSON editor, hash, list, set, zset, stream), member edit, TTL, delete |
| Operation preview | `results/changes.rs` | Every edit shows the statements/commands of the source language before applying |
| 2.7 Bottom panels | `panels/services.rs`, `history.rs`, `local_history.rs`, `output_panel.rs` | Sessions, JDBC bridge, running queries with cancel; query history with filter; Local History snapshots with compare/restore |
| 2.8 Mockups | `views/` | Execution plan, table editor with DDL preview, ER diagram, diff (schema compare, Local History), export/import wizards |
| 2.9 Gallery | `panels/gallery.rs`, `apps/ferrodb-gallery` | Every component on mock data, theme switch, scenarios; `cargo run -p ferrodb-gallery [section]` |

## Contract changes (`ferrodb-api` v0 → v1)

Phase-2 views needed data the v0 contract could not provide. `API_VERSION` is now 1.

- `QueryService`: `sessions`, `estimate_cost`, `explain` (`PlanNode`), `key_value` (`KeyValue`), `preview_changes`.
- `Change`: `SetKeyMember`, `ExpireKey` (Redis members and TTL).
- `DriverCatalogService`: `versions`, `download(id, version)`, `use_custom_artifacts`, `jdbc_bridge` (`BridgeState`, `DriverEvent::BridgeChanged`); `DriverStatus::Custom`.
- `DriverProperty::auth` (fields per authentication method), `DriverDescriptor::url_template`.

## Mock changes

- `MockBackend::deterministic()`: no threads and no latency. GPUI's test scheduler rejects wake-ups from foreign threads, so UI tests use it; the app keeps the threaded mock with latency and backpressure.
- Key scans honour `MATCH` patterns; key values per type; change previews in SQL / MQL / SQL++ / PartiQL / Redis; a plausible execution plan; per-source DDL (production lags by one column for the compare demo); the JDBC bridge state follows JDBC connections.

## Open items

- User demo and UX fixes (criterion above).
- Grid and tree performance with these views on real hardware (the phase-0 spike measured bare components; see `docs/spike-results.md`).
- Mockup-level pieces by design: DDL in the table editor is generated locally; export/import only prepare the transfer (phase 6); schema compare is a DDL diff (phase 7).
- Local History is in memory (moves to `ferrodb-storage` with the core).
- Inserted rows are counted and submitted but not shown in the grid yet.
