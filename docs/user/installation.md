# Installation

## System requirements

| | Supported |
|---|---|
| Windows | Windows 10 and 11, x64 |
| macOS | Apple Silicon and Intel |
| Linux | x64, X11 or Wayland |
| Java | Only for the 9 databases that use the JDBC bridge: Java 17 or newer. FerroDb can download a Java runtime for you (see below) |
| Network | Only for your databases, driver downloads, update checks and, if you turn it on, the AI assistant |

Two parts are not in the installer and are downloaded only when you need them:

- the Java runtime and JDBC drivers (Oracle, IBM Db2, Sybase ASE, H2, Apache Derby, HyperSQL, Exasol, Vertica, Apache Hive, and custom JDBC drivers);
- the ADBC driver libraries (Snowflake, Google BigQuery).

All other databases use drivers built into FerroDb.

## Installers

| System | Package | How to install |
|---|---|---|
| Windows | `FerroDb_<version>_x64-setup.exe` (NSIS installer) | Run the installer and follow the steps |
| macOS | `FerroDb_<version>_<arch>.dmg` | Open the disk image and drag FerroDb to Applications |
| Linux (Debian, Ubuntu) | `.deb` | `sudo apt install ./<file>.deb` |
| Linux (other) | `FerroDb_<version>_x86_64.AppImage` | `chmod +x` the file, then run it |

### Signed packages

Release packages are signed so your system can confirm where they come from:

- **Windows:** the installer carries an Authenticode signature. Windows SmartScreen shows the publisher name; if it reports an unknown publisher, do not run the file.
- **macOS:** the application is signed and notarized by Apple, so Gatekeeper opens it without a warning.

Updates downloaded by FerroDb itself are checked as well; see [Updates](updates.md).

## Drivers downloaded on demand

When you create a data source for a database whose driver is not present, the data source dialog shows **Not downloaded** and a **Download** button. You can also manage drivers in **File ▸ Driver Manager…**.

- Drivers come from a catalog built into FerroDb. Each entry has the source (Maven Central for JDBC drivers, the Apache Arrow ADBC release packages for ADBC drivers), the version, the license and a SHA-256 checksum.
- A download is verified against the checksum **before** it is installed. A file that does not match is rejected.
- The Driver Manager shows each driver's license before you download it.
- **Offline installation:** on a machine without internet access, download the driver files elsewhere and choose them with **Use custom driver files…** in the Driver Manager. Files that match the catalog checksums are installed as the verified catalog version.
- Files that do not match the catalog are used as **custom driver files**: they are copied into FerroDb's data directory and are not verified. Use them for drivers or versions that are not in the catalog; see [Data sources](data-sources.md#custom-jdbc-drivers).

### Java runtime

The JDBC bridge needs Java 17 or newer. FerroDb looks for an installed JDK or JRE in `JAVA_HOME`, on `PATH` and in the usual install locations. If none is found when you download a JDBC driver, FerroDb also downloads an Eclipse Temurin 21 JRE from the Adoptium API, verifies it with the checksum Adoptium publishes, and keeps it in its data directory. The Java process starts with the first JDBC connection, so databases that do not use JDBC never start Java.

## Where FerroDb keeps its files

FerroDb uses two directories: one for configuration and one for data.

| System | Configuration | Data |
|---|---|---|
| Windows | `%APPDATA%\FerroDb\FerroDb\config` | `%APPDATA%\FerroDb\FerroDb\data` |
| macOS | `~/Library/Application Support/dev.FerroDb.FerroDb` | the same directory |
| Linux | `~/.config/ferrodb` | `~/.local/share/ferrodb` |

If the environment variable `FERRODB_HOME` is set, FerroDb uses `$FERRODB_HOME/config` and `$FERRODB_HOME/data` instead. This is useful for a portable setup, for demos and for testing a beta next to your normal setup.

Passwords and tokens are not in these directories. They are kept in the system keyring (Windows Credential Manager, macOS Keychain, Secret Service on Linux) under the service name `FerroDb`.

[Settings and files](settings-and-files.md) lists every file.

## Starting FerroDb

```sh
ferrodb                 # opens the last project, or the default project
ferrodb path/to/folder  # opens a folder as a project
ferrodb mcp             # no window: the MCP bridge for AI agents (see ai-and-mcp.md)
```

## Uninstalling

1. Remove the application:
   - **Windows:** Settings ▸ Apps ▸ Installed apps ▸ FerroDb ▸ Uninstall.
   - **macOS:** move FerroDb from Applications to the Trash.
   - **Linux (.deb):** remove the package with `apt remove` (`dpkg -l | grep -i ferrodb` shows its name).
   - **Linux (AppImage):** delete the AppImage file.
2. Optional, to remove your settings and data as well: delete the configuration and data directories from the table above. The data directory also holds downloaded drivers, the Java runtime FerroDb downloaded, query history and crash reports.
3. Optional: delete the `FerroDb` entries from the system keyring.

Project folders (with their `.ferrodb/` subfolder) are yours and are never removed.
