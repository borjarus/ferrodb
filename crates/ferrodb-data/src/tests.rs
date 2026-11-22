//! Export → import round trips for every format that can be read back.

use std::sync::Arc;

use arrow_array::{
    ArrayRef, BooleanArray, Date32Array, Decimal128Array, Float64Array, Int64Array, RecordBatch,
    StringArray, TimestampMicrosecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use bson::doc;
use ferrodb_api::{DataFormat, FormatOptions};
use proptest::prelude::*;

use crate::{
    export::Exporter,
    import::{self, Mapping},
    value::{Value, ValueType},
};

fn sample() -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("price", DataType::Decimal128(10, 2), true),
        Field::new("ratio", DataType::Float64, true),
        Field::new("active", DataType::Boolean, true),
        Field::new("born", DataType::Date32, true),
        Field::new(
            "seen",
            DataType::Timestamp(TimeUnit::Microsecond, Some(Arc::from("UTC"))),
            true,
        ),
    ]));
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(vec![1, 2, 3])),
        Arc::new(StringArray::from(vec![
            Some("Kowalski, \"Jan\""),
            Some("zażółć\ngęślą | <b>"),
            None,
        ])),
        Arc::new(
            Decimal128Array::from(vec![Some(1999), None, Some(-5)])
                .with_precision_and_scale(10, 2)
                .unwrap(),
        ),
        Arc::new(Float64Array::from(vec![Some(0.25), Some(1e10), None])),
        Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])),
        Arc::new(Date32Array::from(vec![Some(19723), None, Some(0)])),
        Arc::new(
            TimestampMicrosecondArray::from(vec![Some(1_704_164_645_123_000), None, Some(0)])
                .with_timezone("UTC"),
        ),
    ];
    RecordBatch::try_new(schema, cols).unwrap()
}

fn target() -> Vec<(String, ValueType)> {
    sample()
        .schema()
        .fields()
        .iter()
        .map(|f| (f.name().clone(), ValueType::from_arrow(f.data_type())))
        .collect()
}

fn expected() -> Vec<Vec<Value>> {
    let b = sample();
    (0..b.num_rows())
        .map(|r| {
            b.columns()
                .iter()
                .map(|c| crate::value::value_at(c.as_ref(), r))
                .collect()
        })
        .collect()
}

fn round_trip(format: DataFormat) -> Vec<Vec<Value>> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("out.{}", format.extension()));
    let opts = FormatOptions::default();
    let batch = sample();
    let mut ex = Exporter::create(&path, format, &opts, "postgresql").unwrap();
    ex.begin_table(&batch.schema()).unwrap();
    ex.write_batch(&batch.slice(0, 2)).unwrap();
    ex.write_batch(&batch.slice(2, 1)).unwrap();
    let stats = ex.finish().unwrap();
    assert_eq!(stats.items, 3);
    assert!(stats.bytes > 0);
    let mut rd = import::open(&path, format, &opts).unwrap();
    let m = Mapping::new(rd.columns(), &target(), &[]).unwrap();
    assert_eq!(m.names().len(), 7, "{format:?}: {:?}", rd.columns());
    let rows = rd.next_rows(100).unwrap();
    rows.iter().map(|r| m.row(r).unwrap()).collect()
}

#[test]
fn round_trips() {
    let want = expected();
    for f in [
        DataFormat::Csv,
        DataFormat::Tsv,
        DataFormat::Json,
        DataFormat::JsonLines,
        DataFormat::ExtendedJson,
        DataFormat::Parquet,
    ] {
        let got = round_trip(f);
        for (g, w) in got.iter().zip(&want) {
            for (c, (a, b)) in g.iter().zip(w).enumerate() {
                // Parquet keeps decimals as text: compare as text.
                assert_eq!(a.to_text(), b.to_text(), "{f:?} column {c}");
            }
        }
        assert_eq!(got.len(), want.len(), "{f:?}");
    }
}

#[test]
fn xlsx_round_trip() {
    let got = round_trip(DataFormat::Xlsx);
    let want = expected();
    assert_eq!(got.len(), 3);
    // Excel keeps numbers as doubles and dates as serials (ms precision).
    assert_eq!(got[0][0], want[0][0]);
    assert_eq!(got[0][1], want[0][1]);
    assert_eq!(got[1][1], want[1][1]);
    assert_eq!(got[0][5], want[0][5]);
    assert_eq!(got[0][2].to_text().as_deref(), Some("19.99"));
    assert_eq!(got[0][6].to_text(), want[0][6].to_text());
}

