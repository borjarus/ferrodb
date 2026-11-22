//! Unit tests on the descriptor and listings; live tests against a server
//! when `FERRODB_TCK_CLICKHOUSE=clickhouse://default:ferrodb@localhost:56323/tck`
//! is set (`cargo xtask db-up` starts one).

use arrow_array::{Array, StringArray, TimestampMicrosecondArray};
use arrow_schema::{DataType, TimeUnit};

use super::*;
use crate::{CancellationToken, Error, sink::collect};

#[test]
fn descriptor_matches_the_catalog() {
    let d = ClickHouseDriver.descriptor();
    assert_eq!(d.id.as_str(), "clickhouse");
    assert_eq!(d.family.as_str(), "clickhouse");
    assert_eq!(d.capabilities.transactions, TxSupport::None);
    assert_eq!(d.default_port, Some(8123));
    assert_eq!(d.category, DriverCategory::AnalyticalCloud);
    assert_eq!(
        d.capabilities.language,
        ferrodb_api::QueryLanguage::sql("clickhouse")
    );
    let q = list_queries(&[(ObjectKind::Database, "it's".into())]);
    assert_eq!(q.len(), 2);
    assert!(q[0].sql.contains("database = 'it\\'s'"), "{}", q[0].sql);
    assert_eq!(ch_ident("a`b"), "`a\\`b`");
}

fn live() -> Option<ResolvedConfig> {
    let url = std::env::var("FERRODB_TCK_CLICKHOUSE").ok()?;
    Some(ResolvedConfig::from_url("clickhouse", &url).expect("valid URL"))
}

#[tokio::test]
async fn live_types_errors_and_bulk_load() {
    let Some(cfg) = live() else { return };
    let mut c = ClickHouseDriver.connect(&cfg).await.unwrap();
    assert!(c.server_version().starts_with("ClickHouse "));
    let cancel = CancellationToken::new();
    let out = collect(
        c.as_mut(),
        "select toDateTime('2026-11-09 10:11:12', 'UTC') ts, \
         CAST('b', 'Enum8(\\'a\\' = 1, \\'b\\' = 2)') e, toIPv4('1.2.3.4') ip, \
         toUUID('12345678-9abc-def0-0001-020304050607') u, toInt128(-5) i128, \
         toUInt32(7) u32, toFixedString('ab', 3) fs, [1, 2] arr, map('k', 1) m, \
         tuple(1, 'x') t, toLowCardinality('lc') lc, toNullable(toDecimal64(1.25, 2)) d",
        cancel.clone(),
    )
    .await
    .unwrap();
    let r = &out.results[0];
    let s = &r.schema;
    assert_eq!(
        s.field(0).data_type(),
        &DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
    );
    for i in [1, 2, 3, 4, 6, 10] {
        assert_eq!(s.field(i).data_type(), &DataType::Utf8, "{}", s.field(i));
    }
    assert_eq!(s.field(5).data_type(), &DataType::UInt32);
    assert!(matches!(s.field(7).data_type(), DataType::List(_)));
    assert!(matches!(s.field(8).data_type(), DataType::Map(..)));
    assert!(matches!(s.field(9).data_type(), DataType::Struct(_)));
    assert!(matches!(
        s.field(11).data_type(),
        DataType::Decimal128(18, 2)
    ));
    let b = &r.batches[0];
    let text = |i: usize| {
        b.column(i)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0)
            .to_owned()
    };
    assert_eq!(text(1), "b");
    assert_eq!(text(2), "1.2.3.4");
    assert_eq!(text(3), "12345678-9abc-def0-0001-020304050607");
    assert_eq!(text(4), "-5");
    assert_eq!(text(6), "ab");
    let ts = b
        .column(0)
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .unwrap();
    assert_eq!(ts.value(0), 1_794_219_072_000_000);

    // Syntax errors carry the position.
    let sql = "selec 1";
    let e = collect(c.as_mut(), sql, cancel.clone()).await.unwrap_err();
    let Error::Query(q) = e else { panic!("{e:?}") };
    assert_eq!(q.code.as_deref(), Some("SYNTAX_ERROR"));
    assert_eq!(&sql[q.range.unwrap()], "selec");

    // Session: USE and temporary tables persist; bulk load.
    collect(
        c.as_mut(),
        "create temporary table tmp_bulk (a Int32, s Nullable(String))",
        cancel.clone(),
    )
    .await
    .unwrap();
    let n = c
        .bulk_load(
            "tmp_bulk",
            &["a".into(), "s".into()],
            b"1\tx\n2\t\\N\n3\ta\\tb\n".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(n, 3);
    let out = collect(
        c.as_mut(),
        "select count(), countIf(s is null) from tmp_bulk",
        cancel.clone(),
    )
    .await
    .unwrap();
    assert_eq!(out.results[0].num_rows(), 1);
    c.set_schema("system").await.unwrap();
    assert_eq!(c.current_schema().await.unwrap().as_deref(), Some("system"));
    collect(c.as_mut(), "USE default", cancel.clone())
        .await
        .unwrap();
    assert_eq!(
        c.current_schema().await.unwrap().as_deref(),
        Some("default")
    );

    // Explicit formats come back as text.
    let out = collect(c.as_mut(), "select 1 as x format JSONEachRow", cancel)
        .await
        .unwrap();
    assert_eq!(out.results[0].schema.field(0).name(), "result");
}
