# ADR-011: Schema inference for document databases

- Status: **Accepted** (MongoDB; Couchbase and DynamoDB follow in phase 8)
- Date: 2026-11-10

## Context

Document collections have no declared schema. The schema panel, field completion, the table view of documents and later export mappings all need field paths and types.

## Decision

- **Sampling.** MongoDB is sampled with `$sample` (1000 documents by default, `SampleOptions`). The sample runs on the source's explorer connection, never on the UI thread.
- **Statistics** (`ferrodb-meta/src/infer.rs`):
  - one entry per dotted path (`address.city`). Elements of arrays of documents contribute to the array's path (`items.sku`), as in Compass;
  - BSON type names in `$type` spelling, each with its share of the values;
  - presence: the share of sampled documents that contain the path;
  - up to three distinct sample values, shown in shell syntax;
  - nesting is followed to depth 8 and to the first 100 elements of an array.
- **Validators take precedence.** Properties of a `$jsonSchema` validator come first, with their declared `bsonType`. Required fields get presence 1.0. Sampled statistics fill in the rest.
- **Cache.** The core keeps the field paths of every sampled collection in memory. MQL completion uses them.
  - The first completion that needs the fields of an unsampled collection starts a sample in the background.
  - The suggestions then show the fields from the next keystroke on.
- Inferred schemas are not written to the disk cache. They change with the data and are cheap to compute again.

## Consequences

- Completion offers real field names without blocking the editor. Measured with the corpus in `mql/complete/tests.rs`: ≥ 90 % accuracy.
- Large collections cost one `$sample` aggregation per session and collection.
- DynamoDB sampling (`Scan` with a limit) has a read cost, so the UI will show a warning there (phase 8).
