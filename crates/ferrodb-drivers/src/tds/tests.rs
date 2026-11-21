//! SQL Server against a real server when `FERRODB_TCK_SQLSERVER` holds a
//! URL such as `sqlserver://sa:Secret%231@localhost:1433/master` (the
//! password is taken literally).

use std::time::{Duration, Instant};

use arrow_array::{
    Array, BinaryArray, BooleanArray, Date32Array, Decimal128Array, Float64Array, Int16Array,
    Int32Array, Int64Array, StringArray, Time64MicrosecondArray, TimestampMicrosecondArray,
};
use arrow_schema::DataType;
use ferrodb_api::{AuthMethod, Error};

use super::*;
use crate::{CancellationToken, Request, sink::collect};

#[test]
fn descriptors_match_the_catalog() {
    let d = TdsDriver(&SQLSERVER).descriptor();
    assert_eq!((d.id.as_str(), d.family.as_str()), ("sqlserver", "tds"));
    assert_eq!(d.default_port, Some(1433));
    assert_eq!(
        d.capabilities.auth_methods,
        [AuthMethod::Password, AuthMethod::Kerberos]
    );
    assert_eq!(
        d.capabilities.explain,
        ferrodb_api::ExplainSupport::Structured
    );
    let a = TdsDriver(&AZURESQL).descriptor();
    assert_eq!(
        a.capabilities.auth_methods,
        [AuthMethod::Password, AuthMethod::EntraId]
    );
    assert!(a.properties.iter().any(|p| p.key == "tenant"));
    assert_eq!(a.category, ferrodb_api::DriverCategory::AnalyticalCloud);
    assert!(TdsDriver(&SQLSERVER).variant().quirks().transactional_ddl);
    let q = list_queries(&[
        (ObjectKind::Schema, "dbo".into()),
        (ObjectKind::Table, "it's".into()),
    ]);
    assert!(q[0].sql.contains("quotename(N'it''s')"), "{}", q[0].sql);
}

fn config() -> Option<ResolvedConfig> {
    let url = std::env::var("FERRODB_TCK_SQLSERVER").ok()?;
    Some(ResolvedConfig::from_url("sqlserver", &url).unwrap())
}

async fn open() -> Option<Box<dyn Connection>> {
    Some(TdsDriver(&SQLSERVER).connect(&config()?).await.unwrap())
}

async fn run(c: &mut Box<dyn Connection>, sql: &str) -> crate::sink::CollectedOutput {
    collect(c.as_mut(), sql, CancellationToken::new())
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"))
}

fn col<T: 'static>(r: &crate::sink::Collected, i: usize) -> &T {
    r.batches[0]
        .column(i)
        .as_any()
        .downcast_ref::<T>()
        .unwrap_or_else(|| panic!("column {i}: {:?}", r.batches[0].column(i).data_type()))
}

