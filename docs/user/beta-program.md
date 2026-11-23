# Beta program

Beta builds are early versions of the next FerroDb release. They let you try new features and fixes before they are stable, and your reports help us fix problems before the release.

## What beta builds are

- Beta builds (`1.0.0-beta.N`) and release candidates (`1.0.0-rc.N`) are full FerroDb builds, signed and verified like stable releases.
- They have passed the automated tests, but have been used by fewer people. Expect more bugs than in a stable release.
- They use the same configuration and data as stable releases. Your data sources, settings and history carry over in both directions.
- While you are in the beta, you are also offered stable releases when they are newer than your beta.

## Joining

Choose one of:

- **Help ▸ Join Beta Program…**
- **Settings ▸ Updates and Privacy ▸ Join the beta program** (sets the channel to Beta)

FerroDb then checks for updates on the beta channel. Use **Help ▸ Check for Updates…** to get the current beta right away.

### Before you join

- Back up `data-sources.toml` and the rest of the configuration directory (see [Settings and files](settings-and-files.md)). Secrets are in the system keyring and are not affected.
- To keep a beta fully separate from your normal setup, start it with `FERRODB_HOME` pointing to another folder. It then uses its own configuration and data directories, but the same keyring.
- Prefer data sources marked **Read-only**, or test databases, for your first sessions with a new build. Production data sources ask before every modifying statement.

## Reporting problems

### Crash reports

If FerroDb crashes, it saves a crash report on your disk. With crash reports set to **Ask**, the next start shows **FerroDb closed unexpectedly last time**:

- add a comment: what you were doing, which database, what you expected;
- **Send** sends this report, **Don't Send** keeps it on disk only, **Always Send** sends this one and all future reports automatically.

Nothing is sent without your consent. See [Privacy](privacy.md#crash-reports) for what a report contains and how secrets are masked. **Settings ▸ Updates and Privacy ▸ View Reports** shows each report's id; quote it in a bug report.

### Bugs and feedback

Choose **Help ▸ Report a Problem…** to open the problem report form. There are three forms:

| Form | Use for |
|---|---|
| Bug report | Something does not work or works wrongly |
| Beta feedback | Impressions, usability, missing features, performance |
| Database support | A problem specific to one database: connecting, the explorer, types, editing |

A good report includes:

- the FerroDb version (**Help ▸ About FerroDb**) and your operating system;
- the database, its version and how you connect (native, ADBC or JDBC; SSH or TLS);
- the steps to reproduce, what you expected and what happened;
- the crash report id, if FerroDb crashed;
- the log, if you can reproduce it: start FerroDb with `FERRODB_LOG=debug` and attach the output (see [Settings and files](settings-and-files.md#logs)). Read it first: it may contain object names, host names or query text.

Never attach passwords, API keys or data you are not allowed to share.

## Known limitations

These are known in the 1.0 betas and need no report:

- **Rename…** and **Insert Live Template…** show *Coming in phase 5*; quick fixes for inspections are not offered in the editor.
- Snowflake has no warehouse or role picker and BigQuery no jobs panel in the UI.
- Cassandra connections cannot use TLS.
- SQL Server imports use `INSERT` (no bulk load); TDS 8.0 strict encryption is not supported.
- Imports into Couchbase and DynamoDB are not supported.
- ER diagrams have no zoom or image export and show up to 14 columns per table.
- The AI assistant's **Share sample values** option has no effect.
- The export and import wizards match columns by name only.
- Data sources are global; project-level data sources are not available.

The [database pages](databases/README.md) list database-specific limitations.

## Leaving the beta

Turn off **Join the beta program** in **Settings ▸ Updates and Privacy** (the channel becomes Stable). FerroDb does not downgrade: you keep the beta you have until a newer stable release is offered. To go back to the previous stable release at once, install it from its package.
