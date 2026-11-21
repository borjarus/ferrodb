//! Greenplum, CockroachDB and Redshift flavors: descriptors and version
//! parsing always; CockroachDB against a server when
//! `FERRODB_TCK_COCKROACH` holds a URL such as
//! `postgres://root@localhost:26257/defaultdb?ssl.mode=disable`.

use ferrodb_api::{AuthMethod, LanguageLevel};

use super::{conn::*, *};
use crate::{CancellationToken, sink::collect};

#[test]
fn flavors_match_the_catalog() {
    let ids: Vec<String> = drivers().iter().map(|d| d.id().to_string()).collect();
    assert_eq!(ids, ["postgresql", "greenplum", "cockroachdb", "redshift"]);
    let rs = PgDriver(&REDSHIFT).descriptor();
    assert_eq!(rs.default_port, Some(5439));
    assert_eq!(
        rs.capabilities.auth_methods,
        [AuthMethod::Password, AuthMethod::AwsIam]
    );
    assert_eq!(rs.capabilities.language_level, LanguageLevel::B);
    assert!(rs.properties.iter().any(|p| p.key == "aws.profile"));
    let user = rs.properties.iter().find(|p| p.key == "user").unwrap();
    assert!(user.auth.contains(&AuthMethod::AwsIam));
    let crdb = PgDriver(&COCKROACHDB).descriptor();
    assert_eq!(crdb.default_port, Some(26257));
    assert_eq!(
        crdb.capabilities.language,
        ferrodb_api::QueryLanguage::sql("cockroachdb")
    );
    assert!(!PgDriver(&COCKROACHDB).variant().quirks().transactional_ddl);
    assert!(!PgDriver(&REDSHIFT).variant().quirks().read_only_sessions);
}

#[test]
fn product_versions() {
    assert_eq!(
        crdb_version("CockroachDB CCL v24.3.1 (x86_64-pc-linux-gnu, built 2024/12/01, go1.22.8)"),
        "CockroachDB 24.3.1"
    );
    assert_eq!(
        product_version(
            "PostgreSQL 12.12 (Greenplum Database 7.1.0 build commit:e7c2b1f) on x86_64-pc-linux-gnu",
            Some("12.12")
        )
        .as_deref(),
        Some("Greenplum 7.1.0 (PostgreSQL 12.12)")
    );
    assert_eq!(
        product_version(
            "PostgreSQL 8.0.2 on i686-pc-linux-gnu, compiled by GCC gcc (GCC) 3.4.2 20041017 (Red Hat 3.4.2-6.fc3), Redshift 1.0.77467",
            Some("8.0.2")
        )
        .as_deref(),
        Some("Redshift 1.0.77467")
    );
    assert_eq!(product_version("PostgreSQL 16.1", None), None);
}

#[test]
fn listing_queries_per_flavor() {
    let path = [(ObjectKind::Schema, "public".to_owned())];
    let rs = redshift_list_queries(&path);
    assert_eq!(rs.len(), 2);
    assert!(rs[0].sql.contains("svv_all_tables"));
    assert!(rs[0].sql.contains("'EXTERNAL TABLE'"));
    let crdb = cockroach_list_queries(&[
        (ObjectKind::Schema, "public".to_owned()),
        (ObjectKind::Table, "t".to_owned()),
    ]);
    assert!(crdb[0].sql.contains("is_hidden = 'NO'"));
}

fn cockroach() -> Option<ResolvedConfig> {
    let url = std::env::var("FERRODB_TCK_COCKROACH").ok()?;
    Some(ResolvedConfig::from_url("cockroachdb", &url).unwrap())
}

#[tokio::test]
async fn cockroach_retries_and_versions() {
    let Some(cfg) = cockroach() else { return };
    let mut c = PgDriver(&COCKROACHDB).connect(&cfg).await.unwrap();
    assert!(
        c.server_version().starts_with("CockroachDB "),
        "{}",
        c.server_version()
    );
    // `SHOW` statements stream like queries.
    let out = collect(c.as_mut(), "show databases", CancellationToken::new())
        .await
        .unwrap();
    assert!(out.results[0].num_rows() >= 1);
    // A forced retry error: auto-commit retries `crdb_internal.force_retry`
    // until the interval elapses; inside a transaction it is reported with
    // advice.
    // CockroachDB 25+ guards crdb_internal behind a session variable.
    let _ = collect(
        c.as_mut(),
        "set allow_unsafe_internals = true",
        CancellationToken::new(),
    )
    .await;
    c.begin().await.unwrap();
    // Once a result reached the client the server cannot retry by itself.
    collect(c.as_mut(), "select 1", CancellationToken::new())
        .await
        .unwrap();
    let e = collect(
        c.as_mut(),
        "select crdb_internal.force_retry('10s')",
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    c.rollback().await.unwrap();
    let ferrodb_api::Error::Query(q) = e else {
        panic!("{e:?}")
    };
    assert_eq!(q.code.as_deref(), Some("40001"), "{q:?}");
    assert!(q.message.contains("ROLLBACK and run"), "{}", q.message);
    // The PostgreSQL driver recognizes CockroachDB as well.
    let pg = ResolvedConfig {
        driver: "postgresql".into(),
        ..cfg.clone()
    };
    let c2 = PgDriver(&POSTGRESQL).connect(&pg).await.unwrap();
    assert!(c2.server_version().starts_with("CockroachDB"));
}
