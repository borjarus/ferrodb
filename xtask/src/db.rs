//! Integration-test databases (plan, section 9 and Appendix D): containers
//! for the TCK, started with Docker. On Windows Docker usually runs inside
//! WSL, so commands go through `wsl -e docker` when `docker` is not on PATH.
//!
//! Every container belongs to a group (`native`, `nosql`, `jdbc`): the full
//! set needs far more memory than a laptop has, so `db-up`, `db-down`,
//! `tck` and `env` take `--only key,key` or `--group name`. The same data
//! feeds `cargo xtask matrix` (the test matrix in `docs/test-matrix.md`) and
//! the nightly workflow (`matrix --gha`).

use std::{
    net::{TcpStream, ToSocketAddrs},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// Native SQL drivers (pgwire, MySQL protocol, TDS, ClickHouse) and the
    /// SSH bastion.
    Native,
    /// MongoDB, Redis, Cassandra, DynamoDB Local, Couchbase.
    Nosql,
    /// Databases reached through the JDBC bridge (needs Java ≥ 17 and
    /// `ferrodb-jdbc-host.jar`).
    Jdbc,
}

impl Group {
    pub const ALL: [Group; 3] = [Group::Native, Group::Nosql, Group::Jdbc];

    pub fn name(self) -> &'static str {
        match self {
            Group::Native => "native",
            Group::Nosql => "nosql",
            Group::Jdbc => "jdbc",
        }
    }
}

/// How `db-up` knows a container accepts connections.
#[derive(Clone, Copy, Debug)]
pub enum Ready {
    /// A command run inside the container succeeds.
    Exec(&'static [&'static str]),
    /// The container log contains this text.
    Log(&'static str),
    /// The published main port accepts TCP connections.
    Tcp,
    /// An initialisation step of `db-up` waits for it (MongoDB, Couchbase).
    Init,
}

#[derive(Clone, Copy, Debug)]
pub struct Container {
    /// Short name used by `--only` (`pg`, `mysql`…).
    pub key: &'static str,
    pub name: &'static str,
    pub group: Group,
    /// Default image; `--image key=image` replaces it.
    pub image: &'static str,
    /// Tags run by the nightly server-version matrix (the repository of
    /// `image` with each tag). The default image's tag is one of them.
    pub versions: &'static [&'static str],
    /// Published ports, the main one first.
    pub ports: &'static [&'static str],
    pub env: &'static [&'static str],
    pub args: &'static [&'static str],
    /// `docker run -m`.
    pub memory: Option<&'static str>,
    pub privileged: bool,
    pub ready: Ready,
    /// How long `db-up` waits for `ready`.
    pub ready_secs: u64,
    /// Variables for the tests, `{h}` standing for the host.
    pub vars: &'static [(&'static str, &'static str)],
    /// Tests of `ferrodb-driver-tck` (`tests/tck.rs`) using this container.
    pub tests: &'static [&'static str],
    /// Other containers it needs (the SSH bastion tunnels to PostgreSQL).
    pub needs: &'static [&'static str],
    pub note: &'static str,
}

const BASE: Container = Container {
    key: "",
    name: "",
    group: Group::Native,
    image: "",
    versions: &[],
    ports: &[],
    env: &[],
    args: &[],
    memory: None,
    privileged: false,
    ready: Ready::Tcp,
    ready_secs: 180,
    vars: &[],
    tests: &[],
    needs: &[],
    note: "",
};

