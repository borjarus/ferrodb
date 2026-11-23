//! The integration test matrix (plan, section 9 and Appendix D) as data:
//! how each of the 26 databases of 1.0 (plus DuckDB) is tested, with which
//! image and server versions, which variables the tests read, when it runs
//! in CI and on which runners. `cargo xtask matrix` prints it as markdown
//! (`docs/test-matrix.md` holds a copy checked by `--check`), and
//! `--gha` prints the nightly server matrix for GitHub Actions.

use crate::db::{CONTAINERS, Container, Group, container};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Docker container started by `cargo xtask db-up`.
    Container,
    /// Local emulator in a container (DynamoDB Local).
    Emulator,
    /// In-process library or local file.
    Embedded,
    /// Embedded in the JDBC host process (H2, Derby, HyperSQL).
    JdbcEmbedded,
    /// Real cloud service on a test account with a budget limit.
    Cloud,
    /// Dedicated environment (no usable public image).
    Dedicated,
}

impl Method {
    fn label(self) -> &'static str {
        match self {
            Method::Container => "container",
            Method::Emulator => "emulator",
            Method::Embedded => "embedded",
            Method::JdbcEmbedded => "embedded (JDBC host)",
            Method::Cloud => "cloud account",
            Method::Dedicated => "dedicated environment",
        }
    }
}

const LINUX: &[&str] = &["ubuntu"];
const ALL_OS: &[&str] = &["ubuntu", "windows", "macos"];

#[derive(Clone, Copy, Debug)]
pub struct Database {
    /// Number in plan section 2.8 (DuckDB: 27).
    pub n: u8,
    pub name: &'static str,
    /// N native, A ADBC, J JDBC bridge.
    pub layer: &'static str,
    pub method: Method,
    /// Container keys (`db.rs`); their images, versions and variables are
    /// shown.
    pub containers: &'static [&'static str],
    /// Variables besides the containers' ones (cloud accounts, dedicated
    /// environments, switches).
    pub vars: &'static [&'static str],
    /// Run on every pull request (`ci.yml`).
    pub pr: bool,
    /// Job of `nightly.yml`.
    pub nightly: &'static str,
    pub os: &'static [&'static str],
    /// Local command.
    pub local: &'static str,
    pub note: &'static str,
}

const BASE: Database = Database {
    n: 0,
    name: "",
    layer: "N",
    method: Method::Container,
    containers: &[],
    vars: &[],
    pr: false,
    nightly: "servers",
    os: LINUX,
    local: "",
    note: "",
};

