//! Integration tests against real servers. They run only when
//! `FERRODB_TEST_MYSQL` / `FERRODB_TEST_MARIADB` hold connection URLs
//! (`mysql://root:pw@localhost:3306/db`) and are skipped silently otherwise.
//! Every test runs against each configured server.

use std::time::{Duration, Instant};

use arrow_array::{
    Array, BinaryArray, Date32Array, Decimal128Array, Float32Array, Float64Array, Int64Array,
    StringArray, TimestampMicrosecondArray, UInt64Array,
};

use super::super::{Product, drivers};
use crate::{
    CancellationToken, Connection, Error, ResolvedConfig, columns::DB_TYPE_KEY, sink::collect,
};
use ferrodb_net::TlsMode;

fn targets() -> Vec<(Product, ResolvedConfig)> {
    [
        (Product::MySql, "FERRODB_TEST_MYSQL"),
        (Product::MariaDb, "FERRODB_TEST_MARIADB"),
    ]
    .into_iter()
    .filter_map(|(p, var)| {
        let url = std::env::var(var).ok().filter(|u| !u.is_empty())?;
        Some((
            p,
            ResolvedConfig::from_url(p.id(), &url).expect("valid URL"),
        ))
    })
    .collect()
}

/// Connects, retrying while the server is still starting.
async fn open(p: Product, cfg: &ResolvedConfig) -> Box<dyn Connection> {
    let driver = drivers()
        .into_iter()
        .find(|d| d.id().as_str() == p.id())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match driver.connect(cfg).await {
            Ok(c) => return c,
            Err(e) if Instant::now() < deadline => {
                eprintln!("{}: waiting for server: {e}", p.name());
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => panic!("{}: cannot connect: {e}", p.name()),
        }
    }
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("ferrodb_{prefix}_{}_{nanos}", std::process::id())
}

async fn run(c: &mut dyn Connection, sql: &str) -> crate::sink::CollectedOutput {
    collect(c, sql, CancellationToken::new())
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn col<T: 'static>(out: &crate::sink::CollectedOutput, i: usize) -> &T {
    out.results[0].batches[0]
        .column(i)
        .as_any()
        .downcast_ref::<T>()
        .unwrap_or_else(|| panic!("column {i}: {:?}", out.results[0].schema.field(i)))
}

