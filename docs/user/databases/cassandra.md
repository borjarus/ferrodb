# Apache Cassandra

Wide-column database (tabular results), CQL, reached by a **native (N)** driver (`scylla` 1.x, compatible with Apache Cassandra) over the CQL native protocol; language intelligence **level B**.

## Requirements

| What | Details |
|---|---|
| Tested | `cassandra:5` (5.0.9): driver TCK passed; transactions, listing and affected-rows scenarios are skipped by capability |
| Download | Nothing: the driver is built into FerroDb |
| Network | CQL native protocol port **9042**; the driver connects to every node of the cluster it discovers |

## Connecting

| Field | Default | Notes |
|---|---|---|
| Contact points | `localhost` | `host[:port], …` |
| Port | `9042` | For contact points without a port |
| Keyspace | — | `USE` at connect time |
| User / Password | — | Password authentication |
| Local data center | — | Data-center-aware routing |
| Consistency | `LOCAL_ONE` | `ONE`, `LOCAL_ONE`, `QUORUM`, `LOCAL_QUORUM`, `ALL`… |
| Serial consistency | — | `SERIAL`, `LOCAL_SERIAL` (lightweight transactions) |
| Address translation | `none` | `contact_point`: send every node's traffic to the first contact point (single node behind NAT, Docker port mappings) |

URL shape: `cassandra://{host}:{port}/{database}`.

- **TLS**: not available (see Limitations); SSL mode `require` / `verify-full` is refused, `disable` / `prefer` connect without TLS.
- **SSH tunnel**: not supported ("SSH tunnels are not supported for Cassandra (the driver connects to every node)"). Use `address_translation = contact_point` with a port forward to a single node instead.

## Authentication

| Method | Fields |
|---|---|
| None | — |
| Password | **User**, **Password** (keyring); Cassandra's `PasswordAuthenticator` |

## What works

- **Explorer**: keyspaces (with replication) → tables, materialized views, functions / aggregates / user-defined types → columns, primary key (partition key then clustering columns with their order, static columns marked) and indexes (secondary and SAI), all from `system_schema`.
- **Console**: CQL (level B) on the SQL engine with the `cql` dialect file; `BEGIN BATCH … APPLY BATCH` is one statement; cqlsh commands `USE`, `CONSISTENCY [level]`, `SERIAL CONSISTENCY [level]` change the console's session; partition-key-first suggestions in `WHERE`.
- **Warnings**: `ALLOW FILTERING` gets a warning before the statement runs; `DROP`, `TRUNCATE` and `DELETE` without a restriction ask for confirmation. Server warnings arrive as notices.
- **Results**: grid; pages fetched with the server's paging state, one page per batch, so a paused result stops fetching. Every CQL type is mapped; lightweight transactions show the `[applied]` column.
- **Editing**: rows are edited by their full primary key with the operation preview.
- **Definitions**: generated `CREATE TABLE … PRIMARY KEY ((pk), ck) WITH CLUSTERING ORDER BY …`, `CREATE TYPE` for UDTs.
- **Read-only mode**: the connection refuses statements that are not reads.
- **Export**: all result export formats.

## Limitations

- **No TLS**: the CQL driver's TLS support needs `aws-lc-rs`, which FerroDb does not build; encrypted clusters cannot be reached.
- **No SSH tunnel** (see above).
- **No interactive transactions**: Manual mode is refused; a batch is one statement.
- **No joins** and **no execution plans**.
- **No server-side cancel**: cancelling abandons the request on the client; the server finishes it.
- Affected-row counts are not reported (Cassandra does not return them).

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "TLS to Cassandra is not available in this build …" | Set SSL mode to `disable` / `prefer` — only unencrypted clusters are supported |
| Connects, then times out on queries / unreachable node addresses | Nodes advertise addresses this machine cannot reach: set **Address translation** to `contact_point` |
| "consistency: unknown level …" | Use a valid level name (`LOCAL_ONE`, `QUORUM`…) |
| "keyspace …: …" at connect | The keyspace in **Keyspace** does not exist or the user cannot use it |
| Queries need `ALLOW FILTERING` | The `WHERE` clause does not restrict the partition key; add it, or accept the full-scan warning |