#[test]
fn text_formats() {
    let dir = tempfile::tempdir().unwrap();
    let batch = sample();
    let mut out = std::collections::HashMap::new();
    for f in [
        DataFormat::Markdown,
        DataFormat::Html,
        DataFormat::SqlInsert,
    ] {
        let path = dir.path().join(format!("o.{}", f.extension()));
        let opts = FormatOptions {
            table: Some("people".into()),
            rows_per_statement: 2,
            ..Default::default()
        };
        let mut ex = Exporter::create(&path, f, &opts, "postgresql").unwrap();
        ex.begin_table(&batch.schema()).unwrap();
        ex.write_batch(&batch).unwrap();
        ex.finish().unwrap();
        out.insert(f, std::fs::read_to_string(&path).unwrap());
    }
    let md = &out[&DataFormat::Markdown];
    assert!(md.starts_with("| id | name | price |"));
    assert!(md.contains("zażółć<br>gęślą \\| <b>"));
    let html = &out[&DataFormat::Html];
    assert!(html.contains("<td>zażółć\ngęślą | &lt;b&gt;</td>"));
    let sql = &out[&DataFormat::SqlInsert];
    assert_eq!(sql.matches("INSERT INTO").count(), 2, "{sql}");
    assert!(sql.starts_with(
        "INSERT INTO people (id, name, price, ratio, active, born, seen) VALUES (1, 'Kowalski, \"Jan\"', 19.99, 0.25, TRUE, '2024-01-01', '2024-01-02 03:04:05.123000+00:00'), ("
    ), "{sql}");
}

#[test]
fn documents_to_tabular_and_back() {
    let dir = tempfile::tempdir().unwrap();
    let docs = vec![
        doc! { "_id": 1, "name": "a", "tags": ["x", "y"], "n": 1.5 },
        doc! { "_id": 2, "name": "b", "late": true },
    ];
    let p = dir.path().join("d.csv");
    let mut ex = Exporter::create(&p, DataFormat::Csv, &FormatOptions::default(), "sql").unwrap();
    ex.write_decoded(&docs).unwrap();
    let stats = ex.finish().unwrap();
    assert_eq!(stats.items, 2);
    let text = std::fs::read_to_string(&p).unwrap();
    assert_eq!(
        text,
        "_id,name,tags,n,late\n1,a,\"[\"\"x\"\",\"\"y\"\"]\",1.5,\n2,b,,,true\n"
    );

    // Canonical Extended JSON keeps BSON types.
    let p = dir.path().join("d.ejson");
    let mut ex = Exporter::create(
        &p,
        DataFormat::ExtendedJson,
        &FormatOptions::default(),
        "sql",
    )
    .unwrap();
    let typed = vec![
        doc! { "_id": bson::oid::ObjectId::new(), "big": 1i64 << 40, "d": bson::DateTime::from_millis(5) },
    ];
    ex.write_decoded(&typed).unwrap();
    ex.finish().unwrap();
    let mut rd = import::open(&p, DataFormat::ExtendedJson, &FormatOptions::default()).unwrap();
    assert_eq!(rd.next_documents(10).unwrap(), typed);
}

#[test]
fn large_csv_export_streams() {
    // Batches go straight to the file: 200k rows in 200 batches.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.csv");
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("v", DataType::Utf8, false),
    ]));
    let mut ex = Exporter::create(&p, DataFormat::Csv, &FormatOptions::default(), "sql").unwrap();
    ex.begin_table(&schema).unwrap();
    for b in 0..200i64 {
        let ids: Vec<i64> = (b * 1000..(b + 1) * 1000).collect();
        let vs: Vec<String> = ids.iter().map(|i| format!("row {i}")).collect();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(vs)),
            ],
        )
        .unwrap();
        ex.write_batch(&batch).unwrap();
    }
    assert_eq!(ex.finish().unwrap().items, 200_000);
    let mut rd = import::open(&p, DataFormat::Csv, &FormatOptions::default()).unwrap();
    let mut n = 0;
    loop {
        let rows = rd.next_rows(5000).unwrap();
        if rows.is_empty() {
            break;
        }
        n += rows.len();
    }
    assert_eq!(n, 200_000);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any text (quotes, separators, new lines, Unicode) survives CSV and
    /// JSON Lines.
    #[test]
    fn text_survives(cells in proptest::collection::vec("\\PC{0,12}|[\",\\n\\t\\\\]{1,3}", 1..6)) {
        let dir = tempfile::tempdir().unwrap();
        for f in [DataFormat::Csv, DataFormat::JsonLines, DataFormat::Parquet] {
            let p = dir.path().join(format!("p.{}", f.extension()));
            let mut ex = Exporter::create(&p, f, &FormatOptions { null_text: "\\N".into(), ..Default::default() }, "sql").unwrap();
            ex.begin_rows(&[("t".into(), ValueType::Text)]).unwrap();
            for c in &cells {
                ex.write_row(&[Value::Text(c.clone())]).unwrap();
            }
            ex.finish().unwrap();
            let mut rd = import::open(&p, f, &FormatOptions { null_text: "\\N".into(), ..Default::default() }).unwrap();
            let m = Mapping::new(rd.columns(), &[("t".into(), ValueType::Text)], &[]).unwrap();
            let got: Vec<Value> = rd.next_rows(100).unwrap().iter().map(|r| m.row(r).unwrap()[0].clone()).collect();
            let want: Vec<Value> = cells.iter().map(|c| {
                // An empty JSON string is still a string; an empty CSV
                // field is not NULL because NULL is `\N` here.
                Value::Text(c.clone())
            }).collect();
            prop_assert_eq!(got, want, "{:?}", f);
        }
    }
}

