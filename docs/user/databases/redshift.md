# Amazon Redshift

Tabular · SQL (Redshift dialect, derived from PostgreSQL) · access layer **N** (native, `tokio-postgres`, PostgreSQL wire protocol, with IAM authentication) · language intelligence level **B**.

## Requirements

| | |
|---|---|
| Supported | Provisioned clusters and Redshift Serverless workgroups |
| Tested | **Not run against a real Redshift service.** Covered by unit tests on recorded responses; the TCK entry runs only when `FERRODB_TCK_*` variables point to a cluster |
| Download | Nothing. The driver is built into FerroDb. The AWS SDK is not used: IAM calls are signed by FerroDb (Signature V4) |
| Optional | AWS CLI v2 on `PATH`, for SSO and assumed-role profiles (see Authentication) |
| Network | TCP to the endpoint, port **5439** by default; HTTPS to `redshift.<region>.amazonaws.com` or `redshift-serverless.<region>.amazonaws.com` for IAM |

## Connecting

Driver id `redshift`, family `pgwire`. Listed under analytical / cloud databases.

| Field | Default | Auth | Notes |
|---|---|---|---|
| Host | `localhost` | all | The cluster or workgroup endpoint, e.g. `examplecluster.abc123.us-west-2.redshift.amazonaws.com` |
| Port | `5439` | all | |
| Database | `dev` | all | |
| User | `awsuser` when empty (password) | Password, AWS IAM | With IAM: the database user to issue credentials for |
| Password | — | Password | Keyring |
| AWS profile | `default` | AWS IAM | `aws.profile` |
| AWS region | from the host | AWS IAM | `aws.region` |
| Cluster identifier / serverless workgroup | from the host | AWS IAM | `aws.cluster` |
| Database groups (comma-separated) | — | AWS IAM | `aws.db_groups` |
| Create the database user if missing | `false` | AWS IAM | `aws.auto_create` |
| AWS access key ID / secret access key / session token | — | AWS IAM | Secret key and token go to the keyring |

URL preview: `jdbc:redshift://{host}:{port}/{database}` (display only).

Driver options (`search_path`, `options`, `connect_timeout`, `query_timeout`), TLS modes (`disable` / `prefer` / `require` / `verify-full`, optional `ssl.ca`) and the SSH tunnel work as for [PostgreSQL](postgresql.md#connecting). With an SSH tunnel, IAM still uses the real endpoint name to find the cluster and region.

## Authentication

| Method | How it works |
|---|---|
| Password | Database user and password |
| AWS IAM, provisioned cluster, user given | `redshift:GetClusterCredentials` with `DbUser`, `AutoCreate` and `DbGroups` |
| AWS IAM, provisioned cluster, user empty | `redshift:GetClusterCredentialsWithIAM`: the database user is derived from the IAM identity |
| AWS IAM, Serverless (host contains `.redshift-serverless.`) | `redshift-serverless:GetCredentials` for the workgroup |

Temporary credentials are requested for 900 seconds at every connect.

**AWS credentials** are looked up in this order:

1. The connection fields (access key ID + secret access key, optional session token).
2. `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`, only when the profile field is empty or `default`.
3. The profile (field, else `AWS_PROFILE`, else `default`) in `~/.aws/credentials`, then in `~/.aws/config` (static keys or `credential_process`). `AWS_SHARED_CREDENTIALS_FILE` and `AWS_CONFIG_FILE` are honoured.
4. `aws configure export-credentials --profile <profile>` (AWS CLI v2), which covers SSO, assumed roles and instance metadata.

**Region**: the field, else the host name, else `AWS_REGION` / `AWS_DEFAULT_REGION`, else the profile's `region`.

## What works

| Area | Support |
|---|---|
| Explorer | Local and external schemas (Spectrum, federated, datashares) from `svv_all_schemas`; tables, views and columns from `svv_all_tables` / `svv_all_columns` |
| DDL | Distribution style, `DISTKEY`, `SORTKEY`, `IDENTITY(seed,step)` columns; external tables from `svv_external_tables` / `svv_external_columns` |
| Console | Level B: highlighting, statement splitting, schema-aware completion, syntax diagnostics (Redshift parser), formatter. The dialect removes PostgreSQL types and functions Redshift lacks and adds its own (SUPER, HLL, date functions) |
| Results | Streamed, PostgreSQL type mapping |
| Cancel | PostgreSQL cancel request |
| Editing | Single-table `SELECT` results with a primary or unique key |
| Transactions | Auto and Manual modes |
| Read-only | Enforced by FerroDb before sending: Redshift has no read-only sessions |
| Explain | `EXPLAIN` text plans; `DS_BCAST_INNER` / `DS_DIST_BOTH` redistribution steps are recognized |
| Export | All export formats |

## Limitations

- **Never run against a real service**; report problems with the exact error text.
- No Explain Analyze (Redshift has none; actual figures are in `SVL_QUERY_SUMMARY` after a run).
- Import cannot use `COPY … FROM STDIN` (Redshift copies from S3 only); imports use `INSERT`.
- Redshift has no indexes and no arrays: nothing is listed for them.
- Read-only mode is a client-side check only.
- IAM credentials are not cached; every new connection calls the AWS API.
- Level B: no inspections with quick fixes and no rename refactoring.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `AWS region unknown: set the AWS region property` | Fill AWS region, or use the standard endpoint host name that contains the region |
| `no AWS credentials for profile … the AWS CLI fallback failed too` | Sign in (`aws sso login --profile …`), set the access key fields, or install the AWS CLI v2 |
| `Redshift GetClusterCredentials: …` | The IAM identity lacks `redshift:GetClusterCredentials` for that cluster / user, or the user does not exist (set "Create the database user if missing") |
| `Redshift Serverless GetCredentials: …` | Check the workgroup name (field or host) and `redshift-serverless:GetCredentials` permission |
| Wrong cluster in IAM calls through an SSH tunnel or a custom DNS name | Set "Cluster identifier / serverless workgroup" and the region explicitly |
| Statement refused as read-only | The source is read-only; FerroDb refuses writes before sending them |
