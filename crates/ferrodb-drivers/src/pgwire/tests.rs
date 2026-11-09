//! Integration tests against a real server; they run only when
//! `FERRODB_TEST_POSTGRES` holds a URL such as
//! `postgres://postgres:secret@localhost:5432/postgres?ssl.mode=disable`.

use std::time::{Duration, Instant};

use arrow_array::{
    Array, BinaryArray, BooleanArray, Date32Array, Decimal128Array, Float32Array, Float64Array,
    Int16Array, Int32Array, Int64Array, RecordBatch, StringArray, Time64MicrosecondArray,
    TimestampMicrosecondArray,
};
use arrow_schema::DataType;
use ferrodb_api::{Error, NoticeLevel, ObjectKind};
use ferrodb_net::TlsMode;

use super::*;
use crate::{
    CancellationToken, Connection, Request, ResultSink, SinkEvent,
    columns::DB_TYPE_KEY,
    sink::{CollectedOutput, collect},
};

fn url() -> Option<String> {
    std::env::var("FERRODB_TEST_POSTGRES")
        .ok()
        .filter(|u| !u.is_empty())
}

fn config() -> Option<ResolvedConfig> {
    Some(ResolvedConfig::from_url("postgresql", &url()?).unwrap())
}

async fn open() -> Option<Box<dyn Connection>> {
    let cfg = config()?;
    Some(PgDriver(&POSTGRESQL).connect(&cfg).await.unwrap())
}

async fn run(c: &mut Box<dyn Connection>, sql: &str) -> CollectedOutput {
    collect(c.as_mut(), sql, CancellationToken::new())
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"))
}

fn unique(prefix: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}_{}_{}", std::process::id(), n % 1_000_000_000)
}

fn col<'a, T: 'static>(b: &'a RecordBatch, name: &str) -> &'a T {
    let i = b.schema().index_of(name).unwrap();
    b.column(i)
        .as_any()
        .downcast_ref::<T>()
        .unwrap_or_else(|| panic!("{name}: {:?}", b.column(i).data_type()))
}

fn text<'a>(b: &'a RecordBatch, name: &str) -> &'a str {
    col::<StringArray>(b, name).value(0)
}

#[tokio::test]
async fn pg_connects_and_reports_version() {
    let Some(c) = open().await else { return };
    assert!(
        c.server_version().starts_with("PostgreSQL "),
        "{}",
        c.server_version()
    );
    assert!(c.is_alive());
}

