# Phase 10 — 10.3 Crash reports, signed updates, packaging and signing: status

Scope: plan, section 7, phase 10: "crash reporting (with consent), packaging and signing (Authenticode, Apple notarization), signed updates; signatures and verification for downloaded ADBC drivers and Java runtime on macOS", and the beta program. Security rules of section 3: no telemetry without consent, secret masking in logs, downloaded code verified.

Built on the API v8 contracts (`ferrodb-api/src/release.rs`, unchanged) in the `p10-release` worktree.

## What was built

### `ferrodb-release` (new crate)

| Item | Where | Notes |
|---|---|---|
| Composition | `src/lib.rs` | `ReleaseBackend` wraps any `Backend` (core or mock) like `AiBackend`, adds `updates()` and `crash_reports()`. `ReleaseOptions::from_build` reads `option_env!` values: `FERRODB_UPDATE_URL`, `FERRODB_UPDATE_PUBKEY` (base64 Ed25519, 32 bytes), `FERRODB_CRASH_ENDPOINT`, `FERRODB_RELEASE_CHANNEL`. None is set by default: a developer build checks for nothing and sends nothing |
| Secret masking | `src/mask.rs` | There was no masking code in the workspace (secrets live in the keyring as `secret://` references). Masks URI passwords (`user:***@`), `password=` / `pwd=` / `secret=` / `token=` / `api_key=` / `client_secret=`… pairs (also JSON, TOML, ODBC `{…}`), `Bearer` / `Basic` credentials, AWS key ids (`AKIA…`, `ASIA…`), `sk-ant-…` and other `sk-…` keys. `secret://` references stay. Greedy by design |
| Log ring buffer | `src/logbuf.rs` | `log_layer()` — a `tracing` layer keeping the last 200 lines (masked on the way in, 2000 chars max) in memory; read with `try_lock` so a panic inside the logger cannot deadlock |
| Panic hook | `src/crash.rs` | `install_panic_hook(data_dir, version)` — first thing in `main`; writes `<data>/crashes/<id>.json` (`CrashReport`: id `YYYYMMDD-HHMMSS-xxxx`, RFC 3339 time, version, `os arch version` probed once at install, thread name, masked message, `file:line:col`, `Backtrace::force_capture`, log tail), atomically (temp + rename), then chains to the previous hook |
| Crash report service | `src/crash.rs` | `pending()` = not sent, not dismissed, written before this process started; `dismiss` keeps files and lists ids in `crashes/dismissed.txt`; `send` POSTs each report as JSON (comment masked) with `ureq` on a blocking thread, marks it sent; without an endpoint: `"no crash endpoint configured in this build"` (the UI then offers Copy Report / Open Reports Folder); `clear` deletes everything |
| Settings | `src/settings.rs` | `<config>/release.toml`: `consent` (`ask` default / `always` / `never`), `[updates] channel`, `check_automatically`, `last_update_check` (Unix seconds); atomic writes; broken file → defaults with a warning; in memory for the mock |
| Manifest and signature | `src/manifest.rs` | `{"schema":1,"releases":[{version, channel, date, notes, platforms:{"<os>-<arch>":{url, sha256, size}}}]}` — one entry per channel. `<manifest>.sig` = base64 Ed25519 signature of the exact bytes, verified with `ring::signature::ED25519` **before** parsing. Selection: channel filter (stable → stable only, and never a pre-release even if mislabelled; beta → beta + stable), newer than the running version, package for this platform, well-formed SHA-256 |
| Versions | `src/version.rs` | Small semver 2.0 comparator (pre-release precedence: `1.0.0-alpha < … < 1.0.0-rc.1 < 1.0.0`), no new dependency |
| Update service | `src/updates.rs` | `check` (HTTPS only; plain HTTP to loopback only with a test flag), records the attempt for `check_due` (enabled, URL and key present, 24 h); `download` on a thread to `<data>/updates/` with progress, cancellation per 64 KiB chunk, size and SHA-256 against the signed manifest, `.part` removed on failure; `install` only for files inside the download directory |
| Installers | `src/updates.rs` (`install_plan`) | Windows: runs the NSIS `…-setup.exe` normally (its own UI). macOS: `open` the `.dmg`. Linux: when running as an AppImage (`$APPIMAGE`), copy + rename over it and start it; otherwise `xdg-open` the folder; `.deb` opened with `xdg-open`. The UI quits after `install` |

### Driver manager on macOS (`ferrodb-driver-manager/src/codesign.rs`)

