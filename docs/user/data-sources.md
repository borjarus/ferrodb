# Data sources

A data source is a saved connection to one database. Data sources are listed in the Database Explorer and shared by all projects.

## Create or edit a data source

- New: **File ▸ New Data Source…**
- Edit: select the data source and choose **File ▸ Data Source Properties…**, or **Data Source Properties…** in the explorer's context menu.
- Remove: **Remove Data Source…** in the explorer's context menu. Removing a data source also deletes its stored secrets and its cached metadata.

The left side of the dialog lists the 26 databases in three groups (**Relational**, **Analytical and cloud**, **NoSQL**) and a few **Templates**. **Search databases…** filters the list. The form on the right is built from the chosen driver, so the fields differ per database; the [database pages](databases/README.md) describe them.

### General tab

| Field | Meaning |
|---|---|
| **Name** | Shown in the explorer. Defaults to the database name |
| **Folder** | Explorer folder, with `/` for nesting, e.g. `clients/acme` |
| **Authentication** | The methods this database supports: **Password**, **AWS IAM**, **Entra ID**, **Service account**, **Key pair**, **SSO**, **Kerberos**, **X.509**, or **None**. The fields below change with the method |
| Connection fields | Host, port, database, file, URI… depending on the driver |
| **User**, password and other secret fields | Secret fields show *stored in the system keyring* |
| **Environment** | **None**, **Development**, **Test**, **Staging**, **Production** (see below) |
| **Read-only** | Refuse statements that change data (see below) |
| **Manual commit** | Run statements in a transaction that you commit or roll back yourself |

The dialog also shows the database's **Driver**, **Language**, **Data model**, **Access layer** and **Transactions** support, the driver status, and a connection URL preview where the driver has one.

**Test Connection** connects with the values in the form, including secrets you just typed, and reports the server version and the time it took. **OK** saves.

### SSH tab

Use an SSH tunnel when the database is reachable only through a bastion host.

| Field | Meaning |
|---|---|
| **Use SSH tunnel** | Turns the tunnel on |
| **SSH host** | `host` or `host:port` (port 22 by default) |
| **User** | SSH user name |
| **Private key file** | Path to the private key; `~` is expanded |

Host keys are checked against `~/.ssh/known_hosts` (`%USERPROFILE%\.ssh\known_hosts` on Windows). An unknown host is accepted on first use, with a warning, and recorded. A host whose key has changed is refused.

One SSH session serves all connections of the data source through the tunnel, so cancel requests reach the server as well.

### SSL tab