pub const DATABASES: &[Database] = &[
    Database {
        n: 1,
        name: "PostgreSQL",
        containers: &["pg", "ssh"],
        pr: true,
        local: "cargo xtask db-up --only pg,ssh && cargo xtask tck --only pg,ssh",
        note: "also through an SSH tunnel (bastion container)",
        ..BASE
    },
    Database {
        n: 2,
        name: "Greenplum",
        containers: &["greenplum"],
        local: "cargo xtask db-up --only greenplum && cargo xtask tck --only greenplum",
        ..BASE
    },
    Database {
        n: 3,
        name: "CockroachDB",
        containers: &["cockroach"],
        local: "cargo xtask db-up --only cockroach && cargo xtask tck --only cockroach",
        ..BASE
    },
    Database {
        n: 4,
        name: "Amazon Redshift",
        method: Method::Cloud,
        vars: &["FERRODB_TCK_REDSHIFT", "FERRODB_TCK_REDSHIFT_IAM"],
        nightly: "cloud",
        local: "FERRODB_TCK_REDSHIFT=… cargo test -p ferrodb-driver-tck --test tck -- --exact redshift",
        note: "Redshift Serverless, smallest base capacity",
        ..BASE
    },
    Database {
        n: 5,
        name: "MySQL",
        containers: &["mysql"],
        pr: true,
        local: "cargo xtask db-up --only mysql && cargo xtask tck --only mysql",
        ..BASE
    },
    Database {
        n: 6,
        name: "MariaDB",
        containers: &["mariadb"],
        pr: true,
        local: "cargo xtask db-up --only mariadb && cargo xtask tck --only mariadb",
        ..BASE
    },
    Database {
        n: 7,
        name: "SQLite",
        method: Method::Embedded,
        pr: true,
        nightly: "os",
        os: ALL_OS,
        local: "cargo test -p ferrodb-driver-tck --test tck -- --exact sqlite",
        note: "temporary file; always runs",
        ..BASE
    },
    Database {
        n: 8,
        name: "SQL Server",
        containers: &["mssql"],
        local: "cargo xtask db-up --only mssql && cargo xtask tck --only mssql",
        ..BASE
    },
    Database {
        n: 9,
        name: "Azure SQL",
        method: Method::Cloud,
        vars: &["FERRODB_TCK_AZURESQL", "FERRODB_TCK_AZURESQL_ENTRA"],
        nightly: "cloud",
        local: "FERRODB_TCK_AZURESQL=… cargo test -p ferrodb-driver-tck --test tck -- --exact azuresql",
        note: "serverless database with auto-pause",
        ..BASE
    },
    Database {
        n: 10,
        name: "Oracle",
        layer: "J",
        containers: &["oracle"],
        local: "cargo xtask db-up --only oracle && cargo xtask tck --only oracle",
        ..BASE
    },
    Database {
        n: 11,
        name: "IBM Db2",
        layer: "J",
        containers: &["db2"],
        local: "cargo xtask db-up --only db2 && cargo xtask tck --only db2",
        note: "TCK profile not yet run against a server",
        ..BASE
    },
    Database {
        n: 12,
        name: "Sybase ASE",
        layer: "J",
        containers: &["sybase"],
        local: "cargo xtask db-up --only sybase && cargo xtask tck --only sybase",
        note: "release sign-off on the dedicated environment; profile not yet run",
        ..BASE
    },
    Database {
        n: 13,
        name: "H2",
        layer: "J",
        method: Method::JdbcEmbedded,
        vars: &["FERRODB_TCK_JDBC"],
        pr: true,
        nightly: "os",
        os: ALL_OS,
        local: "cargo xtask tck --only embedded",
        note: "Java ≥ 17 and ferrodb-jdbc-host.jar",
        ..BASE
    },
    Database {
        n: 14,
        name: "Apache Derby",
        layer: "J",
        method: Method::JdbcEmbedded,
        vars: &["FERRODB_TCK_JDBC"],
        pr: true,
        nightly: "os",
        os: ALL_OS,
        local: "cargo xtask tck --only embedded",
        note: "Java ≥ 17 and ferrodb-jdbc-host.jar",
        ..BASE
    },
    Database {
        n: 15,
        name: "HyperSQL",
        layer: "J",
        method: Method::JdbcEmbedded,
        vars: &["FERRODB_TCK_JDBC"],
        pr: true,
        nightly: "os",
        os: ALL_OS,
        local: "cargo xtask tck --only embedded",
        note: "Java ≥ 17 and ferrodb-jdbc-host.jar",
        ..BASE
    },
    Database {
        n: 16,
        name: "Exasol",
        layer: "J",
        containers: &["exasol"],
        local: "cargo xtask db-up --only exasol && cargo xtask tck --only exasol",
        note: "TCK profile not yet run against a server",
        ..BASE
    },
    Database {
        n: 17,
        name: "Vertica",
        layer: "J",
        method: Method::Dedicated,
        vars: &["FERRODB_TCK_VERTICA"],
        nightly: "dedicated",
        local: "FERRODB_TCK_VERTICA=vertica://dbadmin:…@host:5433/VMart cargo test -p ferrodb-driver-tck --test tck -- --exact vertica",
        note: "Vertica CE image withdrawn from Docker Hub; profile not yet run",
        ..BASE
    },
    Database {
        n: 18,
        name: "Apache Hive",
        layer: "J",
        containers: &["hive"],
        local: "cargo xtask db-up --only hive && cargo xtask tck --only hive",
        note: "TCK profile not yet run against a server",
        ..BASE
    },
    Database {
        n: 19,
        name: "Snowflake",
        layer: "A",
        method: Method::Cloud,
        vars: &[
            "FERRODB_TCK_SNOWFLAKE_ACCOUNT",
            "…_USER",
            "…_PASSWORD / …_PRIVATE_KEY_FILE",
            "…_WAREHOUSE",
            "…_DATABASE",
            "…_SCHEMA",
            "…_ROLE",
            "FERRODB_ADBC_SNOWFLAKE",
        ],
        nightly: "cloud",
        local: "cargo test -p ferrodb-adbc --test cloud -- --exact snowflake",
        note: "X-Small warehouse, auto-suspend 60 s, resource monitor",
        ..BASE
    },
    Database {
        n: 20,
        name: "Google BigQuery",
        layer: "A",
        method: Method::Cloud,
        vars: &[
            "FERRODB_TCK_BIGQUERY_PROJECT",
            "…_DATASET",
            "…_LOCATION",
            "…_CREDENTIALS",
            "FERRODB_ADBC_BIGQUERY",
        ],
        nightly: "cloud",
        local: "cargo test -p ferrodb-adbc --test cloud -- --exact bigquery",
        note: "on-demand pricing with a daily query quota",
        ..BASE
    },
    Database {
        n: 21,
        name: "ClickHouse",
        containers: &["clickhouse"],
        local: "cargo xtask db-up --only clickhouse && cargo xtask tck --only clickhouse",
        ..BASE
    },
    Database {
        n: 22,
        name: "Apache Cassandra",
        containers: &["cassandra"],
        local: "cargo xtask db-up --only cassandra && cargo xtask tck --only cassandra",
        ..BASE
    },
    Database {
        n: 23,
        name: "MongoDB",
        containers: &["mongo"],
        local: "cargo xtask db-up --only mongo && cargo xtask tck --only mongo",
        ..BASE
    },
    Database {
        n: 24,
        name: "Couchbase",
        containers: &["couchbase"],
        local: "cargo xtask db-up --only couchbase && cargo xtask tck --only couchbase",
        ..BASE
    },
    Database {
        n: 25,
        name: "Amazon DynamoDB",
        method: Method::Emulator,
        containers: &["dynamodb"],
        vars: &["FERRODB_TCK_DYNAMODB_AWS (real AWS, region)"],
        pr: true,
        local: "cargo xtask db-up --only dynamodb && cargo xtask tck --only dynamodb",
        note: "nightly also on a real AWS account (job cloud)",
        ..BASE
    },
    Database {
        n: 26,
        name: "Redis",
        containers: &["redis"],
        local: "cargo xtask db-up --only redis && cargo xtask tck --only redis",
        note: "Cluster and Sentinel: FERRODB_TCK_REDIS_CLUSTER / _SENTINEL, manual",
        ..BASE
    },
    Database {
        n: 27,
        name: "DuckDB",
        method: Method::Embedded,
        nightly: "os",
        os: ALL_OS,
        local: "cargo test -p ferrodb-drivers --features duckdb --lib duckdb",
        note: "driver tests (compiles DuckDB from C++ sources)",
        ..BASE
    },
];

