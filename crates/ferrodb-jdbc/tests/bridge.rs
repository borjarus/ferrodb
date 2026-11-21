//! The bridge end to end on embedded H2 (no server needed): types, metadata
//! calls, cancellation, crash and restart, streaming at constant memory.
//!
//! Runs when Java ≥ 17, `ferrodb-jdbc-host.jar` and the H2 jar are present;
//! `FERRODB_TCK_JDBC=1` downloads the jar into `target/ferrodb-drivers-test`.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use arrow_array::{Array, Decimal128Array, StringArray, TimestampMicrosecondArray};
use arrow_schema::{DataType, TimeUnit};
use ferrodb_api::{BridgeState, Error};
use ferrodb_drivers::{
    CancellationToken, Driver, Request, ResolvedConfig, ResultSink, SinkEvent,
    sink::{collect, collect_request},
};
use ferrodb_jdbc::{Bridge, JdbcDriver, flavor, testing};

fn setup() -> Option<(tokio::runtime::Runtime, Arc<Bridge>)> {
    match testing::bridge(&["h2"]) {
        Ok(b) => Some((tokio::runtime::Runtime::new().unwrap(), b)),
        Err(why) => {
            eprintln!("skipped: {why}");
            None
        }
    }
}

fn h2(bridge: &Arc<Bridge>) -> JdbcDriver {
    JdbcDriver::new(flavor("h2").unwrap(), bridge.clone())
}

fn mem(name: &str) -> ResolvedConfig {
    ResolvedConfig::new("h2")
        .with("mode", "memory")
        .with("database", name)
        .with("user", "sa")
}