#[tokio::test]
async fn pg_types_round_trip() {
    let Some(mut c) = open().await else { return };
    run(&mut c, "set timezone = 'UTC'").await;
    let enum_ty = unique("mood");
    run(
        &mut c,
        &format!("create type {enum_ty} as enum ('sad', 'happy')"),
    )
    .await;
    let out = run(
        &mut c,
        &format!(
            "select true as b, 7::int2 as i2, 42::int4 as i4, 9000000000::int8 as i8, 12::oid as o,
             1.5::float4 as f4, 2.25::float8 as f8,
             12345.67::numeric(10,2) as dec,
             '123456789012345678901234567890.123456789012345'::numeric as big,
             'NaN'::numeric as nan,
             -0.000123::numeric as small,
             'hello'::text as t, 'v'::varchar(5) as vc, 'c'::char(3) as bp, 'nm'::name as nm,
             '<a/>'::xml as x,
             '\\xdeadbeef'::bytea as by,
             '2024-02-29'::date as d, 'infinity'::date as dinf,
             '13:14:15.5'::time as tm, '13:14:15+02'::timetz as ttz,
             '1 year 2 mons 3 days 04:05:06.5'::interval as iv,
             '2024-01-02 03:04:05.123456'::timestamp as ts,
             '2024-01-02 03:04:05+02'::timestamptz as tstz,
             'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid as u,
             '{{\"a\": [1, 2]}}'::json as js, '{{\"a\": [1, 2]}}'::jsonb as jb,
             'happy'::{enum_ty} as en,
             array[1, 2, null]::int4[] as arr,
             array[['a', 'b c'], ['', null]]::text[] as arr2,
             '192.168.0.1/24'::inet as ip, '10.0.0.0/8'::cidr as net,
             '12.34'::money as m, point(1.5, 2) as pt,
             int4range(1, 5) as rg, row(1, 'x y') as rec"
        ),
    )
    .await;
    let b = &out.results[0].batches[0];
    let schema = b.schema();
    let dbt = |n: &str| {
        schema
            .field_with_name(n)
            .unwrap()
            .metadata()
            .get(DB_TYPE_KEY)
            .cloned()
            .unwrap()
    };
    assert!(col::<BooleanArray>(b, "b").value(0));
    assert_eq!(col::<Int16Array>(b, "i2").value(0), 7);
    assert_eq!(col::<Int32Array>(b, "i4").value(0), 42);
    assert_eq!(col::<Int64Array>(b, "i8").value(0), 9_000_000_000);
    assert_eq!(col::<Int64Array>(b, "o").value(0), 12);
    assert_eq!(col::<Float32Array>(b, "f4").value(0), 1.5);
    assert_eq!(col::<Float64Array>(b, "f8").value(0), 2.25);
    let dec = col::<Decimal128Array>(b, "dec");
    assert_eq!(dec.data_type(), &DataType::Decimal128(10, 2));
    assert_eq!(dec.value(0), 1_234_567);
    assert_eq!(dbt("dec"), "numeric(10,2)");
    assert_eq!(
        text(b, "big"),
        "123456789012345678901234567890.123456789012345"
    );
    assert_eq!(text(b, "nan"), "NaN");
    assert_eq!(text(b, "small"), "-0.000123");
    assert_eq!(text(b, "t"), "hello");
    assert_eq!(text(b, "vc"), "v");
    assert_eq!(dbt("vc"), "varchar(5)");
    assert_eq!(text(b, "bp"), "c  ");
    assert_eq!(dbt("bp"), "bpchar(3)");
    assert_eq!(text(b, "nm"), "nm");
    assert_eq!(text(b, "x"), "<a/>");
    assert_eq!(
        col::<BinaryArray>(b, "by").value(0),
        [0xde, 0xad, 0xbe, 0xef]
    );
    let d = col::<Date32Array>(b, "d").value(0);
    assert_eq!(crate::columns::format_date(d), "2024-02-29");
    // infinity does not fit Date32: NULL plus a lossy warning.
    assert!(col::<Date32Array>(b, "dinf").is_null(0));
    assert!(
        out.notices
            .iter()
            .any(|n| n.level == NoticeLevel::Warning && n.message.contains("1 value")),
        "{:?}",
        out.notices
    );
    assert_eq!(
        col::<Time64MicrosecondArray>(b, "tm").value(0),
        (13 * 3600 + 14 * 60 + 15) * 1_000_000 + 500_000
    );
    assert_eq!(text(b, "ttz"), "13:14:15+02");
    assert_eq!(text(b, "iv"), "1 year 2 mons 3 days 04:05:06.5");
    let ts = col::<TimestampMicrosecondArray>(b, "ts");
    assert_eq!(
        crate::columns::format_timestamp(ts.value(0)),
        "2024-01-02 03:04:05.123456"
    );
    assert!(ts.timezone().is_none());
    let tstz = col::<TimestampMicrosecondArray>(b, "tstz");
    assert_eq!(tstz.timezone(), Some("UTC"));
    assert_eq!(
        crate::columns::format_timestamp(tstz.value(0)),
        "2024-01-02 01:04:05"
    );
    assert_eq!(text(b, "u"), "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11");
    assert_eq!(text(b, "js"), "{\"a\": [1, 2]}");
    assert_eq!(text(b, "jb"), "{\"a\": [1, 2]}");
    assert_eq!(text(b, "en"), "happy");
    assert_eq!(dbt("en"), enum_ty);
    assert_eq!(text(b, "arr"), "{1,2,NULL}");
    assert_eq!(dbt("arr"), "int4[]");
    assert_eq!(text(b, "arr2"), r#"{{a,"b c"},{"",NULL}}"#);
    assert_eq!(text(b, "ip"), "192.168.0.1/24");
    assert_eq!(text(b, "net"), "10.0.0.0/8");
    assert_eq!(text(b, "m"), "12.34");
    assert_eq!(text(b, "pt"), "(1.5,2)");
    assert_eq!(text(b, "rg"), "[1,5)");
    assert_eq!(text(b, "rec"), "(1,\"x y\")");
    // Text renderings match the server's own output.
    let check = run(&mut c, "select array[1, 2, null]::int4[]::text as at").await;
    assert_eq!(
        col::<StringArray>(&check.results[0].batches[0], "at").value(0),
        "{1,2,NULL}"
    );
    run(&mut c, &format!("drop type {enum_ty}")).await;
}

#[tokio::test]
async fn pg_dml_counts_and_empty_results() {
    let Some(mut c) = open().await else { return };
    let t = unique("ferrodb_dml");
    let out = run(&mut c, &format!("create table {t} (a int, b text)")).await;
    assert_eq!(out.summary.affected, None);
    assert!(out.results.is_empty());
    let out = run(
        &mut c,
        &format!("insert into {t} values (1, 'x'), (2, 'y'), (3, 'z')"),
    )
    .await;
    assert_eq!(out.summary.affected, Some(3));
    let out = run(&mut c, &format!("update {t} set b = 'w' where a >= 2")).await;
    assert_eq!(out.summary.affected, Some(2));
    let out = run(&mut c, &format!("delete from {t} where a = 1")).await;
    assert_eq!(out.summary.affected, Some(1));
    let out = run(&mut c, &format!("select a, b from {t} where false")).await;
    assert_eq!(out.results.len(), 1);
    assert_eq!(out.results[0].num_rows(), 0);
    let s = &out.results[0].schema;
    assert_eq!(s.fields().len(), 2);
    assert_eq!(s.field(0).data_type(), &DataType::Int32);
    assert_eq!(s.field(1).data_type(), &DataType::Utf8);
    run(&mut c, &format!("drop table {t}")).await;
}

#[tokio::test]
async fn pg_streams_large_results_in_batches() {
    let Some(mut c) = open().await else { return };
    let out = run(
        &mut c,
        "select g, g::text as t from generate_series(1, 100000) g",
    )
    .await;
    let r = &out.results[0];
    assert_eq!(r.num_rows(), 100_000);
    assert!(r.batches.len() >= 100, "{} batches", r.batches.len());
    assert!(
        r.batches
            .iter()
            .all(|b| b.num_rows() <= ResultSink::BATCH_ROWS)
    );
    let last = r.batches.last().unwrap();
    assert_eq!(
        col::<Int32Array>(last, "g").value(last.num_rows() - 1),
        100_000
    );
}

#[tokio::test]
async fn pg_truncates_when_consumer_goes_away() {
    let Some(mut c) = open().await else { return };
    let (sink, mut rx) = ResultSink::channel(2, 100);
    let exec = c.execute(
        Request::Text {
            text: "select g from generate_series(1, 1000000) g".into(),
        },
        sink,
        CancellationToken::new(),
    );
    let consumer = async move {
        // Read the header and one batch, then hang up.
        while let Some(ev) = rx.recv().await {
            if matches!(ev, SinkEvent::Batch(_)) {
                break;
            }
        }
    };
    let (summary, ()) = futures::join!(exec, consumer);
    assert!(summary.unwrap().truncated);
    c.ping().await.unwrap();
}

#[tokio::test]
async fn pg_errors_carry_position() {
    let Some(mut c) = open().await else { return };
    let sql = "select * from nope";
    let e = collect(c.as_mut(), sql, CancellationToken::new())
        .await
        .unwrap_err();
    let Error::Query(q) = e else { panic!("{e:?}") };
    assert_eq!(q.code.as_deref(), Some("42P01"));
    assert_eq!(&sql[q.range.clone().unwrap()], "nope");
    // Positions are characters, the range is bytes.
    let sql = "select 'żółć' as x, brak";
    let Error::Query(q) = collect(c.as_mut(), sql, CancellationToken::new())
        .await
        .unwrap_err()
    else {
        panic!()
    };
    assert_eq!(&sql[q.range.unwrap()], "brak");
    c.ping().await.unwrap();
}

#[tokio::test]
async fn pg_cancel_stops_a_long_query() {
    let Some(mut c) = open().await else { return };
    let cancel = CancellationToken::new();
    let t = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        t.cancel();
    });
    let started = Instant::now();
    let r = collect(c.as_mut(), "select pg_sleep(30)", cancel).await;
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    c.ping().await.unwrap();
    let out = run(&mut c, "select 1 as one").await;
    assert_eq!(out.results[0].num_rows(), 1);
}

