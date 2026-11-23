# Troubleshooting

Start with the message in the notification or the **Output** panel. If that is not enough, collect a log (see [Logs](#logs)) and check the sections below.

## JDBC bridge and Java

Oracle, IBM Db2, Sybase ASE, H2, Apache Derby, HyperSQL, Exasol, Vertica, Apache Hive and custom JDBC drivers run in a separate Java process, the JDBC bridge. Its state is shown in the **Services** panel and in **File ▸ Driver Manager…**.

| Message | Cause and fix |
|---|---|
| ``driver `…` is not downloaded`` | Download the driver in the data source dialog or the Driver Manager |
| ``driver `Java runtime ≥ 17 (Drivers → Download Java)` is not downloaded`` | No Java 17 or newer was found. Download any JDBC driver in the Driver Manager: FerroDb then downloads a Temurin 21 JRE. Or install Java 17+ yourself and set `JAVA_HOME` |
| `… needs Java N, the bridge runs on Java M (…): install a newer Java or pick an older driver version` | The driver version needs a newer Java than the one found. Install a newer Java, point `JAVA_HOME` to it, or choose an older driver version in the Driver Manager |
| `JDBC bridge: the Java host did not start in time` | Java started but did not answer. Check that the Java in `JAVA_HOME` works (`java -version`), and that security software does not block it. Collect a log with `FERRODB_LOG=ferrodb_jdbc=debug` |
| **restarting after a failure; sessions were disconnected** | The Java process stopped (for example out of memory). FerroDb restarts it; run your statement again. If it keeps happening, report it with the log |

Notes:

- FerroDb uses the first Java 17+ it finds in this order: `JAVA_HOME`, the runtime it downloaded (`drivers/runtime/` in the data directory), `PATH`, the usual install locations.
- The bridge starts with the first JDBC connection. The first connection takes longer; later ones reuse the process.
- With custom driver files, check that you selected every `.jar` the driver needs and, if the driver does not register itself, the **Driver class**.

## ADBC drivers (Snowflake, BigQuery)

| Problem | Fix |
|---|---|
| ``driver `snowflake` is not downloaded`` (or `bigquery`) | Download the driver in the data source dialog or the Driver Manager |
| No internet access on this machine | Download the driver package elsewhere and install it with **Use custom driver files…**, or put the library into `drivers/adbc/` in the data directory, or point `FERRODB_ADBC_SNOWFLAKE` / `FERRODB_ADBC_BIGQUERY` to the library file |
| The library fails to load | The library must match your system and processor (x86_64 or aarch64). Download it again with **Download again** |

## TLS errors

| Symptom | Fix |
|---|---|
| Certificate not trusted (unknown issuer) with `verify-full` | The server's certificate is signed by a private CA. Set `ssl.ca` to the CA's PEM file (see [Data sources](data-sources.md#properties-in-data-sourcestoml)), or add the CA to the system certificate store |
| Host name does not match | Connect with the host name in the certificate, not an IP address or alias |
| The server refuses an unencrypted connection | Choose `require` or `verify-full` in the **SSL** tab |
| The server does not support TLS | Choose `prefer` or `disable` (only on trusted networks) |
| Cassandra with TLS | Not supported in 1.0; FerroDb refuses encrypted modes with a message |

FerroDb uses its own TLS implementation (rustls) for every database, not the system's OpenSSL. Very old servers that only offer TLS 1.0 or 1.1 or outdated cipher suites cannot be reached.

## SSH tunnels

| Message or symptom | Fix |
|---|---|
| `SSH: user is required` | Fill in **User** in the **SSH** tab |
| `SSH: a key file or a password is required` | Enter the **Private key file** |
| The host key changed | FerroDb refuses hosts whose key differs from `known_hosts`. If the change is expected (the server was reinstalled), remove the old line from `~/.ssh/known_hosts` and connect again |
| Unknown host warning | Normal on the first connection: the key is recorded in `known_hosts` |
| MongoDB through SSH with a seed list or `mongodb+srv://` | Not possible: the tunnel reaches one host. Connect to one member with its host and port |

SSH agents (`ssh-agent`, Pageant) are not used; give the key file directly.

## Keyring on Linux

FerroDb stores secrets through the Secret Service D-Bus API, which GNOME Keyring and KWallet provide.

If the log shows `system keyring unavailable; secrets are kept in memory for this session`:

- Make sure a Secret Service provider is installed and running (for example `gnome-keyring` with its daemon started by your session).
- On a minimal or headless system, there may be no D-Bus session. Start FerroDb from a desktop session.
- The keyring must be unlocked. Many desktops unlock it at login; otherwise a prompt appears when FerroDb first needs a secret.

Until the keyring works, FerroDb keeps secrets in memory: you can connect, but you have to type passwords again after a restart.

## Linux display: Wayland and X11

FerroDb uses Wayland when `WAYLAND_DISPLAY` is set, and X11 (`DISPLAY`) otherwise. If the window does not appear or misbehaves under Wayland, start FerroDb under X11 (XWayland) for comparison:

```sh
env -u WAYLAND_DISPLAY ferrodb
```

Report the problem with your desktop environment, compositor and its version.

## HiDPI and scaling

FerroDb follows the scale factor the system reports for each display. If text is too small or too large:

- adjust the system's display scaling;
- or change **Interface font size** and **Editor font size** in **Settings ▸ General** and **Settings ▸ Editor**.

## Connection problems

| Symptom | Fix |
|---|---|
| Timeout while connecting | Check host, port and firewall. Raise the timeout with `connect_timeout=<seconds>` in the **Options** tab |
| Statements are cancelled with `FERRODB-TIMEOUT` | `query_timeout` is set in the **Options** tab; raise or remove it |
| `connection lost` | FerroDb reconnects on the next statement and warns you. An open transaction is lost |
| Explorer shows old objects | Use **Refresh** (Ctrl+F5 / Cmd+F5). FerroDb does not detect schema changes made elsewhere |

The [database pages](databases/README.md) cover database-specific problems.

## Logs

FerroDb writes its log to standard output. Start it from a terminal with `FERRODB_LOG` set and redirect the output to a file:

```sh
FERRODB_LOG=debug ferrodb > ferrodb.log 2>&1
```

Useful filters: `FERRODB_LOG=debug` (everything), `FERRODB_LOG=info,ferrodb_jdbc=debug` (JDBC bridge), `FERRODB_LOG=info,ferrodb_core=debug` (sessions and queries). See [Settings and files](settings-and-files.md#logs).

## Crash reports

After a crash, FerroDb writes a report to `crashes/` in the data directory and, depending on your consent setting, asks at the next start whether to send it. **Settings ▸ Updates and Privacy** has **View Reports**, **Open Reports Folder** and **Clear**. When you report a crash, quote the report id. See [Privacy](privacy.md#crash-reports).

## Starting fresh

To rule out a damaged configuration without losing it, start FerroDb with an empty home:

```sh
FERRODB_HOME=/tmp/ferrodb-clean ferrodb
```

If the problem goes away, compare the files in your configuration directory with the fresh ones. Deleting `metadata/` in the data directory clears the metadata cache; it is rebuilt on the next connection.