#[test]
fn export_through_extractor_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let lib = ferrodb_script::ScriptLibrary::in_memory();

    // Arrow rows through a built-in: decimals, dates and timestamps as text.
    let p = dir.path().join("rows.sql");
    let opts = FormatOptions::default();
    let batch = sample();
    let mut ex =
        Exporter::create_script(&p, lib.extractor("builtin:sql-values").unwrap(), &opts).unwrap();
    ex.begin_table(&batch.schema()).unwrap();
    ex.write_batch(&batch.slice(0, 2)).unwrap();
    ex.write_batch(&batch.slice(2, 1)).unwrap();
    let stats = ex.finish().unwrap();
    assert_eq!(stats.items, 3);
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "(1, 'Kowalski, \"Jan\"', '19.99', 0.25, TRUE, '2024-01-01', '2024-01-02 03:04:05.123000+00:00'),\n\
         (2, 'zażółć\ngęślą | <b>', NULL, 10000000000, FALSE, NULL, NULL),\n\
         (3, NULL, '-0.05', NULL, NULL, '1970-01-01', '1970-01-01 00:00:00+00:00')\n"
    );

    // Column types and options reach the script.
    let (_, s) = ferrodb_script::compile(
        ferrodb_api::ExtScriptKind::Extractor,
        "types",
        "fn header(columns) { join(columns.map(|c| c.type), \",\") + \"/\" + options().table + \"\\n\" }\n\
         fn row(values) { text(values[0], options().null_text) + \"\\n\" }",
        None,
    );
    let p = dir.path().join("types.txt");
    let opts = FormatOptions {
        table: Some("t".into()),
        null_text: "-".into(),
        ..Default::default()
    };
    let mut ex = Exporter::create_script(&p, s.unwrap(), &opts).unwrap();
    ex.begin_table(&batch.schema()).unwrap();
    ex.write_batch(&batch).unwrap();
    ex.finish().unwrap();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "integer,text,decimal,float,boolean,date,timestamptz/t\n1\n2\n3\n"
    );

    // BSON documents: top-level fields become columns, nested values JSON text.
    let docs = vec![
        doc! { "_id": 1, "name": "a", "tags": ["x", "y"] },
        doc! { "_id": 2, "name": "b" },
    ];
    let p = dir.path().join("docs.json");
    let mut ex = Exporter::create_script(
        &p,
        lib.extractor("builtin:json-keyed").unwrap(),
        &FormatOptions::default(),
    )
    .unwrap();
    ex.write_decoded(&docs).unwrap();
    assert_eq!(ex.finish().unwrap().items, 2);
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "{\n  \"1\": {\"name\": \"a\", \"tags\": \"[\\\"x\\\",\\\"y\\\"]\"},\n  \
         \"2\": {\"name\": \"b\", \"tags\": null}\n}\n"
    );

    // An empty result still gets the script's header and footer.
    let p = dir.path().join("empty.sql");
    let ex = Exporter::create_script(
        &p,
        lib.extractor("builtin:sql-in-list").unwrap(),
        &FormatOptions::default(),
    )
    .unwrap();
    ex.finish().unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "()");

    // A failing script fails the export with its message.
    let (_, s) = ferrodb_script::compile(
        ferrodb_api::ExtScriptKind::Extractor,
        "bad",
        "fn row(values) { values[99] }",
        None,
    );
    let p = dir.path().join("bad.txt");
    let mut ex = Exporter::create_script(&p, s.unwrap(), &FormatOptions::default()).unwrap();
    ex.begin_table(&batch.schema()).unwrap();
    let e = ex.write_batch(&batch).unwrap_err();
    assert!(e.to_string().contains("script `bad`"), "{e}");
    assert!(Exporter::create(&p, DataFormat::Script, &FormatOptions::default(), "sql").is_err());
}
