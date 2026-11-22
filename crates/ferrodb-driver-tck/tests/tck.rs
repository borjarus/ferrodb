//! Runs the TCK against every phase-3 database.
//!
//! SQLite always runs (temporary file). Server databases run when their URL
//! is set (see `cargo xtask db-up`, which starts the containers and prints
//! these variables):
//!
//! - `FERRODB_TCK_POSTGRES=postgres://postgres:ferrodb@localhost:55432/postgres?ssl.mode=disable`
//! - `FERRODB_TCK_MYSQL=mysql://root:ferrodb@localhost:53306/tck`
//! - `FERRODB_TCK_MARIADB=mariadb://root:ferrodb@localhost:53307/tck`
//! - `FERRODB_TCK_MONGODB=mongodb://localhost:57017/tck?directConnection=true`
//! - `FERRODB_TCK_DYNAMODB=http://localhost:56520` (DynamoDB Local)
//! - `FERRODB_TCK_COUCHBASE=couchbase://Administrator:ferrodb@localhost:56503/tck?mgmt_port=56501&scope=app`
//! - `FERRODB_TCK_SQLSERVER=sqlserver://sa:Ferrodb#2026@localhost:56233/master`
//! - `FERRODB_TCK_COCKROACH=postgres://root@localhost:56257/defaultdb?ssl.mode=disable`
//! - `FERRODB_TCK_GREENPLUM=postgres://gpadmin@localhost:56254/postgres?ssl.mode=disable`
//! - cloud only (nightly CI with secrets): `FERRODB_TCK_AZURESQL=sqlserver://user:pw@srv.database.windows.net:1433/db`,
//!   `FERRODB_TCK_REDSHIFT=redshift://user:pw@cluster.xxx.region.redshift.amazonaws.com:5439/dev`
//!
//! JDBC databases embedded in the bridge (H2, Derby, HyperSQL) run when
//! Java ≥ 17 and `ferrodb-jdbc-host.jar` are present and their jars are
//! installed in `target/ferrodb-drivers-test` (`FERRODB_TCK_JDBC=1`
//! downloads them).
//!
//! Each report is also written to `target/tck/<driver>.md`.

use ferrodb_driver_tck::{Profile, Report, profiles, run};
use ferrodb_drivers::{Registry, ResolvedConfig};

fn check(driver: &str, cfg: ResolvedConfig, profile: Profile) {
    let registry = Registry::builtin();
    let d = registry.get(&driver.into()).expect("driver is built in");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let report: Report = rt.block_on(run(d.as_ref(), &cfg, &profile));
    let md = report.markdown();
    println!("{md}");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tck");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{driver}.md")), &md);
    assert!(report.passed(), "TCK failed for {driver}:\n{md}");
}

fn from_env(var: &str, driver: &str) -> Option<ResolvedConfig> {
    let url = std::env::var(var).ok()?;
    Some(ResolvedConfig::from_url(driver, &url).expect("valid URL"))
}

#[test]
fn sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tck.sqlite");
    let cfg = ResolvedConfig::new("sqlite").with("file", file.to_string_lossy());
    check("sqlite", cfg, profiles::sqlite());
}

#[test]
fn postgresql() {
    if let Some(cfg) = from_env("FERRODB_TCK_POSTGRES", "postgresql") {
        check("postgresql", cfg, profiles::postgresql());
    }
}

#[test]
fn mysql() {
    if let Some(cfg) = from_env("FERRODB_TCK_MYSQL", "mysql") {
        check("mysql", cfg, profiles::mysql());
    }
}

/// The document-model TCK: `FERRODB_TCK_MONGODB=mongodb://host:57017/tck?directConnection=true`.
#[test]
fn mongodb() {
    let Ok(url) = std::env::var("FERRODB_TCK_MONGODB") else {
        return;
    };
    let cfg = ResolvedConfig::new("mongodb").with("uri", url);
    let registry = Registry::builtin();
    let d = registry.get(&"mongodb".into()).expect("driver is built in");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let report = rt.block_on(ferrodb_driver_tck::run_documents(d.as_ref(), &cfg));
    let md = report.markdown();
    println!("{md}");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tck");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("mongodb.md"), &md);
    assert!(
        report.passed(),
        "TCK failed for mongodb:
{md}"
    );
}

fn documents(driver: &str, cfg: ResolvedConfig) {
    let registry = Registry::builtin();
    let d = registry.get(&driver.into()).expect("driver is built in");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let report = rt.block_on(ferrodb_driver_tck::run_documents(d.as_ref(), &cfg));
    let md = report.markdown();
    println!("{md}");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tck");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{driver}.md")), &md);
    assert!(
        report.passed(),
        "TCK failed for {driver}:
{md}"
    );
}

