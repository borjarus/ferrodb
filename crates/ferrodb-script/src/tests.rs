use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use ferrodb_api::{
    Error, ExtScriptKind, FormatOptions, ScriptService, ScriptTable, ScriptValue as V,
};
use futures::executor::block_on;

use crate::*;

fn lib() -> ScriptLibrary {
    ScriptLibrary::in_memory()
}

fn table() -> ScriptTable {
    ScriptTable {
        columns: vec!["id".into(), "name".into(), "price".into()],
        types: vec!["integer".into(), "text".into(), "decimal".into()],
        rows: vec![
            vec![V::Int(1), V::Text("O'Brien".into()), V::Text("1.50".into())],
            vec![V::Int(2), V::Null, V::Float(2.25)],
            vec![V::Int(3), V::Text("a|b".into()), V::Bool(true)],
        ],
    }
}

fn extract(id: &str, t: &ScriptTable) -> String {
    lib()
        .extractor(id)
        .unwrap()
        .extract(t, &FormatOptions::default())
        .unwrap()
}

fn agg(id: &str, values: Vec<V>) -> String {
    let t = ScriptTable {
        columns: vec!["x".into()],
        types: vec![String::new()],
        rows: values.into_iter().map(|v| vec![v]).collect(),
    };
    block_on(lib().aggregate(id, t)).unwrap()
}

fn ints(xs: &[i64]) -> Vec<V> {
    xs.iter().map(|&i| V::Int(i)).collect()
}

fn adhoc(kind: ExtScriptKind, src: &str) -> CompiledScript {
    let (info, s) = compile(kind, "test", src, Some("test.rhai".into()));
    s.unwrap_or_else(|e| panic!("{e}: {:?}", info.error))
}

// ------------------------------------------------------------------ built-ins

#[test]
fn builtins_compile_and_are_listed() {
    let l = lib();
    let ex = l.list(ExtScriptKind::Extractor);
    let ag = l.list(ExtScriptKind::Aggregator);
    assert_eq!(ex.len(), 6);
    assert_eq!(ag.len(), 7);
    for i in ex.iter().chain(&ag) {
        assert!(i.builtin && i.path.is_none(), "{i:?}");
        assert!(i.error.is_none(), "{i:?}");
        assert!(i.id.starts_with("builtin:"));
        assert!(!i.description.is_empty());
    }
    assert_eq!(ex[0].id, "builtin:sql-in-list");
    assert_eq!(ex[0].name, "SQL IN list");
    assert_eq!(ex[0].extension, "sql");
    assert!(ag.iter().all(|i| i.extension.is_empty()));
    assert!(block_on(l.reload()).is_ok());
    assert_eq!(l.directory(), None);
}

#[test]
fn sql_in_list() {
    assert_eq!(extract("builtin:sql-in-list", &table()), "(1, 2, 3)");
    let t = ScriptTable {
        columns: vec!["s".into()],
        types: vec!["text".into()],
        rows: vec![
            vec![V::Text("x".into())],
            vec![V::Text("it's".into())],
            vec![V::Null],
            vec![V::Float(1.5)],
        ],
    };
    assert_eq!(
        extract("builtin:sql-in-list", &t),
        "('x', 'it''s', NULL, 1.5)"
    );
    let empty = ScriptTable {
        columns: vec!["s".into()],
        ..Default::default()
    };
    assert_eq!(extract("builtin:sql-in-list", &empty), "()");
}

#[test]
fn sql_values() {
    assert_eq!(
        extract("builtin:sql-values", &table()),
        "(1, 'O''Brien', '1.50'),\n(2, NULL, 2.25),\n(3, 'a|b', TRUE)\n"
    );
}

#[test]
fn json_keyed() {
    let out = extract("builtin:json-keyed", &table());
    assert_eq!(
        out,
        "{\n  \"1\": {\"name\": \"O'Brien\", \"price\": \"1.50\"},\n  \
         \"2\": {\"name\": null, \"price\": 2.25},\n  \
         \"3\": {\"name\": \"a|b\", \"price\": true}\n}\n"
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["2"]["price"], 2.25);
}

