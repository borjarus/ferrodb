# ADR-004: Result formats — Arrow, raw BSON, keys; disk spill

- Status: **Accepted**
- Date: 2026-11-05

## Decision

| Model | In-memory format | Source |
|---|---|---|
| Tabular | `Arc<arrow_array::RecordBatch>` + `Arc<arrow_schema::Schema>` | Native drivers map types to Arrow. ADBC returns Arrow natively. The JDBC bridge sends Arrow IPC. |
| Document | `Arc<Vec<bson::RawDocumentBuf>>`, decoded lazily | MongoDB natively. Couchbase JSON and DynamoDB attributes are mapped to BSON, preserving numbers, sets and binaries. |
| Key-value | `Arc<Vec<KeyEntry>>` (key, type, TTL, size); values loaded on demand | Redis `SCAN` |

- Decimal values never pass through `f64` (`Decimal128/256` in Arrow, `Decimal128` in BSON).
- **Disk spill (phase 2/6):** each result has a memory budget. Batches over the budget go to Arrow IPC files in the data directory and are memory-mapped back on demand.
- Chunks are reference-counted. Views, export and diff share them without copying.

## Evidence

- Mock generators produce deterministic Arrow batches and BSON documents (`ferrodb_mock::data`). A 5 MB document is generated in a single `RawDocumentBuf`.
- The GUI spike renders a 1M × 50 grid by formatting cells straight from cached `RecordBatch` pages (`spikes/gui`).
- The JDBC-reader spike reads 1M rows of Arrow IPC at 1.37M rows/s with flat memory (RSS 5 → 9 MB) (ADR-006).