fn code(s: &str) -> String {
    format!("`{s}`")
}

/// The images and tags of `containers`, e.g. `postgres` 14, 15, 16.
fn images(keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|k| container(k))
        .map(|c| {
            let tags: Vec<&str> = c.versions.to_vec();
            if c.repository() == c.image {
                code(c.image)
            } else {
                format!("{} {}", code(c.repository()), tags.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn tck_vars(db: &Database) -> String {
    let mut v: Vec<String> = db
        .containers
        .iter()
        .filter_map(|k| container(k))
        .flat_map(|c| c.vars.iter())
        .filter(|(k, _)| k.starts_with("FERRODB_TCK_"))
        .map(|(k, _)| code(k))
        .collect();
    v.extend(db.vars.iter().map(|s| code(s)));
    v.dedup();
    if v.is_empty() {
        "—".into()
    } else {
        v.join(", ")
    }
}

/// The two markdown tables (databases, containers).
pub fn markdown() -> String {
    let mut s = String::new();
    s.push_str(
        "| # | Database | Layer | Method | Image and nightly versions | Variables | PR | Nightly job | Runners | Notes |\n",
    );
    s.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
    for d in DATABASES {
        let img = images(d.containers);
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            d.n,
            d.name,
            d.layer,
            d.method.label(),
            if img.is_empty() { "—".into() } else { img },
            tck_vars(d),
            if d.pr { "yes" } else { "—" },
            code(d.nightly),
            d.os.join(", "),
            d.note,
        ));
    }
    s.push_str("\nContainers (`cargo xtask db-up --only <key>` or `--group <group>`):\n\n");
    s.push_str("| Key | Group | Default image | Port | Memory | Ready when | Notes |\n");
    s.push_str("|---|---|---|---|---|---|---|\n");
    for c in CONTAINERS {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            code(c.key),
            c.group.name(),
            code(c.image),
            c.host_port(),
            c.memory.unwrap_or("—"),
            ready_label(c),
            c.note,
        ));
    }
    s.push_str("\nLocal commands:\n\n");
    s.push_str("| Database | Command |\n|---|---|\n");
    for d in DATABASES {
        s.push_str(&format!("| {} | {} |\n", d.name, code(d.local)));
    }
    s
}

