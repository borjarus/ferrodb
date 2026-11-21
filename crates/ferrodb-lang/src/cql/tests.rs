#![cfg(test)]

use ferrodb_api::{FormatStyle, RiskKind, Severity};

use super::*;
use crate::sql::schema::{ColumnInfo, SchemaInfo, SchemaModel, TableInfo};

fn col(name: &str, ty: &str, pk: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: ty.into(),
        nullable: !pk,
        primary_key: pk,
        generated: false,
        comment: None,
    }
}

fn model() -> SchemaModel {
    SchemaModel::new(
        vec![
            SchemaInfo {
                name: "shop".into(),
                loaded: true,
                tables: vec![TableInfo {
                    name: "orders".into(),
                    view: false,
                    columns: vec![
                        col("customer_id", "uuid", true),
                        col("order_time", "timeuuid", true),
                        col("total", "decimal", false),
                        col("items", "map<text, int>", false),
                        col("status", "text", false),
                    ],
                    foreign_keys: Vec::new(),
                    comment: None,
                }],
                routines: Vec::new(),
            },
            SchemaInfo {
                name: "audit".into(),
                loaded: false,
                tables: Vec::new(),
                routines: Vec::new(),
            },
        ],
        vec!["shop".into()],
    )
}

fn labels(text: &str, offset: usize, m: &SchemaModel) -> Vec<String> {
    let doc = Document::analyze(text, DIALECT, m, Some("shop"));
    complete(&doc, offset, &[])
        .into_iter()
        .map(|i| i.label)
        .collect()
}

#[test]
fn batches_are_one_statement() {
    let src = "BEGIN BATCH\n  INSERT INTO t (a) VALUES (1);\n  UPDATE t SET b = 2 WHERE a = 1;\nAPPLY BATCH;\nSELECT * FROM t; // done\nBEGIN UNLOGGED BATCH INSERT INTO t (a) VALUES (2); APPLY BATCH;";
    let st = split(src);
    assert_eq!(st.len(), 3, "{st:?}");
    assert!(st[0].text(src).ends_with("APPLY BATCH"));
    assert_eq!(st[1].text(src), "SELECT * FROM t");
}

#[test]
fn classification_and_risks() {
    assert_eq!(classify("SELECT * FROM t"), StatementClass::Read);
    assert_eq!(classify("USE shop"), StatementClass::Session);
    assert_eq!(classify("CONSISTENCY QUORUM"), StatementClass::Session);
    assert_eq!(
        classify("BEGIN BATCH INSERT INTO t (a) VALUES (1); APPLY BATCH"),
        StatementClass::Modification
    );
    let r: Vec<RiskKind> = risks(
        "DROP TABLE t; TRUNCATE t; DELETE FROM t; DELETE FROM t WHERE a = 1; SELECT 1",
        false,
    )
    .into_iter()
    .map(|r| r.kind)
    .collect();
    assert_eq!(
        r,
        vec![RiskKind::Drop, RiskKind::Truncate, RiskKind::DeleteAll]
    );
    assert_eq!(risks("INSERT INTO t (a) VALUES (1)", true).len(), 1);
    assert!(risks("SELECT * FROM t", true).is_empty());
}

#[test]
fn allow_filtering_awareness() {
    assert!(allows_filtering(
        "SELECT * FROM t WHERE x = 1 ALLOW FILTERING"
    ));
    assert!(!allows_filtering("SELECT * FROM t WHERE x = 1"));
    let m = model();
    let src = "SELECT * FROM orders WHERE customer_id = ? AND status = 'new';\nSELECT * FROM orders WHERE status = 'x' ALLOW FILTERING;";
    let doc = Document::analyze(src, DIALECT, &m, Some("shop"));
    let d = diagnostics(&doc);
    let warn: Vec<_> = d
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .collect();
    assert_eq!(warn.len(), 1, "{d:?}");
    assert!(warn[0].message.contains("'status'"));
    assert!(
        d.iter()
            .any(|d| d.severity == Severity::Info && d.message.contains("ALLOW FILTERING"))
    );
    // Unknown table in a loaded keyspace.
    let doc = Document::analyze("SELECT * FROM ordrs", DIALECT, &m, Some("shop"));
    assert!(
        diagnostics(&doc)
            .iter()
            .any(|d| d.severity == Severity::Error)
    );
    // CQL specifics are not syntax errors.
    for ok in [
        "INSERT INTO orders (customer_id, order_time, total) VALUES (uuid(), now(), 1.5) USING TTL 86400",
        "UPDATE orders USING TTL 10 SET status = 'x' WHERE customer_id = ? AND order_time = ?",
        "SELECT JSON customer_id, writetime(status) FROM orders WHERE customer_id = ? LIMIT 10",
        "CONSISTENCY LOCAL_QUORUM",
        "CREATE KEYSPACE k WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}",
    ] {
        let doc = Document::analyze(ok, DIALECT, &m, Some("shop"));
        let errs: Vec<_> = diagnostics(&doc)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(errs.is_empty(), "{ok}: {errs:?}");
    }
}

#[test]
fn completion() {
    let m = model();
    let l = labels("", 0, &m);
    assert!(l.contains(&"CONSISTENCY".to_owned()) && l.contains(&"BEGIN BATCH".to_owned()));
    let l = labels("USE ", 4, &m);
    assert_eq!(l, vec!["audit", "shop"]);
    let l = labels("CONSISTENCY lo", 14, &m);
    assert_eq!(l[0], "LOCAL_ONE");
    let l = labels("SELECT * FROM ", 14, &m);
    assert!(l.contains(&"orders".to_owned()), "{l:?}");
    // Primary key columns first in WHERE.
    let l = labels("SELECT * FROM orders WHERE ", 27, &m);
    assert_eq!(
        &l[..2],
        &["customer_id".to_owned(), "order_time".to_owned()],
        "{l:?}"
    );
    let src = "SELECT * FROM orders WHERE customer_id = ? ";
    let l = labels(src, src.len(), &m);
    assert!(l.contains(&"ALLOW FILTERING".to_owned()), "{l:?}");
    let src = "SELECT * FROM orders WHERE status = 'x' ALLOW ";
    assert_eq!(labels(src, src.len(), &m), vec!["FILTERING"]);
    let src = "CREATE TABLE t (id ";
    let l = labels(src, src.len(), &m);
    assert!(
        l.contains(&"uuid".to_owned()) && l.contains(&"frozen<".to_owned()),
        "{l:?}"
    );
    let src = "INSERT INTO orders (customer_id) VALUES (?) ";
    assert!(labels(src, src.len(), &m).contains(&"USING TTL".to_owned()));
    let l = labels("SELECT to", 9, &m);
    assert!(
        l.iter().any(|x| x == "token" || x == "totimestamp"),
        "{l:?}"
    );
}

#[test]
fn format_is_idempotent() {
    let src = "begin batch insert into t (a) values (1); update t set b=2 where a=1; apply batch;\nselect * from t where a = 1 allow filtering";
    let f = crate::sql::format::format(src, DIALECT, &FormatStyle::default());
    assert_eq!(
        crate::sql::format::format(&f, DIALECT, &FormatStyle::default()),
        f
    );
    assert!(f.contains("ALLOW"), "{f}");
    assert_eq!(split(&f).len(), 2);
}
