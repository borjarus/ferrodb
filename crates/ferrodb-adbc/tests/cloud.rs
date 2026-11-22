//! Snowflake and BigQuery against real accounts, when configured (they are
//! cloud-only services). The driver library is found through
//! `FERRODB_ADBC_SNOWFLAKE` / `FERRODB_ADBC_BIGQUERY` (path of the library
//! extracted from the wheel, see `ferrodb_adbc::artifacts`).
//!
//! Snowflake: `FERRODB_TCK_SNOWFLAKE_ACCOUNT`, `…_USER`, `…_PASSWORD` (or
//! `…_PRIVATE_KEY_FILE`), optional `…_WAREHOUSE`, `…_DATABASE`, `…_SCHEMA`,
//! `…_ROLE`.
//!
//! BigQuery: `FERRODB_TCK_BIGQUERY_PROJECT`, `…_DATASET` (a scratch dataset
//! the TCK may create tables in), optional `…_LOCATION` and
//! `…_CREDENTIALS` (key file; default: application default credentials).

use std::sync::Arc;

use arrow_schema::{DataType, TimeUnit};
use ferrodb_adbc::{AdbcDriver, DefaultLocator, Flavor};
use ferrodb_api::ObjectKind;
use ferrodb_driver_tck::{Profile, run_except};
use ferrodb_drivers::{CancellationToken, Driver, Request, ResolvedConfig, sink::collect_request};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn check(d: &AdbcDriver, cfg: &ResolvedConfig, profile: &Profile, skip: &[(&'static str, &str)]) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let report = rt.block_on(run_except(d, cfg, profile, skip));
    let md = report.markdown();
    println!("{md}");
    assert!(report.passed(), "TCK failed:\n{md}");
}

#[test]
fn snowflake() {
    let Some(account) = env("FERRODB_TCK_SNOWFLAKE_ACCOUNT") else {
        return;
    };
    let mut cfg = ResolvedConfig::new("snowflake").with("account", account);
    for (var, prop) in [
        ("USER", "user"),
        ("WAREHOUSE", "warehouse"),
        ("DATABASE", "database"),
        ("SCHEMA", "schema"),
        ("ROLE", "role"),
        ("PRIVATE_KEY_FILE", "private_key_file"),
    ] {
        if let Some(v) = env(&format!("FERRODB_TCK_SNOWFLAKE_{var}")) {
            cfg = cfg.with(prop, v);
        }
    }
    if let Some(p) = env("FERRODB_TCK_SNOWFLAKE_PASSWORD") {
        cfg = cfg.with_secret("password", p);
    }
    let d = AdbcDriver::new(Flavor::Snowflake, Arc::new(DefaultLocator::default()));
    let profile = Profile {
        series: |n| format!("select seq8() + 1 as i from table(generator(rowcount => {n}))"),
        sleep: |s| format!("call system$wait({s})"),
        types_table: |t| {
            format!(
                "create table {t} (i integer, d decimal(10,2), s varchar(20), b boolean, dt date, ts timestamp_ntz)"
            )
        },
        types_row: "(42, 12.5, 'zażółć', true, '2026-11-09', '2026-11-09 10:11:12')",
        // NUMBER(38,0) with the driver's default high precision.
        types: [
            DataType::Decimal128(38, 0),
            DataType::Decimal128(10, 2),
            DataType::Utf8,
            DataType::Boolean,
            DataType::Date32,
            DataType::Timestamp(TimeUnit::Nanosecond, None),
        ],
        error_positions: true,
        container: ObjectKind::Schema,
    };
    check(&d, &cfg, &profile, &[]);
    // Session objects.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut c = d.connect(&cfg).await.unwrap();
        for m in ["warehouses", "roles", "current_context"] {
            let out = collect_request(
                c.as_mut(),
                Request::native(m, &[]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert!(out.results[0].num_rows() > 0, "{m}");
        }
    });
}

#[test]
fn bigquery() {
    let Some(project) = env("FERRODB_TCK_BIGQUERY_PROJECT") else {
        return;
    };
    let mut cfg = ResolvedConfig::new("bigquery").with("project", project);
    if let Some(d) = env("FERRODB_TCK_BIGQUERY_DATASET") {
        cfg = cfg.with("dataset", d);
    }
    if let Some(l) = env("FERRODB_TCK_BIGQUERY_LOCATION") {
        cfg = cfg.with("location", l);
    }
    if let Some(f) = env("FERRODB_TCK_BIGQUERY_CREDENTIALS") {
        cfg = cfg.with("credentials_file", f);
    }
    let d = AdbcDriver::new(Flavor::BigQuery, Arc::new(DefaultLocator::default()));
    let profile = Profile {
        series: |n| format!("select i from unnest(generate_array(1, {n})) as i"),
        // No sleep function: a large cross join stands in.
        sleep: |_| {
            "select count(*) from unnest(generate_array(1, 100000)) a, \
             unnest(generate_array(1, 100000)) b"
                .into()
        },
        types_table: |t| {
            format!(
                "create table {t} (i int64, d numeric(10,2), s string(20), b bool, dt date, ts datetime)"
            )
        },
        types_row: "(42, 12.5, 'zażółć', true, '2026-11-09', '2026-11-09 10:11:12')",
        types: [
            DataType::Int64,
            DataType::Decimal128(38, 9),
            DataType::Utf8,
            DataType::Boolean,
            DataType::Date32,
            DataType::Timestamp(TimeUnit::Microsecond, None),
        ],
        error_positions: true,
        container: ObjectKind::Dataset,
    };
    check(
        &d,
        &cfg,
        &profile,
        &[(
            "read-only",
            "BigQuery sessions are read-only only through IAM",
        )],
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut c = d.connect(&cfg).await.unwrap();
        let out = collect_request(
            c.as_mut(),
            Request::native(
                "dry_run",
                &[(
                    "sql",
                    "select word from `bigquery-public-data.samples.shakespeare`",
                )],
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.results[0].num_rows(), 1);
        let out = collect_request(
            c.as_mut(),
            Request::native("jobs", &[("limit", "5")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!out.results.is_empty());
    });
}

/// Without an account: the real libraries accept every option this crate
/// sets (an unknown option fails at `AdbcDatabaseSetOption`, before any
/// network access). Needs `FERRODB_ADBC_SNOWFLAKE` / `FERRODB_ADBC_BIGQUERY`.
#[test]
fn libraries_accept_the_options() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pem = include_str!("data/test_rsa_pkcs8.pem");
    let cases: Vec<(Flavor, &str, ResolvedConfig)> = vec![
        (
            Flavor::Snowflake,
            "FERRODB_ADBC_SNOWFLAKE",
            ResolvedConfig::new("snowflake")
                .with("account", "ferrodb-invalid-account")
                .with("warehouse", "WH")
                .with("database", "DB")
                .with("schema", "S")
                .with("role", "R")
                .with("user", "ME")
                .with("auth", "KeyPair")
                .with_secret("private_key", pem),
        ),
        (
            Flavor::BigQuery,
            "FERRODB_ADBC_BIGQUERY",
            ResolvedConfig::new("bigquery")
                .with("project", "ferrodb-invalid-project")
                .with("dataset", "ds")
                .with("location", "EU")
                .with_secret(
                    "credentials",
                    serde_json::json!({
                        "type": "service_account",
                        "project_id": "p",
                        "private_key": pem,
                        "client_email": "sa@p.iam.gserviceaccount.com",
                        "token_uri": "https://oauth2.googleapis.com/token",
                    })
                    .to_string(),
                ),
        ),
    ];
    for (flavor, var, cfg) in cases {
        if env(var).is_none() {
            continue;
        }
        let d = AdbcDriver::new(flavor.clone(), Arc::new(DefaultLocator::default()));
        let r = rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(60), d.connect(&cfg)).await
        });
        let msg = match r {
            Ok(Ok(_)) => "connected".to_owned(),
            Ok(Err(e)) => e.to_string(),
            Err(_) => "timed out".to_owned(),
        };
        println!("{}: {msg}", flavor.name());
        let lower = msg.to_ascii_lowercase();
        assert!(
            !lower.contains("option") && !lower.contains("not implemented"),
            "{}: {msg}",
            flavor.name()
        );
    }
}