pub const CONTAINERS: &[Container] = &[
    Container {
        key: "pg",
        name: "PostgreSQL",
        image: "postgres:18",
        versions: &["14", "15", "16", "17", "18"],
        ports: &["55432:5432"],
        env: &["POSTGRES_PASSWORD=ferrodb"],
        ready: Ready::Exec(&["pg_isready", "-U", "postgres", "-h", "127.0.0.1"]),
        vars: &[
            (
                "FERRODB_TCK_POSTGRES",
                "postgres://postgres:ferrodb@{h}:55432/postgres?ssl.mode=disable",
            ),
            (
                "FERRODB_TEST_POSTGRES",
                "postgres://postgres:ferrodb@{h}:55432/postgres?ssl.mode=disable",
            ),
        ],
        tests: &["postgresql"],
        ..BASE
    },
    Container {
        key: "mysql",
        name: "MySQL",
        image: "mysql:8.4",
        versions: &["8.0", "8.4"],
        ports: &["53306:3306"],
        env: &["MYSQL_ROOT_PASSWORD=ferrodb", "MYSQL_DATABASE=tck"],
        // TCP on 127.0.0.1: the temporary server of the first start only
        // listens on the socket.
        ready: Ready::Exec(&[
            "mysqladmin",
            "ping",
            "-h",
            "127.0.0.1",
            "-uroot",
            "-pferrodb",
            "--silent",
        ]),
        vars: &[
            ("FERRODB_TCK_MYSQL", "mysql://root:ferrodb@{h}:53306/tck"),
            ("FERRODB_TEST_MYSQL", "mysql://root:ferrodb@{h}:53306/tck"),
        ],
        tests: &["mysql"],
        ..BASE
    },
    Container {
        key: "mariadb",
        name: "MariaDB",
        image: "mariadb:11.4",
        versions: &["10.11", "11.4"],
        ports: &["53307:3306"],
        env: &["MARIADB_ROOT_PASSWORD=ferrodb", "MARIADB_DATABASE=tck"],
        ready: Ready::Exec(&["healthcheck.sh", "--connect", "--innodb_initialized"]),
        vars: &[
            (
                "FERRODB_TCK_MARIADB",
                "mariadb://root:ferrodb@{h}:53307/tck",
            ),
            (
                "FERRODB_TEST_MARIADB",
                "mariadb://root:ferrodb@{h}:53307/tck",
            ),
        ],
        tests: &["mariadb"],
        ..BASE
    },
    // Phase 8 (native variants). SQL Server needs about 2 GB.
    Container {
        key: "mssql",
        name: "SQL Server",
        image: "mcr.microsoft.com/mssql/server:2022-latest",
        versions: &["2019-latest", "2022-latest", "2025-latest"],
        ports: &["56233:1433"],
        env: &[
            "ACCEPT_EULA=Y",
            "MSSQL_SA_PASSWORD=Ferrodb#2026",
            "MSSQL_MEMORY_LIMIT_MB=2048",
        ],
        ready: Ready::Log("SQL Server is now ready for client connections"),
        vars: &[(
            "FERRODB_TCK_SQLSERVER",
            "sqlserver://sa:Ferrodb#2026@{h}:56233/master",
        )],
        tests: &["sqlserver"],
        note: "about 2 GB",
        ..BASE
    },
    Container {
        key: "cockroach",
        name: "CockroachDB",
        image: "cockroachdb/cockroach:latest",
        versions: &["latest"],
        ports: &["56257:26257"],
        args: &["start-single-node", "--insecure"],
        ready: Ready::Exec(&[
            "/cockroach/cockroach",
            "sql",
            "--insecure",
            "-e",
            "select 1",
        ]),
        vars: &[(
            "FERRODB_TCK_COCKROACH",
            "postgres://root@{h}:56257/defaultdb?ssl.mode=disable",
        )],
        tests: &["cockroachdb"],
        ..BASE
    },
    // Greenplum 7 single host, 2 segments, trust authentication.
    Container {
        key: "greenplum",
        name: "Greenplum",
        image: "andruche/greenplum:7",
        versions: &["7"],
        ports: &["56254:5432"],
        ready_secs: 300,
        vars: &[(
            "FERRODB_TCK_GREENPLUM",
            "postgres://gpadmin@{h}:56254/postgres?ssl.mode=disable",
        )],
        tests: &["greenplum"],
        note: "community image; Greenplum 6 is not supported",
        ..BASE
    },
    // ClickHouse over HTTP (phase 8).
    Container {
        key: "clickhouse",
        name: "ClickHouse",
        image: "clickhouse/clickhouse-server:26.10",
        versions: &["26.10"],
        ports: &["56323:8123"],
        env: &["CLICKHOUSE_PASSWORD=ferrodb", "CLICKHOUSE_DB=tck"],
        ready: Ready::Exec(&[
            "clickhouse-client",
            "--password",
            "ferrodb",
            "-q",
            "select 1",
        ]),
        vars: &[(
            "FERRODB_TCK_CLICKHOUSE",
            "clickhouse://default:ferrodb@{h}:56323/tck",
        )],
        tests: &["clickhouse"],
        ..BASE
    },
    // SSH bastion on the same Docker network: tunnels to `ferrodb-tck-pg`.
    Container {
        key: "ssh",
        name: "SSH bastion",
        image: "linuxserver/openssh-server:latest",
        versions: &["latest"],
        ports: &["52222:2222"],
        env: &[
            "PUID=1000",
            "PGID=1000",
            "USER_NAME=ferrodb",
            "USER_PASSWORD=ferrodb",
            "PASSWORD_ACCESS=true",
            "DOCKER_MODS=linuxserver/mods:openssh-server-ssh-tunnel",
        ],
        vars: &[
            ("FERRODB_TEST_SSH", "ssh://ferrodb:ferrodb@{h}:52222"),
            ("FERRODB_TCK_SSH", "ssh://ferrodb:ferrodb@{h}:52222"),
        ],
        tests: &["postgresql_via_ssh_tunnel"],
        needs: &["pg"],
        note: "PostgreSQL TCK through an SSH tunnel",
        ..BASE
    },
    // A one-member replica set: transactions need one (phase 4b).
    Container {
        key: "mongo",
        name: "MongoDB",
        group: Group::Nosql,
        image: "mongo:8.0",
        versions: &["7.0", "8.0"],
        ports: &["57017:27017"],
        args: &["--replSet", "rs0", "--bind_ip_all"],
        ready: Ready::Init,
        vars: &[(
            "FERRODB_TCK_MONGODB",
            "mongodb://{h}:57017/tck?directConnection=true",
        )],
        tests: &["mongodb"],
        note: "one-member replica set rs0",
        ..BASE
    },
    // Redis 8 has RedisJSON built in; on 7.4 the JSON scenario is skipped.
    Container {
        key: "redis",
        name: "Redis",
        group: Group::Nosql,
        image: "redis:8",
        versions: &["7.4", "8"],
        ports: &["56401:6379"],
        ready: Ready::Exec(&["redis-cli", "ping"]),
        vars: &[("FERRODB_TCK_REDIS", "redis://{h}:56401/0")],
        tests: &["redis"],
        note: "Cluster / Sentinel: manual (host networking)",
        ..BASE
    },
    // About a minute to start; give Docker 3 GB for it.
    Container {
        key: "cassandra",
        name: "Apache Cassandra",
        group: Group::Nosql,
        image: "cassandra:5",
        versions: &["4.1", "5"],
        ports: &["56442:9042"],
        env: &["MAX_HEAP_SIZE=1G", "HEAP_NEWSIZE=200M"],
        memory: Some("3g"),
        ready: Ready::Exec(&["cqlsh", "-e", "describe keyspaces"]),
        ready_secs: 300,
        vars: &[(
            "FERRODB_TCK_CASSANDRA",
            // The node advertises its container address: route through the
            // published port.
            "cassandra://{h}:56442/tck?address_translation=contact_point",
        )],
        tests: &["cassandra"],
        ..BASE
    },
    // DynamoDB Local: in memory, shared by every region / key.
    Container {
        key: "dynamodb",
        name: "DynamoDB Local",
        group: Group::Nosql,
        image: "amazon/dynamodb-local:latest",
        versions: &["latest"],
        ports: &["56520:8000"],
        args: &["-jar", "DynamoDBLocal.jar", "-inMemory", "-sharedDb"],
        memory: Some("1g"),
        vars: &[("FERRODB_TCK_DYNAMODB", "http://{h}:56520")],
        tests: &["dynamodb"],
        note: "emulator",
        ..BASE
    },
    // Couchbase Server community: cluster manager on 56501, query service
    // on 56503; initialised by `init_couchbase`.
    Container {
        key: "couchbase",
        name: "Couchbase",
        group: Group::Nosql,
        image: "couchbase/server:community",
        versions: &["community"],
        ports: &[
            "56501:8091",
            "56502:8092",
            "56503:8093",
            "56511:18091",
            "56513:18093",
        ],
        memory: Some("3g"),
        ready: Ready::Init,
        vars: &[(
            "FERRODB_TCK_COUCHBASE",
            "couchbase://Administrator:ferrodb@{h}:56503/tck?mgmt_port=56501&scope=app",
        )],
        tests: &["couchbase"],
        note: "bucket tck, scope app",
        ..BASE
    },
    // Oracle Database Free: ~3 GB and a few minutes before the PDB
    // FREEPDB1 opens; user `tck` / `ferrodb`.
    Container {
        key: "oracle",
        name: "Oracle",
        group: Group::Jdbc,
        image: "gvenzl/oracle-free:slim",
        versions: &["slim"],
        ports: &["56121:1521"],
        env: &[
            "ORACLE_PASSWORD=ferrodb",
            "APP_USER=tck",
            "APP_USER_PASSWORD=ferrodb",
        ],
        memory: Some("3g"),
        ready: Ready::Exec(&["healthcheck.sh"]),
        ready_secs: 900,
        vars: &[(
            "FERRODB_TCK_ORACLE",
            "oracle://tck:ferrodb@{h}:56121/FREEPDB1",
        )],
        tests: &["oracle"],
        note: "Oracle Database 23ai Free",
        ..BASE
    },
    // IBM Db2 Community Edition: privileged, ~4 GB, 5–10 minutes to
    // create the instance and the database `tck`.
    Container {
        key: "db2",
        name: "IBM Db2",
        group: Group::Jdbc,
        image: "icr.io/db2_community/db2:latest",
        versions: &["latest"],
        ports: &["55000:50000"],
        env: &[
            "LICENSE=accept",
            "DB2INSTANCE=db2inst1",
            "DB2INST1_PASSWORD=ferrodb",
            "DBNAME=tck",
            "BLU=false",
            "ENABLE_ORACLE_COMPATIBILITY=false",
            "UPDATEAVAIL=NO",
            "TO_CREATE_SAMPLEDB=false",
            "REPODB=false",
            "IS_OSXFS=false",
            "PERSISTENT_HOME=false",
            "HADR_ENABLED=false",
            "ARCHIVE_LOGS=false",
            "AUTOCONFIG=false",
        ],
        memory: Some("4g"),
        privileged: true,
        ready: Ready::Log("Setup has completed"),
        ready_secs: 1200,
        vars: &[("FERRODB_TCK_DB2", "db2://db2inst1:ferrodb@{h}:55000/tck")],
        tests: &["db2"],
        note: "Community Edition, privileged",
        ..BASE
    },
    // Exasol (single-node docker-db): privileged; TLS with a self-signed
    // certificate, so the TCK passes a full JDBC URL.
    Container {
        key: "exasol",
        name: "Exasol",
        group: Group::Jdbc,
        image: "exasol/docker-db:2025.1.17",
        versions: &["2025.1.17"],
        ports: &["58563:8563"],
        memory: Some("4g"),
        privileged: true,
        ready: Ready::Log("All stages finished"),
        ready_secs: 900,
        vars: &[(
            "FERRODB_TCK_EXASOL",
            "exasol://sys:exasol@{h}:58563/?url=jdbc:exa:{h}:58563;validateservercertificate=0",
        )],
        tests: &["exasol"],
        note: "privileged",
        ..BASE
    },
    // Apache Hive 4: HiveServer2 with an embedded Derby metastore.
    Container {
        key: "hive",
        name: "Apache Hive",
        group: Group::Jdbc,
        image: "apache/hive:4.2.1",
        versions: &["4.2.1"],
        ports: &["56100:10000"],
        env: &["SERVICE_NAME=hiveserver2"],
        memory: Some("2g"),
        ready_secs: 300,
        vars: &[("FERRODB_TCK_HIVE", "hive://hive@{h}:56100/default")],
        tests: &["hive"],
        note: "HiveServer2, embedded metastore",
        ..BASE
    },
    // Sybase ASE has no official image: JetBrains' DataGrip test image
    // (based on nguoianphu/docker-sybase) for the nightly run; the release
    // sign-off stays on the dedicated environment (plan, section 9).
    Container {
        key: "sybase",
        name: "Sybase ASE",
        group: Group::Jdbc,
        image: "datagrip/sybase:16.0",
        versions: &["16.0"],
        ports: &["55001:5000"],
        memory: Some("2g"),
        ready_secs: 300,
        vars: &[(
            "FERRODB_TCK_SYBASE",
            "sybase://sa:myPassword@{h}:55001/testdb",
        )],
        tests: &["sybase"],
        note: "unofficial image (JetBrains DataGrip tests)",
        ..BASE
    },
];

