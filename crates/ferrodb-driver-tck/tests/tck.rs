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