#[test]
fn markdown_aligned() {
    let out = extract("builtin:markdown-aligned", &table());
    assert_eq!(
        out,
        "|  id | name    | price |\n\
         | --: | ------- | ----: |\n\
         |   1 | O'Brien |  1.50 |\n\
         |   2 |         |  2.25 |\n\
         |   3 | a\\|b    |  true |\n"
    );
}

#[test]
fn php_array() {
    assert_eq!(
        extract("builtin:php-array", &table()),
        "<?php\nreturn [\n\
         \x20   ['id' => 1, 'name' => 'O\\'Brien', 'price' => '1.50'],\n\
         \x20   ['id' => 2, 'name' => null, 'price' => 2.25],\n\
         \x20   ['id' => 3, 'name' => 'a|b', 'price' => true],\n\
         ];\n"
    );
}

#[test]
fn pipe_separated_honours_options() {
    let s = lib().extractor("builtin:pipe-separated").unwrap();
    assert_eq!(
        s.extract(&table(), &FormatOptions::default()).unwrap(),
        "id|name|price\n1|O'Brien|1.50\n2||2.25\n3|a\\|b|true\n"
    );
    let opts = FormatOptions {
        header: false,
        null_text: "NULL".into(),
        ..Default::default()
    };
    assert_eq!(
        s.extract(&table(), &opts).unwrap(),
        "1|O'Brien|1.50\n2|NULL|2.25\n3|a\\|b|true\n"
    );
}