/// Pseudo-target of `tck --only` without a container: SQLite and the JDBC
/// databases embedded in the bridge (H2, Derby, HyperSQL).
pub const EMBEDDED: &str = "embedded";
const EMBEDDED_TESTS: &[&str] = &["sqlite", "h2", "derby", "hsqldb"];
/// Downloads the H2 / Derby / HyperSQL jars into
/// `target/ferrodb-drivers-test`.
const EMBEDDED_VARS: &[(&str, &str)] = &[("FERRODB_TCK_JDBC", "1")];

const NETWORK: &str = "ferrodb-tck";

pub fn container(key: &str) -> Option<&'static Container> {
    CONTAINERS.iter().find(|c| c.key == key)
}

impl Container {
    pub fn docker_name(&self) -> String {
        format!("ferrodb-tck-{}", self.key)
    }

    /// The repository part of the image (without the tag).
    pub fn repository(&self) -> &'static str {
        match self.image.rsplit_once(':') {
            Some((r, t)) if !t.contains('/') => r,
            _ => self.image,
        }
    }

    pub fn host_port(&self) -> u16 {
        self.ports
            .first()
            .and_then(|p| p.split(':').next())
            .and_then(|p| p.parse().ok())
            .unwrap_or(0)
    }
}

/// Options shared by `db-up`, `db-down`, `tck` and `env`.
#[derive(Debug, Default)]
pub struct Selection {
    /// Container keys (plus [`EMBEDDED`]); empty = everything.
    keys: Vec<String>,
    /// `key=image` replacements.
    images: Vec<(String, String)>,
    pub no_wait: bool,
    pub skip_meta: bool,
}

