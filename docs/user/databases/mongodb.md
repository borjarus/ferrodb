# MongoDB

Document database, MQL in mongosh shell syntax, reached by a **native (N)** driver (`mongodb` 3.x, rustls); language intelligence **level A (MQL)**.

## Requirements

| What | Details |
|---|---|
| Tested | MongoDB 8.0.34, replica set `rs0` (container `mongo:8.0 --replSet rs0` of `cargo xtask db-up`): document TCK passed (`docs/phase-4b.md`) |
| Download | Nothing: the driver is built into FerroDb |
| Network | Port **27017**; `mongodb+srv://` needs DNS SRV / TXT lookups |

## Connecting

Either a connection string or the individual fields (fields are added to the URI as parameters):

| Field | Default | Notes |
|---|---|---|
| Connection string | — | Must start with `mongodb://` or `mongodb+srv://`; user / password from the form are inserted when the URI has none |
| Host | `localhost` | `host`, `host:port`, or a seed list `a:27017,b:27017` |
| Port | `27017` | |
| Database | `test` | Default database of the console |
| User / Password | — | |
| Authentication database | — | `authSource` |
| Mechanism | — | `authMechanism`, e.g. `SCRAM-SHA-256`, `SCRAM-SHA-1` |
| Client certificate + key (PEM) | — | `tlsCertificateKeyFile` (X.509) |
| Replica set | — | `replicaSet` |
| Read preference | — | `primary`, `secondaryPreferred`… |

The topology (replica set name or `mongos`) is read from `hello`. FerroDb adds `appName=FerroDb` and the connect timeouts.

**TLS** (SSL tab): `disable` → `tls=false`; `prefer` → TLS only when a client certificate or a CA file is set; `require` → TLS without certificate verification (`tlsAllowInvalidCertificates`); `verify-full` → TLS with verification. A CA bundle (`ssl.ca` in the data source configuration) becomes `tlsCAFile`.

**SSH tunnel**: one host only, connected with `directConnection`. A seed list or a `mongodb+srv://` URI is refused through SSH.

## Authentication

| Method | How |
|---|---|
| None | No credentials |
| Password (SCRAM) | User, Password, optional authentication database and mechanism (`SCRAM-SHA-256` is the server default) |
| X.509 | Client certificate + key file; without a user, `authMechanism=MONGODB-X509` is set automatically |

The mechanism field is passed through as typed. Kerberos (GSSAPI) is not offered.

## What works

- **Explorer**: databases → collections, views and time-series collections → indexes; validators. The connection's database comes first; `system.*` collections are hidden.
- **Console** (level A): `use`, `show dbs`, `show collections`, `db.coll.find / findOne / aggregate / countDocuments / distinct / insertOne / insertMany / updateOne / updateMany / replaceOne / deleteOne / deleteMany / findOneAndUpdate…`, `createIndex`, `dropIndex`, `getIndexes`, `drop`, `renameCollection`, `db.createCollection`, `db.createView`, `db.runCommand`, cursor modifiers (`sort`, `limit`, `skip`, `projection`, `hint`, `maxTimeMS`…). Completion of collections, methods, operators and field names from the inferred schema; diagnostics with positions; formatter. `// title` comments name result tabs.
- **Schema inference**: `$sample` of 1000 documents (a `$jsonSchema` validator is used first).
- **Results**: documents as tree, JSON and table (flattened fields), streamed as raw BSON in batches (first 500, then more on demand). BSON types round-trip exactly (decimal128, int64, dates, UUID, binary, regex, timestamp…).
- **Write results**: `insertedId(s)`, `matchedCount` / `modifiedCount` / `upsertedId`, `deletedCount`, as mongosh prints them.
- **Cancel**: drops the cursor and kills the server operation (`killOp` by the statement's `comment` tag).
- **Editing**: field edits → `updateOne({_id}, {$set})`, deletes → `deleteOne({_id})`, shown in the operation preview and applied in one transaction when the server supports it.
- **Definitions**: collection / view / database scripts (`db.createCollection` with options and validator, `db.createView`, `createIndex` per index); snippets for `find`, `insertOne`, `updateOne`, `deleteOne`, `drop`.
- **Transactions**: Manual mode runs statements in a session transaction on replica sets and sharded clusters.
- **Execution plan**: `explain` of `find`, `aggregate`, `countDocuments`, `distinct` (`queryPlanner` / `executionStats`), with `COLLSCAN` and in-memory sort warnings.
- **Warnings / read-only**: `deleteMany({})`, `updateMany({}, …)`, `drop`, `dropDatabase`, `dropIndex(es)` ask for confirmation; read-only sources refuse writes, `$out` / `$merge` pipelines and write commands inside `runCommand` before sending them.
- **Export / import**: all export formats including Extended JSON; imports use `insertMany` in batches of 1000.

## Limitations

- **JavaScript is not executed**: variables, loops, `forEach`, `mapReduce` and `bulkWrite` are rejected with a message. No visual aggregation builder.
- **Standalone servers have no transactions**: Manual mode reports why and continues in auto-commit.
- **Read-only** is enforced by FerroDb, not by the server (MongoDB has no read-only sessions).
- **X.509, SCRAM-SHA-1 and SRV** were not run against a server; PLAIN / LDAP and Kerberos are not supported.
- Inferred schemas are not cached on disk.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "the connection string must start with mongodb:// or mongodb+srv://" | Fix the scheme |
| "authentication failed: …" | Check user / password and the **Authentication database** (often `admin`) |
| Server selection timeout on a replica set | The members' advertised host names are not reachable from here; connect to one host with `directConnection=true` in the URI |
| SSH: seed list / SRV refused | Use a single host through the tunnel |
| Certificate errors | Use `verify-full` with the right CA, or `require` to skip verification |
| Transaction errors on a single server | Start it as a replica set (`--replSet`) to use Manual mode |
