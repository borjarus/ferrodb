# Phase 10.2 — profiling, regression benchmarks, fuzzing: status

Scope: plan, section 7, phase 10: "Profiling, regression benchmarks,
fuzzing of parsers (all languages), JSON → BSON conversion and the JDBC
bridge protocol"; section 9: fuzzing with `cargo-fuzz`, performance with
`divan` + budgets in CI, `proptest`.

Done on branch `p10-quality` (from `phase-10`).

## What was built

### Fuzzing (`fuzz/`, `crates/*/src/fuzzing.rs`)

One harness per area, in a `#[doc(hidden)] pub mod fuzzing` of the crate
under test. The cargo-fuzz targets and the stable proptest smoke tests call
the same functions, so a crash found by either reproduces in the other.

| Target | Harness | What runs, what is checked |
|---|---|---|
| `lang` | `ferrodb_lang::fuzzing::language_service_bytes` | Byte 0 picks one of 28 languages (every SQL dialect file incl. T-SQL / Sybase / PL-SQL, the `sqlserver` alias, MQL, CQL, SQL++, PartiQL, Redis), bytes 1..3 the cursor. Statement spans (ordered, in bounds, on char boundaries), SQL tokens (contiguous, covering the text), SQL split bodies, MQL lex / split / parse, Redis parse / split, `classify` per statement, the formatter (two styles), parameters, risks; then the `Engine` with a schema model and NoSQL schemas: completion (replace ranges), diagnostics, code actions (non-overlapping edits in bounds), signature help, hover, structure, declaration, prepare rename + rename, template expansion |
| `mql_value` | `ferrodb_lang::fuzzing::mql_value` | Shell values (`{a: 1}`, `ObjectId(…)`, regexes): parse → `print_inline` → parse must give the same BSON; `print_pretty` |
| `ext_json` | `ferrodb_data::fuzzing::ext_json` | JSON → BSON (`Bson::try_from`, as the import and the document view do); canonical Extended JSON must round-trip exactly; relaxed output; every conversion of the value (`Value::from_bson` → text / JSON / BSON / every `ValueType`, SQL++ / PartiQL literals and statements, DynamoDB JSON, rows ↔ documents, column inference) |
| `cell_value` | `ferrodb_data::fuzzing::cell_text` | Grid cell text and JSON to every column type; date / time / timestamp / hex parsers, type guessing, SQL type names |
| `import` | `ferrodb_data::fuzzing::import_bytes` | Byte 0 picks CSV / TSV / JSON / JSON Lines, header, separator (incl. a non-ASCII one) and NULL text; the whole file read as rows (identity and to-text mappings) and as documents |
| `jdbc_protocol` | `ferrodb_jdbc::fuzzing::host_stream` | Bytes from the Java host's stdout: frames (`read_frame`), routing (`protocol::incoming`, shared with the reader task), JSON answers, host errors → API errors (ranges in bounds, on char boundaries), Arrow IPC schema (`protocol::stream_schema`, shared with the query loop) and record batches through `StreamDecoder` |
| `jdbc_errors` | `ferrodb_jdbc::fuzzing::error_text` | Error positions read from vendor messages (H2 `[*]`, Hive / Exasol line:column, named objects) against a statement; `clean` |

| Item | Where | Notes |
|---|---|---|
| cargo-fuzz project | `fuzz/Cargo.toml`, `fuzz/fuzz_targets/*.rs` | Its own workspace, excluded from the main one (`exclude = ["third_party", "fuzz"]`); `cargo xtask fuzz` copies the main `Cargo.lock` into it so it builds the same versions |
| Seed corpora | `fuzz/corpus/<target>/` | Small hand-written inputs per target (scripts per language family, Extended JSON canonical / relaxed, CSV / TSV / JSON files with selector bytes, vendor messages); the binary `jdbc_protocol` seeds are written by `cargo test -p ferrodb-jdbc --test fuzz_smoke -- --ignored write_seed` |
| Smoke tests | `crates/ferrodb-{lang,data,jdbc}/tests/fuzz_smoke.rs` | proptest on random bytes / strings and structured generators (SQL / MQL / Redis atoms, recursive JSON with Extended JSON wrappers, CSV / JSON atoms, frame sequences with real and mutated Arrow streams, vendor message atoms), plus a replay of every seed file. `PROPTEST_CASES` scales them; they run in every `cargo nextest run` |
| In-memory import | `ferrodb-data/src/import.rs` | `import::from_bytes(data, format, opts)`: the CSV / TSV and JSON readers now take any `Read` (files and memory), so the import can be fuzzed (and pasted text imported) without temporary files |

