# ADR-013: Data source configuration and secret storage

- Status: **Accepted** (global configuration; project-level files follow with project sharing)
- Date: 2026-11-09

## Context

Data source definitions should be easy to back up, diff and share. Passwords, tokens and key passphrases must never land in those files, in logs or in process arguments (plan, sections 3 and 5.10).

## Decision

- **Configuration:** `data-sources.toml` in the configuration directory, with one `[[data_source]]` table per `DataSourceConfig`. Writes are atomic (temporary file + rename). The file contains `secret_ref = "secret://<id>"` and never a secret.
- **Secrets:** `ferrodb-secrets` stores all secrets of a source as one credential in the system keyring (Windows Credential Manager, macOS Keychain, Secret Service), under service `FerroDb` and account `<id>`, as a small JSON object (`password`, `ssh.password`, `ssh.passphrase`…). One credential per source means it is saved, read and deleted atomically.
- **Contract (API v2):**
  - `DataSourceService::save(config, SecretValues)` and `test(config, SecretValues)`.
  - Typed values replace stored ones key by key; empty fields keep what is stored, so editing a source does not require retyping its password.
  - `SecretValues` redacts values in `Debug`.
- **Fallback:** if the keyring cannot be used (headless Linux without Secret Service), secrets are kept in memory for the session and a warning is logged.
- **Resolution:** the core merges stored secrets with typed ones into `ResolvedConfig` immediately before connecting. Secrets reach drivers in memory only.
- **TLS:** follows libpq semantics (`disable`, `prefer`, `require`, `verify-full`, optional CA file `ssl.ca`) for every driver, using rustls with the `ring` provider and the platform verifier for `verify-full`.
- **SSH:** `ssh.host[:port]`, `ssh.user`, and `ssh.key` or the secret `ssh.password`. Host keys are trusted on first use, recorded in `known_hosts`, and a changed key is refused.

## Consequences

- Removing a data source also deletes its credential and its query history.
- Project-level `.ferrodb/data-sources.toml` (committable, without secrets) reuses the same format; it is wired when projects can carry data sources.
- Keyring access can block (D-Bus), so the core always calls it on the blocking pool.