impl Selection {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut s = Selection::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = |flag: &str| {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match a.as_str() {
                "--only" => {
                    for k in value("--only")?.split(',').map(str::trim) {
                        if k != EMBEDDED && container(k).is_none() {
                            return Err(format!("unknown database `{k}` ({})", keys_help()));
                        }
                        s.keys.push(k.to_owned());
                    }
                }
                "--group" => {
                    let g = value("--group")?;
                    if g == "all" {
                        s.keys.extend(CONTAINERS.iter().map(|c| c.key.to_owned()));
                        s.keys.push(EMBEDDED.into());
                        continue;
                    }
                    let group = Group::ALL
                        .into_iter()
                        .find(|x| x.name() == g)
                        .ok_or_else(|| format!("unknown group `{g}` (native, nosql, jdbc, all)"))?;
                    s.keys.extend(
                        CONTAINERS
                            .iter()
                            .filter(|c| c.group == group)
                            .map(|c| c.key.to_owned()),
                    );
                    if group == Group::Jdbc {
                        s.keys.push(EMBEDDED.into());
                    }
                }
                "--image" => {
                    let v = value("--image")?;
                    let (k, img) = v
                        .split_once('=')
                        .ok_or("--image takes key=image (e.g. pg=postgres:14)")?;
                    if container(k).is_none() {
                        return Err(format!("unknown database `{k}` ({})", keys_help()));
                    }
                    s.images.push((k.to_owned(), img.to_owned()));
                }
                "--no-wait" => s.no_wait = true,
                "--skip-meta" => s.skip_meta = true,
                other => return Err(format!("unknown option `{other}`")),
            }
        }
        Ok(s)
    }

    fn all(&self) -> bool {
        self.keys.is_empty()
    }

    fn wants_embedded(&self) -> bool {
        self.all() || self.keys.iter().any(|k| k == EMBEDDED)
    }

    /// Selected containers with what they need, in table order.
    pub fn containers(&self) -> Vec<&'static Container> {
        let mut wanted: Vec<&str> = self.keys.iter().map(String::as_str).collect();
        let mut i = 0;
        while i < wanted.len() {
            if let Some(c) = container(wanted[i]) {
                for n in c.needs {
                    if !wanted.contains(n) {
                        wanted.push(n);
                    }
                }
            }
            i += 1;
        }
        CONTAINERS
            .iter()
            .filter(|c| self.all() || wanted.contains(&c.key))
            .collect()
    }

    fn image(&self, c: &Container) -> String {
        self.images
            .iter()
            .find(|(k, _)| k == c.key)
            .map_or_else(|| c.image.to_owned(), |(_, i)| i.clone())
    }

    /// Variables of the selected containers (and of the embedded JDBC
    /// databases when selected).
    pub fn vars(&self) -> Vec<(String, String)> {
        let h = host();
        let containers = self.containers();
        let mut v: Vec<(String, String)> = containers
            .iter()
            .flat_map(|c| c.vars.iter())
            .map(|(k, val)| ((*k).to_owned(), val.replace("{h}", &h)))
            .collect();
        // JDBC servers need their driver jars too.
        if self.wants_embedded() || containers.iter().any(|c| c.group == Group::Jdbc) {
            v.extend(
                EMBEDDED_VARS
                    .iter()
                    .map(|(k, val)| ((*k).to_owned(), (*val).to_owned())),
            );
        }
        v
    }

    /// `ferrodb-driver-tck` tests to run (`None`: all of them).
    fn tests(&self) -> Option<Vec<&'static str>> {
        if self.all() {
            return None;
        }
        let mut t: Vec<&'static str> = Vec::new();
        for k in &self.keys {
            let add: &[&'static str] = if k == EMBEDDED {
                EMBEDDED_TESTS
            } else {
                container(k).map_or(&[], |c| c.tests)
            };
            for name in add {
                if !t.contains(name) {
                    t.push(name);
                }
            }
        }
        Some(t)
    }
}