#[tokio::test]
async fn sqlserver_types() {
    let Some(mut c) = open().await else { return };
    assert!(
        c.server_version().starts_with("Microsoft SQL Server 1"),
        "{}",
        c.server_version()
    );
    let out = run(
        &mut c,
        "select cast(255 as tinyint) a, cast(-3 as smallint) b, 42 c, cast(9000000000 as bigint) d, \
         cast(12.5 as decimal(10,2)) e, cast(922337203685477.5807 as money) f, cast(1.5 as float) g, \
         cast(1 as bit) h, N'zażółć' i, cast('abc' as varchar(10)) j, cast(0x01ff as varbinary(4)) k, \
         cast('2026-11-09' as date) l, cast('10:11:12.1234567' as time(7)) m, \
         cast('2026-11-09 10:11:12.123' as datetime) n, cast('2026-11-09 10:11:12.1234567' as datetime2(7)) o, \
         cast('2026-11-09 10:11:12 +02:00' as datetimeoffset) p, \
         cast('6F9619FF-8B86-D011-B42D-00C04FC964FF' as uniqueidentifier) q, \
         cast('<a>1</a>' as xml) r, cast(cast(12.5 as decimal(4,1)) as sql_variant) s, \
         cast(null as int) t, cast('2026-11-09 10:11:00' as smalldatetime) u",
    )
    .await;
    let r = &out.results[0];
    let types: Vec<DataType> = r
        .schema
        .fields()
        .iter()
        .map(|f| f.data_type().clone())
        .collect();
    assert_eq!(types[4], DataType::Decimal128(10, 2));
    assert_eq!(types[5], DataType::Decimal128(19, 4));
    assert_eq!(col::<Int16Array>(r, 0).value(0), 255);
    assert_eq!(col::<Int16Array>(r, 1).value(0), -3);
    assert_eq!(col::<Int32Array>(r, 2).value(0), 42);
    assert_eq!(col::<Int64Array>(r, 3).value(0), 9_000_000_000);
    assert_eq!(col::<Decimal128Array>(r, 4).value(0), 1250);
    assert_eq!(
        col::<Decimal128Array>(r, 5).value(0),
        9_223_372_036_854_775_807
    );
    assert_eq!(col::<Float64Array>(r, 6).value(0), 1.5);
    assert!(col::<BooleanArray>(r, 7).value(0));
    assert_eq!(col::<StringArray>(r, 8).value(0), "zażółć");
    assert_eq!(col::<StringArray>(r, 9).value(0), "abc");
    assert_eq!(col::<BinaryArray>(r, 10).value(0), [1, 255]);
    assert_eq!(col::<Date32Array>(r, 11).value(0), 20_766);
    assert_eq!(
        col::<Time64MicrosecondArray>(r, 12).value(0),
        (10 * 3600 + 11 * 60 + 12) * 1_000_000 + 123_456
    );
    let base = 20_766i64 * 86_400_000_000 + (10 * 3600 + 11 * 60 + 12) * 1_000_000;
    assert_eq!(
        col::<TimestampMicrosecondArray>(r, 13).value(0),
        base + 123_000
    );
    assert_eq!(
        col::<TimestampMicrosecondArray>(r, 14).value(0),
        base + 123_456
    );
    // 10:11:12 +02:00 = 08:11:12 UTC.
    assert_eq!(
        col::<TimestampMicrosecondArray>(r, 15).value(0),
        base - 2 * 3_600_000_000
    );
    assert_eq!(
        col::<StringArray>(r, 16).value(0),
        "6F9619FF-8B86-D011-B42D-00C04FC964FF"
    );
    assert_eq!(col::<StringArray>(r, 17).value(0), "<a>1</a>");
    assert_eq!(col::<StringArray>(r, 18).value(0), "12.5");
    assert!(r.batches[0].column(19).is_null(0));
    assert_eq!(
        col::<TimestampMicrosecondArray>(r, 20).value(0),
        20_766i64 * 86_400_000_000 + (10 * 3600 + 11 * 60) * 1_000_000
    );
    let meta = r.schema.field(14).metadata();
    assert_eq!(
        meta.get(crate::columns::DB_TYPE_KEY).map(String::as_str),
        Some("datetime2(7)")
    );
}