#[tokio::test]
async fn types_round_trip() {
    for (p, cfg) in targets() {
        let mut c = open(p, &cfg).await;
        let c = c.as_mut();
        assert!(
            c.server_version().starts_with(p.name()),
            "{}",
            c.server_version()
        );
        let t = unique("types");
        run(c, "set session sql_mode = ''").await;
        run(
            c,
            &format!(
                "create temporary table {t} (ti tinyint, si smallint, mi mediumint, i int, \
                 bi bigint, ubi bigint unsigned, y year, f float, d double, dec_ decimal(10,2), \
                 dt date, tm time, dtt datetime(3), ts timestamp null, vc varchar(20), ch char(3), \
                 en enum('a','b'), st set('x','y'), j json, vb varbinary(4), bl blob, tx text, \
                 bt bit(8), g geometry, b1 tinyint(1))"
            ),
        )
        .await;
        run(
            c,
            &format!(
                "insert into {t} values (-1, 2, 3, -4, 9000000000, 18446744073709551615, 2024, \
                 1.5, 2.25, 12345678.91, '2024-02-29', '-838:59:59', '2024-01-02 03:04:05.678', \
                 '2024-01-02 03:04:05', 'zażółć', 'ab', 'b', 'x,y', '{{\"a\": 1}}', x'00ff', \
                 x'0102', 'long text', b'10000001', ST_GeomFromText('POINT(1 2)'), 1), \
                 (null, null, null, null, null, null, null, null, null, null, '0000-00-00', \
                 null, '0000-00-00 00:00:00', null, null, null, null, null, null, null, null, \
                 null, null, null, null)"
            ),
        )
        .await;
        let out = run(c, &format!("select * from {t} order by ti is null, ti")).await;
        let r = &out.results[0];
        assert_eq!(r.num_rows(), 2);
        let db = |i: usize| r.schema.field(i).metadata()[DB_TYPE_KEY].clone();
        assert_eq!(col::<Int64Array>(&out, 0).value(0), -1);
        assert_eq!(col::<Int64Array>(&out, 4).value(0), 9_000_000_000);
        assert_eq!(col::<UInt64Array>(&out, 5).value(0), u64::MAX);
        assert_eq!(db(5), "bigint unsigned");
        assert_eq!(col::<Int64Array>(&out, 6).value(0), 2024);
        assert_eq!(col::<Float32Array>(&out, 7).value(0), 1.5);
        assert_eq!(col::<Float64Array>(&out, 8).value(0), 2.25);
        let dec = col::<Decimal128Array>(&out, 9);
        assert_eq!(dec.value(0), 1_234_567_891);
        assert_eq!(dec.scale(), 2);
        assert_eq!(db(9), "decimal(10,2)");
        let date = col::<Date32Array>(&out, 10);
        assert_eq!(crate::columns::format_date(date.value(0)), "2024-02-29");
        assert!(date.is_null(1), "zero date → NULL");
        assert_eq!(col::<StringArray>(&out, 11).value(0), "-838:59:59");
        let dtt = col::<TimestampMicrosecondArray>(&out, 12);
        assert_eq!(
            crate::columns::format_timestamp(dtt.value(0)),
            "2024-01-02 03:04:05.678"
        );
        assert!(dtt.is_null(1));
        assert_eq!(db(12), "datetime");
        assert!(col::<TimestampMicrosecondArray>(&out, 13).is_valid(0));
        assert_eq!(col::<StringArray>(&out, 14).value(0), "zażółć");
        assert_eq!(db(14), "varchar");
        assert_eq!(col::<StringArray>(&out, 15).value(0), "ab");
        assert_eq!(col::<StringArray>(&out, 16).value(0), "b");
        assert_eq!(col::<StringArray>(&out, 17).value(0), "x,y");
        assert!(col::<StringArray>(&out, 18).value(0).contains("\"a\""));
        if p == Product::MySql {
            assert_eq!(db(18), "json");
        }
        assert_eq!(col::<BinaryArray>(&out, 19).value(0), [0x00, 0xff]);
        assert_eq!(col::<BinaryArray>(&out, 20).value(0), [0x01, 0x02]);
        assert_eq!(db(20), "blob");
        assert_eq!(col::<StringArray>(&out, 21).value(0), "long text");
        assert_eq!(col::<Int64Array>(&out, 22).value(0), 0b1000_0001);
        assert!(!col::<BinaryArray>(&out, 23).value(0).is_empty());
        assert_eq!(col::<Int64Array>(&out, 24).value(0), 1);
        // Zero dates are reported as lossy.
        assert!(
            out.notices.iter().any(|n| n.message.contains("NULL")),
            "{:?}",
            out.notices
        );

        // Literals.
        let out = run(
            c,
            "select 1 as a, 'x' as b, 1.5 as c, x'00ff' as d, null as e, \
             date '2024-01-02' as f, cast(18446744073709551615 as unsigned) as g",
        )
        .await;
        assert_eq!(col::<Int64Array>(&out, 0).value(0), 1);
        assert_eq!(col::<StringArray>(&out, 1).value(0), "x");
        assert_eq!(col::<Decimal128Array>(&out, 2).value(0), 15);
        assert_eq!(col::<BinaryArray>(&out, 3).value(0), [0x00, 0xff]);
        assert!(out.results[0].batches[0].column(4).is_null(0));
        assert!(col::<Date32Array>(&out, 5).is_valid(0));
        assert_eq!(col::<UInt64Array>(&out, 6).value(0), u64::MAX);
    }
}

#[tokio::test]
async fn dml_counts_empty_results_and_warnings() {
    for (p, cfg) in targets() {
        let mut c = open(p, &cfg).await;
        let c = c.as_mut();
        let t = unique("dml");
        let out = run(
            c,
            &format!("create temporary table {t} (a int, b varchar(10))"),
        )
        .await;
        assert_eq!(out.summary.affected, None);
        assert_eq!(out.summary.results, 0);
        assert!(out.results.is_empty());
        let out = run(
            c,
            &format!("insert into {t} values (1, 'a'), (2, 'b'), (3, 'c')"),
        )
        .await;
        assert_eq!(out.summary.affected, Some(3));
        let out = run(c, &format!("update {t} set b = 'z' where a >= 2")).await;
        assert_eq!(out.summary.affected, Some(2));
        let out = run(c, &format!("delete from {t} where a = 1")).await;
        assert_eq!(out.summary.affected, Some(1));
        let out = run(c, &format!("select a, b from {t} where a > 100")).await;
        assert_eq!(out.summary.results, 1);
        assert_eq!(out.results[0].schema.fields().len(), 2);
        assert_eq!(out.results[0].num_rows(), 0);
        let out = run(c, "select cast('abc' as signed) as x").await;
        assert!(
            out.notices
                .iter()
                .any(|n| n.message.to_lowercase().contains("truncated")),
            "{}: {:?}",
            p.name(),
            out.notices
        );
    }
}