#[test]
fn types_metadata_and_errors() {
    let Some((rt, bridge)) = setup() else { return };
    rt.block_on(async {
        let d = h2(&bridge);
        let mut c = d.connect(&mem("types")).await.unwrap();
        assert!(c.server_version().starts_with("H2 "), "{}", c.server_version());
        assert!(matches!(bridge.state(), BridgeState::Running { connections: 1, .. }));
        collect(
            c.as_mut(),
            "create table t (id int primary key, d decimal(12,3), ts timestamp, tz timestamp with time zone, \
             b varbinary(4), arr int array, c clob, n varchar(10) not null default 'x')",
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let ins = collect(
            c.as_mut(),
            "insert into t values (1, 12.345, timestamp '2026-11-21 10:11:12.123456', \
             timestamp with time zone '2026-11-21 10:00:00+02:00', x'0102', array[1,2], 'zażółć', 'y')",
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(ins.summary.affected, Some(1));
        let out = collect(c.as_mut(), "select d, ts, tz, b, arr, c from t", CancellationToken::new())
            .await
            .unwrap();
        let r = &out.results[0];
        let types: Vec<DataType> = r.schema.fields().iter().map(|f| f.data_type().clone()).collect();
        assert_eq!(
            types,
            [
                DataType::Decimal128(12, 3),
                DataType::Timestamp(TimeUnit::Microsecond, None),
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                DataType::Binary,
                DataType::Utf8,
                DataType::Utf8,
            ]
        );
        let b = &r.batches[0];
        let d = b.column(0).as_any().downcast_ref::<Decimal128Array>().unwrap();
        assert_eq!(d.value(0), 12345);
        let ts = b.column(1).as_any().downcast_ref::<TimestampMicrosecondArray>().unwrap();
        assert_eq!(ts.value(0), 1_795_255_872_123_456);
        let tz = b.column(2).as_any().downcast_ref::<TimestampMicrosecondArray>().unwrap();
        assert_eq!(tz.value(0), 1_795_248_000_000_000, "10:00+02:00 = 08:00 UTC");
        let arr = b.column(4).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(arr.value(0), "[1, 2]");
        let clob = b.column(5).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(clob.value(0), "zażółć");
        assert_eq!(
            r.schema.field(0).metadata().get(ferrodb_drivers::columns::DB_TYPE_KEY).map(String::as_str),
            Some("decimal")
        );

        // DatabaseMetaData through native calls.
        let cols = collect_request(
            c.as_mut(),
            Request::native("columns", &[("schema", "PUBLIC"), ("table", "T")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let rows = cols.results[0].num_rows();
        assert_eq!(rows, 8);
        let pks = collect_request(
            c.as_mut(),
            Request::native("primary_keys", &[("schema", "PUBLIC"), ("each_table", "true")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(pks.results[0].num_rows(), 1);
        let info = collect_request(c.as_mut(), Request::native("info", &[]), CancellationToken::new())
            .await
            .unwrap();
        assert!(info.results[0].num_rows() > 5);

        // Errors carry SQLState and position.
        let sql = "select * from no_such_table";
        match collect(c.as_mut(), sql, CancellationToken::new()).await {
            Err(Error::Query(q)) => {
                assert!(q.code.as_deref().unwrap_or_default().starts_with("42S02"), "{q:?}");
                assert_eq!(&sql[q.range.unwrap().start..], "no_such_table");
                assert!(!q.message.contains("SQL statement:"), "{}", q.message);
            }
            other => panic!("{other:?}"),
        }
        c.ping().await.unwrap();

        // Read-only sessions refuse writes even where the driver does not.
        c.set_read_only(true).await.unwrap();
        assert_eq!(
            collect(c.as_mut(), "delete from t", CancellationToken::new()).await.err(),
            Some(Error::ReadOnly)
        );
        collect(c.as_mut(), "select count(*) from t", CancellationToken::new())
            .await
            .unwrap();
    });
}

#[test]
fn cancellation_is_prompt() {
    let Some((rt, bridge)) = setup() else { return };
    rt.block_on(async {
        let d = h2(&bridge);
        let mut c = d.connect(&mem("cancel")).await.unwrap();
        let token = CancellationToken::new();
        let t2 = token.clone();
        let sql = "select count(*) from system_range(1, 100000000) a, system_range(1, 100000) b";
        let canceller = async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            t2.cancel();
            Instant::now()
        };
        let (r, at) = futures::join!(collect(c.as_mut(), sql, token), canceller);
        assert_eq!(r.err(), Some(Error::Cancelled));
        assert!(at.elapsed() < Duration::from_secs(1), "{:?}", at.elapsed());
        c.ping().await.unwrap();

        // Remote cancel (the core's Cancel button) works the same way.
        let rc = c.remote_cancel().unwrap();
        let canceller = async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            rc.cancel().await.unwrap();
            Instant::now()
        };
        let (r, at) = futures::join!(
            collect(c.as_mut(), sql, CancellationToken::new()),
            canceller
        );
        assert_eq!(r.err(), Some(Error::Cancelled));
        assert!(at.elapsed() < Duration::from_secs(1), "{:?}", at.elapsed());
        c.ping().await.unwrap();
    });
}

#[test]
fn crash_and_restart() {
    let Some((rt, bridge)) = setup() else { return };
    let states = Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = states.clone();
    bridge.on_change(move |s| s2.lock().unwrap().push(s.clone()));
    rt.block_on(async {
        let d = h2(&bridge);
        let mut c = d.connect(&mem("crash")).await.unwrap();
        let pid = match bridge.state() {
            BridgeState::Running { pid, .. } => pid,
            other => panic!("{other:?}"),
        };
        bridge.kill().await;
        // The old session reports a connection error, never hangs.
        let r = tokio::time::timeout(Duration::from_secs(10), c.ping())
            .await
            .unwrap();
        assert!(
            matches!(r, Err(Error::Connection(_) | Error::ConnectionLost)),
            "{r:?}"
        );
        assert!(!c.is_alive());
        // A new connection gets a fresh process.
        let mut c2 = d.connect(&mem("crash")).await.unwrap();
        c2.ping().await.unwrap();
        match bridge.state() {
            BridgeState::Running { pid: p2, .. } => assert_ne!(p2, pid),
            other => panic!("{other:?}"),
        }
        drop(c);
    });
    let seen = states.lock().unwrap().clone();
    assert!(seen.contains(&BridgeState::Restarting), "{seen:?}");
}

/// 1M rows through the bridge at constant memory (release-like speed needs
/// `--release`; run with `cargo test -p ferrodb-jdbc --test bridge -- --ignored`).
#[test]
#[ignore]
fn million_rows_stream() {
    let Some((rt, bridge)) = setup() else { return };
    rt.block_on(async {
        let d = h2(&bridge);
        let mut c = d.connect(&mem("million")).await.unwrap();
        let sql = "select x as id, concat('name ', x) as name, x * 0.5 as amount, \
                   mod(x, 2) = 0 as flag, timestamp '2026-01-01 00:00:00' as ts \
                   from system_range(1, 1000000)";
        let (sink, mut rx) = ResultSink::channel(8, ResultSink::BATCH_ROWS);
        let started = Instant::now();
        let rss = || memory_stats::memory_stats().map_or(0, |m| m.physical_mem) as f64 / 1_048_576.0;
        let before = rss();
        let exec = c.execute(Request::Text { text: sql.into() }, sink, CancellationToken::new());
        let reader = async move {
            let (mut rows, mut peak, mut first) = (0usize, 0f64, None);
            while let Some(ev) = rx.recv().await {
                if let SinkEvent::Batch(b) = ev {
                    first.get_or_insert(started.elapsed());
                    rows += b.num_rows();
                    if rows % 100_000 < b.num_rows() {
                        peak = peak.max(rss());
                    }
                }
            }
            (rows, peak, first)
        };
        let (summary, (rows, peak, first)) = futures::join!(exec, reader);
        summary.unwrap();
        let secs = started.elapsed().as_secs_f64();
        println!(
            "1M rows in {secs:.2} s ({:.0} rows/s), first batch {first:?}, RSS {before:.1} → peak {peak:.1} MB",
            rows as f64 / secs
        );
        assert_eq!(rows, 1_000_000);
        assert!(peak - before < 200.0, "memory grew by {:.1} MB", peak - before);
    });
}