/// DynamoDB Local: `FERRODB_TCK_DYNAMODB=http://localhost:56520`.
#[test]
fn dynamodb() {
    let Ok(endpoint) = std::env::var("FERRODB_TCK_DYNAMODB") else {
        return;
    };
    let cfg = ResolvedConfig::new("dynamodb")
        .with("region", "eu-central-1")
        .with("endpoint", endpoint)
        .with_secret("aws.access_key_id", "tck")
        .with_secret("aws.secret_access_key", "tck");
    documents("dynamodb", cfg);
}

/// Couchbase Server (bucket `tck` with scope `app`, see `cargo xtask
/// db-up`): `FERRODB_TCK_COUCHBASE=couchbase://user:pw@host:56503/tck?mgmt_port=56501&scope=app`.
#[test]
fn couchbase() {
    let Some(cfg) = from_env("FERRODB_TCK_COUCHBASE", "couchbase") else {
        return;
    };
    documents("couchbase", cfg.with("scan_consistency", "request_plus"));
}

#[test]
fn sqlserver() {
    if let Some(cfg) = from_env("FERRODB_TCK_SQLSERVER", "sqlserver") {
        check("sqlserver", cfg, profiles::sqlserver());
    }
}

/// Azure SQL with a SQL login, or with Microsoft Entra ID when
/// `FERRODB_TCK_AZURESQL_ENTRA=1` (token from the Azure CLI or the
/// `entra.*` properties in the URL query).
#[test]
fn azuresql() {
    if let Some(mut cfg) = from_env("FERRODB_TCK_AZURESQL", "azuresql") {
        cfg.tls = ferrodb_net::TlsMode::VerifyFull;
        if std::env::var("FERRODB_TCK_AZURESQL_ENTRA").is_ok() {
            cfg = cfg.with("auth", "EntraId");
        }
        check("azuresql", cfg, profiles::sqlserver());
    }
}

#[test]
fn cockroachdb() {
    if let Some(cfg) = from_env("FERRODB_TCK_COCKROACH", "cockroachdb") {
        check("cockroachdb", cfg, profiles::cockroachdb());
    }
}

#[test]
fn greenplum() {
    if let Some(cfg) = from_env("FERRODB_TCK_GREENPLUM", "greenplum") {
        check("greenplum", cfg, profiles::postgresql_derivative());
    }
}

/// Redshift with a password, or IAM credentials when
/// `FERRODB_TCK_REDSHIFT_IAM=1` (AWS profile / environment).
#[test]
fn redshift() {
    if let Some(mut cfg) = from_env("FERRODB_TCK_REDSHIFT", "redshift") {
        cfg.tls = ferrodb_net::TlsMode::Require;
        if std::env::var("FERRODB_TCK_REDSHIFT_IAM").is_ok() {
            cfg = cfg.with("auth", "AwsIam");
        }
        check("redshift", cfg, profiles::postgresql_derivative());
    }
}

#[test]
fn mariadb() {
    if let Some(cfg) = from_env("FERRODB_TCK_MARIADB", "mariadb") {
        check("mariadb", cfg, profiles::mysql());
    }
}

/// The PostgreSQL TCK through an SSH tunnel (bastion container on the same
/// Docker network): `FERRODB_TCK_SSH=ssh://ferrodb:ferrodb@<host>:52222`,
/// database reached as `ferrodb-tck-pg:5432` from the bastion.
#[test]
fn postgresql_via_ssh_tunnel() {
    let Ok(ssh) = std::env::var("FERRODB_TCK_SSH") else {
        return;
    };
    let s = ResolvedConfig::from_url("ssh", &ssh).expect("valid SSH URL");
    let known_hosts = tempfile::tempdir().unwrap();
    let cfg = ResolvedConfig {
        ssh: Some(ferrodb_net::SshConfig {
            host: s.host().to_owned(),
            port: s.port(22).unwrap(),
            user: s.user().unwrap_or("ferrodb").to_owned(),
            auth: ferrodb_net::SshAuth::Password(s.password().unwrap_or_default().to_owned()),
            known_hosts: Some(known_hosts.path().join("known_hosts")),
        }),
        ..ResolvedConfig::new("postgresql")
            .with("host", "ferrodb-tck-pg")
            .with("port", "5432")
            .with("user", "postgres")
            .with("database", "postgres")
            .with_secret("password", "ferrodb")
    };
    let cfg = ResolvedConfig {
        tls: ferrodb_net::TlsMode::Disable,
        ..cfg
    };
    check("postgresql", cfg, profiles::postgresql());
}

