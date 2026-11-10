# Phase 4 — SQL introspection and live Database Explorer: status

Scope: plan, section 7, phase 4. Databases: **PostgreSQL, MySQL, MariaDB, SQLite**. The explorer, DDL, scripts, quick documentation, "Go to declaration" and Search Everywhere now read a metadata catalog. The catalog is cached in memory and on disk.

## Completion criteria

| Criterion (plan) | Status | Evidence |
|---|---|---|
| Correct DDL for the test corpus | **Done** | `ferrodb-meta/tests/roundtrip.rs` (`cargo xtask tck` runs it). Each corpus is created in `rt_src` and introspected. The DDL of the whole container is generated for `rt_dst` and run there. Introspecting `rt_dst` must give the same model (container names aside). PostgreSQL 18, MySQL 8.4 and MariaDB 11.4 pass; SQLite passes in the unit tests. The corpora cover the table below |
| Start from cache < 300 ms for 10k objects | **Done** | `start_from_cache_is_fast`: 10,000 tables with 10 columns, 2 indexes and a key each (140,001 objects, a 9.4 MB file). Reading the file, decoding it and building the first two explorer levels takes **158 ms** in a debug build. That is 14× the object count the criterion asks for. The core test `live_explorer_cache_refresh_and_ddl` shows a restarted core listing the tree from the disk cache without connecting |

### What the DDL corpora contain

| | PostgreSQL | MySQL / MariaDB | SQLite |
|---|---|---|---|
| Columns | serial, identity (`ALWAYS`), stored generated, defaults, arrays, quoted mixed-case names, comments | `AUTO_INCREMENT`, `DEFAULT CURRENT_TIMESTAMP ON UPDATE …`, stored generated, column comments | defaults, `NOT NULL` |
| Keys | PK, UNIQUE, CHECK, FK with `ON DELETE CASCADE`, `DEFERRABLE INITIALLY DEFERRED`, a foreign-key **cycle** (closed with `ALTER TABLE`) | PK, UNIQUE KEY, FK, CHECK | PK, composite PK, UNIQUE, FK, CHECK |
| Indexes | partial, `DESC NULLS LAST`, GIN, expression with `INCLUDE` | composite with `DESC`, prefix `(5)`, FULLTEXT | composite, unique partial |
| Other | sequence, views (one on another), materialized view, overloaded SQL functions, PL/pgSQL procedure, trigger + trigger function, range partition | view, procedure with `IN`/`OUT`, function, trigger | view, trigger |

## What was built

| Plan item | Where | Notes |
|---|---|---|
| Metadata model with a `HierarchySpec` per database | `ferrodb-meta/src/model.rs`, `tree.rs` | Catalog → containers (schemas / databases) → tables, views, materialized views, routines (overloads by signature), sequences → columns, PK, FKs, unique constraints, indexes, triggers. Plain serde data |
| Introspectors: PostgreSQL, MySQL/MariaDB, SQLite | `ferrodb-meta/src/{pg,mysql,sqlite}.rs` | One catalog query per object kind for a whole container. PostgreSQL uses `pg_catalog` (server-side view, function and trigger definitions). MySQL/MariaDB use `information_schema`, with the product differences listed in the module docs. SQLite uses `sqlite_master` (its `sql` column is the exact DDL) and the table-valued pragmas |
| Incremental refresh | `ferrodb-core/src/metadata.rs` (`refresh_scope`), `ferrodb_meta::merge` | Root: containers are listed again, and the ones that were loaded are introspected again. Container: that container. Table / view / collection: queries narrowed to that object, then merged. The explorer's Refresh calls the backend before reloading |
| Disk cache | `ferrodb-meta/src/cache.rs` | `<data dir>/metadata/<source>.json`, written atomically. Unreadable or outdated files are ignored. Read off the UI path at startup. Readers get an immutable `Arc` while an introspection runs. The cache is dropped when the driver or the connection properties of a source change, and when the source is removed |
| Schema filter | `ferrodb_meta::tree::Filter`, property `schema.filter` | `public, sales_*, !tmp_*`: case-insensitive wildcards, `!` excludes. Applies to the tree, Search Everywhere and the snapshot |
| DDL generation (single, bulk) | `ferrodb-meta/src/ddl.rs` | Tables are generated from the model; views, routines and triggers use the server-side definition. Several tables come in foreign-key order (partitions after their parent); cycles become `ALTER TABLE … ADD CONSTRAINT`. A whole container gives sequences, tables, views, routines, then triggers. Objects can be re-targeted to another container. Also: a single column, key, index or trigger |
| Procedure call scripts, object snippets | `ddl::script`, explorer menu "Generate …" | `SELECT` with every column; `INSERT` without generated, identity and serial columns; `UPDATE` / `DELETE` by the primary key, else a unique key, else every column (with a warning comment); `CALL` / `SELECT f(…)` / `SELECT * FROM f(…)` with `:name` parameters (MySQL `OUT` parameters as `@name`); `DROP` |
| Navigation to declarations | console `GoToDeclaration` (Ctrl+B), `MetadataService::resolve`, `ExplorerPanel::reveal` | The qualified name under the cursor (`orders`, `sales.orders`, `orders.total`) is resolved against the cached catalog. Exact names come first, then case-insensitive ones, and the current schema first. The explorer loads the parents and selects the object |
| Quick documentation | console `QuickDocumentation` (Ctrl+Q), explorer menu, `MetadataService::documentation` | Shown in the inspector. Tables and views: columns with type, null, default, PK, FK target, indexes, comment or view query. Routines: arguments, result, language, definition. Keys, indexes, triggers and sequences: their DDL |
| Search Everywhere index | `SearchIndexService::entries` | Every cached table, view, routine, sequence and collection of every source, already at startup from the disk cache |

