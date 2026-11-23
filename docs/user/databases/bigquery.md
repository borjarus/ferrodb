# Google BigQuery

Cloud analytical tabular database, GoogleSQL, reached through **ADBC (A)** with the Apache Arrow ADBC BigQuery driver plus the BigQuery REST API for dry runs, plans and jobs; language intelligence **level B**. Cost estimate before running.

## Requirements

| What | Details |
|---|---|
| Tested | Not run against a real Google Cloud project (`docs/phase-8.md`). Covered by unit tests on recorded responses and a live test that runs when `FERRODB_TCK_BIGQUERY_*` variables are set |
| Driver library | `adbc_driver_bigquery` **1.11.0** (Apache Arrow ADBC), downloaded on demand by the driver manager from the project's PyPI wheel, verified with its SHA-256, extracted under the platform's library name |
| Platforms | Windows x86_64, Linux x86_64 / aarch64, macOS x86_64 / aarch64 |
| License | Apache-2.0 |
| Java | Not needed |
| Network | HTTPS to `bigquery.googleapis.com` and `oauth2.googleapis.com` |

A hand-supplied library is also accepted: a `driver.library` path in the data source configuration, the `FERRODB_ADBC_BIGQUERY` environment variable, or a file in `<data dir>/drivers/adbc`.

## Connecting

| Field | Default | Notes |
|---|---|---|
| Project | — | Required. The explorer lists this project's datasets; also the default project of every statement |
| Default dataset | — | Default dataset of every statement |
| Location | `EU` | Job location (`US`, `EU`, a region…); also used to list and cancel jobs |
| Service account key | — | Secret: the JSON key pasted as text |
| Service account key file | — | Path to the JSON key; empty = application default credentials |

- No host / port; the **SSL** and **SSH** tabs are not used.
- **Options tab**: `adbc.…=value` pairs are passed to the ADBC driver as raw options.

## Authentication

One method in the dialog, **Google service account / ADC**, resolved in this order:

1. **Service account key** pasted into the secret field (stored in the keyring);
2. **Service account key file** (path);
3. **Application Default Credentials** when both are empty: `GOOGLE_APPLICATION_CREDENTIALS`, then the gcloud ADC file (service account or authorized user).

For the REST calls (dry run, plans, jobs), FerroDb additionally falls back to `gcloud auth print-access-token` and to the GCE / GKE metadata server. The service account token is obtained by signing a JWT (RS256) and exchanging it at Google's token endpoint.

## What works

- **Explorer**: datasets of the project (ADBC `GetObjects`, fallback `INFORMATION_SCHEMA.SCHEMATA`) → tables and views → columns from the dataset's `INFORMATION_SCHEMA`.
- **Console**: GoogleSQL dialect file (keywords, types, functions with docs), splitting, completion, diagnostics, formatter (level B); errors are mapped to a position in the statement.
- **Cost estimate**: before a statement runs, the console asks for a dry run and shows the bytes the query would process in a confirmation dialog (run or cancel); the last estimate stays visible in the console.
- **Results**: grid, Arrow batches re-chunked and streamed with backpressure.
- **Cancel**: ADBC statement cancel, plus `jobs.cancel` through the REST API for the jobs with the same SQL text started since the statement began.
- **Editing**: grid edits generate GoogleSQL (backtick quoting, `FROM_HEX` binaries) shown in the operation preview.
- **Definitions**: generated DDL.
- **Execution plan**: the job's query plan stages; the estimate comes from a dry run, *Explain Analyze* runs the job.
- **Read-only mode**: ADBC read-only option when accepted, otherwise statements that change data are refused by the connection.
- **Export**: all result export formats.

## Limitations

- **No transactions** in FerroDb: the source is auto-commit only; Manual mode / `BEGIN` from the transaction controls are refused.
- **No jobs panel** in the UI: the backend can list jobs and cancel them, but there is no view for it yet.
- **Explain Analyze runs the query** (and bills it).
- Never tested against a real project.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| "BigQuery: the project is required" | Fill in **Project** |
| "BigQuery REST: no credentials (service account key, application default credentials or gcloud)" | No key in the form, no ADC file, no working `gcloud`: give a key or run `gcloud auth application-default login` |
| "Google credentials: no private_key" / parse errors | The pasted text or file is not a service account / authorized-user JSON |
| "Not found: Dataset … was not found in location …" | Set **Location** to the dataset's location (the form defaults to `EU`) |
| No cost dialog | The dry run failed or returned no byte count; the statement then runs without the dialog |
| Driver missing | Download the BigQuery driver in the driver manager |