#[tokio::test]
async fn pg_remote_cancel() {
    let Some(mut c) = open().await else { return };
    let rc = c.remote_cancel().unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        rc.cancel().await.unwrap();
    });
    let started = Instant::now();
    let r = collect(c.as_mut(), "select pg_sleep(30)", CancellationToken::new()).await;
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(1));
    c.ping().await.unwrap();
}

#[tokio::test]
async fn pg_notices_reach_the_sink() {
    let Some(mut c) = open().await else { return };
    let out = run(
        &mut c,
        "do $$ begin raise warning 'w'; raise notice 'n'; end $$",
    )
    .await;
    assert_eq!(out.notices.len(), 2, "{:?}", out.notices);
    assert_eq!(out.notices[0].level, NoticeLevel::Warning);
    assert_eq!(out.notices[0].message, "w");
    assert_eq!(out.notices[1].level, NoticeLevel::Info);
    // Notices of a query with results arrive too.
    run(
        &mut c,
        "create function pg_temp.ferrodb_f() returns int language plpgsql as          $$ begin raise notice 'hi'; return 1; end $$",
    )
    .await;
    let out = run(&mut c, "select pg_temp.ferrodb_f() as a").await;
    assert_eq!(out.results[0].num_rows(), 1);
    assert_eq!(out.notices.len(), 1, "{:?}", out.notices);
    assert_eq!(out.notices[0].message, "hi");
}

