//! Integration-test databases (plan, section 9 and Appendix D): containers
//! for the TCK, started with Docker. On Windows Docker usually runs inside
//! WSL, so commands go through `wsl -e docker` when `docker` is not on PATH.

use std::process::Command;

/// (container, image, port mapping, env, command arguments)
type Container = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

const CONTAINERS: &[Container] = &[
    (
        "ferrodb-tck-pg",
        "postgres:18",
        "55432:5432",
        &["POSTGRES_PASSWORD=ferrodb"],
        &[],
    ),
    (
        "ferrodb-tck-mysql",
        "mysql:8.4",
        "53306:3306",
        &["MYSQL_ROOT_PASSWORD=ferrodb", "MYSQL_DATABASE=tck"],
        &[],
    ),
    (
        "ferrodb-tck-mariadb",
        "mariadb:11.4",
        "53307:3306",
        &["MARIADB_ROOT_PASSWORD=ferrodb", "MARIADB_DATABASE=tck"],
        &[],
    ),
    // A one-member replica set: transactions need one (phase 4b).
    (
        "ferrodb-tck-mongo",
        "mongo:8.0",
        "57017:27017",
        &[],
        &["--replSet", "rs0", "--bind_ip_all"],
    ),
    (
        "ferrodb-tck-ssh",
        "linuxserver/openssh-server:latest",
        "52222:2222",
        &[
            "PUID=1000",
            "PGID=1000",
            "USER_NAME=ferrodb",
            "USER_PASSWORD=ferrodb",
            "PASSWORD_ACCESS=true",
            "DOCKER_MODS=linuxserver/mods:openssh-server-ssh-tunnel",
        ],
        &[],
    ),
];

const NETWORK: &str = "ferrodb-tck";

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

/// Host the published ports are reachable at. WSL forwards `localhost` for
/// some ports only; the WSL address always works.
fn host() -> String {
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

pub fn up() -> Result<(), String> {
    let _ = docker().args(["network", "create", NETWORK]).output();
    for (name, image, ports, env, args) in CONTAINERS {
        let running = docker()
            .args(["ps", "-q", "-f", &format!("name=^{name}$")])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);
        if running {
            eprintln!("{name}: running");
            continue;
        }
        let _ = docker().args(["rm", "-f", name]).output();
        let mut c = docker();
        c.args([
            "run",
            "-d",
            "--name",
            name,
            "--network",
            NETWORK,
            "-p",
            ports,
        ]);
        for e in *env {
            c.args(["-e", e]);
        }
        c.arg(image);
        c.args(*args);
        crate::run(&mut c)?;
    }
    init_mongo_replica_set();
    env_hint();
    Ok(())
}

/// `rs.initiate()` once mongod accepts connections (up to ~60 s).
#[allow(clippy::disallowed_methods)] // a command-line tool, not the UI thread
fn init_mongo_replica_set() {
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
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    eprintln!("ferrodb-tck-mongo: replica set not initiated (is the container running?)");
}

pub fn down() -> Result<(), String> {
    for (name, ..) in CONTAINERS {
        let _ = crate::run(docker().args(["rm", "-f", name]));
    }
    let _ = docker().args(["network", "rm", NETWORK]).output();
    Ok(())
}

pub fn env_vars() -> Vec<(String, String)> {
    let h = host();
    vec![
        (
            "FERRODB_TCK_POSTGRES".into(),
            format!("postgres://postgres:ferrodb@{h}:55432/postgres?ssl.mode=disable"),
        ),
        (
            "FERRODB_TCK_MYSQL".into(),
            format!("mysql://root:ferrodb@{h}:53306/tck"),
        ),
        (
            "FERRODB_TCK_MARIADB".into(),
            format!("mariadb://root:ferrodb@{h}:53307/tck"),
        ),
        (
            "FERRODB_TCK_MONGODB".into(),
            format!("mongodb://{h}:57017/tck?directConnection=true"),
        ),
        (
            "FERRODB_TEST_SSH".into(),
            format!("ssh://ferrodb:ferrodb@{h}:52222"),
        ),
        (
            "FERRODB_TCK_SSH".into(),
            format!("ssh://ferrodb:ferrodb@{h}:52222"),
        ),
    ]
}

fn env_hint() {
    eprintln!("\nContainers need ~30 s to accept connections. TCK variables:");
    for (k, v) in env_vars() {
        eprintln!("  {k}={v}");
    }
    eprintln!("Run `cargo xtask tck` to run the suite against them.");
}

/// Runs the TCK and the metadata round trips with the container URLs.
pub fn tck() -> Result<(), String> {
    for package in ["ferrodb-driver-tck", "ferrodb-meta"] {
        let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        c.args([
            "test",
            "-p",
            package,
            "--",
            "--nocapture",
            "--test-threads=1",
        ]);
        for (k, v) in env_vars() {
            c.env(k, v);
        }
        crate::run(&mut c)?;
    }
    Ok(())
}
