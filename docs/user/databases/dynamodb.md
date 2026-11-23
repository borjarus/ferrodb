# Amazon DynamoDB

Document (key-value) database, PartiQL plus DynamoDB API operations, reached by a **native (N)** driver (`aws-sdk-dynamodb`, rustls + ring); language intelligence **level B**.

## Requirements

| What | Details |
|---|---|
| Tested | DynamoDB Local (`amazon/dynamodb-local:latest`, in memory, shared DB): document TCK 13/13. Real AWS DynamoDB was not tested (`docs/phase-8.md`) |
| Download | Nothing: the driver is built into FerroDb |
| Network | HTTPS to the regional endpoint (`dynamodb.<region>.amazonaws.com`), or the endpoint you give |
| Emulator | DynamoDB Local, e.g. `docker run -p 8000:8000 amazon/dynamodb-local` → endpoint `http://localhost:8000` |

## Connecting

| Field | Default | Notes |
|---|---|---|
| Region | `eu-central-1` | Required; `us-east-1` is used when neither region field is set |
| Endpoint | — | Endpoint override, e.g. `http://localhost:8000` for DynamoDB Local or a VPC endpoint |
| AWS profile | `default` | Profile of `~/.aws/config` / `~/.aws/credentials` |
| AWS region | — | Alternative region field |
| Access key ID / Secret access key / Session token | — | Static keys (keyring) |

- No host / port, no TLS settings (HTTPS is used for AWS endpoints); the **SSL** and **SSH** tabs are not used.
- The explorer shows one container: the region (or "DynamoDB Local (…)").

## Authentication

One dialog method, **AWS IAM**:

| Credentials | When |
|---|---|
| Static keys | Access key ID + secret access key (+ optional session token) from the form |
| AWS credential chain | Otherwise: the selected **AWS profile** (static keys, `credential_process`, IAM Identity Center / **SSO**, assumed roles), environment variables (`AWS_ACCESS_KEY_ID`…), and the other sources of the AWS SDK default chain |
| DynamoDB Local | With an endpoint on `localhost` / `127.0.0.1`, no keys and an empty profile field, dummy credentials are used (DynamoDB Local accepts any signature) |

For SSO profiles run `aws sso login --profile <name>` first.

## What works

- **Explorer**: region → tables (billing mode, item count) → key attributes, primary key (partition key, sort key), GSIs and LSIs with their projections; TTL and streams noted in the table script.
- **Console**: PartiQL (level B) — `SELECT`, `INSERT INTO … VALUE {…}`, `UPDATE … SET / REMOVE`, `DELETE`, `"table"."index"` names, `?` parameters — run with `ExecuteStatement` and paged with `NextToken`. Lines of the form `Operation {json}` call the API in the AWS CLI / API shape: `Query`, `Scan`, `GetItem`, `PutItem`, `UpdateItem`, `DeleteItem`, `TransactWriteItems`, `ExecuteTransaction`, `CreateTable`, `DeleteTable`, `ListTables`, `DescribeTable`. `Limit` of `Query` / `Scan` is the total number of items; the `LastEvaluatedKey` to continue from is reported as a notice.
- **Cost warnings**: a `SELECT` without an `=` / `IN` condition on the partition key is a full table **Scan**: the editor flags it and the driver warns before reading. Every call reports its **consumed capacity** as a notice. `DELETE` / `UPDATE` without `WHERE` ask for confirmation.
- **Schema inference**: from a sampled Scan.
- **Results**: documents (tree, JSON, table). Numbers are exact (int32 / int64 / decimal128), sets are kept as `{ "$SS": […] }`, `{ "$NS": […] }`, `{ "$BS": […] }` so they stay sets when written back.
- **Editing**: items of `SELECT *` results are edited with PartiQL `UPDATE "t" SET / REMOVE … WHERE pk = …` and `DELETE`, applied in one `ExecuteTransaction`, shown in the operation preview.
- **Definitions**: a `CreateTable {…}` request runnable in the console.
- **Transactions**: single-call transactions only (`ExecuteTransaction`, `TransactWriteItems`).
- **Export**: all result export formats.

## Limitations

- **No interactive transactions**: Manual mode is not available.
- **No execution plans**, no joins.
- **No cost estimate before running** (the capacity is reported after each call).
- **Cancel** abandons the HTTP call; there is no server-side operation to stop.
- **Read-only** is enforced by FerroDb (use IAM policies for a server-side guarantee).
- **No "Copy Script" menu** for tables in the explorer yet; copying data **into** DynamoDB with the transfer wizard is not done.
- Not tested against real AWS.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| DynamoDB Local: credential errors at connect | Leave **AWS profile** empty or at `default` (FerroDb then uses dummy credentials for a `localhost` endpoint), or type any access key and secret |
| "timed out connecting to DynamoDB" | Wrong region / endpoint, or no network route to AWS |
| SSO profile fails | Run `aws sso login --profile <name>`; the token in the SSO cache must be valid |
| `ResourceNotFoundException` | The table is in another region: change **Region** |
| Scan warning on every query | Add a partition-key `=` / `IN` condition, or query a GSI with `"table"."index"` |