fn ready_label(c: &Container) -> String {
    use crate::db::Ready;
    let secs = c.ready_secs;
    match c.ready {
        Ready::Exec(cmd) => format!("{} (≤ {secs} s)", code(cmd[0])),
        Ready::Log(text) => format!("log: {} (≤ {secs} s)", code(text)),
        Ready::Tcp => format!("TCP port open (≤ {secs} s)"),
        Ready::Init => "initialised by `db-up`".into(),
    }
}

/// `matrix --gha`: `servers=<json>` for `$GITHUB_OUTPUT`, one entry per
/// container and server version.
pub fn gha() -> String {
    let mut entries = Vec::new();
    for c in CONTAINERS {
        for v in c.versions {
            let image = if c.repository() == c.image {
                c.image.to_owned()
            } else {
                format!("{}:{v}", c.repository())
            };
            entries.push(format!(
                r#"{{"key":"{}","group":"{}","name":"{} {}","image":"{}","java":{}}}"#,
                c.key,
                c.group.name(),
                c.name,
                v,
                image,
                c.group == Group::Jdbc,
            ));
        }
    }
    format!("servers={{\"include\":[{}]}}", entries.join(","))
}

const BEGIN: &str = "<!-- matrix:begin (generated by `cargo xtask matrix`) -->";
const END: &str = "<!-- matrix:end -->";

/// `cargo xtask matrix [--check FILE | --write FILE | --gha]`.
pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        None => {
            print!("{}", markdown());
            Ok(())
        }
        Some("--gha") => {
            println!("{}", gha());
            Ok(())
        }
        Some(flag @ ("--check" | "--write")) => {
            let file = args
                .get(1)
                .map_or_else(|| crate::root().join("docs/test-matrix.md"), Into::into);
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let (head, rest) = text
                .split_once(BEGIN)
                .ok_or_else(|| format!("{}: no `{BEGIN}` marker", file.display()))?;
            let (current, tail) = rest
                .split_once(END)
                .ok_or_else(|| format!("{}: no `{END}` marker", file.display()))?;
            let generated = format!("\n{}", markdown());
            let current = current.replace("\r\n", "\n");
            if flag == "--check" {
                if current == generated {
                    Ok(())
                } else {
                    Err(format!(
                        "{} is out of date: run `cargo xtask matrix --write`",
                        file.display()
                    ))
                }
            } else {
                std::fs::write(&file, format!("{head}{BEGIN}{generated}{END}{tail}"))
                    .map_err(|e| format!("{}: {e}", file.display()))
            }
        }
        Some(other) => Err(format!("unknown option `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_database_once_and_known_containers() {
        let mut n: Vec<u8> = DATABASES.iter().map(|d| d.n).collect();
        n.sort_unstable();
        assert_eq!(n, (1..=27).collect::<Vec<_>>());
        for d in DATABASES {
            for k in d.containers {
                assert!(container(k).is_some(), "{}: {k}", d.name);
            }
            let has = !d.containers.is_empty();
            let needs = matches!(d.method, Method::Container | Method::Emulator);
            assert_eq!(has, needs, "{}: containers vs method", d.name);
        }
    }

    #[test]
    fn every_container_belongs_to_a_database() {
        for c in CONTAINERS {
            assert!(
                DATABASES.iter().any(|d| d.containers.contains(&c.key)),
                "{} is in no matrix row",
                c.key
            );
        }
    }

    #[test]
    fn gha_lists_every_version() {
        let out = gha();
        let n: usize = CONTAINERS.iter().map(|c| c.versions.len()).sum();
        assert_eq!(out.matches("\"key\"").count(), n);
        assert!(out.contains(r#""image":"postgres:14""#));
        assert!(out.contains(r#""image":"mcr.microsoft.com/mssql/server:2019-latest""#));
    }
}