### Benchmarks (`crates/*/benches/`, `benches/budgets.toml`)

| Bench | Where | What |
|---|---|---|
| `split/{postgresql,tsql,mql}_50k_lines` | `ferrodb-lang/benches/lang.rs` | Statement splitting of a 50k-line script |
| `tokenize/{postgresql,mql}_50k_lines` | same | Lexing (highlighting input) of 50k lines |
| `statements_service_50k_lines` | same | Statement spans through the language service |
| `format/{postgresql,mql}_2k_lines` | same | Formatter on a 2k-line console |
| `complete/{tables_10k,columns_10k,in_50k_line_console}` | same | Completion with 10k tables in the schema model (plan budget 50 ms) |
| `diagnostics_1k_lines` | same | Inspections with the 10k-table model |
| `ext_json_to_bson_5mb`, `bson_to_ext_json_5mb` | `ferrodb-data/benches/data.rs` | A 5 MB Extended JSON document → BSON and back |
| `export/{csv,json,json_lines}_100k_rows` | same | `Exporter` on 100k rows × 6 columns of Arrow batches, to a file |
| `import_csv_100k_rows` | same | CSV import of 100k rows from memory (type guessing, conversion) |
| `grid_cell_text_100k_rows` | same | Cell text of 600k cells from Arrow arrays (`value_at` + `to_text`): the non-GPUI part of painting grid cells |
| `read_frames_64mib` | `ferrodb-jdbc/benches/jdbc.rs` | Bridge frame decoding and routing of 64 MiB |
| `arrow_result_100k_rows` | same | An Arrow IPC result of 100k rows decoded from 256 KiB frames |

Budgets: `benches/budgets.toml` (`"<package>/<bench>/<path>" = "<duration>"`,
`tolerance = 1.25`).