fn keys_help() -> String {
    let mut k: Vec<&str> = CONTAINERS.iter().map(|c| c.key).collect();
    k.push(EMBEDDED);
    k.join(", ")
}

fn docker() -> Command {
    let native = Command::new("docker")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if native || !cfg!(windows) {
        Command::new("docker")
    } else {
        let mut c = Command::new("wsl");
        c.args(["-e", "docker"]);
        c
    }
}

/// Host the published ports are reachable at (`FERRODB_TCK_HOST`
/// overrides). WSL forwards `localhost` for some ports only; the WSL
/// address always works.
fn host() -> String {
    if let Ok(h) = std::env::var("FERRODB_TCK_HOST") {
        return h;
    }
    if cfg!(windows) {
        let ip = Command::new("wsl")
            .args(["-e", "hostname", "-I"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.split_whitespace().next().map(str::to_owned));
        if let Some(ip) = ip {
            return ip;
        }
    }
    "localhost".into()
}

fn is_running(name: &str) -> bool {
    docker()
        .args(["ps", "-q", "-f", &format!("name=^{name}$")])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

pub fn up(sel: &Selection) -> Result<(), String> {
    let _ = docker().args(["network", "create", NETWORK]).output();
    let selected = sel.containers();
    for c in &selected {
        let name = c.docker_name();
        if is_running(&name) {
            eprintln!("{name}: running");
            continue;
        }
        let _ = docker().args(["rm", "-f", &name]).output();
        let mut cmd = docker();
        cmd.args(["run", "-d", "--name", &name, "--network", NETWORK]);
        for p in c.ports {
            cmd.args(["-p", p]);
        }
        for e in c.env {
            cmd.args(["-e", e]);
        }
        if let Some(m) = c.memory {
            cmd.args(["-m", m]);
        }
        if c.privileged {
            cmd.arg("--privileged");
        }
        cmd.arg(sel.image(c));
        cmd.args(c.args);
        crate::run(&mut cmd)?;
    }
    let mut failed = Vec::new();
    for c in &selected {
        let ok = match c.key {
            "mongo" => init_mongo_replica_set(),
            "couchbase" => init_couchbase(),
            _ if sel.no_wait => true,
            _ => wait_ready(c),
        };
        if !ok {
            failed.push(c.key);
        }
    }
    env_hint(sel);
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("not ready: {}", failed.join(", ")))
    }
}

