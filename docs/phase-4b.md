# Phase 4b — MongoDB: status

Scope: plan, section 7, phase 4b. MongoDB runs on the real backend: driver, explorer, console language, execution with cursor streaming, editing, schema inference and the document TCK.

## Completion criteria

| Criterion (plan) | Status | Evidence |
|---|---|---|
| Document flows from phase 2 work on a real server | **Done (backend level)** | `ferrodb-core/src/tests_mongo.rs` runs the phase-2 document flows on MongoDB 8.0 through `CoreBackend`. This is the backend the UI talks to. The flows: script with a titled result, write counts, result node, first page of 500 then `fetch_more`, explorer database → collection → indexes, schema inference, field completion, collection script, editing through the operation preview, read-only refusal, risk warning, manual transaction with rollback, cancel from the UI handle, SSH tunnel. Not yet clicked through by a person in the GUI |
| 100k documents streamed | **Done** | TCK `streaming-100k`: 100 batches of ≤ 1000 raw documents. The first batch arrives after 87 ms; all 100,000 arrive in 122 ms. `backpressure-truncate`: a consumer that stops after 500 documents stops the cursor, and the connection stays usable |
| Field suggestion accuracy ≥ 90 % | **Done** | `mql/complete/tests.rs`: a corpus of 74 cursor positions (collections, methods, cursor modifiers, query / update / pipeline operators, field names at all depths, `$field` references in pipelines, helpers, multi-line and multi-statement consoles). The expected label must be among the first three suggestions. Result: **100 %** |
| BSON type round trip | **Done** | TCK `bson-types`: 14 types are printed in shell syntax, inserted through the console path, read back and compared for equality. The types: int32, int64 beyond 2^53, double, decimal128, UTF-8 string, bool, null, date with milliseconds, binary, UUID, regex, timestamp, array, nested document with MinKey/MaxKey. The parser/printer round trip is also unit-tested |

### Document TCK (MongoDB 8.0.34, replica set `rs0`)

| Scenario | Result | Detail |
|---|---|---|
| connect / descriptor / ping | pass | Document model, MQL, multi-statement transactions |
| bson-types | pass | 14 types |
| writes-affected | pass | insertMany 3 / updateMany 2 / deleteOne 1, `insertedIds` |
| streaming-100k | pass | 122 ms |
| backpressure-truncate | pass | stopped after 500 |
| cancel | pass | 13 ms from cancel to `Cancelled`. The server operation (`$where` sleep) is killed (`currentOp` + `killOp` by `comment` tag) |
| errors | pass | duplicate key `11000 DuplicateKey`; a parse error with a position; an unknown command |
| transactions | pass | rollback and commit |
| database-switch | pass | `use admin` and back |
| listings | pass | `show dbs`, `show collections`, `getIndexes` |

The SQL TCK (SQLite, PostgreSQL also through SSH, MySQL, MariaDB) still passes.

## What was built

