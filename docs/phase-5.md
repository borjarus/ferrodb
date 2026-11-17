# Phase 5 — Intelligent SQL editor, level A: status

Scope: plan, section 7, phase 5. The SQL language engine is in `ferrodb-lang`: dialects as data, a structural parser, name resolution, completion, inspections with quick fixes, a formatter and refactorings. The backends (core and mock) serve it through `LanguageService`. The editor UI does not use most of it yet.

This document was written after the phase was committed (`abcd8ca`), from the commit and the code.

## Completion criteria

| Criterion (plan) | Status | Evidence |
|---|---|---|
| ≥ 90 % suggestion accuracy on the corpus | **Not measured** | There is no SQL accuracy corpus yet. The completion engine has unit tests (`sql/complete.rs`), but they do not cover a corpus of cursor positions per dialect the way `mql/complete/tests.rs` does for MongoDB in phase 4b |
| Diagnostics < 100 ms for 5k lines | **Not measured** | There is no latency benchmark |
| Idempotent formatter | **Done** | `crates/ferrodb-lang/tests/format_props.rs` (proptest): formatting twice gives the same text, and every token and comment is kept. One regression case is saved in `format_props.proptest-regressions` |

## What was built

| Plan item | Where | Notes |
|---|---|---|
| Dialects as data (plan 5.8) | `assets/dialects/*.toml`, `ferrodb-lang/src/sql/dialect.rs` | Base `sql`, `postgresql`, `mysql`, `mariadb` (`inherits = "mysql"`), `sqlite`. `tsql` and `oracle` are skeletons, to be completed in phase 8.6. Each file holds keywords (reserved and soft), types, built-in functions with signatures, identifier quoting and case folding, and the default schema. Derived dialects (Greenplum, CockroachDB, Redshift → PostgreSQL; Azure SQL, Sybase → T-SQL) map to their base |
| Parser | `sql/syntax.rs` | A hand-written permissive structural parser over the existing lexer. It reads scopes, sources (tables, subqueries, table functions), CTEs, output columns, name references with roles, `INSERT` columns and rows, and tables created by `CREATE TABLE` / `CREATE VIEW` |
| Name resolution | `sql/resolve.rs`, `sql/schema.rs`, `sql/document.rs` | Resolves names against a `SchemaModel` built from the cached metadata catalog (`ferrodb-meta/src/sql_model.rs`), with a search path. Tables created earlier in the console are visible to later statements |
| Context-aware completion | `sql/complete.rs`, `sql/matcher.rs` | Tables, `JOIN` conditions on foreign keys, columns in `INSERT`, qualified columns, keywords, functions and live templates. Matching accepts a prefix, an abbreviation (`cu_id` → `customer_id`) and text typed in a Cyrillic keyboard layout |
| Inspections and quick fixes | `sql/inspect.rs` | Unknown names (with suggestions), ambiguous columns, `= NULL`, `UPDATE` / `DELETE` without `WHERE`, unused CTEs, duplicate aliases, wrong `INSERT` value counts, and `sqlparser` syntax errors for dialects it supports |
| Live templates | `sql/templates.rs`, `templates.toml` in the configuration directory | Built-in templates plus the user's own, loaded by the core |
| Formatter | `sql/format.rs` | Idempotent; keeps all tokens and comments |
| Rename, go to declaration, quick documentation, parameter info, structure view | `sql/refactor.rs` | Exposed as `LanguageService::prepare_rename` / `rename`, `declaration`, `hover`, `signature_help` and `structure` |
| Test corpus per dialect + snapshot tests | — | **Not done** (see below) |

### API (v4)

- `DocSnapshot::schema`: the console's schema / database, used for unqualified names.
- `LanguageService` gained default-implemented methods: `code_actions`, `signature_help`, `hover`, `structure`, `declaration`, `prepare_rename`, `rename`, `templates`, `expand_template`.

### Backends

- Core (`ferrodb-core/src/language.rs`): builds a `SchemaModel` from the cached catalog of the source, keeps it until the catalog changes, and loads `templates.toml`.
- Mock: delegates the new features to the same engine.

## Not done / deviations

- **UI wiring.** The console does not call the new features yet: quick fixes (Alt+Enter), rename (Shift+F6), live templates (Ctrl+J), the structure view, parameter info and the new hover. Completion and diagnostics go through the existing console paths.
- **The UI did not compile after this phase.** `ferrodb-ui/src/completion.rs` built a `DocSnapshot` without the new `schema` field. This was fixed during phase 6 with `schema: None`. The console's schema selection is therefore not passed to the language service yet.
- **Accuracy corpus and latency budget**: not written and not measured, so the two numeric criteria above are open.
- **Snapshot tests** (`insta`, plan section 9): not added.
- **tree-sitter**: the plan (5.8) names a tree-sitter parser shared by the dialects. The parser is hand-written instead, on the existing lexer. This deviation has no ADR yet.
- **ADR-009** (dialects as data; language levels A/B/C): not written.
- **T-SQL and Oracle** are skeletons only, as planned; they are completed in phase 8.6.
- **No real databases were used**: the engine works on the cached catalog, and the TCK was not run in that change.

## Test runs

- At the phase-5 commit: `ferrodb-lang` passed 66 unit tests plus the format and split property tests; `ferrodb-meta` and `ferrodb-core` built and passed.
- During phase 6 (2026-11-17, Windows 11): `ferrodb-lang` and every crate that depends on it pass, and the GUI crates build again.