#[test]
fn aggregators() {
    let xs = ints(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    assert_eq!(
        agg(
            "builtin:count",
            vec![V::Int(1), V::Null, V::Text("a".into())]
        ),
        "2"
    );
    assert_eq!(
        agg(
            "builtin:distinct",
            vec![
                V::Int(1),
                V::Int(1),
                V::Null,
                V::Text("a".into()),
                V::Text("a".into())
            ]
        ),
        "2"
    );
    assert_eq!(agg("builtin:median", ints(&[5, 1, 3])), "3");
    assert_eq!(agg("builtin:median", ints(&[4, 1, 3, 2])), "2.5");
    assert_eq!(agg("builtin:p90", xs.clone()), "9.1");
    assert_eq!(agg("builtin:p90", ints(&[7])), "7");
    assert_eq!(
        agg("builtin:stddev", ints(&[2, 4, 4, 4, 5, 5, 7, 9])),
        "2.1380899353"
    );
    assert_eq!(agg("builtin:stddev", ints(&[1])), "");
    assert_eq!(
        agg(
            "builtin:concat",
            vec![V::Int(1), V::Null, V::Text("x".into()), V::Float(2.5)]
        ),
        "1, x, 2.5"
    );
    // Numeric text counts as a number; other text and NULL are skipped.
    assert_eq!(
        agg(
            "builtin:min-max",
            vec![
                V::Text("-3".into()),
                V::Null,
                V::Text("x".into()),
                V::Float(8.5)
            ]
        ),
        "-3 – 8.5"
    );
    assert_eq!(agg("builtin:median", vec![V::Text("x".into())]), "");
}

#[test]
fn aggregator_sees_the_table() {
    let s = adhoc(
        ExtScriptKind::Aggregator,
        "fn aggregate(values, table) { `${table.columns.len()}x${table.rows.len()} ${table.columns[1].type} ${values.len()}` }",
    );
    assert_eq!(s.aggregate(&table()).unwrap(), "3x3 text 9");
}

// ------------------------------------------------------------------ format

#[test]
fn header_parsing() {
    let h = parse_header(
        "// name:  My format \n\n// description: does things: well\n//extension: .CSV\nfn row(v) { \"\" }\n// name: ignored",
    );
    assert_eq!(h.name.as_deref(), Some("My format"));
    assert_eq!(h.description.as_deref(), Some("does things: well"));
    assert_eq!(h.extension.as_deref(), Some(".CSV"));
    let (info, s) = compile(
        ExtScriptKind::Extractor,
        "my-format",
        "//extension: .CSV\nfn row(v) { \"\" }",
        Some("my-format.rhai".into()),
    );
    assert!(s.is_ok());
    assert_eq!(info.name, "my-format");
    assert_eq!(info.extension, "csv");
    assert!(!info.builtin);
    assert_eq!(parse_header("fn row(v) {}\n// name: x"), Header::default());
}

#[test]
fn streaming_writer_and_state() {
    let s = adhoc(
        ExtScriptKind::Extractor,
        r#"
        fn header(columns) { this.n = 0; `${columns[0].name}:${columns[0].type}/${options().table}[` }
        fn row(values, columns, index) { this.n += 1; `${index}=${text(values[0], "-")}` }
        fn separator() { ";" }
        fn footer(count) { `]${count}/${this.n}` }
        "#,
    );
    let opts = FormatOptions {
        table: Some("t".into()),
        ..Default::default()
    };
    let mut w = ExtractorWriter::new(&s, vec![ScriptColumn::new("a", "integer")], &opts).unwrap();
    assert_eq!(w.write_row(&[V::Int(5)]).unwrap(), "a:integer/t[0=5");
    assert_eq!(w.write_row(&[V::Null]).unwrap(), ";1=-");
    assert_eq!(w.rows(), 2);
    assert_eq!(w.finish().unwrap(), "]2/2");
    // No rows: header and footer only.
    let w = ExtractorWriter::new(&s, vec![ScriptColumn::new("a", "")], &opts).unwrap();
    assert_eq!(w.finish().unwrap(), "a:/t[]0/0");
}

#[test]
fn helpers() {
    let s = adhoc(
        ExtScriptKind::Aggregator,
        r#"fn aggregate(values) {
            [
                sql_ident("ok_1"), sql_ident("Has \"q\""), json(#{a: [1, 2.5, (), "x"]}),
                csv_field("a,b"), csv_field("a;b", ";"), csv_field("plain", ";"),
                html_escape("<a&'>"), md_escape("a|b\nc"),
                is_null(()), is_number(1.5), is_number("1"), to_number(" 42 "),
                to_number("1e3"), to_number("abc"), pad("ab", 4) + "|", pad_left(7, 3),
                format_number(3.14159, 2), num_text(2.0), round_to(1.23456, 2), text(true),
            ].map(|v| text(v, "∅"))
        }"#,
    );
    assert_eq!(
        s.aggregate(&ScriptTable::default()).unwrap(),
        "[\"ok_1\",\"\\\"Has \\\"\\\"q\\\"\\\"\\\"\",\"{\\\"a\\\":[1,2.5,null,\\\"x\\\"]}\",\
         \"\\\"a,b\\\"\",\"\\\"a;b\\\"\",\"plain\",\"&lt;a&amp;&#39;&gt;\",\"a\\\\|b<br>c\",\
         \"true\",\"true\",\"false\",\"42\",\"1000\",\"∅\",\"ab  |\",\"  7\",\"3.14\",\"2\",\
         \"1.23\",\"true\"]"
    );
}

// ------------------------------------------------------------------ sandbox

#[test]
fn endless_loop_stops_with_an_error() {
    let s = adhoc(ExtScriptKind::Aggregator, "fn aggregate(v) { loop { } }");
    let t0 = Instant::now();
    let e = s.aggregate(&ScriptTable::default()).unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(30));
    assert!(e.to_string().contains("operations"), "{e}");
    // Deep recursion is capped too.
    let s = adhoc(
        ExtScriptKind::Aggregator,
        "fn f(x) { f(x + 1) } fn aggregate(v) { f(0) }",
    );
    let e = s.aggregate(&ScriptTable::default()).unwrap_err();
    assert!(e.to_string().contains("test"), "{e}");
}

