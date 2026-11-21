#![cfg(test)]

use ferrodb_api::{FormatStyle, RiskKind, Severity};

use super::*;

fn schema() -> Schema {
    Schema {
        loaded: true,
        tables: vec![KeyTable {
            name: "Orders".into(),
            partition_key: Some("customerId".into()),
            sort_key: Some("orderDate".into()),
            indexes: vec![KeyIndex {
                name: "byStatus".into(),
                partition_key: "status".into(),
                sort_key: Some("orderDate".into()),
            }],
            attributes: vec!["total".into(), "status".into(), "items".into()],
        }],
    }
}

fn labels(src: &str, offset: usize) -> Vec<String> {
    let s = schema();
    let m = s.to_model();
    complete(src, offset, &s, &m, &[])
        .into_iter()
        .map(|i| i.label)
        .collect()
}

#[test]
fn targets_and_scans() {
    let s = schema();
    assert_eq!(
        target(r#"SELECT * FROM "Orders"."byStatus" WHERE status = 'x'"#).map(|t| (t.0, t.1)),
        Some(("Orders".into(), Some("byStatus".into())))
    );
    assert!(!will_scan(
        r#"SELECT * FROM "Orders" WHERE "customerId" = ?"#,
        &s
    ));
    assert!(!will_scan(
        r#"SELECT * FROM "Orders" WHERE customerId IN ['a', 'b'] AND begins_with(orderDate, '2024')"#,
        &s
    ));
    assert!(will_scan(r#"SELECT * FROM "Orders" WHERE total > 10"#, &s));
    assert!(will_scan(r#"SELECT * FROM "Orders""#, &s));
    assert!(will_scan(
        r#"SELECT * FROM "Orders" WHERE customerId = 'a' OR total = 1"#,
        &s
    ));
    assert!(!will_scan(
        r#"SELECT * FROM "Orders"."byStatus" WHERE status = 'new'"#,
        &s
    ));
    assert!(will_scan(
        r#"SELECT * FROM "Orders"."byStatus" WHERE customerId = 'a'"#,
        &s
    ));
    assert!(!will_scan(r#"UPDATE "Orders" SET total = 1"#, &s));
    // Unknown tables: only a missing WHERE is certain.
    assert!(!will_scan(r#"SELECT * FROM "Other" WHERE x = 1"#, &s));
    assert!(will_scan(r#"SELECT * FROM "Other""#, &Schema::default()));
}

#[test]
fn diagnostics_and_risks() {
    let s = schema();
    let d = diagnostics(
        "SELECT * FROM \"Orders\" WHERE total > 1;\nINSERT INTO \"Orders\" {'a': 1};\nDELETE FROM \"Orders\";\nSELECT * FROM \"Nope\" WHERE x = 1;\nSELECT * FROM \"Orders\".\"bad\" WHERE x = 1;\nCREATE TABLE x;\nSELECT * FROM \"Orders\" WHERE customerId = '1' AND x = {'a': [1, 2]};",
        &s,
    );
    let msgs: Vec<(Severity, &str)> = d.iter().map(|d| (d.severity, d.message.as_str())).collect();
    assert!(
        msgs[0].0 == Severity::Warning && msgs[0].1.contains("partition key \"customerId\""),
        "{msgs:?}"
    );
    assert!(msgs.iter().any(|m| m.1.contains("VALUE")), "{msgs:?}");
    assert!(
        msgs.iter().any(|m| m.1.starts_with("DELETE needs a WHERE")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.1.contains("Unknown table \"Nope\"")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.1.contains("no index \"bad\"")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|m| m.1.contains("SELECT, INSERT, UPDATE and DELETE only")),
        "{msgs:?}"
    );
    assert_eq!(d.len(), 6, "{msgs:?}");
    assert!(
        diagnostics("SELECT * FROM t WHERE a = 'x", &s)
            .iter()
            .any(|d| d.message.contains("Unterminated"))
    );
    let r: Vec<RiskKind> = risks(
        "DELETE FROM \"Orders\"; UPDATE t SET a = 1 WHERE k = 1",
        true,
    )
    .into_iter()
    .map(|r| r.kind)
    .collect();
    assert_eq!(r, vec![RiskKind::DeleteAll, RiskKind::Modification]);
    assert_eq!(classify("SELECT * FROM t"), StatementClass::Read);
    assert_eq!(
        classify("INSERT INTO t VALUE {'a': 1}"),
        StatementClass::Modification
    );
}

#[test]
fn completion() {
    let l = labels("", 0);
    assert!(l.contains(&"SELECT * FROM".to_owned()));
    let l = labels("SELECT * FROM ", 14);
    assert!(l.contains(&"Orders".to_owned()), "{l:?}");
    let l = labels("SELECT * FROM \"Orders\".", 23);
    assert_eq!(l, vec!["byStatus"]);
    let src = "SELECT * FROM \"Orders\" WHERE ";
    let l = labels(src, src.len());
    assert_eq!(
        &l[..2],
        &["customerId".to_owned(), "orderDate".to_owned()],
        "{l:?}"
    );
    let src = "INSERT INTO \"Orders\" VALUE ";
    assert_eq!(
        labels(src, src.len()),
        vec!["{'customerId': ?, 'orderDate': ?}"]
    );
    let src = "SELECT * FROM \"Orders\" WHERE begins";
    assert!(labels(src, src.len()).contains(&"begins_with".to_owned()));
}

#[test]
fn format_keeps_documents() {
    let src = "insert into \"Orders\" value {'customerId': 'a', 'items': [1, 2], 'm': {'x': 1}}";
    let f = crate::sql::format::format(src, DIALECT, &FormatStyle::default());
    assert_eq!(
        crate::sql::format::format(&f, DIALECT, &FormatStyle::default()),
        f
    );
    assert!(
        f.contains("'customerId'") && f.contains("\"Orders\""),
        "{f}"
    );
}
