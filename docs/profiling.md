# Profiling and performance budgets

How to measure FerroDb's hot paths, where the budgets live and how CI
checks them (plan, sections 3 and 9; phase 10.2).

## Budgets

| Budget (plan, section 3) | Checked by | Where |
|---|---|---|
| Completion < 50 ms with 10k objects in the metadata cache | `complete/tables_10k`, `complete/columns_10k`, `complete/in_50k_line_console` | `crates/ferrodb-lang/benches/lang.rs` |
| Editor key response < 16 ms in a 50k-line file | `split/*_50k_lines`, `tokenize/*_50k_lines`, `statements_service_50k_lines` (the work redone on an edit) | `crates/ferrodb-lang/benches/lang.rs` |
| 5 MB document without blocking the UI | `ext_json_to_bson_5mb`, `bson_to_ext_json_5mb` (runs off the UI thread; the budget keeps it short) | `crates/ferrodb-data/benches/data.rs` |
| Export / import throughput | `export/{csv,json,json_lines}_100k_rows`, `import_csv_100k_rows` | `crates/ferrodb-data/benches/data.rs` |
| Grid: 1M rows × 50 columns at 60 fps | `grid_cell_text_100k_rows` (cell text from Arrow, the non-GPUI part of painting a cell) | `crates/ferrodb-data/benches/data.rs` |
| JDBC bridge throughput | `read_frames_64mib`, `arrow_result_100k_rows` | `crates/ferrodb-jdbc/benches/jdbc.rs` |

The medians allowed are in [`benches/budgets.toml`](../benches/budgets.toml)
(`"<package>/<bench>/<path>" = "<duration>"`, plus a global `tolerance`
factor). They are set at about 1.5× the medians measured on a busy developer
machine (3–4× quiet ones), so that a CI runner passes and a real regression (an accidentally
quadratic loop, a lost index) fails. When a change makes a benchmark
legitimately slower, raise its budget in the same PR and say why.

## Running benchmarks

| Command | What |
|---|---|
| `cargo xtask bench` | Every divan benchmark (`ferrodb-lang`, `ferrodb-data`, `ferrodb-jdbc`); extra arguments go to divan, e.g. `cargo xtask bench complete` (name filter), `--sample-count 20` |
| `cargo xtask bench-check` | The same, then each median against its budget × tolerance; fails on a regression, on a budget whose benchmark is gone, and lists benchmarks without a budget. `FERRODB_BENCH_TOLERANCE=2` overrides the tolerance (noisy machines) |
| `cargo bench -p ferrodb-lang --bench lang -- split` | One crate, one group |

Benchmarks use the `bench` profile (release, thin LTO). On a machine short
of memory, `CARGO_PROFILE_BENCH_LTO=false` builds them faster; the numbers
move by a few percent.

CI: the `bench` job of `.github/workflows/quality.yml` runs
`cargo xtask bench-check` every night (and on demand) and keeps the output
as an artifact.

## Profiling

### CPU: flame graphs

- **samply** (Linux, macOS, Windows): `cargo install samply`; build the
  release binary with line tables so frames have names:

  ```sh
  CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build --release -p ferrodb-app
  samply record target/release/ferrodb
  ```

  The profile opens in the Firefox Profiler. Do the slow thing in the app,
  then close it.
- **cargo-flamegraph** (Linux `perf`, macOS `dtrace`, Windows needs
  `blondie`/ETW and an elevated shell): `cargo flamegraph -p ferrodb-app`.
- **A single benchmark**: profile the bench binary itself, filtered to one
  benchmark, so the profile holds only that code:

  ```sh
  CARGO_PROFILE_BENCH_DEBUG=line-tables-only cargo bench -p ferrodb-lang --bench lang --no-run
  samply record target/release/deps/lang-<hash> --bench complete::tables_10k
  ```

### Allocations

divan counts allocations when its `AllocProfiler` is the global allocator;
add to a bench file locally (not committed, it slows the timings):

```rust
#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();
```

On Linux, `heaptrack target/release/ferrodb` shows allocations by call
site.

### Tracing spans

The app logs through `tracing`; `FERRODB_LOG` takes an `EnvFilter`
(`FERRODB_LOG=ferrodb_core=debug,jdbc-host=debug`). The JDBC host's stderr
is logged under the `jdbc-host` target. To time a code path in the running
app, wrap it in a span (`let _s = tracing::info_span!("complete").entered();`)
and enable span close events in the subscriber
(`fmt().with_span_events(FmtSpan::CLOSE)` in `ferrodb-app/src/main.rs`);
each close line carries `time.busy` / `time.idle`. Remove ad-hoc spans
before committing unless they mark a long operation worth keeping.

### Memory

The plan's budget is < 200 MB after startup with three native
connections. Measure the resident set with the OS tools (Task Manager /
`ps -o rss` / Activity Monitor) after connecting; the JDBC host is a
separate process, counted separately. `ferrodb-jdbc`'s tests use
`memory-stats` for the streaming bound of large results.
