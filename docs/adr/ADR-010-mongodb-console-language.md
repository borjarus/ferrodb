# ADR-010: MongoDB console language — supported subset of shell syntax

- Status: **Accepted**
- Date: 2026-11-10

## Context

The MongoDB console must accept what users paste from mongosh, docs and Stack Overflow. Running real JavaScript (an embedded engine such as QuickJS or a `mongosh` process) gives full compatibility. It also brings a second runtime and a large dependency, and it makes statements opaque to the IDE. Read-only checks, warnings, completion and the operation preview all need to know what a statement does before it runs.

## Decision

- The console language is a **parsed subset of the mongosh syntax** (`ferrodb-lang/src/mql`). It is not JavaScript.
  - Statements: `use <db>`, `show dbs|collections`, `db.<method>(…)`, `db.<coll>.<method>(…).<modifier>(…)…`.
  - Collections are addressed as `db.a.b`, `db["x"]`, `db.getCollection("x")` and `db.getSiblingDB("d")`.
  - Methods:
    - reads: `find`, `findOne`, `aggregate`, `countDocuments`, `estimatedDocumentCount`, `distinct`;
    - writes: `insertOne/Many`, `updateOne/Many`, `replaceOne`, `deleteOne/Many`, `findOneAnd*`;
    - indexes: `createIndex(es)`, `dropIndex(es)`, `getIndexes`;
    - collections and databases: `drop`, `renameCollection`, `createCollection`, `createView`, `dropDatabase`, `getCollectionInfos/Names`;
    - commands: `runCommand`, `adminCommand`.
  - Legacy forms are accepted: `insert`, `update`, `remove`, `count`, `ensureIndex`.
  - Cursor modifiers: `sort`, `limit`, `skip`, `projection`, `hint`, `maxTimeMS`, `batchSize`, `collation`, `comment`, `count`, `toArray`, `pretty`.
- **Values** are relaxed JSON:
  - unquoted keys, single quotes, trailing commas, comments, regex literals;
  - helpers: `ObjectId`, `ISODate`/`new Date`, `NumberDecimal`, `NumberLong`, `NumberInt`, `Double`, `Timestamp`, `UUID`, `BinData`, `HexData`, `MinKey`, `MaxKey`, `RegExp`;
  - integer literals in the `int32` range become `Int32`, other numbers become `Double`, as in mongosh.
- Statements are separated by `;` or by a line break outside brackets, unless the next line continues the method chain (`.sort(…)`). A `// comment` on the line above names the result tab.
- Each statement becomes a `Command` (BSON arguments). The driver runs it as a database command and reads cursors as raw BSON.
- Anything else is a parse error with a position: variables, loops, functions, `bulkWrite`, `forEach`, `explain` (phase 7).

## Consequences

- Read-only mode, warnings (`deleteMany({})`, `updateMany({}, …)`, `drop`), completion, formatting and editing all work on the same parsed form.
- Scripts that rely on JavaScript (loops, variables) do not run. The plan lists "full JavaScript scripts like in mongosh" as *Later*. An embedded engine can be added then for statements this parser rejects.
- The printer writes values back in shell syntax (`ObjectId("…")`, `ISODate("…")`, `NumberLong("…")`). Generated scripts, the operation preview and collection scripts can therefore be pasted back into the console. The value tests check the print → parse round trip.
