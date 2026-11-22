//! The ADBC layer end to end with the ADBC SQLite driver: the library is
//! extracted from the wheel in the repository root (phase-0 spike), loaded
//! through `adbc_driver_manager` and run through the driver TCK (tabular
//! suite) plus the native metadata calls. Skipped when this platform has no
//! wheel in the repository.

use std::{path::PathBuf, sync::Arc};

use arrow_schema::DataType;
use ferrodb_adbc::{AdbcDriver, DefaultLocator, Flavor, artifacts};
use ferrodb_api::Error;
use ferrodb_driver_tck::{Profile, profiles, run_except};
use ferrodb_drivers::{CancellationToken, Driver, Request, ResolvedConfig, sink::collect_request};

fn wheel() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let name = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "adbc_driver_sqlite-1.13.0-py3-none-win_amd64.whl",
        ("linux", "x86_64") => "adbc_driver_sqlite-1.13.0-py3-none-manylinux_2_28_x86_64.whl",
        ("macos", "aarch64") => "adbc_driver_sqlite-1.8.0-py3-none-macosx_11_0_arm64.whl",
        _ => return None,
    };
    let p = root.join(name);
    p.is_file().then_some(p)
}

/// The SQLite flavor with its library extracted into `dir`.
fn driver(dir: &std::path::Path) -> Option<AdbcDriver> {
    let wheel = wheel()?;
    artifacts::extract_library(
        &wheel,
        "adbc_driver_sqlite/libadbc_driver_sqlite.so",
        dir,
        &artifacts::library_file_name("adbc_driver_sqlite", std::env::consts::OS),
    )
    .unwrap();
    Some(AdbcDriver::new(
        Flavor::generic(
            "adbc-sqlite",
            "SQLite (ADBC)",
            "sqlite",
            "adbc_driver_sqlite",
        ),
        Arc::new(DefaultLocator::new(vec![dir.to_owned()])),
    ))
}

/// The SQLite profile with the types the ADBC driver infers: SQLite
/// values carry no declared type, so booleans are integers.
fn profile() -> Profile {
    Profile {
        types: [
            DataType::Int64,
            DataType::Float64,
            DataType::Utf8,
            DataType::Int64,
            DataType::Utf8,
            DataType::Utf8,
        ],
        ..profiles::sqlite()
    }
}

#[test]
fn tck_through_adbc() {
    let lib = tempfile::tempdir().unwrap();
    let Some(d) = driver(lib.path()) else { return };
    let db = tempfile::tempdir().unwrap();
    let file = db.path().join("tck.sqlite");
    let cfg = ResolvedConfig::new("adbc-sqlite").with("uri", file.to_string_lossy());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let report = rt.block_on(run_except(
        &d,
        &cfg,
        &profile(),
        &[(
            "cancel",
            "the ADBC SQLite driver has no AdbcStatementCancel (see the test cancel_without_driver_support_gives_up_the_connection)",
        )],
    ));
    let md = report.markdown();
    println!("{md}");
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tck");
    let _ = std::fs::create_dir_all(&out);
    let _ = std::fs::write(out.join("adbc-sqlite.md"), &md);
    assert!(report.passed(), "TCK failed through ADBC:\n{md}");
}

#[test]
fn native_metadata_calls() {
    let lib = tempfile::tempdir().unwrap();
    let Some(d) = driver(lib.path()) else { return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let cfg = ResolvedConfig::new("adbc-sqlite").with("uri", ":memory:");
        let mut c = d.connect(&cfg).await.unwrap();
        assert!(
            c.server_version().contains("SQLite"),
            "{}",
            c.server_version()
        );
        let cancel = CancellationToken::new();
        ferrodb_drivers::sink::collect(
            c.as_mut(),
            "create table orders (id integer primary key, note text)",
            cancel.clone(),
        )
        .await
        .unwrap();
        let out = collect_request(
            c.as_mut(),
            Request::native("get_objects", &[("depth", "columns")]),
            cancel.clone(),
        )
        .await
        .unwrap();
        let rows = &out.results[0];
        assert_eq!(rows.schema.field(2).name(), "table_name");
        let b = &rows.batches[0];
        let col = |i: usize| {
            b.column(i)
                .as_any()
                .downcast_ref::<arrow_array::StringArray>()
                .unwrap()
                .clone()
        };
        let (tables, columns) = (col(2), col(4));
        let found: Vec<(String, String)> = (0..b.num_rows())
            .map(|i| (tables.value(i).to_owned(), columns.value(i).to_owned()))
            .collect();
        assert!(
            found.contains(&("orders".into(), "note".into())),
            "{found:?}"
        );

        let out = collect_request(
            c.as_mut(),
            Request::native("get_table_schema", &[("table", "orders")]),
            cancel.clone(),
        )
        .await
        .unwrap();
        assert_eq!(out.results[0].num_rows(), 2);
        let out = collect_request(c.as_mut(), Request::native("get_info", &[]), cancel.clone())
            .await
            .unwrap();
        assert!(out.results[0].num_rows() >= 2);
        let e = collect_request(
            c.as_mut(),
            Request::native("dry_run", &[("sql", "select 1")]),
            cancel,
        )
        .await
        .unwrap_err();
        assert!(matches!(e, Error::Unsupported(_)), "{e:?}");
    });
}

#[test]
fn missing_library_is_reported() {
    let empty = tempfile::tempdir().unwrap();
    let d = AdbcDriver::new(
        Flavor::Snowflake,
        Arc::new(DefaultLocator::new(vec![empty.path().to_owned()])),
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    let cfg = ResolvedConfig::new("snowflake").with("account", "x");
    let e = rt.block_on(d.connect(&cfg)).err().unwrap();
    assert_eq!(e, Error::DriverMissing("snowflake".into()));
}

/// The ADBC SQLite driver does not implement `AdbcStatementCancel`: the
/// statement is abandoned on its thread, the call returns `Cancelled`
/// promptly and the connection reports itself dead (the core reconnects).
#[test]
fn cancel_without_driver_support_gives_up_the_connection() {
    let lib = tempfile::tempdir().unwrap();
    let Some(d) = driver(lib.path()) else { return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let cfg = ResolvedConfig::new("adbc-sqlite").with("uri", ":memory:");
        let mut c = d.connect(&cfg).await.unwrap();
        let token = CancellationToken::new();
        let t2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            t2.cancel();
        });
        let started = std::time::Instant::now();
        let r = ferrodb_drivers::sink::collect(
            c.as_mut(),
            "with recursive r(i) as (select 1 union all select i + 1 from r) select count(*) from r",
            token,
        )
        .await;
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(!c.is_alive());
        assert_eq!(c.ping().await, Err(Error::ConnectionLost));
    });
}