#[tokio::test]
async fn pg_read_only_and_schemas() {
    let Some(mut c) = open().await else { return };
    c.set_read_only(true).await.unwrap();
    let t = unique("ferrodb_ro");
    let e = collect(
        c.as_mut(),
        &format!("create table {t} (a int)"),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(e, Error::Query(ref q) if q.code.as_deref() == Some("25006")),
        "{e:?}"
    );
    c.set_read_only(false).await.unwrap();

    let s = unique("Ferro Schema");
    run(
        &mut c,
        &format!("create schema {}", crate::quote_ident(&s, '"')),
    )
    .await;
    c.set_schema(&s).await.unwrap();
    assert_eq!(
        c.current_schema().await.unwrap().as_deref(),
        Some(s.as_str())
    );
    run(&mut c, "create table t1 (id int, name varchar(20))").await;
    run(&mut c, "create view v1 as select id from t1").await;
    let v = PgDriver(&POSTGRESQL).variant();
    let list = |path: &[(ObjectKind, String)]| v.list_queries(path);
    let roots = list(&[]);
    let out = run(&mut c, &roots[0].sql).await;
    let names = col::<StringArray>(&out.results[0].batches[0], "name");
    assert_eq!(names.value(0), s, "current schema first");
    let schema_path = [(ObjectKind::Schema, s.clone())];
    let qs = list(&schema_path);
    assert_eq!(qs.len(), 2);
    let tables = run(&mut c, &qs[0].sql).await;
    assert_eq!(text(&tables.results[0].batches[0], "name"), "t1");
    let views = run(&mut c, &qs[1].sql).await;
    assert_eq!(text(&views.results[0].batches[0], "name"), "v1");
    let cols = list(&[schema_path[0].clone(), (ObjectKind::Table, "t1".into())]);
    let out = run(&mut c, &cols[0].sql).await;
    let b = &out.results[0].batches[0];
    assert_eq!(col::<StringArray>(b, "name").value(1), "name");
    assert_eq!(
        col::<StringArray>(b, "type").value(1),
        "character varying(20)"
    );

    // begin / rollback discards an insert.
    c.begin().await.unwrap();
    run(&mut c, "insert into t1 values (1, 'a')").await;
    c.rollback().await.unwrap();
    let out = run(&mut c, "select count(*) as n from t1").await;
    assert_eq!(
        col::<Int64Array>(&out.results[0].batches[0], "n").value(0),
        0
    );
    c.begin().await.unwrap();
    run(&mut c, "insert into t1 values (1, 'a')").await;
    c.commit().await.unwrap();
    let out = run(&mut c, "select count(*) as n from t1").await;
    assert_eq!(
        col::<Int64Array>(&out.results[0].batches[0], "n").value(0),
        1
    );

    c.set_schema("public").await.unwrap();
    run(
        &mut c,
        &format!("drop schema {} cascade", crate::quote_ident(&s, '"')),
    )
    .await;
}