#[test]
fn eval_and_import_are_unavailable() {
    for src in [
        "fn aggregate(v) { eval(\"1\") }",
        "import \"std\" as s; fn aggregate(v) { 1 }",
        "fn aggregate(v) { import \"x\" as m; 1 }",
    ] {
        let (info, s) = compile(ExtScriptKind::Aggregator, "bad", src, None);
        assert!(s.is_err(), "{src}");
        assert!(info.error.is_some());
    }
    // Function pointers cannot reach `eval` either.
    let s = adhoc(
        ExtScriptKind::Aggregator,
        "fn aggregate(v) { let f = Fn(\"ev\" + \"al\"); f.call(\"40 + 2\") }",
    );
    assert!(s.aggregate(&ScriptTable::default()).is_err());
}

#[test]
fn huge_strings_and_arrays_are_capped() {
    let s = adhoc(
        ExtScriptKind::Aggregator,
        "fn aggregate(v) { let s = \"x\"; loop { s += s; } }",
    );
    let e = s.aggregate(&ScriptTable::default()).unwrap_err();
    assert!(e.to_string().contains("sandbox limit"), "{e}");
    let s = adhoc(
        ExtScriptKind::Aggregator,
        "fn aggregate(v) { let a = [1]; loop { a += a; } }",
    );
    let e = s.aggregate(&ScriptTable::default()).unwrap_err();
    assert!(e.to_string().contains("sandbox limit"), "{e}");
}

#[test]
fn cancellation_stops_a_running_script() {
    // The flag is raised while the script runs (after a few polls, long
    // before the operation limit).
    let polls = Arc::new(AtomicUsize::new(0));
    let p = polls.clone();
    let s = adhoc(
        ExtScriptKind::Extractor,
        "fn row(v) { let i = 0; loop { i += 1; } }",
    )
    .with_cancel(move || p.fetch_add(1, Ordering::SeqCst) >= 5);
    let mut w =
        ExtractorWriter::new(&s, vec![ScriptColumn::new("a", "")], &Default::default()).unwrap();
    let r = w.write_row(&[V::Int(1)]);
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert_eq!(polls.load(Ordering::SeqCst), 6);
}

#[test]
fn print_and_top_level_code_do_nothing() {
    // Top-level statements are never run; print goes nowhere.
    let s = adhoc(
        ExtScriptKind::Aggregator,
        "loop { } fn aggregate(v) { print(\"x\"); debug(1); \"ok\" }",
    );
    assert_eq!(s.aggregate(&ScriptTable::default()).unwrap(), "ok");
}

#[test]
fn kind_mismatch_and_missing_entry_points() {
    let (info, s) = compile(ExtScriptKind::Extractor, "x", "fn header() { \"\" }", None);
    assert!(s.is_err());
    assert!(info.error.unwrap().contains("fn row"));
    let (_, s) = compile(ExtScriptKind::Aggregator, "x", "fn row(v) { \"\" }", None);
    assert!(s.is_err());
    let ag = lib().aggregator("builtin:count").unwrap();
    assert!(ExtractorWriter::new(&ag, Vec::new(), &Default::default()).is_err());
    assert!(matches!(
        lib().extractor("builtin:nope"),
        Err(Error::NotFound(_))
    ));
    assert!(lib().extractor("builtin:count").is_err());
}

// ------------------------------------------------------------------ library on disk

