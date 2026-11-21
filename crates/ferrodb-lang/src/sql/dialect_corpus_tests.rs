//! Corpus per level B dialect (phase 8.6): typical scripts of each product
//! give no error diagnostics (lexical, `sqlparser` syntax where the dialect
//! has a parser, statement words), split into the expected statements and
//! format idempotently; distinctive built-in functions complete.

use ferrodb_api::{FormatStyle, Severity};

use super::{
    Flavor, complete, dialect::Dialect, document::Document, format::format, inspect,
    schema::SchemaModel, split,
};

/// (dialect, script, statement count, (typed prefix, function expected in the top 3)).
/// (typed prefix, expected function).
type Probes = &'static [(&'static str, &'static str)];

const CORPUS: &[(&str, &str, usize, Probes)] = &[
    (
        "greenplum",
        "CREATE TABLE sales (id int, amount numeric) DISTRIBUTED BY (id);\nSELECT gp_segment_id, count(*) FROM sales GROUP BY 1;\nSELECT median(amount) FROM sales;",
        3,
        &[("SELECT medi", "median")],
    ),
    (
        "cockroachdb",
        "SELECT unique_rowid(), gen_random_uuid();\nSELECT * FROM t AS OF SYSTEM TIME '-10s';\nUPSERT INTO t (id, v) VALUES (1, 'a');",
        3,
        &[("SELECT unique_r", "unique_rowid")],
    ),
    (
        "redshift",
        "SELECT listagg(name, ',') WITHIN GROUP (ORDER BY name) FROM users;\nSELECT getdate(), dateadd(day, 1, getdate());\nSELECT nvl(a, 0) FROM t WHERE b IS NOT NULL;",
        3,
        &[("SELECT lista", "listagg"), ("SELECT getd", "getdate")],
    ),
    (
        "db2",
        "SELECT * FROM employee FETCH FIRST 10 ROWS ONLY;\nSELECT varchar_format(current timestamp, 'YYYY-MM-DD') FROM sysibm.sysdummy1;\nVALUES current date;",
        3,
        &[("SELECT varchar_f", "varchar_format")],
    ),
    (
        "sybase",
        "SELECT TOP 10 * FROM sysobjects\ngo\nSELECT str_replace(name, 'a', 'b'), getdate() FROM sysusers\ngo\nsp_help 'tbl'",
        3,
        &[("SELECT str_re", "str_replace")],
    ),
    (
        "h2",
        "CREATE TABLE t (id IDENTITY PRIMARY KEY, j JSON);\nSELECT random_uuid(), dateadd('DAY', 1, current_date) FROM dual;\nMERGE INTO t KEY (id) VALUES (1, NULL);",
        3,
        &[("SELECT random_u", "random_uuid")],
    ),
    (
        "derby",
        "SELECT * FROM app.t FETCH FIRST 5 ROWS ONLY;\nVALUES current_timestamp;\nCALL syscs_util.syscs_compress_table('APP', 'T', 1);",
        3,
        &[("SELECT ucas", "ucase")],
    ),
    (
        "hsqldb",
        "SELECT * FROM information_schema.tables LIMIT 5;\nSELECT datediff('day', a, b), regexp_replace(s, 'a', 'b') FROM t;\nCALL now();",
        3,
        &[("SELECT regexp_rep", "regexp_replace")],
    ),
    (
        "exasol",
        "SELECT add_days(current_date, 1), zeroifnull(x) FROM t;\nSELECT group_concat(name ORDER BY name SEPARATOR ',') FROM users;\nOPEN SCHEMA retail;",
        3,
        &[
            ("SELECT add_da", "add_days"),
            ("SELECT zeroif", "zeroifnull"),
        ],
    ),
    (
        "vertica",
        "SELECT time_slice(ts, 5, 'MINUTE'), count(*) FROM events GROUP BY 1;\nCREATE PROJECTION p AS SELECT * FROM events ORDER BY ts SEGMENTED BY hash(id) ALL NODES;\nSELECT approximate_count_distinct(user_id) FROM events;",
        3,
        &[("SELECT time_sl", "time_slice")],
    ),
    (
        "hiveql",
        "SELECT `user`, collect_set(item) FROM orders LATERAL VIEW explode(items) t AS item GROUP BY `user`;\nINSERT OVERWRITE TABLE daily PARTITION (dt = '2024-01-01') SELECT * FROM staging;\nSELECT get_json_object(payload, \"$.id\") FROM raw;",
        3,
        &[
            ("SELECT collect_s", "collect_set"),
            ("SELECT get_json", "get_json_object"),
        ],
    ),
    (
        "snowflake",
        "SELECT f.value:name::string FROM t, LATERAL FLATTEN(input => t.data) f;\nSELECT iff(a > 0, 'pos', 'neg'), parse_json('{}') FROM t QUALIFY row_number() OVER (ORDER BY a) = 1;\nCREATE OR REPLACE FUNCTION f() RETURNS int AS $$ 1 $$;",
        3,
        &[("SELECT iff", "iff"), ("SELECT parse_j", "parse_json")],
    ),
    (
        "googlesql",
        "SELECT * FROM `proj.dataset.table` WHERE _PARTITIONTIME > TIMESTAMP(\"2024-01-01\");\nSELECT safe_divide(a, b), farm_fingerprint(s) FROM t # comment\n;\nSELECT * FROM t WHERE x = @param;",
        3,
        &[
            ("SELECT safe_di", "safe_divide"),
            ("SELECT farm_f", "farm_fingerprint"),
        ],
    ),
    (
        "clickhouse",
        "SELECT toStartOfMonth(ts) AS m, uniqExact(user_id) FROM events GROUP BY m ORDER BY m;\nCREATE TABLE t (id UInt64, s String) ENGINE = MergeTree ORDER BY id;\nSELECT * FROM t FINAL WHERE id IN (1, 2) SETTINGS max_threads = 4;",
        3,
        &[
            ("SELECT toStartOfM", "toStartOfMonth"),
            ("SELECT uniqE", "uniqExact"),
        ],
    ),
];