## Contract changes (`ferrodb-api` v2 → v3)

- `MetadataService::script(node, ScriptKind)`: Select, Insert, Update, Delete, Call, Drop.
- `MetadataService::resolve(ds, parts)` (no I/O) and `documentation(node) -> ObjectDoc`. These have default implementations, so the mock did not have to change.
- `DdlOptions::default()` now includes indexes and comments.
- `ResultShape::Documents { container, node }`: the container's node in the explorer tree (phase 4b).
- `MetadataService::children` connects on demand. The explorer no longer connects a source before listing it, so a cached catalog is shown without a connection. The mock now connects in `children`, as the core does.

## UI changes

- Explorer: Refresh re-reads metadata in the backend. The context menu has "Generate SELECT / INSERT / UPDATE / DELETE / Call / DROP", "Copy DDL" for every tabular object, and "Quick Documentation". Objects can be revealed ("Go to declaration").
- Console: Ctrl+B and Ctrl+Q are implemented. Before, they showed "coming in phase 4".
- New PL/EN messages for the menu entries and "not found".

## Not done / deviations

- **Introspectors are in `ferrodb-meta`, not in `FamilyVariant`.** This avoids a dependency cycle; see the ADR-008 amendment. The variants' listing queries remain as the fallback.
- **Not modelled:**
  - PostgreSQL enum/domain/composite types, exclusion constraints, policies, rules, grants, non-default operator classes, foreign-table servers, `OWNED BY` of manually created sequences;
  - MySQL events, partitioning and the table `AUTO_INCREMENT` counter.

  None of these are in the corpora. Their DDL is incomplete until they are modelled.
- **Granularity of incremental refresh:** a table refresh re-reads one table. A routine or sequence refresh re-reads its container. There is no automatic change detection (DDL triggers, polling).
- **Navigation does not resolve aliases** (`o.id` with `FROM orders o`). Name resolution in queries is phase 5.
- **No GUI walkthrough by a person.** The UI changes were compiled and the UI test suite was run (see `docs/phase-4b.md`, test runs), but nobody clicked through on a real database.

## Environment notes (this development machine)

- **Windows builds are blocked for now.** The security software quarantines newly built `serde` / `ref-cast` build-script executables and keeps re-created ones locked (`LNK1104`, `os error 32`). Even the unchanged app build fails.
- **Everything in this phase was built and tested in WSL (Linux)** from a mirror of the checkout:
  - non-GUI crates: tests and the TCK;
  - GUI crates: `clippy --tests` and the UI tests (`RUST_FONTCONFIG_DLOPEN=1`).
- `cargo fmt` ran on Windows.
