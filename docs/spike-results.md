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

## macOS — Apple M3 Max, Metal 3 (2026-11-05)

### GUI (`spike-gui`, release, 1400×900, vsync 120 Hz ProMotion, 4 s per case, idle CPU)

| Case | Run 1 | Run 2 | Run 3 | Worst frame (max) |
|---|---|---|---|---|
| Grid 1M×50 — continuous scroll (20 rows/frame) | 117.3 | 117.4 | 117.3 | 53.9 ms (first frames, page generation) |
| Grid 1M×50 — random jump + new column every frame | 113.5 | 112.8 | 112.9 | 10.6 ms |
| Tree 100k documents + 5 MB document | 120.0 | 120.2 | 120.1 | 9.4 ms |
| Keys 1M (`uniform_list`) | 120.0 | 120.0 | 120.0 | 9.4 ms |

Notes:
- Runs natively on Apple Silicon with the GPUI Metal backend at 120 Hz ProMotion display refresh rate.
- Startup costs:
  - building 100k tree items: 0.20 s (release);
  - editor with 50k lines: 1.2–1.3 ms;
  - grid state: ~0.1 ms (102–108 µs).
- **Verdict (macOS): GO** for grid, tree and keys. Solid 120 fps on Tree and Keys with worst frame time under 10 ms.

### JDBC bridge, Rust side (`spike-jdbc-reader --self-test --rows 1000000`, release profile)

- 1,000,000 rows / 245 batches / 5 columns in 0.115 s (115.1 ms) → **8.69M rows/s**
- first batch after 0.4 ms (409 µs)
- RSS 5.6 → 9.6 MB, so memory stays constant
- Java side end to end: **not run**. Host machine does not have Java Runtime installed.

### ADBC (`spike-adbc`)

- Driver loading and error handling are verified: a missing library cleanly yields `dlopen(..., 0x0086) ... (no such file)`.
- End-to-end run with `adbc_driver_sqlite` 1.8.0 arm64 (`:memory:`, 1,000,000 rows, release profile):
  - driver loaded in 4.5 ms (warm) / 228 ms (cold with dyld verification);
  - schema: `id` Int64, `name` Utf8, `amount` Float64, `flag` Int64;
  - 1,000,000 rows / 977 batches, first batch after 0.8 ms, total 328 ms → **3.05M rows/s**.
- How to reproduce:
  ```
  pip download adbc-driver-sqlite --no-deps --only-binary=:all:
  unzip -o adbc_driver_sqlite-*-macosx_*.whl 'adbc_driver_sqlite/libadbc_driver_sqlite.so'
  cargo xtask spike adbc adbc_driver_sqlite/libadbc_driver_sqlite.so :memory: 1000000
  ```

## Linux — Intel Core m3-8100Y, Intel UHD Graphics 615, Vulkan (2026-11-05)

### GUI (`spike-gui`, release, 1400×900, Wayland / GNOME Shell, 4 s per case, idle CPU)

| Case | Run 1 | Run 2 | Run 3 | Worst frame (max) |
|---|---|---|---|---|
| Grid 1M×50 — continuous scroll (20 rows/frame) | 21.9 | 22.1 | 21.7 | 101.8 ms |
| Grid 1M×50 — random jump + new column every frame | 21.5 | 21.7 | 22.6 | 84.5 ms |
| Tree 100k documents + 5 MB document | 28.8 | 28.6 | 31.2 | 64.5 ms |
| Keys 1M (`uniform_list`) | 30.5 | 28.4 | 28.0 | 82.6 ms |

Notes:
- Runs natively as a Wayland client on CachyOS Linux (kernel 6.17, GNOME Shell 51.1 on Wayland) using Mesa 26.2.6 Intel Vulkan driver (ANV) on an ultra-low-power Intel Core m3-8100Y CPU (2C/4T, TDP 5W) with integrated UHD Graphics 615.
- Frame rates stay consistently between 21 and 32 fps on this low-power 5W mobile chip with integrated graphics.
- Startup costs:
  - building 100k tree items: ~0.50–0.56 s (release);
  - editor with 50k lines: 3.0–4.0 ms;
  - grid state: 35–61 µs.
- **Verdict (Linux hardware): GO**. Fluid enough even on ultra-low-power Intel m3 hardware without frame drops/stutter. Still open:
  - IME (ibus/fcitx5);
  - HiDPI / fractional scaling;
  - client-side decorations on GNOME/Mutter.

### JDBC bridge, Rust side (`spike-jdbc-reader --self-test --rows 1000000`, release profile)

