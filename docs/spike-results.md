# Phase 0 spike results

Fill in one section per system. Commands:
- `cargo xtask spike gui --bench --out report.txt`
- `cargo xtask spike jdbc-reader …`
- `cargo xtask spike adbc <path-to-library> <uri> <rows>`

## Windows 11 — Intel Iris Xe, DirectX 11.1 (2026-11-05)

### GUI (`spike-gui`, release, 1400×900, vsync 60 Hz, 4 s per case, idle CPU)

| Case | Run 1 | Run 2 | Run 3 | Worst frame (max) |
|---|---|---|---|---|
| Grid 1M×50 — continuous scroll (20 rows/frame) | 58.9 | 58.8 | 60.2 | 38 ms (first frames, page generation) |
| Grid 1M×50 — random jump + new column every frame | 58.5 | 60.0 | 60.1 | 40 ms |
| Tree 100k documents + 5 MB document | 59.9 | 60.0 | 59.9 | 21 ms |
| Keys 1M (`uniform_list`) | 59.9 | 60.0 | 52.3 | 52 ms |

Notes:
- A run made while `cargo` was compiling in the background measured 24–41 fps. Benchmarks need an idle machine and a foreground window, because Windows throttles background windows.
- Startup costs:
  - building 100k tree items: 0.51–0.60 s (release), done off the UI thread in the product;
  - editor with 50k lines: 4 ms;
  - grid state: < 1 ms.
- **Verdict (Windows): GO** for grid, tree and keys. Still to do:
  - manual check of 50k-line editor typing latency (smooth in a first manual check);
  - IME check (Polish diacritics, Japanese IME).

### JDBC bridge, Rust side (`spike-jdbc-reader --self-test --rows 1000000`, dev profile)

- 1,000,000 rows / 245 batches / 5 columns in 0.73 s → **1.37M rows/s**
- first batch after 0.7 ms
- RSS 5.2 → 9.0 MB, so memory stays constant
- Java side end to end: **not run**. It needs Java 21+ and Gradle; this machine has Java 8 only.

### ADBC (`spike-adbc`)

- Driver loading and error handling are verified: a missing library gives a clean `LoadLibraryExW failed`.
- End-to-end run with `adbc_driver_sqlite` 1.13.0 (`:memory:`, 1,000,000 rows, release profile):
  - driver loaded in 6.3 ms;
  - schema: `id` Int64, `name` Utf8, `amount` Float64, `flag` Int64;
  - 1,000,000 rows / 977 batches, first batch after 4.5 ms, total 484 ms → **2.06M rows/s**.
- How to reproduce. The Windows wheel ships the DLL under the name `libadbc_driver_sqlite.so` (it is a PE file, header `MZ`), so rename it to `.dll`:
  ```
  pip download adbc-driver-sqlite --no-deps --only-binary=:all:
  # extract adbc_driver_sqlite/libadbc_driver_sqlite.so from the wheel and rename it to adbc_driver_sqlite.dll
  cargo xtask spike adbc <path-to-library> :memory: 1000000
  ```
  Do not put `--` before the arguments: xtask already adds one, and a second `--` is passed to the spike as the driver path (`LoadLibraryExW failed`).

## macOS — pending

## Linux X11 — pending

## Linux Wayland — pending