#[tokio::test]
async fn streams_100k_rows_in_batches() {
    for (p, cfg) in targets() {
        let mut c = open(p, &cfg).await;
        let c = c.as_mut();
        run(
            c,
            &format!("set session {} = 200000", p.recursion_limit_var()),
        )
        .await;
        let out = run(
            c,
            "with recursive r(i) as (select 1 union all select i + 1 from r where i < 100000) \
             select i, concat('row ', i) as s from r",
        )
        .await;
        let r = &out.results[0];
        assert_eq!(r.num_rows(), 100_000);
        assert!(r.batches.len() > 1, "{} batches", r.batches.len());
        assert!(r.batches.iter().all(|b| b.num_rows() <= 1000));
        // The connection is idle afterwards.
        run(c, "select 1").await;
    }
}

#[tokio::test]
async fn procedure_returns_multiple_result_sets() {
    for (p, cfg) in targets() {
        let mut c = open(p, &cfg).await;
        let c = c.as_mut();
        let proc = unique("proc");
        run(
            c,
            &format!("create procedure {proc}() begin select 1 as a; select 'x' as b, 2 as c; end"),
        )
        .await;
        let out = collect(c, format!("call {proc}()"), CancellationToken::new()).await;
        run(c, &format!("drop procedure {proc}")).await;
        let out = out.unwrap();
        assert_eq!(out.summary.results, 2);
        assert_eq!(out.results.len(), 2);
        assert_eq!(out.results[0].schema.fields().len(), 1);
        assert_eq!(out.results[1].schema.fields().len(), 2);
        assert_eq!(out.results[1].num_rows(), 1);
        // Multi-statement text.
        let out = run(c, "select 1 as a; select 2 as b").await;
        assert_eq!(out.results.len(), 2);
    }
}

#[tokio::test]
async fn syntax_error_has_a_range() {
    for (p, cfg) in targets() {
        let mut c = open(p, &cfg).await;
        let text = "select 1,\n  2 frm dual where";
        let e = collect(c.as_mut(), text, CancellationToken::new())
            .await
            .unwrap_err();
        let Error::Query(q) = e else {
            panic!("{}: {e:?}", p.name())
        };
        assert!(
            q.code.as_deref().unwrap().starts_with("1064 (42000)"),
            "{q:?}"
        );
        let r = q.range.clone().unwrap_or_else(|| panic!("{q:?}"));
        assert!(text[r].starts_with("dual"), "{}: {q:?}", p.name());
        // Other errors: code, no range; the connection stays usable.
        let e = collect(
            c.as_mut(),
            "select * from no_such_table_x",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(e, Error::Query(ref q) if q.code.as_deref() == Some("1146 (42S02)")),
            "{e:?}"
        );
        c.ping().await.unwrap();
    }
}

#[tokio::test]
async fn cancel_stops_sleep_quickly() {
    for (p, cfg) in targets() {
        let mut c = open(p, &cfg).await;
        let cancel = CancellationToken::new();
        let t = cancel.clone();
        let fired = std::sync::Arc::new(std::sync::Mutex::new(None));
        let f = fired.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            *f.lock().unwrap() = Some(Instant::now());
            t.cancel();
        });
        let r = collect(c.as_mut(), "select sleep(30)", cancel).await;
        let took = fired.lock().unwrap().unwrap().elapsed();
        assert!(matches!(r, Err(Error::Cancelled)), "{}: {r:?}", p.name());
        assert!(took < Duration::from_secs(1), "{}: {took:?}", p.name());
        c.ping().await.unwrap();
        let out = run(c.as_mut(), "select 42 as x").await;
        assert_eq!(col::<Int64Array>(&out, 0).value(0), 42);

        // Remote cancel from another task: the statement is interrupted.
        let rc = c.remote_cancel().unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            rc.cancel().await.unwrap();
        });
        let started = Instant::now();
        let r = collect(
            c.as_mut(),
            "select sleep(30) as s",
            CancellationToken::new(),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(2), "{}", p.name());
        // SLEEP returns 1 when interrupted rather than failing.
        if let Ok(out) = r {
            assert_eq!(col::<Int64Array>(&out, 0).value(0), 1);
        }
        c.ping().await.unwrap();
    }
}