fn check_ready(c: &Container, name: &str, host: &str) -> bool {
    match c.ready {
        Ready::Exec(cmd) => docker()
            .args(["exec", name])
            .args(cmd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false),
        Ready::Log(text) => docker()
            .args(["logs", name])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout).contains(text)
                    || String::from_utf8_lossy(&o.stderr).contains(text)
            })
            .unwrap_or(false),
        Ready::Tcp => (host, c.host_port())
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .is_some_and(|a| TcpStream::connect_timeout(&a, Duration::from_secs(2)).is_ok()),
        Ready::Init => true,
    }
}

/// Polls the container's readiness check until it passes or `ready_secs`
/// runs out.
#[allow(clippy::disallowed_methods)] // a command-line tool, not the UI thread
fn wait_ready(c: &Container) -> bool {
    let name = c.docker_name();
    let start = Instant::now();
    let h = host();
    eprintln!("{name}: waiting up to {} s", c.ready_secs);
    while start.elapsed() < Duration::from_secs(c.ready_secs) {
        if check_ready(c, &name, &h) {
            eprintln!("{name}: ready after {} s", start.elapsed().as_secs());
            return true;
        }
        if !is_running(&name) {
            eprintln!("{name}: the container stopped; `docker logs {name}` tells why");
            return false;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    eprintln!("{name}: not ready after {} s", c.ready_secs);
    false
}

/// `rs.initiate()` once mongod accepts connections (up to ~60 s).
#[allow(clippy::disallowed_methods)] // a command-line tool, not the UI thread
fn init_mongo_replica_set() -> bool {
    let script = "try { rs.status().ok } catch (e) {                   rs.initiate({_id: 'rs0', members: [{_id: 0, host: 'localhost:27017'}]}).ok }";
    for _ in 0..30 {
        let ok = docker()
            .args([
                "exec",
                "ferrodb-tck-mongo",
                "mongosh",
                "--quiet",
                "--eval",
                script,
            ])
            .output()
            .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains('1'))
            .unwrap_or(false);
        if ok {
            eprintln!("ferrodb-tck-mongo: replica set rs0 ready");
            return true;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    eprintln!("ferrodb-tck-mongo: replica set not initiated (is the container running?)");
    false
}

/// Couchbase: cluster initialisation (data, index, query services), bucket
/// `tck` (no replicas: transactions with durability on one node), scope
/// `app`. Each step is skipped when already done (up to ~90 s for the
/// server to start).
#[allow(clippy::disallowed_methods)] // a command-line tool, not the UI thread
fn init_couchbase() -> bool {
    let cli = |args: &[&str]| {
        let mut c = docker();
        c.args([
            "exec",
            "ferrodb-tck-couchbase",
            "/opt/couchbase/bin/couchbase-cli",
        ]);
        c.args(args);
        c.output()
            .map(|o| {
                let out = String::from_utf8_lossy(&o.stdout).into_owned()
                    + &String::from_utf8_lossy(&o.stderr);
                (o.status.success(), out)
            })
            .unwrap_or((false, String::new()))
    };
    let auth = ["-c", "localhost", "-u", "Administrator", "-p", "ferrodb"];
    let mut ready = false;
    for _ in 0..45 {
        let (ok, out) = cli(&[
            "cluster-init",
            "-c",
            "localhost",
            "--cluster-username",
            "Administrator",
            "--cluster-password",
            "ferrodb",
            "--services",
            "data,index,query",
            "--cluster-ramsize",
            "768",
            "--cluster-index-ramsize",
            "256",
            "--index-storage-setting",
            "default",
        ]);
        if ok || out.contains("already initialized") || out.contains("already been initialized") {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    if !ready {
        eprintln!("ferrodb-tck-couchbase: cluster not initialised (is the container running?)");
        return false;
    }
    let mut steps: Vec<Vec<&str>> = vec![
        vec![
            "bucket-create",
            "--bucket",
            "tck",
            "--bucket-type",
            "couchbase",
            "--storage-backend",
            "couchstore",
            "--bucket-ramsize",
            "256",
            "--bucket-replica",
            "0",
            "--wait",
        ],
        vec![
            "collection-manage",
            "--bucket",
            "tck",
            "--create-scope",
            "app",
        ],
    ];
    for s in &mut steps {
        s.extend_from_slice(&auth);
        let (ok, out) = cli(s);
        if !ok && !out.contains("already exists") {
            eprintln!("ferrodb-tck-couchbase: {}: {}", s[0], out.trim());
        }
    }
    eprintln!("ferrodb-tck-couchbase: cluster, bucket tck and scope app ready");
    true
}

pub fn down(sel: &Selection) -> Result<(), String> {
    for c in sel.containers() {
        let _ = crate::run(docker().args(["rm", "-f", &c.docker_name()]));
    }
    if sel.all() {
        let _ = docker().args(["network", "rm", NETWORK]).output();
    }
    Ok(())
}

/// `cargo xtask env`: the variables as `KEY=value` lines (for
/// `$GITHUB_ENV` or a shell).
pub fn print_env(sel: &Selection) -> Result<(), String> {
    for (k, v) in sel.vars() {
        println!("{k}={v}");
    }
    Ok(())
}

fn env_hint(sel: &Selection) {
    eprintln!("\nTCK variables:");
    for (k, v) in sel.vars() {
        eprintln!("  {k}={v}");
    }
    eprintln!("Run `cargo xtask tck` with the same --only / --group to run the suite.");
}

/// Runs the TCK (only the selected databases' tests) and the metadata round
/// trips with the container URLs.
pub fn tck(sel: &Selection) -> Result<(), String> {
    let packages: &[&str] = if sel.skip_meta {
        &["ferrodb-driver-tck"]
    } else {
        &["ferrodb-driver-tck", "ferrodb-meta"]
    };
    let tests = sel.tests();
    let vars = sel.vars();
    for package in packages {
        let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        c.args(["test", "-p", package]);
        let filter = tests.as_ref().filter(|_| *package == "ferrodb-driver-tck");
        if filter.is_some() {
            c.args(["--test", "tck"]);
        }
        c.args(["--", "--nocapture", "--test-threads=1"]);
        if let Some(t) = filter {
            c.arg("--exact");
            c.args(t);
        }
        for (k, v) in &vars {
            c.env(k, v);
        }
        crate::run(&mut c)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(args: &[&str]) -> Selection {
        Selection::parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn keys_are_unique_and_ports_do_not_clash() {
        let mut keys: Vec<_> = CONTAINERS.iter().map(|c| c.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), CONTAINERS.len());
        let mut ports: Vec<_> = CONTAINERS
            .iter()
            .flat_map(|c| c.ports.iter().map(|p| p.split(':').next().unwrap()))
            .collect();
        ports.sort_unstable();
        let n = ports.len();
        ports.dedup();
        assert_eq!(ports.len(), n, "two containers publish the same port");
    }

    #[test]
    fn default_image_is_one_of_the_versions() {
        for c in CONTAINERS {
            let tag = c.image.rsplit_once(':').map_or("latest", |(_, t)| t);
            assert!(c.versions.contains(&tag), "{}: {tag}", c.key);
        }
    }

    #[test]
    fn selection_by_key_group_and_needs() {
        let s = sel(&["--only", "ssh"]);
        let keys: Vec<_> = s.containers().iter().map(|c| c.key).collect();
        assert_eq!(keys, ["pg", "ssh"]);
        assert_eq!(s.tests().unwrap(), ["postgresql_via_ssh_tunnel"]);
        let s = sel(&["--group", "jdbc"]);
        assert!(s.containers().iter().all(|c| c.group == Group::Jdbc));
        assert!(s.tests().unwrap().contains(&"h2"));
        assert!(s.vars().iter().any(|(k, _)| k == "FERRODB_TCK_JDBC"));
        let s = sel(&["--only", "redis", "--image", "redis=redis:7.4"]);
        assert_eq!(s.image(container("redis").unwrap()), "redis:7.4");
        assert!(!s.vars().iter().any(|(k, _)| k == "FERRODB_TCK_JDBC"));
        let s = sel(&["--only", "oracle"]);
        assert!(s.vars().iter().any(|(k, _)| k == "FERRODB_TCK_JDBC"));
        assert!(Selection::parse(&["--only".into(), "nope".into()]).is_err());
        assert!(sel(&[]).tests().is_none());
    }

    #[test]
    fn repository_strips_the_tag_only() {
        assert_eq!(container("pg").unwrap().repository(), "postgres");
        assert_eq!(
            container("mssql").unwrap().repository(),
            "mcr.microsoft.com/mssql/server"
        );
    }
}