**SSL mode** works the same way for every database (the modes come from PostgreSQL's libpq):

| Mode | Encrypted | Certificate checked |
|---|---|---|
| `disable` | No | — |
| `prefer` (default) | If the server offers it | No |
| `require` | Yes | No, unless a CA file is set |
| `verify-full` | Yes | Certificate chain and host name, against the system certificate store or the CA file |

Use `verify-full` for any server reached over a network you do not control. To trust a private CA, set the `ssl.ca` property to the path of a PEM file (see [Properties in data-sources.toml](#properties-in-data-sourcestoml)). Some drivers (MongoDB, Couchbase, Redis) have a client certificate field in the General tab.

### Options tab

**Driver options** take `key=value` pairs separated by `;`. Every pair is stored as `option.<key>`. Options that FerroDb itself reads:

| Option | Meaning |
|---|---|
| `connect_timeout=<seconds>` | Time allowed to connect |
| `query_timeout=<seconds>` | Statements running longer are cancelled and reported as `FERRODB-TIMEOUT` |
| `retries=<n>` | CockroachDB: how many times a statement failing with a retryable error (40001) is retried in auto-commit |

Other options are database-specific; see the database pages.

### Schemas tab

**Show schemas** limits which schemas or databases the explorer, Search Everywhere and the AI assistant see. Enter comma-separated names or patterns; `*` is a wildcard and `!` excludes. Matching ignores case. Example: `public, sales_*, !tmp_*`. Empty shows all.

## Environments and colors

The environment marks a data source with a color in the explorer and in the console toolbar:

| Environment | Color |
|---|---|
| None | — |
| Development | Green |
| Test | Blue |
| Staging | Amber |
| Production | Red |

Choosing **Production** turns on **Read-only** (you can turn it off again). On a Production data source, every statement that changes data asks for confirmation before it runs.

## Read-only mode

A read-only data source refuses statements that change data or schema. FerroDb checks each statement before it sends it, and where the database supports it also opens a read-only session, so the server refuses writes as well. Result editing, imports and schema changes are disabled for the data source. Agents connected over MCP cannot write to it either.

## Secrets and the keyring

Passwords, tokens, key passphrases and API keys are never written to configuration files.

- All secrets of a data source are stored as one entry in the system keyring: Windows Credential Manager, macOS Keychain, or Secret Service on Linux (GNOME Keyring, KWallet). The service name is `FerroDb`; the account is the data source id.
- `data-sources.toml` holds only a reference: `secret_ref = "secret://<id>"`.
- When you edit a data source, leave a secret field empty to keep the stored value.
- If no keyring is available (for example on a Linux system without Secret Service), FerroDb keeps secrets in memory for the session and logs a warning. You will have to type them again after a restart. See [Troubleshooting](troubleshooting.md#keyring-on-linux).

## Properties in data-sources.toml

Data sources are saved in `data-sources.toml` in the configuration directory (see [Settings and files](settings-and-files.md)). The file is safe to back up or share: it contains no secrets.

```toml
# FerroDb data sources. Secrets live in the system keyring.
[[data_source]]
id = "orders-prod"
name = "orders (prod)"
driver = "postgresql"
folder = ["clients", "acme"]
environment = "Production"
read_only = true
tx_mode = "Auto"
properties = [
  ["host", "db.example.com"],
  ["port", "5432"],
  ["database", "orders"],
  ["auth", "Password"],
  ["ssl.mode", "verify-full"],
  ["ssl.ca", "~/certs/company-ca.pem"],
  ["option.query_timeout", "60"],
]
secret_ref = "secret://orders-prod"
```

Properties that the dialog does not show can be added here while FerroDb is closed:

| Property | Meaning |
|---|---|
| `ssl.ca` | PEM file with the CA certificates used to verify the server |
| `ssh.known_hosts` | A `known_hosts` file other than `~/.ssh/known_hosts` |

## Drivers and the Driver Manager

FerroDb reaches databases through three access layers:

| Layer | Databases | Driver files |
|---|---|---|
| Native (built in) | PostgreSQL, Greenplum, CockroachDB, Amazon Redshift, MySQL, MariaDB, SQLite, SQL Server, Azure SQL, ClickHouse, Cassandra, MongoDB, Couchbase, DynamoDB, Redis, DuckDB | None needed |
| ADBC | Snowflake, Google BigQuery | Downloaded on demand |
| JDBC bridge | Oracle, IBM Db2, Sybase ASE, H2, Apache Derby, HyperSQL, Exasol, Vertica, Apache Hive, custom JDBC | Java runtime and JDBC driver, downloaded on demand |

**File ▸ Driver Manager…** shows the status of every driver:

| Status | Meaning |
|---|---|
| **Built in** | Native driver, nothing to download |
| **Not downloaded** | Click **Download** |
| **Downloading… N%** | Download in progress |
| **Downloaded, version X** | Ready. **Download again** re-installs it |
| **Version X installed, Y available** | A newer catalog version exists |
| **Custom files (N)** | Uses your own driver files |

For each driver you can choose the version, see the license (**License: …**), download it, or use your own files with **Use custom driver files…**. Downloads are verified with SHA-256 before they are installed; see [Installation](installation.md#drivers-downloaded-on-demand).

The **JDBC bridge** line shows the state of the Java process that runs JDBC drivers: **not started (starts with the first JDBC connection)**, **starting…**, **running (PID …, connections: …)**, or **restarting after a failure; sessions were disconnected**. If the Java process fails, FerroDb restarts it; open sessions are reported as disconnected and reconnect on the next statement. The same state is shown in the **Services** panel.

### Custom JDBC drivers

Any JDBC driver can be used through the **Custom JDBC** entry:

1. Create a data source with **Custom JDBC**.
2. Enter the **URL** (`jdbc:…`), the **Driver class** if the driver does not register itself, and the user and password.
3. In the Driver Manager, choose **Use custom driver files…** for Custom JDBC and select the driver's `.jar` files. They are copied into FerroDb's data directory, so moving the originals later does not break the data source.

Custom JDBC drivers get basic support (level C): generic SQL highlighting, statement splitting, and object name suggestions from the driver's metadata. There are no execution plans. Custom files are not verified by FerroDb.