Medians on this machine (Windows 11 x64, `bench` profile without LTO).
"Busy" = while other builds were running (the budgets' basis); "quiet" =
a short run without them, where one was made.

| Benchmark | Quiet | Busy | Budget |
|---|---|---|---|
| `complete/tables_10k` | 13.4 ms | 37.2 ms | 50 ms (plan) |
| `complete/columns_10k` | — (10.5 ms before bug 8) | 102 µs | 1 ms |
| `complete/in_50k_line_console` | 276 ms (2.5 min before bug 8) | 1.02 s | 1.5 s (plan: 50 ms, not met) |
| `diagnostics_1k_lines` | 12–20 ms (2.2 s before bug 8) | 22.2 ms | 40 ms |
| `split/postgresql_50k_lines` | 20.2 ms | 71.7 ms | 110 ms |
| `split/tsql_50k_lines` | 11.6 ms | 39.8 ms | 60 ms |
| `split/mql_50k_lines` | 53.2 ms | 176.7 ms | 260 ms |
| `tokenize/postgresql_50k_lines` | 17.2 ms | 47.8 ms | 75 ms |
| `tokenize/mql_50k_lines` | 45.1 ms | 140.9 ms | 210 ms |
| `statements_service_50k_lines` | 32.7 ms | 75.8 ms | 120 ms |
| `format/postgresql_2k_lines` | 7.4 ms | 24.1 ms | 40 ms |
| `format/mql_2k_lines` | 15.7 ms | 53.0 ms | 80 ms |
| `ext_json_to_bson_5mb` | | 230.7 ms (23 MB/s) | 350 ms |
| `bson_to_ext_json_5mb` | | 219.1 ms | 350 ms |
| `export/csv_100k_rows` | | 193.7 ms | 300 ms |
| `export/json_100k_rows` | | 309.5 ms | 470 ms |
| `export/json_lines_100k_rows` | | 292.8 ms | 450 ms |
| `import_csv_100k_rows` | | 273.2 ms | 420 ms |
| `grid_cell_text_100k_rows` (600k cells) | | 139.0 ms | 220 ms |
| `jdbc: read_frames_64mib` | | 6.3 ms (10.7 GB/s) | 15 ms |
| `jdbc: arrow_result_100k_rows` | | 3.7 ms (946 MB/s) | 10 ms |

### xtask, CI, docs

| Item | Where | Notes |
|---|---|---|
| `cargo xtask fuzz <target\|all> [secs]` | `xtask/src/quality.rs` | `cargo +nightly fuzz run` with a scratch corpus in `target/fuzz-corpus/<target>` (new inputs) and the checked-in seeds (read only), `-max_total_time`, `-timeout=10`; without nightly or cargo-fuzz it says what to install and points to `fuzz-smoke` |
| `cargo xtask fuzz-smoke [cases]` | same | The three smoke test files with `PROPTEST_CASES` (default 2000) |
| `cargo xtask bench [args]`, `bench-check [args]` | same | divan benchmarks of the three crates; `bench-check` parses divan's tree output (`--color never`), compares each median with its budget × tolerance (`FERRODB_BENCH_TOLERANCE` overrides), fails on regressions and on budgets whose benchmark disappeared, lists benchmarks without a budget. Parser and budget file are unit-tested |
| Nightly quality workflow | `.github/workflows/quality.yml` | Nightly + `workflow_dispatch`: `fuzz` matrix (7 targets, nightly, 10 min each by default; crash artifacts and the grown corpus uploaded), `fuzz-smoke` (5000 cases), `bench` (`bench-check`, output kept as an artifact). Separate from `nightly.yml` (database matrix, another package) |
| Profiling notes | `docs/profiling.md` | Budgets and where they are checked, running benchmarks, samply / cargo-flamegraph, allocation profiling, `tracing` spans with `FERRODB_LOG`, memory |

## Bugs found and fixed

| # | Bug | Found by | Fix | Regression test |
|---|---|---|---|---|
| 1 | **MQL lexer endless loop** (unbounded memory: the process aborted allocating 2 GiB) on a non-alphanumeric non-ASCII char outside strings — an emoji, a BOM (`\u{feff}`), `€`: the identifier branch accepted every byte ≥ 0x80 but consumed only alphanumeric chars, pushing an empty token without advancing. Reachable from the MongoDB console (typing or pasting), splitting, diagnostics, formatting | `mql_values` smoke test | Such chars are punctuation tokens (`ferrodb-lang/src/mql/lexer.rs`) | `mql::lexer::tests::non_alphanumeric_unicode_is_punctuation`, seed replay |
| 2 | **Integer overflow on extreme dates** (panic in debug builds, silently wrong values in release): a BSON date beyond ±292k years (`{"$date": -9223372036854776}`) → `Value::from_bson` multiplied milliseconds by 1000; the same class in `format_date(i32::MAX)` (days + epoch), `format_time` of a negative time (wrapped to `u32`, then `frac * 1000` overflowed), `Date → Timestamp` conversion of a large date, Arrow second / millisecond timestamps → microseconds | `ext_json_structured` smoke test (100k cases); the other sites by reviewing the same arithmetic | Checked arithmetic: such dates stay BSON values, out-of-range dates / times print their number, the conversion fails with the usual "cannot convert" error, Arrow timestamps saturate (`ferrodb-data/src/value.rs`) | `tests::extreme_dates_and_times_do_not_overflow`, seeds `ext_json/crash-date-overflow.json`, `extreme-dates.json` |
| 3 | **JDBC error range ended inside a char**: the range of a host error position was `start..start + 1` bytes, cutting a multi-byte char (`ż`); the editor slices the statement with it | Char-boundary invariant of the `jdbc_protocol` harness (seen while writing it) | The range covers the whole char at the position (`protocol.rs`) | `protocol::tests::error_range_covers_a_whole_char` |
| 4 | **JDBC error position off by one after `İ`** (and other chars whose lower case has more chars): `named_object` counted chars in the lower-cased statement | Review while writing the `jdbc_errors` harness | Lower-cases char by char, mapping each byte back to the statement's char index (`errors.rs`) | `errors::tests::positions_count_chars_of_the_statement` |
| 5 | **Bridge frame header allocated before reading**: a corrupt frame header announcing up to 256 MiB allocated and zeroed the whole payload before the stream turned out short (an out-of-memory risk with a misbehaving host, and a 256 MiB memset per fuzz input) | Review while writing the `jdbc_protocol` harness | The payload grows as bytes arrive (`take(len).read_to_end`, 1 MiB reserved up front); a short stream is `UnexpectedEof` (`protocol.rs`) | `protocol::tests::truncated_frame_is_an_error_without_a_large_allocation` |
| 6 | **Non-ASCII CSV separator silently truncated** to its low byte (`é` → `0xE9`) in both import and export | Review of the `import` harness options | Refused with a clear error; one helper `export::csv_delimiter` for both directions | `import` smoke tests (separator `é`) |
| 7 | **Panic on a malformed Arrow result from the JDBC host**: arrow-ipc 60.1 slices the message body with the buffer offsets / lengths of a record batch without checking them (`Buffer::slice_with_length` panics), so a corrupt data frame panicked the query task | `arrow_results_mutated` smoke test (first run, 256 cases) | `protocol::ArrowStream` (used by the connection, the harness and the benchmark) follows the IPC framing across frames and checks every buffer of record / dictionary batches against the message body before decoding; a panic inside arrow-ipc on input the check does not cover (e.g. union type ids shorter than the node) is caught and becomes an error (`protocol::in_guarded_decode` lets the libFuzzer target tolerate exactly those). No caught panic in 50k cases after the check | `protocol::tests::arrow_stream_refuses_buffers_outside_the_body`, `arrow_stream_decodes_in_any_chunks`, proptest regression file |
| 8 | **Analysis cost exploded with a large catalog** (performance): every unresolved table reference made the resolver compute "did you mean" suggestions by an allocating edit-distance scan of all tables, during every analysis of the whole console (completion, hover, diagnostics…). With 10k tables: inspections of a 1k-line script 2.2 s, completion at the end of a 50k-line console 2.5 minutes. Also the lexical-error check of the inspections scanned every diagnostic per statement (quadratic) | `diagnostics_1k_lines` and `complete/in_50k_line_console` benchmarks | Table suggestions computed only by the inspections, once per distinct name per run, with an allocation-free edit distance; lexical errors searched in sorted ranges (`sql/resolve.rs`, `sql/inspect.rs`). Now ≈ 15 ms and ≈ 275 ms | Existing level A tests (suggestions, quick fixes) unchanged and green; the benchmarks with budgets |

## Tests

| Crate | What |
|---|---|
| `ferrodb-lang` | `tests/fuzz_smoke.rs` (7): random bytes, random text, structured text in every language, MQL values (structured and random), seed replay, every seed text in every language at three cursors; `mql::lexer` regression test. All 102 unit tests and the existing property tests unchanged and green |
| `ferrodb-data` | `tests/fuzz_smoke.rs` (7): Extended JSON structured / random, cell values (text and JSON), import random / structured, seed replay; `tests::extreme_dates_and_times_do_not_overflow` (55 unit tests) |
| `ferrodb-jdbc` | `tests/fuzz_smoke.rs` (7 + 1 ignored seed writer): random bytes, frame sequences, whole and mutated / truncated Arrow results in any chunking, vendor messages random / structured, seed replay; protocol and error-position unit tests (14) |
| `xtask` (4) | divan output parser (synthetic tree and verbatim divan 0.1.21 output with groups and throughput rows), budget file parsing, the checked-in budgets name existing bench targets |

## Test runs (2026-11-23, Windows 11 x64, touched crates only)

The machine was short of memory (other builds running): every command used
`CARGO_TARGET_DIR=…/target-p9-script CARGO_INCREMENTAL=0 -j 1`; builds that
died of out-of-memory were retried.

- `cargo test -p ferrodb-lang` — 102 unit + 1 + 7 + 5 integration tests passed.
- `cargo test -p ferrodb-data --lib --test fuzz_smoke` — 55 + 7 passed.
- `cargo test -p ferrodb-jdbc --lib --test fuzz_smoke` — 14 + 7 passed (1 ignored: the seed writer). `tests/bridge.rs` needs Java and was not run.
- `cargo test -p xtask` — 4 passed (divan output parser on verbatim output, budget files incl. the checked-in one).
- Smoke tests with many cases (test binaries run directly): `PROPTEST_CASES=40000` for `ferrodb-lang` (188 s), `100000` for `ferrodb-data` (25 s), `50000` for `ferrodb-jdbc` (8 s): all green after the fixes.
- `cargo clippy -p ferrodb-lang -p ferrodb-data -p ferrodb-jdbc -p xtask --all-targets -- -D warnings` — clean.
- `cargo fmt` of the four crates — clean.
- Benchmarks: `cargo bench -p <crate> --bench <name>` with `CARGO_PROFILE_BENCH_LTO=false` (memory); medians below.

## Not done / deviations

- **cargo-fuzz was not run here**: this Windows machine has no nightly toolchain (only stable 1.85–1.99), and installing one was out of scope. The fuzz project, its targets and `cargo xtask fuzz` are written for Linux CI (`quality.yml`); the identical harness ran on stable through the smoke tests, which found the crashes listed above (bugs 1, 2, 7). The fuzz crate was not compiled locally (libFuzzer needs nightly `-Zsanitizer`), so the first nightly run is its first build.
- **`fuzz/Cargo.lock` is not committed**: `cargo xtask fuzz` copies the main lock file into `fuzz/` so the targets build the same dependency versions (cargo adds `libfuzzer-sys`).
- **Completion at the end of a 50k-line console misses the plan's 50 ms** (≈ 275 ms on a quiet machine): the engine analyses the whole console (tokens, split, parse, resolution of every statement) on each request. Completion in a normal console (`tables_10k`, `columns_10k`) is well within budget. Analysing only the statements up to the cursor, and parsing earlier ones only when they create tables, would fix it; left for the editor work (needs care: rename, structure and inspections use the whole document). Its budget records the current value so that it does not get worse.
- **Statement splitting of 50k lines takes 20–50 ms**, above the 16 ms key-response budget if it runs on every key press on the UI thread (how the console schedules it was not measured here; the UI was not built). Incremental splitting would be the fix.
- **No grid / tree benchmark through GPUI**: the grid model lives in `ferrodb-ui` (GPUI), which this package does not build (memory). `grid_cell_text_100k_rows` covers the non-GPUI part (cell text from Arrow arrays).
- **Budgets are calibrated on a loaded developer machine** (other builds running made timings vary by 2–3×): they are generous (about 4–5× the quiet medians including the tolerance) and catch order-of-magnitude regressions like bug 8, not 10 % drifts. Recalibrate them from the first CI runs (`bench-results` artifact).
- **XLSX and Parquet import are not fuzzed**: they parse through `calamine` / `parquet` from files; the fuzzed readers are the ones FerroDb implements (CSV / TSV, JSON, JSON Lines, Extended JSON).
- **Arrow IPC decoding is protected, not fixed upstream**: the message check covers the panic found; other arrow-ipc panics on malformed input are turned into errors. Reporting the unchecked slicing to arrow-rs is a follow-up.
