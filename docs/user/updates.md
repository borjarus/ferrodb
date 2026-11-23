# Updates

FerroDb can check for new versions, download them and start the installer. Every step is verified; see [Privacy](privacy.md#updates).

## Automatic checks

With **Check automatically** on (the default), FerroDb checks once a day: at startup, if the last check was more than 24 hours ago. A check is one HTTPS request for the update manifest; it sends no identifiers. When a newer version exists on your channel, FerroDb shows its version, date and release notes and offers to download it.

Turn automatic checks off in **Settings ▸ Updates and Privacy**. You can still check by hand.

## Checking by hand

Choose **Help ▸ Check for Updates…**, or **Check Now** in **Settings ▸ Updates and Privacy**.

## Installing an update

1. Accept the update. FerroDb downloads the package for your system and shows the progress.
2. The package is verified: its SHA-256 checksum must match the value in the signed manifest. A package that does not match is discarded.
3. FerroDb starts the installer and quits. Your settings, data sources, history and drivers are kept.

| System | Package used for updates |
|---|---|
| Windows | NSIS installer (`.exe`) |
| macOS | Disk image (`.dmg`) |
| Linux | AppImage |

If you installed the `.deb` package, update through your package manager by installing the new `.deb`.

## Channels

| Channel | Offers |
|---|---|
| **Stable** (default) | Stable releases only |
| **Beta** | Beta builds and release candidates, and stable releases when they are newer |

Choose the channel in **Settings ▸ Updates and Privacy**: **Join the beta program** switches to Beta. **Help ▸ Join Beta Program…** does the same. See [Beta program](beta-program.md).

Versions follow semantic versioning: `1.0.0-beta.2` < `1.0.0-rc.1` < `1.0.0` < `1.0.1`. Switching back to Stable does not downgrade: you stay on your current version until a newer stable release appears.

## Current version

**Help ▸ About FerroDb** (**FerroDb ▸ About FerroDb** on macOS) shows the installed version.
