# ADR-006: JDBC bridge — separate process, JSON-RPC + Arrow IPC

- Status: **Proposed**. The Rust side is measured. The Java side still has to be measured on a machine with Java 21+ and Gradle.
- Date: 2026-11-05

## Options considered

| Option | Pros | Cons |
|---|---|---|
| **Separate Java process** (chosen) | A crash or out-of-memory error in a vendor driver cannot take down the IDE; restart is automatic. Java versions are isolated. No JNI in the address space. | Process startup cost (target < 2 s for the first connection); one extra hop |
| JNI (embedded JVM) | No IPC | A JVM crash kills the IDE; JNI is fragile; signal-handling conflicts; harder packaging |
| ODBC | No Java | Poor driver availability for these 9 databases; level C only |

## Decision

- **`ferrodb-jdbc-host`** is a small Java LTS application. It starts on the first connection to a JDBC database and stops after an idle period.
- **Transport:** a loopback socket. The host connects *back* to a port the IDE is listening on, then authenticates with a one-time token passed in an **environment variable**. The token is never passed as a process argument, because other users can read process arguments.
- **Protocol:**
  - Control messages use JSON-RPC: connect, execute, cancel, metadata, transactions.
  - Result data is an **Arrow IPC stream**, produced on the Java side by Apache Arrow's JDBC adapter (`JdbcToArrow`, batches of 4096 rows, reused `VectorSchemaRoot`).
- **Java runtime:** a `jlink`-minimized runtime, downloaded by the driver manager (ADR-007). JVM flag: `--add-opens=java.base/java.nio=ALL-UNNAMED`, which Arrow memory requires.
- **Resilience:** if the process dies, the core marks the affected sessions as disconnected and restarts the host. Mock scenario "JDBC process failure" exercises the UI for this case.

## Evidence (spike, `spikes/jdbc-bridge`)

- **Rust reader:** `cargo xtask spike jdbc-reader -- --self-test --rows 1000000`. An in-process producer writes the same wire format over loopback TCP. Windows 11, dev profile:
  - **1,000,000 rows in 245 batches (5 columns): 0.73 s → 1.37M rows/s**
  - first batch after 0.7 ms
  - RSS 5.2 MB before, 9.0 MB peak, so memory stays **constant**
  - → The Rust side is not the bottleneck; the go threshold is ≥ 200k rows/s.
- **Java host:** `spikes/jdbc-bridge/java`. It builds with `gradle fatJar`, runs H2 `SYSTEM_RANGE(1, N)`, and streams through `JdbcToArrow` → `ArrowStreamWriter`.
  - **Not run in this environment:** only Java 8 is installed, and there is no Gradle.
  - To run end to end: `cargo xtask spike jdbc-reader -- --jar spikes/jdbc-bridge/java/build/libs/jdbc-host-spike-all.jar --rows 1000000`. Record the result in `docs/spike-results.md`.

## Go / no-go

- **GO** if the end-to-end run reaches ≥ 200k rows/s with constant memory and the host is ready in < 2 s.
- **Otherwise:** revisit the batch size and compression. JNI is the last resort.
