# FerroDb 1.0 beta program plan

For maintainers. The user-facing description is [`docs/user/beta-program.md`](../user/beta-program.md).

Scope: plan section 7, phase 10 ("Beta program"), within the phase 10 schedule (plan section 8.3). The goal is to find the problems the automated tests cannot: real servers, real networks, real desktops, and the 26 databases in the hands of people who use them daily.

## Timeline

| Date | Milestone (plan 8.3) | Beta program |
|---|---|---|
| 08.11.2027 | **M7** All 26 databases pass the TCK (feature complete); phase 10 starts | Beta 1 to cohort 1 (internal) |
| 15.11.2027 | | Beta 2: cohort 2 (invited) joins |
| 29.11.2027 | | Open beta (cohort 3): beta channel announced, **Help ▸ Join Beta Program…** enabled in stable builds |
| 13.12.2027 | | Feature freeze for 1.0; only fixes from now on |
| 20.12.2027 | **RC** Release candidate | `1.0.0-rc.1` on the beta channel |
| 20.12.2027 – 06.01.2028 | | Further RCs only for S1/S2 fixes |
| 10.01.2028 | **1.0** Stabilization, signed packages, documentation for 26 databases | `1.0.0` on the stable channel; betas are offered 1.0 automatically |

Builds are versioned `1.0.0-beta.N` and `1.0.0-rc.N`. The beta channel of the update manifest always carries the newest beta or RC; the stable channel stays empty (or on the previous stable release) until 1.0.

## Cohorts

| Cohort | Who | Size (target) | Joins | Focus |
|---|---|---|---|---|
| 1 — internal | The team and colleagues | 5–10 | Beta 1 | Smoke test of every build on Windows, macOS and Linux (X11 and Wayland) before it goes wider |
| 2 — invited | Users chosen per database family: at least two per access layer (native, ADBC, JDBC) and at least one per database of plan 2.8, including the cloud-only ones (Redshift, Azure SQL, Snowflake, BigQuery, DynamoDB) | 30–60 | Beta 2 | Databases and setups we cannot run in CI: cloud accounts, Kerberos, Entra ID, IAM, SSO, TLS with private CAs, SSH bastions, large schemas |
| 3 — open | Anyone who joins through the beta channel | open | 29.11.2027 | Breadth: desktops, HiDPI, keyboard layouts, locales, everyday use |

Cohort 2 is the priority: phase 8 left Redshift, Azure SQL, Snowflake, BigQuery, real DynamoDB, Db2, Sybase ASE, Exasol, Vertica and Hive without runs against real servers (`docs/phase-8.md`). Recruit testers for these first, and ask each to run the checklist of plan 2.9 ("supported database") on their database.

## Entry criteria (a build goes to the beta channel)

- `cargo xtask ci` passes on all three systems.
- The driver TCK passes for every database with a container (`cargo xtask db-up`, `cargo xtask tck`), and no TCK scenario regressed since the previous beta.
- Packages are built for every platform, signed (Authenticode, Apple notarization), and the manifest is signed (Ed25519); the update path from the previous beta is tested on each system.
- Cohort 1 installed it and found no S1 issue.
- Release notes list fixes, known issues and what to test.

## Exit criteria (1.0 is released)

- No open S1 or S2 issue.
- Every database of plan 2.8 has at least one confirmed report from a real server (TCK in CI, or a cohort 2 tester running the 2.9 checklist), and its user documentation page is reviewed.
- The last RC was on the beta channel for at least 7 days without a new S1/S2 issue.
- No crash signature (same location and message) received more than once in the last RC remains unexplained.
- The update path from the last RC to 1.0 is tested on each system.

## Triage

Every new issue is triaged within two working days: label it, set the severity, ask for missing information (version, OS, database and version, driver layer, steps, crash report id).

| Severity | Definition | Examples | Target |
|---|---|---|---|
| **S1 — critical** | Data loss or corruption, wrong data shown or written, secrets exposed, crash on startup, security problem | An edit changes other rows than shown; a password in a log or crash report; the app does not start on a supported system | Fix in the next build; a beta can be withdrawn from the manifest |
| **S2 — major** | A main feature fails for a database or a platform with no workaround | Cannot connect with a supported authentication method; results stall; export produces a broken file | Fix before the next RC; blocks 1.0 |
| **S3 — minor** | A feature fails with a workaround, or a secondary feature fails | Wrong completion, a misleading message, a layout problem | Fix when possible; may move to 1.0.x |
| **S4 — trivial** | Cosmetic, wording, translation | | Backlog |

Labels: `beta`, the severity (`S1`…`S4`), the area (`ui`, `console`, `explorer`, `results`, `transfer`, `drivers`, `jdbc`, `adbc`, `ai`, `mcp`, `scripts`, `updates`, `crash`, `docs`), and `db:<slug>` for database-specific issues (slugs as in `docs/user/databases/`).

Crash reports are grouped by location and message. A group with a new location gets an issue; repeated groups are linked to it.

## Release cadence

| Period | Cadence |
|---|---|
| Beta (08.11. – 13.12.2027) | One beta a week (Monday), plus a hotfix build for an S1 issue at any time |
| RC (20.12.2027 – 06.01.2028) | A new RC only for S1/S2 fixes; otherwise the RC stays |
| After 1.0 | 1.0.x fix releases on the stable channel; 1.1 betas on the beta channel |

Each build: release notes in the manifest (short Markdown), a known-issues list, and a "what to test" list for the cohorts.

## Feedback channels

| Channel | For |
|---|---|
| Issue forms (`.github/ISSUE_TEMPLATE/`): bug report, beta feedback, database support | All reports; **Help ▸ Report a Problem…** leads here |
| Crash reports | Sent from the app only with consent (Ask / Always / Never); the report id links an issue to a report |
| Cohort 2 check-ins | A short call or written questionnaire per tester after their first week: which database, which features, what failed |
| Discussions | Questions and ideas that are not bugs |

## Metrics without telemetry

FerroDb has no telemetry, and the beta does not change that. Measure only what users send on purpose, plus aggregate counts on our own servers:

| Metric | Source |
|---|---|
| Open S1/S2 issues, trend per week | Issue tracker |
| Time to triage, time to fix S1/S2 | Issue tracker |
| Issues per database and per access layer | `db:<slug>` labels, issue form fields |
| Databases confirmed on real servers (of 26) | TCK results, cohort 2 check-ins, database support issues |
| Crash groups and how many reports each | Crash reports sent with consent |
| Crash reports per build (relative, not per user) | Crash reports sent with consent |
| Package downloads per build and platform | Aggregate download counts of the release host; no per-user data, no IP addresses kept |
| TCK pass rate per build | CI |

Do not add identifiers, counters or "phone home" calls to the update check or anywhere else to improve these numbers. The update check stays one request with no identifiers.

## Communication

- Announce each build with its notes and what to test.
- Close the loop: when an issue is fixed, say in which build.
- At 1.0, thank the beta testers in the release notes (with their permission).
