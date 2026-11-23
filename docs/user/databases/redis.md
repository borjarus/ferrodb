# Redis

Key-value database, Redis commands, reached by a **native (N)** driver (`redis` 1.x on tokio, rustls / ring); language intelligence **level B (commands)**. Standalone, Cluster and Sentinel.

## Requirements

| What | Details |
|---|---|
| Tested | `redis:8` (8.10.3): key-value TCK 13/13 on a standalone server, a 3-master Cluster and a Sentinel setup (`docs/phase-8.md`) |
| Download | Nothing: the driver is built into FerroDb |
| Network | Port **6379** (servers, cluster nodes), **26379** (Sentinels) |

## Connecting

| Field | Default | Notes |
|---|---|---|
| Host | `localhost` | Standalone: one host. Cluster seeds / Sentinels: `host:port, …` |
| Port | `6379` | Default for hosts without a port (26379 for Sentinels) |
| Database index | `0` | |
| User (ACL) / Password | — | |
| Mode | `standalone` | `standalone`, `cluster`, `sentinel` |
| Sentinel master name | — | Required in `sentinel` mode |
| Sentinel user / Sentinel password | — | Credentials of the Sentinels themselves |
| Protocol | `resp3` | `resp3` or `resp2` |
| Client certificate (PEM) / Client key (PEM) | — | Mutual TLS |

URL shape: `redis://{host}:{port}/{database}`.

**TLS** (SSL tab): `disable` → plain TCP; `prefer` → plain unless a CA bundle or client certificate is set, then verified TLS; `require` → TLS without certificate verification; `verify-full` → verified TLS. A CA bundle can be given as `ssl.ca` in the data source configuration.

**SSH tunnel**: standalone mode only (Cluster and Sentinel are refused through SSH). Through a tunnel the TLS certificate is not verified, because it names the real server.

## Authentication

| Method | Fields |
|---|---|
| None | — |
| Password | **Password** alone (`requirepass`), or **User (ACL)** + **Password** |
| Sentinel | Separate **Sentinel user / password** for the Sentinels; the master uses the main user / password |
| Mutual TLS | Client certificate + key (with any of the above) |

## What works

- **Key browser**: databases (`INFO keyspace`, `CONFIG GET databases`) → key prefixes split on `:` → keys, listed live with a paged `SCAN` (across every master in a Cluster). Views per type (string, hash, list, set, sorted set, stream, JSON), TTL shown and editable, rename, delete, add / change / remove members, all with an operation preview.
- **Console**: one command per line, arguments parsed like `redis-cli`; a catalog of Redis 7/8 core commands plus RedisJSON and RediSearch basics with completion of commands, subcommands, options and key names, signature help, hover, arity diagnostics, formatter (level B). `SCAN … MATCH …` and `KEYS …` results open as key lists in the browser.
- **Results**: RESP3 replies rendered as tables.
- **Transactions**: `MULTI … EXEC` runs as one unit (in a Cluster as one pipeline); no interactive transactions.
- **Cancel**: blocking commands (`BLPOP`, `XREAD BLOCK`…) are released with `CLIENT UNBLOCK` from a second connection; other commands are abandoned and the connection reopened.
- **Read-only / warnings**: read-only sources refuse write commands (classified from the command table); dangerous commands (`FLUSHALL`, `FLUSHDB`, `CONFIG SET`…) ask for confirmation.
- **Cluster**: slot routing by the client; `SELECT` and databases other than 0 are refused.
- **Export**: console results can be exported like other results.

## Limitations

- **Pub/Sub and `MONITOR`** are not available in the console ("… in the console (it never returns)").
- **No interactive transactions**, no execution plans, no joins.
- **Cluster has only database 0**.
- **SSH** works for a single standalone server only.
- Key listings are not cached; very large keyspaces are browsed page by page.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `NOAUTH` / `WRONGPASS` | Give the password (and the ACL user for Redis 6+ ACLs) |
| "Sentinel: the master name (sentinel.master) is required" | Fill in **Sentinel master name** |
| "Sentinel gave host:port for …, but it is a replica" | The Sentinels have not finished a failover; retry |
| "Redis Cluster has only database 0" | Set **Database index** to 0 in cluster mode |
| "SSH tunnels reach one Redis server …" | Use standalone mode through SSH, or connect directly |
| `MOVED` errors | The server is a cluster node: set **Mode** to `cluster` |
| "TLS file …: …" | The certificate or key path is wrong |
