# ADR-002: Async runtime and UI ↔ core bridge

- Status: **Accepted**
- Date: 2026-11-05

## Context

GPUI runs its own foreground executor on the UI thread. Database drivers are a mix:
- some are tokio-based (`tokio-postgres`, `mongodb`, `redis`, `scylla`);
- some are blocking (`rusqlite`, `duckdb`, the ADBC C API).

The UI must never block, and every operation must be cancellable.

## Decision

- **The contract (`ferrodb-api`) does not depend on any runtime.**
  - Request/response operations return `futures::future::BoxFuture<'_, Result<T>>`.
  - Streams are delivered as `async_channel::Receiver<QueryEvent>`.
  - Cancellation uses a shared `CancelHandle` (an atomic flag). Executors map it onto server-side mechanisms such as a cancel request, `KILL QUERY` or `killOp`.
- **The core (phase 3) owns a multi-threaded `tokio::runtime::Runtime`.**
  - Futures it hands to the UI complete through channels or oneshots, so the UI can await them on GPUI's executor (`cx.spawn`, `cx.spawn_in`) without knowing tokio exists.
  - Blocking drivers run on dedicated threads.
- **Backpressure:** result channels are bounded (64 events). Producers coalesce items into chunks of at most 1000, and the default first page is 500 items. After the first page, a result pauses (`ResultPaused { has_more }`) until `fetch_more` is called.
- `QueryEvent::Finished` marks the end of the *script*. Later `fetch_more` calls keep delivering `Data` events on the same receiver.

## Evidence

`ferrodb-mock` implements the full contract using plain threads and `async-channel`. The UI consumes it on GPUI's executor: the console panel and the explorer subscriptions use it today. Tests cover streaming, paging past 1M rows, cancellation, errors with byte ranges, and read-only rejection (`crates/ferrodb-mock/src/lib.rs`).

## Consequences

- Futures are boxed, which costs an allocation per call. That is negligible next to network round-trips.
- Executors must keep sending on the bounded channel from background threads only, never from the UI thread.