#[tokio::test]
async fn pg_tls_modes() {
    let Some(cfg) = config() else { return };
    // `prefer` falls back to plain text when the server has no TLS.
    let prefer = ResolvedConfig {
        tls: TlsMode::Prefer,
        ..cfg.clone()
    };
    let mut c = PgDriver(&POSTGRESQL).connect(&prefer).await.unwrap();
    c.ping().await.unwrap();
    let disable = ResolvedConfig {
        tls: TlsMode::Disable,
        ..cfg.clone()
    };
    let mut c = PgDriver(&POSTGRESQL).connect(&disable).await.unwrap();
    c.ping().await.unwrap();
    // `require` against a server without TLS is a connection error (skip
    // when the test server does offer TLS).
    let require = ResolvedConfig {
        tls: TlsMode::Require,
        ..cfg.clone()
    };
    let ssl_on = run(&mut c, "show ssl").await;
    if text(&ssl_on.results[0].batches[0], "ssl") == "off" {
        let e = PgDriver(&POSTGRESQL).connect(&require).await.err().unwrap();
        assert!(matches!(e, Error::Connection(_)), "{e:?}");
    }
    // Wrong password: a connection error without the secret in it.
    let bad = cfg.with_secret("password", "definitely-wrong-secret");
    let e = PgDriver(&POSTGRESQL).connect(&bad).await.err().unwrap();
    let Error::Connection(m) = e else {
        panic!("{e:?}")
    };
    assert!(!m.contains("definitely-wrong-secret"), "{m}");
}

#[test]
fn descriptor_and_variant() {
    let d = PgDriver(&POSTGRESQL).descriptor();
    assert_eq!(d.id, DriverId::from("postgresql"));
    assert_eq!(d.family, ferrodb_api::FamilyId::from("pgwire"));
    assert_eq!(d.default_port, Some(5432));
    let v = PgDriver(&POSTGRESQL).variant();
    assert!(v.quirks().transactional_ddl);
    assert_eq!(v.list_queries(&[]).len(), 1);
    let q = v.list_queries(&[(ObjectKind::Schema, "it's".into())]);
    assert_eq!(q.len(), 2);
    assert!(q[0].sql.contains("'it''s'"));
    assert_eq!(drivers().len(), 1);
}