| Artifact | Signature | Action |
|---|---|---|
| ADBC library (dylib from a PyPI wheel) | Developer ID, verifies | keep; remove `com.apple.quarantine` |
| ADBC library | ad-hoc / none (usual for Go-built wheels) | ad-hoc sign (`codesign --force --sign -`) so Apple Silicon loads it; remove quarantine. **The catalog SHA-256 (PyPI digest) is the trust anchor** |
| ADBC library | Developer ID that does not verify | reject, delete |
| Temurin JRE bundle | vendor signature, `codesign --verify --deep --strict` passes | remove quarantine (recursively); `spctl --assess` logged |
| Temurin JRE bundle | anything else | reject, delete |

Runs after the checksum, for `download` and offline installs of `NativeLibrary` artifacts and after the JRE is unpacked. The decision table and the parsing of `codesign -dv` are pure and tested on every OS; running the tools is `cfg(target_os = "macos")`, a no-op elsewhere. A signed driver catalog was not added: the catalog is compiled into the (signed) application, so it is covered by the application signature.

### Packaging and signing (`xtask`, `.github/workflows/release.yml`)

| Item | Where | Notes |
|---|---|---|
| `cargo xtask release-keygen [--out F]` | `xtask/src/release.rs` | Ed25519 PKCS#8 via `ring`; writes base64 to `~/.ferrodb-release/update-key.pk8.b64` by default, refuses paths inside the repository and existing files; prints the public key for `FERRODB_UPDATE_PUBKEY` |
| `cargo xtask release-manifest` | `xtask/src/release.rs` | `--version --channel --notes/--notes-file --dir --base-url [--out] [--date]`: SHA-256 and size of `*-setup.exe`, `*.dmg` (arch from the name), `*.AppImage`; merges into an existing manifest (newest release per channel), signs with `FERRODB_RELEASE_KEY_FILE`, verifies its own signature, writes `manifest.json` + `.sig` |
| `cargo xtask package` | `xtask/src/main.rs` | Windows: `signtool sign /fd sha256 /tr <ts> /td sha256` on `ferrodb.exe` before packaging and on the installer after (`FERRODB_SIGN_CERT` + password, or `FERRODB_SIGN_CERT_SHA1`), then `signtool verify /pa`. macOS: with `FERRODB_MACOS_SIGN_IDENTITY` the `.app` is packaged, signed (`--options runtime --timestamp`), put in a `.dmg` with `hdiutil`, the `.dmg` signed, `xcrun notarytool submit --wait` (Apple ID + team + app-specific password, or API key), `stapler staple` + `validate`. Linux: optional detached `.asc` with `FERRODB_GPG_KEY`. Every unconfigured step is skipped with a warning |
| Release workflow | `.github/workflows/release.yml` | On `v*` tags: build + package + sign on Windows, macOS, Ubuntu 22.04 (certificates imported from secrets, temporary keychain on macOS); a publish job writes the signed manifest (merged with the previous release's) and creates the GitHub release (pre-release for `-` versions). Secrets are referenced by name only |

### Application (`ferrodb-app/src/main.rs`)

`ferrodb --version` / `-V` prints version, API version and build channel. The panic hook is installed before tracing and everything else (mock: reports in `<temp>/ferrodb-mock/crashes`); tracing is a registry with the env filter, the stderr formatter and the ring-buffer layer; the backend (core or mock, already wrapped by `AiBackend`) is wrapped by `ReleaseBackend` (mock: settings in memory).

### UI (`ferrodb-ui`, through `Backend` only)

| Item | Where | Notes |
|---|---|---|
| State | `src/release.rs` | `ReleaseHub` global: found update, download state (Idle / Running / Ready / Failed), cancel handle, last check, the crash dialog |
| Startup | `release::startup` (from `Workspace::new`, once per process) | Pending reports and `check_due` read off the UI thread. Consent Ask → crash dialog; Always → sent silently (result in the output log); Never → nothing. Due → background check; a found update → notification "FerroDb X is available" with View Update, and a status-bar button |
| Crash dialog | `release::CrashDialog` | "FerroDb closed unexpectedly last time": reports with expandable details (version, system, thread, location, backtrace, log tail), comment field, Send / Don't Send / Always Send; a failed send keeps the dialog open with the error, Copy Report and Open Reports Folder |
| Update dialog | `release::open_update_dialog` | Version, date, notes, size, beta warning; Download → progress bar with Cancel → "Downloaded and verified" → Install and Restart (calls `install`, then dispatches Quit) |
| Settings page | `release::ReleaseSettingsView`, `settings_view.rs` | "Updates and Privacy": version, channel (Stable / Beta, "Join the beta program"), check automatically, Check Now with result and View Update, crash consent (Ask / Always / Never), View Reports (list with details, sent / local), Open Reports Folder, Clear; "no telemetry" note. Saved at once |
| Help menu, palette | `menus.rs`, `actions.rs`, `workspace.rs` | Check for Updates…, Join Beta Program…, Report a Problem… (copies diagnostics — version, API, channel, OS, backend, last crash — and opens `<repository>/issues/new`), Updates and Privacy, About (version, API version, channel, backend, Check for Updates button); all in Find Action |
| Translations | `assets/i18n/{en,pl}` | All new strings in both languages (Polish plurals for report counts) |

## Tests

| Crate | What |
|---|---|
| `ferrodb-release` unit (15) | Masking (URIs, pairs, JSON, ODBC braces, Bearer, `sk-ant-`, AWS ids, `secret://` kept, Unicode); semver chain of the spec and invalid versions; signature accept / tampered / wrong key / missing / garbage; channel filtering and downgrades; manifest merge; ring buffer (masked, bounded); settings round trip; install plans; package names |
| `ferrodb-release` `tests/updates.rs` (5) | Local HTTP server (std `TcpListener`) with a fresh key: check → download → verify; corrupt and truncated packages rejected, no `.part` left; unsigned, tampered and wrongly signed manifests rejected; stable vs beta, persisted channel, no downgrade; cancellation; `install` refuses foreign files; nothing configured → no checks |
| `ferrodb-release` `tests/crash.rs` (1) | Panic in a named thread (`catch_unwind`), hook restored: report written with masked message and log tail, thread, location, backtrace, OS; pending; send without endpoint → the documented error; send to a local endpoint (comment masked) → sent; dismiss; consent persisted; clear |
| `ferrodb-driver-manager` | `codesign`: parsing of Developer ID / ad-hoc / unsigned output, the decision table, no-op outside macOS |
| `xtask` (2) | Package → platform key mapping, option parsing, "inside the repository" check |
| `ferrodb-ui` `tests/phase10.rs` (10) | Crash dialog: send with comment, Don't Send, Always Send, failure keeps the dialog with copy / folder; consent Always sends silently, Never does nothing; settings page round trip (channel, auto check, consent, Check Now, View Reports, details, Clear); update due → status bar → dialog → download progress → ready → install; cancel; Join Beta → channel + check, About lines and diagnostics |

## Test runs (2026-11-23, Windows x64, crates touched only)

Environment: `CARGO_TARGET_DIR=target-p8`, `CARGO_INCREMENTAL=0`, `-j 1` (low-memory machine).

- `cargo test -p ferrodb-release`: 15 unit + 5 (`updates`) + 1 (`crash`) passed.
- `cargo test -p ferrodb-driver-manager -p xtask`: all passed (codesign table and parsing; xtask 2).
- `cargo clippy -p ferrodb-release -p ferrodb-driver-manager -p xtask --all-targets -- -D warnings`: clean.
- `cargo test -p ferrodb-ui --test phase10`: 10 passed; `--test phase9`: 8 passed; `--lib`: 36 passed (incl. English / Polish message parity and action labels). Built with `--config 'profile.dev.package.ferrodb-ui.debug=0'`: with full debug info the 280 MB `ferrodb-ui` rlib could not be memory-mapped ("paging file is too small").
- `cargo fmt --check` on every touched crate: clean.
- **Not run here**: `cargo clippy -p ferrodb-ui` (needs a check-mode rebuild of GPUI and `windows`, which ran out of memory) and `cargo build -p ferrodb-app` (the DuckDB C++ build did not finish within 50 minutes); `ferrodb-app/src/main.rs` is therefore not compiled on this machine — run `cargo check -p ferrodb-app` and `cargo clippy -p ferrodb-ui -p ferrodb-app --all-targets -- -D warnings` on a machine with more memory.

## Not done / deviations

- **No real certificates or keys here**: Authenticode, `codesign`, notarization, stapling and GPG are implemented in `xtask` and the workflow but **were not run**; the workflow was not executed. The macOS code-signature checks of downloaded drivers run only on macOS and were tested through their pure parts on Windows.
- **No crash endpoint and no update server exist**: both are build-time settings, empty by default. The workflow expects `FERRODB_UPDATE_URL` to point at a stable location; GitHub's `releases/latest/download/manifest.json` skips pre-releases, so a beta manifest must be published elsewhere (or the beta channel waits for the next stable release).
- **Own download code** instead of `ferrodb-driver-manager::download`: that one requires HTTPS (no loopback tests) and has no cancellation; the update download adds both, plus a size check against the signed manifest.
- **Installers**: Windows runs the NSIS installer with its UI (no silent mode); macOS opens the `.dmg` (the user drags the app); Linux replaces the AppImage only when run from one, `.deb` is opened with the system installer. Not exercised on a real installation.
- **`dismissed` state** is kept in `crashes/dismissed.txt` rather than in `CrashReport` (the v8 contract has no field for it); the contract itself is unchanged.
- **Not clicked through in the GUI by a person.** The update notification's View button and the status-bar button are wired but only the underlying functions are driven by the UI tests.
