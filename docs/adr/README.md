# Architecture Decision Records

Format: context → decision → consequences, plus evidence where a spike measured something.
Status values: *Proposed*, *Accepted*, *Superseded by ADR-xxx*.

| No. | Title | Status |
|---|---|---|
| [ADR-001](ADR-001-gui-library.md) | GUI library: GPUI + GPUI Kit (spike results, go/no-go) | Accepted (provisional, pending release-mode runs on macOS/Linux) |
| [ADR-002](ADR-002-async-runtime-and-ui-bridge.md) | Async runtime and UI ↔ core bridge | Accepted |
| [ADR-003](ADR-003-data-models-and-capabilities.md) | Data models and `Capabilities` | Accepted |
| [ADR-004](ADR-004-result-formats.md) | Result formats: Arrow, raw BSON, keys; disk spill | Accepted |
| [ADR-005](ADR-005-access-layers.md) | Access layers: native, ADBC, JDBC | Accepted |
| [ADR-006](ADR-006-jdbc-bridge.md) | JDBC bridge: separate process, JSON-RPC + Arrow IPC | Proposed (Java-side measurement pending) |
| [ADR-007](ADR-007-driver-manager.md) | Driver manager: sources, verification, licenses, offline | Accepted |
| [ADR-008](ADR-008-driver-families.md) | Driver families and database variants | Accepted |
| [ADR-010](ADR-010-mongodb-console-language.md) | MongoDB console language: supported subset of shell syntax | Accepted |
| [ADR-011](ADR-011-schema-inference.md) | Schema inference (MongoDB; Couchbase and DynamoDB in phase 8) | Accepted |
| [ADR-012](ADR-012-driver-tck-and-test-matrix.md) | Driver compatibility test suite (TCK) and test matrix | Accepted |
| [ADR-013](ADR-013-configuration-and-secrets.md) | Data source configuration and secret storage | Accepted |

ADR-009 and ADR-014…015 (plan, Appendix C) are written when their phases start.
