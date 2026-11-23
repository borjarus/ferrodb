//! Fuzzing harness (plan, phase 10.2 and section 9): every entry point of
//! the language service on arbitrary text, with cheap invariants checked
//! (ranges in bounds and on char boundaries). The cargo-fuzz targets in
//! `fuzz/` and the stable proptest smoke tests in `tests/fuzz_smoke.rs`
//! call the same functions, so a crash found by one is reproduced by the
//! other.
//!
//! Not part of the public API.

use std::{ops::Range, sync::Arc};

use ferrodb_api::{Declaration, DocSnapshot, FormatStyle, QueryLanguage, StructureItem};

use crate::{
    mql, partiql, redis,
    service::{self, Engine, NoSqlSchema},
    sql::{
        self, Dialect, Flavor,
        schema::{ColumnInfo, ForeignKeyInfo, RoutineInfo, SchemaInfo, SchemaModel, TableInfo},
    },
    sqlpp,
};

/// Every language: each SQL dialect with a file, the T-SQL aliases, and
/// the level B languages.
pub fn languages() -> Vec<QueryLanguage> {
    let mut v: Vec<QueryLanguage> = Dialect::ids()
        .into_iter()
        .filter(|d| !matches!(*d, "cql" | "sqlpp" | "partiql"))
        .map(QueryLanguage::sql)
        .collect();
    v.push(QueryLanguage::sql("sqlserver"));
    v.extend([
        QueryLanguage::Mql,
        QueryLanguage::Cql,
        QueryLanguage::SqlPlusPlus,
        QueryLanguage::PartiQl,
        QueryLanguage::RedisCommands,
    ]);
    v
}

/// Raw fuzzer input: byte 0 picks the language, bytes 1..3 the cursor,
/// the rest is the document (lossy UTF-8).
pub fn language_service_bytes(data: &[u8]) {
    let langs = languages();
    let (lang, cursor, text) = match data {
        [l, a, b, rest @ ..] => (
            &langs[*l as usize % langs.len()],
            u16::from_le_bytes([*a, *b]) as usize,
            String::from_utf8_lossy(rest),
        ),
        _ => return,
    };
    language_service(lang, &text, cursor);
}

