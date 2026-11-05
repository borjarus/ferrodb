# ADR-007: Driver manager — sources, verification, licenses, offline mode

- Status: **Accepted**
- Date: 2026-11-05

## Decision

- **Nothing vendor-licensed is bundled.** This covers JDBC jars, ADBC libraries and the Java runtime. All are downloaded on demand.
- **Catalog:** `assets/drivers/catalog.toml` ships with each application release. For every artifact it lists:
  - id, coordinates or URL (trusted sources only: Maven Central, vendor or Apache release sites);
  - version and **SHA-256**;
  - license name and URL;
  - supported OS and architectures.
- **Verification:**
  - The checksum is checked before anything is written to the drivers directory and again before loading.
  - On macOS, ADBC libraries must also pass code-signature verification.
  - A mismatch fails closed, with a clear error.
- **Licenses:** the license is shown and must be accepted before the first download of each artifact.
- **Versions:** the user can pick among the versions in the catalog. "Update available" is shown when the catalog has a newer version.
- **Custom drivers:** the user can point to a local `.jar`. It gets level-C support, is marked "unverified", and needs an explicit confirmation.
- **Offline mode:** install from a local file. The checksum is still verified against the catalog when the artifact appears there.
- **Contract:** `DriverCatalogService` exposes `list`, `status`, `download` and `subscribe`. Statuses are `BuiltIn | NotDownloaded | Downloading{progress} | Downloaded | UpdateAvailable | Failed`. Phase 1 implements this in the mock: Oracle starts not downloaded, and the "driver not downloaded" scenario resets all non-native drivers.