/// The key-value TCK: `FERRODB_TCK_REDIS=redis://localhost:56401/0` (add
/// `?mode=cluster` with the seeds as host for a cluster).
#[test]
fn redis() {
    let Some(cfg) = from_env("FERRODB_TCK_REDIS", "redis") else {
        return;
    };
    key_value("redis", cfg);
}

/// Redis Cluster: `FERRODB_TCK_REDIS_CLUSTER=redis://127.0.0.1:56411?mode=cluster`.
#[test]
fn redis_cluster() {
    let Some(cfg) = from_env("FERRODB_TCK_REDIS_CLUSTER", "redis") else {
        return;
    };
    key_value("redis-cluster", cfg.with("mode", "cluster"));
}

/// Redis behind Sentinel: `FERRODB_TCK_REDIS_SENTINEL=redis://127.0.0.1:56421?sentinel.master=mymaster`.
#[test]
fn redis_sentinel() {
    let Some(cfg) = from_env("FERRODB_TCK_REDIS_SENTINEL", "redis") else {
        return;
    };
    key_value("redis-sentinel", cfg.with("mode", "sentinel"));
}

fn key_value(name: &str, cfg: ResolvedConfig) {
    let registry = Registry::builtin();
    let d = registry.get(&"redis".into()).expect("driver is built in");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let report = rt.block_on(ferrodb_driver_tck::run_key_value(d.as_ref(), &cfg));
    let md = report.markdown();
    println!("{md}");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tck");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{name}.md")), &md);
    assert!(
        report.passed(),
        "TCK failed for {name}:
{md}"
    );
}

/// Apache Cassandra on the tabular TCK with CQL adjustments:
/// `FERRODB_TCK_CASSANDRA=cassandra://localhost:56442/tck` (the keyspace is
/// created when missing).
#[test]
fn cassandra() {
    let Some(cfg) = from_env("FERRODB_TCK_CASSANDRA", "cassandra") else {
        return;
    };
    let registry = Registry::builtin();
    let d = registry
        .get(&"cassandra".into())
        .expect("driver is built in");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ks = cfg.database().unwrap_or("tck").to_owned();
    rt.block_on(async {
        let mut admin = cfg.clone();
        admin.properties.remove("database");
        let mut c = d.connect(&admin).await.expect("connect without keyspace");
        ferrodb_drivers::sink::collect(
            c.as_mut(),
            format!(
                "create keyspace if not exists {ks} with replication = {{'class': 'SimpleStrategy', 'replication_factor': 1}}"
            ),
            ferrodb_drivers::CancellationToken::new(),
        )
        .await
        .expect("create the TCK keyspace");
    });
    let report = rt.block_on(ferrodb_driver_tck::run_adjusted(
        d.as_ref(),
        &cfg,
        &profiles::cassandra(),
        &profiles::cassandra_adjustments(),
    ));
    let md = report.markdown();
    println!("{md}");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tck");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("cassandra.md"), &md);
    assert!(report.passed(), "TCK failed for cassandra:\n{md}");
}

/// The TCK on a JDBC flavor through the bridge, embedded in memory.
fn check_jdbc(id: &str, user: &str, profile: Profile) {
    let cfg = ResolvedConfig::new(id)
        .with("mode", "memory")
        .with("database", format!("tck_{id}"))
        .with("user", user);
    check_jdbc_with(id, cfg, profile);
}

fn check_jdbc_with(id: &str, cfg: ResolvedConfig, profile: Profile) {
    let bridge = match ferrodb_jdbc::testing::bridge(&[id]) {
        Ok(b) => b,
        Err(why) => {
            eprintln!("{id}: skipped: {why}");
            return;
        }
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let d = ferrodb_jdbc::JdbcDriver::new(ferrodb_jdbc::flavor(id).unwrap(), bridge.clone());
    let report: Report = rt.block_on(run(&d, &cfg, &profile));
    rt.block_on(bridge.shutdown());
    let md = report.markdown();
    println!("{md}");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tck");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{id}.md")), &md);
    assert!(
        report.passed(),
        "TCK failed for {id}:
{md}"
    );
}

#[test]
fn h2() {
    check_jdbc("h2", "sa", profiles::h2());
}

#[test]
fn derby() {
    check_jdbc("derby", "app", profiles::derby());
}

#[test]
fn hsqldb() {
    check_jdbc("hsqldb", "SA", profiles::hsqldb());
}

/// Oracle through the bridge: `FERRODB_TCK_ORACLE=oracle://tck:ferrodb@localhost:56121/FREEPDB1`.
#[test]
fn oracle() {
    if let Some(cfg) = from_env("FERRODB_TCK_ORACLE", "oracle") {
        check_jdbc_with("oracle", cfg, profiles::oracle());
    }
}