fn floor_boundary(s: &str, at: usize) -> usize {
    let mut i = at.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[track_caller]
fn check(text: &str, r: &Range<usize>, what: &str) {
    assert!(
        r.start <= r.end && r.end <= text.len(),
        "{what}: range {r:?} out of bounds (text of {} bytes)",
        text.len()
    );
    assert!(
        text.is_char_boundary(r.start) && text.is_char_boundary(r.end),
        "{what}: range {r:?} not on char boundaries"
    );
}

fn check_structure(text: &str, items: &[StructureItem]) {
    for it in items {
        check(text, &it.range, "structure range");
        check(text, &it.selection, "structure selection");
        check_structure(text, &it.children);
    }
}

/// A small schema model and NoSQL schema, so that resolution, completion
/// and inspections take their schema-aware paths.
fn schemas() -> (&'static SchemaModel, &'static NoSqlSchema) {
    use std::sync::OnceLock;
    static MODEL: OnceLock<SchemaModel> = OnceLock::new();
    static NOSQL: OnceLock<NoSqlSchema> = OnceLock::new();
    let model = MODEL.get_or_init(|| sample_model(20));
    let nosql = NOSQL.get_or_init(|| NoSqlSchema {
        couchbase: sqlpp::Schema {
            keyspaces: vec![sqlpp::Keyspace {
                bucket: "travel".into(),
                scope: "inventory".into(),
                collection: "airline".into(),
                fields: vec!["name".into(), "address.city".into()],
            }],
            loaded: true,
        },
        dynamodb: partiql::Schema {
            tables: vec![partiql::KeyTable {
                name: "Music".into(),
                partition_key: Some("Artist".into()),
                sort_key: Some("SongTitle".into()),
                indexes: Vec::new(),
                attributes: vec!["Year".into()],
            }],
            loaded: true,
        },
        redis_keys: vec!["user:1".into(), "session:é".into()],
    });
    (model, nosql)
}

/// A schema model with `tables` tables of 8 columns (two schemas, foreign
/// keys to the previous table, a few routines); used by the completion
/// benchmark with 10k objects too.
pub fn sample_model(tables: usize) -> SchemaModel {
    let mut schemas = vec![
        SchemaInfo {
            name: "public".into(),
            loaded: true,
            ..Default::default()
        },
        SchemaInfo {
            name: "sales".into(),
            loaded: true,
            ..Default::default()
        },
    ];
    for i in 0..tables {
        let columns = (0..8)
            .map(|c| ColumnInfo {
                name: if c == 0 {
                    "id".into()
                } else {
                    format!("col_{c}")
                },
                data_type: if c % 2 == 0 { "integer" } else { "text" }.into(),
                nullable: c != 0,
                primary_key: c == 0,
                ..Default::default()
            })
            .collect();
        let foreign_keys = if i > 0 {
            vec![ForeignKeyInfo {
                columns: vec!["col_2".into()],
                ref_schema: None,
                ref_table: format!("table_{}", i - 1),
                ref_columns: vec!["id".into()],
            }]
        } else {
            Vec::new()
        };
        schemas[i % 2].tables.push(TableInfo {
            name: format!("table_{i}"),
            view: i % 10 == 9,
            columns,
            foreign_keys,
            comment: None,
        });
    }
    schemas[0].routines.push(RoutineInfo {
        name: "calc_total".into(),
        procedure: false,
        arguments: vec![(Some("a".into()), "integer".into())],
        returns: Some("numeric".into()),
        comment: None,
    });
    SchemaModel::new(schemas, vec!["public".into()])
}

/// Every language feature on `text` with the cursor near `cursor`.
pub fn language_service(lang: &QueryLanguage, text: &str, cursor: usize) {
    let offset = floor_boundary(text, cursor % (text.len() + 1));
    let doc = DocSnapshot {
        text: Arc::from(text),
        language: lang.clone(),
        data_source: None,
        schema: None,
        version: 1,
    };

    // Splitting, lexing.
    let spans = service::statements(&doc);
    let mut last = 0;
    for s in &spans {
        check(text, &s.range, "statement");
        assert!(s.range.start >= last, "statements overlap: {spans:?}");
        last = s.range.end;
    }
    match lang {
        QueryLanguage::Mql => {
            let (toks, errs) = mql::lex(text);
            for t in &toks {
                check(text, &t.range, "mql token");
            }
            for e in &errs {
                check(text, &e.range, "mql lex error");
            }
            for s in mql::split(text) {
                let _ = mql::parse(s.text(text));
            }
        }
        QueryLanguage::RedisCommands => {
            let _ = redis::parse(text);
            for r in redis::split(text) {
                check(text, &r, "redis statement");
            }
        }
        _ => {
            let flavor = Flavor::for_dialect(service::dialect_of(lang));
            let mut end = 0;
            for t in sql::lexer::tokenize(text, &flavor) {
                check(text, &t.range, "sql token");
                assert_eq!(t.range.start, end, "tokens leave a gap");
                end = t.range.end;
            }
            assert_eq!(end, text.len(), "tokens do not cover the text");
            for s in sql::split(text, &flavor) {
                check(text, &s.body, "statement body");
                check(text, &s.range, "statement range");
            }
        }
    }
    for s in &spans {
        let _ = service::classify(&text[s.range.clone()], lang);
    }

    // Text services.
    let _ = service::format_text(text, lang, &FormatStyle::default());
    let _ = service::format_text(
        text,
        lang,
        &FormatStyle {
            uppercase_keywords: false,
            indent: 2,
        },
    );
    for p in service::parameters(text, lang) {
        check(text, &p.range, "parameter");
    }
    for r in service::risks(text, lang, true) {
        check(text, &r.range, "risk");
    }

    // The engine with a schema model.
    let (model, nosql) = schemas();
    let engine = Engine::new(model, &[]).with_nosql(nosql);
    for c in engine.complete(&doc, offset) {
        check(text, &c.replace, "completion replace");
    }
    let diags = engine.diagnostics(&doc);
    for d in &diags {
        check(text, &d.range, "diagnostic");
    }
    let mut ranges: Vec<Range<usize>> = diags.iter().map(|d| d.range.clone()).take(4).collect();
    ranges.push(offset..offset);
    for r in ranges {
        for a in engine.code_actions(&doc, r) {
            let mut edits = a.edits.clone();
            edits.sort_by_key(|e| (e.range.start, e.range.end));
            for w in edits.windows(2) {
                assert!(
                    w[0].range.end <= w[1].range.start,
                    "overlapping edits: {a:?}"
                );
            }
            for e in &edits {
                check(text, &e.range, "code action edit");
            }
        }
    }
    let _ = engine.signature_help(&doc, offset);
    if let Some(h) = engine.hover(&doc, offset) {
        check(text, &h.range, "hover");
    }
    check_structure(text, &engine.structure(&doc));
    if let Some(Declaration::Local(r)) = engine.declaration(&doc, offset) {
        check(text, &r, "declaration");
    }
    if let Some((r, _)) = engine.prepare_rename(&doc, offset) {
        check(text, &r, "rename range");
        if let Ok(edits) = engine.rename(&doc, offset, "renamed_x") {
            for e in &edits {
                check(text, &e.range, "rename edit");
            }
        }
    }
    let selection = (offset < text.len()).then_some(offset..text.len());
    for sel in [None, selection] {
        if let Some(x) = engine.expand_template(&doc, offset, sel) {
            check(text, &x.replace, "template replace");
            check(&x.text, &x.selection, "template selection");
        }
    }
}

/// A shell-syntax value (`{ a: 1 }`, `ObjectId("…")`): parse, print, and
/// parse the printed form again (it must give the same value).
pub fn mql_value(text: &str) {
    let Ok(v) = mql::value(text) else { return };
    let inline = mql::print_inline(&v);
    let back = mql::value(&inline)
        .unwrap_or_else(|e| panic!("printed value does not parse: {inline:?}: {e:?}"));
    assert!(
        same(&v, &back),
        "print/parse round trip changed the value: {v:?} -> {inline:?} -> {back:?}"
    );
    let _ = mql::print_pretty(&v, 40);
}

/// BSON equality with NaN equal to itself.
fn same(a: &bson::Bson, b: &bson::Bson) -> bool {
    use bson::Bson as B;
    match (a, b) {
        (B::Double(x), B::Double(y)) => x == y || (x.is_nan() && y.is_nan()),
        (B::Array(x), B::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (B::Document(x), B::Document(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|((ka, va), (kb, vb))| ka == kb && same(va, vb))
        }
        _ => a == b,
    }
}
