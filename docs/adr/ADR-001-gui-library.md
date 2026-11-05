# ADR-001: GUI library — GPUI + GPUI Kit

- Status: **Accepted (provisional)**. Windows has been measured. Release-mode runs on macOS and Linux (X11 and Wayland) are still required before milestone M0.
- Date: 2026-11-05

## Context

FerroDb must handle the following without dropping frames:
- result grids of 1M rows × 50 columns,
- document trees of 100k documents, plus a single 5 MB document,
- key lists of 1M keys,
- a code editor holding 50k lines.

It must run on Windows, macOS and Linux from one codebase. The candidates were GPUI + GPUI Kit, egui 0.35, iced 0.14 and Slint 1.x (plan, section 4.1).

## Decision

Use **`gpui-kit = "=0.8.1"`**, pinned exactly. This pins the `gpui-pre 0.3.10` snapshot. The optional `gpui-fast` backend stays disabled.

- Application views import GPUI only through the **`ferrodb-ui-kit`** facade (`ferrodb_ui_kit::{gpui, kit, base, theme, icons}`).
- `ferrodb-ui` and `ferrodb-app` also list `gpui-kit` as a direct dependency, for one reason only. GPUI's derive and action macros locate their paths by looking up the `gpui-kit` crate in the *calling* crate's manifest (`gpui-pre-macros/src/gpui_pre_facade_paths.rs`). Code in those crates still imports through the facade.
- Upgrades happen in planned windows every 6–8 weeks, each as a separate PR.

## Evidence (phase 0 spike, `spikes/gui`)

Commands:
- Interactive: `cargo xtask spike gui`
- Benchmark: `cargo xtask spike gui --bench --out report.txt`

In benchmark mode the spike scrolls each view automatically for 4 s. Every frame jumps to a new position, so nothing comes from cache: the grid regenerates a 1000×50 Arrow page on each frame. The spike reports FPS and the worst frame time.

**Windows 11, Intel Iris Xe, DirectX 11.1, 1400×900 window, dev profile (dependencies at opt-level 2, spike code unoptimized):**

| View | FPS | Worst frame | Notes |
|---|---|---|---|
| Grid 1M × 50 (`DataTable`, row + column virtualization) | 50.9 | 65 ms | A fresh 1000-row × 50-col Arrow page is generated *every frame* by unoptimized mock code — worst case |
| Tree 100k docs + 5 MB doc (`Tree`) | 60.0 (vsync) | 21 ms | Building 100k `TreeItem`s took 1.26 s (that would happen in the background in the product) |
| Keys 1M (`uniform_list`) | 60.0 (vsync) | 20 ms | |
| Editor 50k lines SQL (`EditorState`, tree-sitter) | — | — | Loads in 8 ms; editing was smooth in a manual check |

**Same machine, release profile, idle CPU, 3 runs:**
- grid continuous scroll: 58.8–60.2 fps
- grid random jump: 58.5–60.1 fps
- tree: 59.9–60.0 fps
- keys: 52.3–60.0 fps

All views run at vsync, apart from single outliers in the first frames. Details are in [`spike-results.md`](../spike-results.md).

Other findings:
- The dock layout saves to JSON and restores (`DockArea::dump` / `load` plus the `register_panel` registry).
- UI tests run without a window: `#[gpui_kit::test]`, `TestWindowExt::find`, `wait_for`.
- The command palette in Kit 0.8.1 uses substring filtering, so we turn its filtering off and rank results with `nucleo`.
- Kit ships no Polish UI strings for its built-in components. FerroDb's own strings use Fluent (ADR-014), and missing Kit strings fall back to English.

## Go / no-go

| Criterion | Windows | macOS | Linux X11 | Linux Wayland |
|---|---|---|---|---|
| Starts and renders correctly | ✅ | pending | ✅ VM, lavapipe (functional only) | ✅ VM, sway, lavapipe (functional only) |
| ≥ 60 fps grid | ✅ release 58.5–60.2 (vsync) | pending | pending | pending |
| ≥ 60 fps tree | ✅ | pending | pending | pending |
| Smooth 50k-line editing | ✅ (manual) | pending | pending | pending |
| Correct IME | pending (manual, Polish + CJK) | pending | pending | pending |

The Linux functional run (2026-11-05, Ubuntu 26.04 VM without a GPU) is described in [spike-results.md](../spike-results.md). The fps criteria on Linux still need real GPU hardware.

**If the grid criterion fails in release mode**, apply these mitigations before considering a switch:
- format cells from already-fetched batches only (the real grid never generates data on the UI thread);
- cache formatted strings for each visible page.

Plan B (egui 0.35) applies only if a criterion still fails after the mitigations. Switching would affect only the `ferrodb-ui*` crates.

## Consequences

- The 0.x API will change between versions. The facade and planned upgrade windows limit how much that costs us.
- GPU renderers: Metal on macOS, DirectX 11 on Windows, Vulkan on Linux. Linux CI needs the X11/Wayland/Vulkan development packages (`xtask/linux-deps.sh`).