#[test]
fn user_scripts_errors_create_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("scripts");
    let ex_dir = root.join("extractors");
    std::fs::create_dir_all(&ex_dir).unwrap();
    std::fs::write(
        ex_dir.join("upper.rhai"),
        "// name: Upper\n// extension: up\nfn row(values) { text(values[0]).to_upper() }\nfn separator() { \",\" }\n",
    )
    .unwrap();
    std::fs::write(
        ex_dir.join("broken.rhai"),
        "fn row(values) { values[0] +  }",
    )
    .unwrap();
    std::fs::write(ex_dir.join("notes.txt"), "ignored").unwrap();

    let l = ScriptLibrary::new(&root);
    assert_eq!(l.directory(), Some(root.clone()));
    let list = l.list(ExtScriptKind::Extractor);
    assert_eq!(list.len(), 8);
    let user: Vec<_> = list.iter().filter(|i| !i.builtin).collect();
    assert_eq!(user.len(), 2);
    assert_eq!(user[0].id, "broken");
    assert!(user[0].error.is_some());
    assert_eq!(user[1].id, "upper");
    assert_eq!(user[1].name, "Upper");
    assert_eq!(user[1].extension, "up");
    assert!(user[1].error.is_none());

    let t = ScriptTable {
        columns: vec!["a".into()],
        types: vec!["text".into()],
        rows: vec![vec![V::Text("x".into())], vec![V::Text("y".into())]],
    };
    assert_eq!(block_on(l.extract("upper", t.clone())).unwrap(), "X,Y");
    let e = block_on(l.extract("broken", t.clone())).unwrap_err();
    assert!(e.to_string().contains("does not compile"), "{e}");

    // Edits are picked up by file time; make sure the time moves.
    let upper = ex_dir.join("upper.rhai");
    std::fs::write(
        &upper,
        "// name: Lower\nfn row(values) { text(values[0]) + \"!\" }\n",
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&upper)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    assert_eq!(block_on(l.extract("upper", t.clone())).unwrap(), "x!y!");
    block_on(l.reload()).unwrap();
    let upper = l
        .list(ExtScriptKind::Extractor)
        .into_iter()
        .find(|i| i.id == "upper")
        .unwrap();
    assert_eq!(upper.name, "Lower");
    assert_eq!(upper.extension, "txt");

    // create(): a commented template that compiles and runs, never overwriting.
    let p = block_on(l.create(ExtScriptKind::Extractor, "My Format!")).unwrap();
    assert_eq!(p, ex_dir.join("my-format.rhai"));
    let p2 = block_on(l.create(ExtScriptKind::Extractor, "my format")).unwrap();
    assert_eq!(p2, ex_dir.join("my-format-2.rhai"));
    let info = l
        .list(ExtScriptKind::Extractor)
        .into_iter()
        .find(|i| i.id == "my-format")
        .unwrap();
    assert_eq!(info.name, "My Format!");
    assert!(info.error.is_none());
    let mut t3 = t.clone();
    t3.columns.push("b".into());
    t3.types.push("integer".into());
    t3.rows[0].push(V::Int(1));
    t3.rows[1].push(V::Null);
    assert_eq!(
        block_on(l.extract("my-format", t3)).unwrap(),
        "a\tb\nx\t1\ny\t\n"
    );
    let pa = block_on(l.create(ExtScriptKind::Aggregator, "  ")).unwrap();
    assert_eq!(pa, root.join("aggregators").join("script.rhai"));
    assert_eq!(
        block_on(l.aggregate("script", t3_numbers())).unwrap(),
        "2.5"
    );
    let src = std::fs::read_to_string(&pa).unwrap();
    assert!(src.starts_with("// name: script\n"));
    assert!(src.contains("fn aggregate(values, table)"));

    // A file added behind the library's back is found by id.
    std::fs::write(
        root.join("aggregators").join("late.rhai"),
        "fn aggregate(values) { values.len() }",
    )
    .unwrap();
    assert_eq!(block_on(l.aggregate("late", t3_numbers())).unwrap(), "4");

    // In-memory libraries cannot create scripts.
    assert!(block_on(lib().create(ExtScriptKind::Extractor, "x")).is_err());
}

fn t3_numbers() -> ScriptTable {
    ScriptTable {
        columns: vec!["a".into(), "b".into()],
        types: vec![String::new(), String::new()],
        rows: vec![
            vec![V::Int(1), V::Int(2)],
            vec![V::Text("3".into()), V::Float(4.0)],
        ],
    }
}

#[test]
fn service_calls_do_not_block_the_caller() {
    // The future is created on the caller's thread and resolved by the
    // script thread; an endless script ends with an error, not a hang.
    let l = lib();
    let fut = l.aggregate("builtin:median", t3_numbers());
    assert_eq!(block_on(fut).unwrap(), "2.5");
}