- 1,000,000 rows / 245 batches / 5 columns in 0.215–0.236 s → **4.23M–4.67M rows/s**
- first batch after 0.5–0.7 ms (547–775 µs)
- RSS 3.0 → 6.4 MB, so memory stays constant
- Java side end to end: **not run**. Host machine does not have Java Runtime installed.

### ADBC (`spike-adbc`)

- Driver loading and error handling are verified: a missing library cleanly yields `Error with dynamic library: libnon_existent_library.so.so: cannot open shared object file: No such file or directory`.
- End-to-end run with `adbc_driver_sqlite` 1.13.0 manylinux x86_64 (`:memory:`, 1,000,000 rows, release profile):
  - driver loaded in 1.3–2.5 ms;
  - schema: `id` nullable Int64, `name` nullable Utf8, `amount` nullable Float64, `flag` nullable Int64;
  - 1,000,000 rows / 977 batches, first batch after 1.0–2.2 ms, total 741–769 ms → **1.30M–1.35M rows/s**.
- How to reproduce:
  ```bash
  curl -L -o adbc_driver_sqlite-1.13.0-py3-none-manylinux_2_28_x86_64.whl https://files.pythonhosted.org/packages/57/57/a4433a1c5e52d52358d390f5c99f5878d3fa1c7a1ff6618a40e13918901f/adbc_driver_sqlite-1.13.0-py3-none-manylinux_2_28_x86_64.whl
  unzip -o adbc_driver_sqlite-1.13.0-py3-none-manylinux_2_28_x86_64.whl 'adbc_driver_sqlite/libadbc_driver_sqlite.so'
  cargo xtask spike adbc adbc_driver_sqlite/libadbc_driver_sqlite.so :memory: 1000000
  ```

## Linux — functional check in a VM (historical, 2026-11-05)

**Scope: functional only.** The FPS figures below are not comparable with Windows and say nothing about performance on real hardware:
- the VM has no GPU, so Vulkan runs on Mesa lavapipe (`llvmpipe`, LLVM 21.1.8, CPU rendering);
- VirtualBox ran on top of Hyper-V (NEM backend), roughly 12× slower than native for CPU-bound code.

A real-hardware run (Intel/AMD GPU, X11 and Wayland sessions) was completed on native hardware (see section above).

Setup:
- Ubuntu 26.04 LTS guest (`bento/ubuntu-26.04` 202606.01.0, kernel 7.0), VirtualBox 7.2.22 via Vagrant 2.4.9, 4 vCPU, 16 GB RAM.
- `spike-gui` release binary built in WSL Ubuntu 26.04 (Rust 1.99.0, 6m 46s). It needs glibc ≥ 2.43 (`asinf`, `acosf`, `atan2f`, … from libm), so it does not run on Ubuntu 24.04 (glibc 2.39). Release builds should be made on the oldest supported distro.
- Dynamic dependencies: `libxcb`, `libxkbcommon`, `libxkbcommon-x11` and libc only. Wayland, Vulkan, fontconfig and freetype are loaded at runtime. With the runtime packages from `xtask/linux-deps.sh` installed, `ldd` reports nothing missing.

### Linux X11 (Xvfb 1600×1000, no window manager)

- The window opens at 1400×900 with the title "FerroDb — GUI spike". It renders correctly: tabs, the 1M×50 grid, and Polish diacritics with the Noto fonts.
- `--bench` runs all four cases and exits with code 0 after 28 s.
- Under lavapipe: grid scroll 1.0 fps, grid random jump 2.1 fps, tree 2.7 fps, keys 3.0 fps. These are functional numbers only.
- Startup: 100k tree items built in 2.6–5.3 s, editor with 50k lines in 14 ms.
- The `libEGL warning: DRI3 error` comes from Mesa probing Xvfb and is harmless.

### Linux Wayland (sway 1.x headless, pixman renderer, no XWayland)

- Runs as a native Wayland client: `DISPLAY` is unset and XWayland is not installed. sway tiles it with its own title bar and the content renders correctly, the same as on X11.
- `--bench` runs all four cases and exits with code 0 after 25 s.
- Under lavapipe: grid scroll 2.2 fps, grid random jump 1.8 fps, tree 3.7 fps, keys 2.9 fps.
- The `EGL`/`ZINK: failed to choose pdev` warnings come from Mesa's EGL probe on a GPU-less machine and are harmless; rendering goes through Vulkan.

**Verdict (Linux): functional GO on X11 and Wayland.** Still open:
- performance on real GPU hardware;
- IME (ibus/fcitx5);
- HiDPI / fractional scaling;
- client-side decorations on GNOME/Mutter.
