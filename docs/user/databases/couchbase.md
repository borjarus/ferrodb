# Couchbase

Document database, SQL++ (N1QL), reached by a **native (N)** driver that talks to Couchbase's REST services (query service and cluster manager) over HTTP; language intelligence **level B**.

## Requirements

| What | Details |
|---|---|
| Tested | Couchbase Server Community 8.0.2 (container `couchbase/server:community`): document TCK 13/13 |
| Server version | SQL++ transactions (Manual mode) need Couchbase Server **7.0+**; scopes and collections are listed as in 7.x |
| Download | Nothing: the driver is built into FerroDb |
| Network | Query service **8093** (TLS **18093**) and cluster manager **8091** (TLS **18091**). The Data Service port 11210 is not used |

## Connecting

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | |
| Query service port | `8093` | `18093` with TLS |
| Cluster manager port | `8091` | `18091` with TLS |
| Bucket (query context) | — | Becomes the console's `query_context` |
| Scope (query context) | — | With the bucket: `bucket.scope` |
| User / Password | — | |
| Client certificate + key (PEM) | — | X.509 |
| Scan consistency | `not_bounded` | `request_plus` waits for indexes to catch up |

URL shape: `couchbase://{host}/{database}`.

- **TLS** (HTTPS) is used when the SSL mode is `require` or `verify-full`, or when a client certificate is set; the default ports switch to 18093 / 18091.
- **SSH tunnel**: supported; FerroDb opens one tunnel per service (query and cluster manager).

## Authentication

| Method | Fields |
|---|---|
| Password | **User**, **Password** (HTTP basic authentication) |
| X.509 | **Client certificate + key (PEM)** over TLS |

## What works

- **Explorer**: buckets → scopes → collections → GSI indexes (from `system:indexes`, with the server's `CREATE INDEX` statement).
- **Console**: SQL++ (level B): keyspace-path completion (buckets, scopes, collections), fields of the documents behind each alias (`UNNEST` aliases included), `USE KEYS`, `NEST` / `UNNEST`, `INFER`, parameters `$name` / `$1` / `?`, diagnostics, formatter. `DELETE` / `UPDATE` without `WHERE` ask for confirmation.
- **Schema inference**: `INFER` on the collection.
- **Results**: documents (tree, JSON, table); the JSON response is read incrementally and each result element becomes a document as soon as its bytes arrive (backpressure down to the socket). Errors carry their code and position; warnings and metrics arrive as notices.
- **Cancel**: closes the response and deletes the request from `system:active_requests`.
- **Editing**: `UPDATE … USE KEYS`, `UPSERT`, `DELETE` by document key, shown in the operation preview and applied in one transaction (`BEGIN WORK`).
- **Definitions**: `CREATE SCOPE`, `CREATE COLLECTION`, `CREATE INDEX` scripts and SQL++ snippets.
- **Transactions**: Manual mode with `BEGIN WORK` / `COMMIT` / `ROLLBACK` (the `txid` is carried by every statement).
- **Execution plan**: `EXPLAIN`, shown as tree / table / diagram.
- **Read-only mode**: FerroDb refuses writes before sending them.
- **Export**: all result export formats.

## Limitations

- **Profiling** (`profile = timings`) needs Couchbase Enterprise Edition.
- **Copying data into Couchbase** with the transfer wizard is not done (Couchbase can be a source, not a target).
- **No "Copy Script" menu for scopes** in the explorer yet.
- **KV operations** go through the query service (`USE KEYS`, `UPSERT INTO … (KEY, VALUE)`), not the binary KV protocol.
- Transactions on servers older than 7.0 are refused ("SQL++ transactions need Couchbase Server 7.0+").

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "authentication failed" | Wrong user / password, or the user has no query permissions on the bucket |
| "timed out connecting to Couchbase" | Check both ports (query 8093 and cluster manager 8091, or their TLS ports) |
| Collections not found by name | Set **Bucket** / **Scope** (query context) or use full `bucket.scope.collection` paths |
| Just-written documents missing from a query | Set **Scan consistency** to `request_plus` |
| `No index available on keyspace …` | Create a primary or secondary index (or query with `USE KEYS`) |