| Plan item | Where | Notes |
|---|---|---|
| `mongodb` 3.10 driver | `ferrodb-drivers/src/mongodb/` | Feature `mongodb`; rustls (`ring`), SRV resolver; bson 3 |
| URI, replica set, sharding, read preference | `conn.rs` (`connection_string`) | `mongodb://` / `mongodb+srv://` from the form, or individual fields: user, password, `authSource`, mechanism, replica set, read preference. Topology from `hello` (replica set name or `mongos`) |
| TLS, SCRAM/X.509 | `connection_string` | The source's TLS mode maps to `tls` / `tlsAllowInvalidCertificates`. CA file and client certificate are used as given. A certificate without a user means `MONGODB-X509` |
| SSH | `MongoConnection::connect` | One host through the tunnel with `directConnection`. A seed list or SRV is refused with an explanation. Tested through the TCK bastion |
| Introspection: databases, collections, views, time series, indexes, validators | `ferrodb-meta/src/mongo.rs` | `listDatabases`, `listCollections`, `listIndexes`. The connection's database comes first; `system.*` collections are hidden. The explorer shows database → collections / views → indexes |
| Schema inference | `ferrodb-meta/src/infer.rs`, `MetadataService::infer_schema` | ADR-011. `$sample` of 1000; paths, types with shares, presence, samples. A `$jsonSchema` validator comes first |
| Console language | `ferrodb-lang/src/mql/` | ADR-010. Lexer, relaxed-JSON values with helpers, statement parser, splitter (chains across lines, `// title`), read/write classification, risks, diagnostics with positions, completion, formatter (idempotent; statements with comments are left as they are) |
| Execution with cursor streaming | `mongodb/exec.rs`, `ferrodb-core/src/session.rs` | Every statement runs as a database command. Cursors are read as raw BSON and sent in batches over the bounded sink (backpressure). Results reach the UI as `ResultShape::Documents` with the collection's node. The core pages documents like rows (500, then `fetch_more`) |
| Cancellation | `MongoConnection::execute`, `RemoteCancel` | Every statement carries a `comment` tag. Cancel drops the cursor and runs `killOp` for the tagged operations |
| Write operation results | `exec.rs` | As mongosh prints them: `insertedId(s)`, `matchedCount` / `modifiedCount` / `upsertedId`, `deletedCount`. The affected count is reported to the UI. `_id`s are generated client-side for inserts |
| Read-only mode, warnings | core `run_mql`, `mql::risks` | The core refuses writes before sending them: `$out`/`$merge` pipelines, write commands in `runCommand`. MongoDB has no read-only sessions. Warnings: `deleteMany({})`, `updateMany({}, …)`, `drop`, `dropDatabase`, `dropIndex(es)`, and every write on production sources |
| Transactions | `MongoConnection::begin/commit/rollback` | A `ClientSession` with a transaction; every command and cursor of the session runs in it. On a standalone server, manual mode reports why and continues in auto-commit |
| Document editing | `ferrodb-core/src/documents.rs` | The `_id`s of streamed documents are kept per result. `SetField` → `updateOne({_id}, {$set})`, `DeleteDocument` → `deleteOne({_id})`, shown in the operation preview. All changes run in one transaction when the server supports it |
| Indexes, collection script | `mongo::collection_script`, explorer "Copy Script" / "Generate …" | `db.createCollection(name, options with validator)` / `db.createView(…)` plus `createIndex` per index. The database script recreates the database: a round-trip test checks the same collections, options, validators and indexes. Snippets: `find`, `insertOne` (with known fields), `updateOne`, `deleteOne`, `drop` |
| TCK for the document model | `ferrodb-driver-tck/src/documents.rs`, `tests/tck.rs` | `run_documents`; `FERRODB_TCK_MONGODB` |
| Test infrastructure | `xtask/src/db.rs` | `mongo:8.0` container with `--replSet rs0`, `rs.initiate()` on `db-up`. `cargo xtask tck` also runs the metadata round trips |

## UI changes

- The document view takes the collection's node from the result. Schema inference and indexes therefore use the right database.
- Explorer for document sources: "Copy Script" (database, collection, view, index), "Generate find() / INSERT / UPDATE / DELETE / DROP", "Quick Documentation".
- MQL console: completion from the cached catalog and sampled fields, diagnostics, formatting, risk confirmation. These come through the existing console code paths.

## Not done / deviations

- **Not clicked through in the GUI by a person.** The flows are covered at the backend level on a real server, and by the UI tests on the mock.
- **Explain** (`explain`) is phase 7. `bulkWrite`, JavaScript (variables, loops, `forEach`) and `mapReduce` are rejected with a message (ADR-010).
- **Server-side read-only:** not possible in MongoDB; the core enforces it.
- **X.509, SCRAM-SHA-1 and SRV** were not run against a server (the container has no TLS and no authentication). PLAIN/LDAP and Kerberos are not offered.
- **Editing other change kinds** (insert, whole-document replace, array element insert) arrives with the data editor in phase 6.
- **Inferred schemas are not cached on disk** (ADR-011).

## Test runs (2026-11-10, WSL Ubuntu, Docker containers from `cargo xtask db-up`)

- Non-GUI crates (`api`, `lang`, `meta`, `drivers`, `core`, `driver-tck`, `net`, `storage`, `secrets`) with every `FERRODB_TCK_*` variable set: all pass. This includes the SQL TCK on four databases plus PostgreSQL through SSH, the document TCK, the DDL round trips and the core MongoDB flows.
- GUI crates (`ui`, `mock`, `app`, `gallery`): `clippy --tests` is clean, and all tests pass: 27 lib, 23 flows, 5 UI, 64 mock.
- `cargo fmt --all` is applied.