#[tokio::test]
async fn sqlserver_batches_messages_and_counts() {
    let Some(mut c) = open().await else { return };
    let out = run(
        &mut c,
        "create table #t (id int identity primary key, v nvarchar(10));\n\
         insert #t (v) values (N'a'), (N'b');\n\
         print 'hello';\n\
         raiserror('careful', 10, 1);\n\
         select * from #t order by id;\n\
         select count(*) n from #t;\n\
         update #t set v = N'c' where id = 1",
    )
    .await;
    assert_eq!(out.results.len(), 2);
    assert_eq!(out.results[0].num_rows(), 2);
    assert_eq!(out.summary.affected, Some(3));
    let msgs: Vec<&str> = out.notices.iter().map(|n| n.message.as_str()).collect();
    assert!(
        msgs.contains(&"hello") && msgs.contains(&"careful"),
        "{msgs:?}"
    );
    // GO splits; `GO 3` repeats.
    let out = run(
        &mut c,
        "create table #g (a int)\nGO\ninsert #g values (1)\nGO 3\nselect count(*) from #g",
    )
    .await;
    assert_eq!(col::<Int32Array>(&out.results[0], 0).value(0), 3);
    // A lone `GO 2` repeats the previous batch once more.
    run(&mut c, "insert #g values (1)").await;
    run(&mut c, "GO 2").await;
    let out = run(&mut c, "select count(*) from #g").await;
    assert_eq!(col::<Int32Array>(&out.results[0], 0).value(0), 5);
    // Errors carry their line.
    let sql = "select 1;\nselect * from nope_tbl";
    let e = collect(c.as_mut(), sql, CancellationToken::new())
        .await
        .unwrap_err();
    let Error::Query(q) = e else { panic!("{e:?}") };
    assert_eq!(q.code.as_deref(), Some("208"));
    assert_eq!(&sql[q.range.unwrap()], "select * from nope_tbl");
    c.ping().await.unwrap();
}

#[tokio::test]
async fn sqlserver_cancel_keeps_the_session() {
    let Some(mut c) = open().await else { return };
    run(
        &mut c,
        "create table #keep (a int); insert #keep values (7)",
    )
    .await;
    let token = CancellationToken::new();
    let t2 = token.clone();
    let started = Instant::now();
    let (r, _) = futures::join!(
        collect(c.as_mut(), "waitfor delay '00:00:30'", token),
        async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            t2.cancel();
        }
    );
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    let out = run(&mut c, "select a from #keep").await;
    assert_eq!(col::<Int32Array>(&out.results[0], 0).value(0), 7);
    // The remote handle cancels too.
    let remote = c.remote_cancel().unwrap();
    let (r, _) = futures::join!(
        collect(
            c.as_mut(),
            "waitfor delay '00:00:30'",
            CancellationToken::new()
        ),
        async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            remote.cancel().await.unwrap();
        }
    );
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    c.ping().await.unwrap();
}

#[tokio::test]
async fn sqlserver_plans_schema_and_read_only() {
    let Some(mut c) = open().await else { return };
    let out = crate::sink::collect_request(
        c.as_mut(),
        Request::native(
            "explain",
            &[(
                "statement",
                "select name from sys.objects where object_id = 5",
            )],
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let xml = col::<StringArray>(&out.results[0], 0).value(0).to_owned();
    assert!(xml.contains("ShowPlanXML"), "{xml}");
    let out = crate::sink::collect_request(
        c.as_mut(),
        Request::native(
            "explain",
            &[
                ("statement", "select count(*) from sys.objects"),
                ("analyze", "true"),
            ],
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(out.results.len(), 1, "only the plan comes back");
    assert!(
        col::<StringArray>(&out.results[0], 0)
            .value(0)
            .contains("RunTimeInformation")
    );
    // Showplan is off again: statements run.
    let out = run(&mut c, "select 1 as one").await;
    assert_eq!(col::<Int32Array>(&out.results[0], 0).value(0), 1);
    // Schemas and databases.
    assert_eq!(c.current_schema().await.unwrap().as_deref(), Some("dbo"));
    c.set_schema("sys").await.unwrap();
    assert_eq!(c.current_schema().await.unwrap().as_deref(), Some("sys"));
    c.set_schema("tempdb").await.unwrap();
    let out = run(&mut c, "select db_name()").await;
    assert_eq!(col::<StringArray>(&out.results[0], 0).value(0), "tempdb");
    assert!(c.set_schema("no_such_thing_x").await.is_err());
    c.set_read_only(true).await.unwrap();
    let w = collect(
        c.as_mut(),
        "create table #ro (a int)",
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(w, Err(Error::ReadOnly)), "{w:?}");
    run(&mut c, "select 1").await;
}