#[tokio::test]
async fn session_operations() {
    for (p, cfg) in targets() {
        let mut c = open(p, &cfg).await;
        let c = c.as_mut();
        let db = cfg.database().unwrap().to_owned();
        assert_eq!(
            c.current_schema().await.unwrap().as_deref(),
            Some(db.as_str())
        );
        c.set_schema("information_schema").await.unwrap();
        assert_eq!(
            c.current_schema().await.unwrap().as_deref(),
            Some("information_schema")
        );
        c.set_schema(&db).await.unwrap();

        // Read-only session.
        let t = unique("ro");
        run(c, &format!("create table {t} (a int)")).await;
        c.set_read_only(true).await.unwrap();
        let e = collect(
            c,
            format!("insert into {t} values (1)"),
            CancellationToken::new(),
        )
        .await;
        c.set_read_only(false).await.unwrap();
        assert!(matches!(e, Err(Error::Query(_))), "{}: {e:?}", p.name());
        run(c, &format!("insert into {t} values (1)")).await;

        // Rollback discards an insert.
        c.begin().await.unwrap();
        run(c, &format!("insert into {t} values (2)")).await;
        c.rollback().await.unwrap();
        c.begin().await.unwrap();
        run(c, &format!("insert into {t} values (3)")).await;
        c.commit().await.unwrap();
        let out = run(c, &format!("select a from {t} order by a")).await;
        let a = col::<Int64Array>(&out, 0);
        assert_eq!(a.values().to_vec(), vec![1, 3]);
        run(c, &format!("drop table {t}")).await;

        // Listing queries run.
        let v = drivers()
            .into_iter()
            .find(|d| d.id().as_str() == p.id())
            .unwrap()
            .variant();
        let q = v.list_queries(&[]);
        let out = run(c, &q[0].sql).await;
        let names = col::<StringArray>(&out, 0);
        assert_eq!(names.value(0), db, "current database first");
        let path = [(ferrodb_api::ObjectKind::Database, "mysql".to_owned())];
        for q in v.list_queries(&path) {
            run(c, &q.sql).await;
        }
    }
}

#[tokio::test]
async fn tls_modes() {
    for (p, cfg) in targets() {
        let ssl = "show session status like 'Ssl_cipher'";
        let prefer = ResolvedConfig {
            tls: TlsMode::Prefer,
            ..cfg.clone()
        };
        let mut c = open(p, &prefer).await;
        let out = run(c.as_mut(), ssl).await;
        let cipher = col::<StringArray>(&out, 1).value(0).to_owned();
        eprintln!("{}: prefer → cipher {cipher:?}", p.name());
        let require = ResolvedConfig {
            tls: TlsMode::Require,
            ..cfg.clone()
        };
        let driver = drivers()
            .into_iter()
            .find(|d| d.id().as_str() == p.id())
            .unwrap();
        match driver.connect(&require).await {
            Ok(mut c) => {
                let out = run(c.as_mut(), ssl).await;
                assert!(!col::<StringArray>(&out, 1).value(0).is_empty());
            }
            Err(e) => {
                assert!(p != Product::MySql, "MySQL require: {e}");
                eprintln!("{}: require → {e}", p.name());
            }
        }
        if p == Product::MySql {
            assert!(!cipher.is_empty(), "prefer uses TLS when offered");
        }
        let plain = ResolvedConfig {
            tls: TlsMode::Disable,
            ..cfg.clone()
        };
        let mut c = open(p, &plain).await;
        let out = run(c.as_mut(), ssl).await;
        assert_eq!(col::<StringArray>(&out, 1).value(0), "");
    }
}