#[test]
fn every_catalog_dialect_has_its_own_file() {
    // The 20 SQL dialect ids of the 26 data sources (ferrodb-mock catalog)
    // plus the SQL-like languages.
    for id in [
        "postgresql",
        "greenplum",
        "cockroachdb",
        "redshift",
        "mysql",
        "mariadb",
        "sqlite",
        "tsql",
        "oracle",
        "db2",
        "sybase",
        "h2",
        "derby",
        "hsqldb",
        "exasol",
        "vertica",
        "hiveql",
        "snowflake",
        "googlesql",
        "clickhouse",
        "duckdb",
        "cql",
        "sqlpp",
        "partiql",
    ] {
        let d = Dialect::get(id);
        assert_eq!(d.id, id);
        assert!(
            d.functions.len() >= 8,
            "{id}: {} functions",
            d.functions.len()
        );
    }
    // Derived dialects inherit and remove.
    let rs = Dialect::get("redshift");
    assert!(rs.function("date_trunc").is_some() && rs.function("listagg").is_some());
    assert!(rs.function("string_agg").is_none() && !rs.is_type("jsonb"));
    let syb = Dialect::get("sybase");
    assert!(syb.function("getdate").is_some() && syb.function("string_agg").is_none());
    assert_eq!(syb.quote, '[');
    // Big catalogs are big.
    for (id, min) in [
        ("tsql", 200),
        ("oracle", 200),
        ("snowflake", 150),
        ("googlesql", 150),
        ("clickhouse", 150),
    ] {
        assert!(Dialect::get(id).functions.len() >= min, "{id}");
    }
    // ClickHouse keeps the product's spelling.
    assert_eq!(
        Dialect::get("clickhouse")
            .function("tostartofmonth")
            .unwrap()
            .name,
        "toStartOfMonth"
    );
}

#[test]
fn level_b_corpus() {
    let empty = SchemaModel::empty();
    for (dialect, script, count, probes) in CORPUS {
        let st = split(script, &Flavor::for_dialect(dialect));
        assert_eq!(
            st.len(),
            *count,
            "{dialect}: {:#?}",
            st.iter().map(|s| s.text(script)).collect::<Vec<_>>()
        );
        let doc = Document::analyze(script, dialect, &empty, None);
        let errs: Vec<String> = inspect::diagnostics(&doc)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| format!("{:?}: {}", &script[d.range.clone()], d.message))
            .collect();
        assert!(errs.is_empty(), "{dialect}: {errs:#?}");
        let style = FormatStyle::default();
        let once = format(script, dialect, &style);
        assert_eq!(format(&once, dialect, &style), once, "{dialect}");
        assert_eq!(
            split(&once, &Flavor::for_dialect(dialect)).len(),
            *count,
            "{dialect}: {once}"
        );
        for (typed, expected) in *probes {
            let doc = Document::analyze(typed, dialect, &empty, None);
            let items = complete::complete(&doc, typed.len(), &[]);
            let top: Vec<&str> = items.iter().take(3).map(|i| i.label.as_str()).collect();
            assert!(top.contains(expected), "{dialect}: {typed:?} -> {top:?}");
        }
    }
}

#[test]
fn quoting_and_folding_per_dialect() {
    assert_eq!(
        Dialect::get("snowflake").quote_if_needed("orders"),
        "\"orders\""
    );
    assert_eq!(
        Dialect::get("snowflake").quote_if_needed("ORDERS"),
        "ORDERS"
    );
    assert_eq!(Dialect::get("hiveql").quote_if_needed("Orders"), "`Orders`");
    assert_eq!(
        Dialect::get("clickhouse").quote_if_needed("Orders"),
        "Orders"
    );
    assert_eq!(
        Dialect::get("googlesql").quote_if_needed("my-table"),
        "`my-table`"
    );
    assert_eq!(Dialect::get("tsql").quote_if_needed("order"), "[order]");
    assert_eq!(Dialect::get("oracle").quote_if_needed("Emp"), "\"Emp\"");
    assert_eq!(Dialect::get("db2").default_schema, None);
    assert_eq!(Dialect::get("derby").default_schema.as_deref(), Some("APP"));
}
