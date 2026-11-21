#![cfg(test)]

use ferrodb_api::{FormatStyle, RiskKind, Severity};

use super::*;

fn schema() -> Schema {
    let ks = |b: &str, s: &str, c: &str, f: &[&str]| Keyspace {
        bucket: b.into(),
        scope: s.into(),
        collection: c.into(),
        fields: f.iter().map(|x| (*x).to_owned()).collect(),
    };
    Schema {
        loaded: true,
        keyspaces: vec![
            ks(
                "travel-sample",
                "inventory",
                "airline",
                &["name", "country", "iata", "icao"],
            ),
            ks(
                "travel-sample",
                "inventory",
                "route",
                &[
                    "airline",
                    "sourceairport",
                    "schedule",
                    "schedule[].day",
                    "schedule[].flight",
                    "stops",
                ],
            ),
            ks("travel-sample", "_default", "_default", &["type"]),
            ks(
                "shop",
                "sales",
                "orders",
                &["total", "address.city", "address.zip", "items"],
            ),
        ],
    }
}

fn labels(src: &str, ctx: Option<&str>) -> Vec<String> {
    let at = src.find('|').expect("cursor");
    let text = src.replace('|', "");
    complete(&text, at, &schema(), ctx, &[])
        .into_iter()
        .map(|i| i.label)
        .collect()
}

#[test]
fn query_context_parses() {
    assert_eq!(
        QueryContext::parse("default:`travel-sample`.inventory"),
        Some(QueryContext {
            bucket: "travel-sample".into(),
            scope: Some("inventory".into())
        })
    );
    assert_eq!(QueryContext::parse("shop").unwrap().scope, None);
    let s = schema();
    let ctx = QueryContext::parse("travel-sample.inventory");
    assert!(s.resolve(&["airline".into()], ctx.as_ref()).is_some());
    assert!(s.resolve(&["travel-sample".into()], None).is_some());
    assert!(
        s.resolve(&["shop".into(), "sales".into(), "orders".into()], None)
            .is_some()
    );
}

#[test]
fn keyspace_and_field_completion() {
    let ctx = Some("travel-sample.inventory");
    let l = labels("SELECT * FROM |", ctx);
    assert_eq!(
        &l[..2],
        &["airline".to_owned(), "route".to_owned()],
        "{l:?}"
    );
    assert!(l.contains(&"shop.sales.orders".to_owned()));
    let l = labels("SELECT * FROM `travel-sample`.|", None);
    assert_eq!(l, vec!["_default", "inventory"]);
    let l = labels("SELECT * FROM shop.sales.|", None);
    assert_eq!(l, vec!["orders"]);
    let l = labels("SELECT o.| FROM shop.sales.orders AS o", None);
    assert!(
        l.contains(&"total".to_owned()) && l.contains(&"address".to_owned()),
        "{l:?}"
    );
    let l = labels("SELECT o.address.| FROM shop.sales.orders o", None);
    assert_eq!(l, vec!["city", "zip"]);
    let l = labels(
        "SELECT r.airline, s.| FROM route AS r UNNEST r.schedule AS s",
        ctx,
    );
    assert_eq!(l, vec!["day", "flight"], "{l:?}");
    let l = labels("SELECT * FROM airline a WHERE cou|", ctx);
    assert_eq!(l[0], "country");
    let l = labels("SELECT * FROM airline a |", ctx);
    assert!(
        l.contains(&"USE KEYS".to_owned()) && l.contains(&"UNNEST".to_owned()),
        "{l:?}"
    );
    let l = labels("SELECT * FROM airline USE |", ctx);
    assert_eq!(l, vec!["INDEX", "KEYS"]);
    let l = labels("SELECT array_c|", ctx);
    assert!(l.contains(&"array_contains".to_owned()), "{l:?}");
    let l = labels("|", ctx);
    assert!(l.contains(&"UPSERT INTO".to_owned()) && l.contains(&"INFER".to_owned()));
    // Names needing backticks are quoted on insert.
    let text = "SELECT * FROM ";
    let items = complete(text, text.len(), &schema(), None, &[]);
    let b = items.iter().find(|i| i.label == "travel-sample").unwrap();
    assert_eq!(b.insert_text, "`travel-sample`");
}

#[test]
fn diagnostics_risks_and_classification() {
    let s = schema();
    let ctx = Some("travel-sample.inventory");
    let d = diagnostics(
        "SELECT * FROM airline;\nSELECT * FROM airlines;\nDELETE FROM route;\nSELEC 1;\nSELECT r.* FROM route r UNNEST r.schedule s;\nSELECT \"str\", `x` FROM shop.sales.orders USE KEYS [\"k1\"];\nUPDATE airline USE KEYS \"a\" SET name = \"x\"",
        &s,
        ctx,
    );
    let msgs: Vec<(Severity, &str)> = d.iter().map(|d| (d.severity, d.message.as_str())).collect();
    assert_eq!(msgs.len(), 3, "{msgs:?}");
    assert!(msgs[0].1.contains("Unknown keyspace 'airlines'"));
    assert!(msgs[1].0 == Severity::Warning && msgs[1].1.starts_with("DELETE without WHERE"));
    assert!(msgs[2].1.contains("did you mean SELECT"));
    assert!(
        diagnostics("SELECT \"abc", &s, ctx)
            .iter()
            .any(|d| d.message.contains("Unterminated"))
    );
    let r: Vec<RiskKind> = risks("DELETE FROM a; DELETE FROM a USE KEYS 'k'; UPDATE a SET x = 1; DROP INDEX ix ON a; SELECT 1", false)
        .into_iter()
        .map(|r| r.kind)
        .collect();
    assert_eq!(
        r,
        vec![RiskKind::DeleteAll, RiskKind::UpdateAll, RiskKind::Drop]
    );
    assert_eq!(classify("SELECT 1"), StatementClass::Read);
    assert_eq!(classify("INFER airline"), StatementClass::Read);
    assert_eq!(
        classify("BEGIN WORK"),
        StatementClass::Transaction(TxCommand::Begin)
    );
    assert_eq!(
        classify("UPSERT INTO a (KEY, VALUE) VALUES ('k', {})"),
        StatementClass::Modification
    );
}

#[test]
fn parameters_and_format() {
    let p = crate::sql::parameters(
        "SELECT * FROM a WHERE x = $name AND y = $1 AND z = ?",
        &Flavor::for_dialect(DIALECT),
    );
    let names: Vec<&str> = p.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["name", "1", "?1"]);
    let src = "select a.name, meta(a).id from `travel-sample`.inventory.airline a use keys [\"x\"] where any s in a.stops satisfies s = \"y\" end";
    let f = crate::sql::format::format(src, DIALECT, &FormatStyle::default());
    assert_eq!(
        crate::sql::format::format(&f, DIALECT, &FormatStyle::default()),
        f
    );
    assert!(f.contains("`travel-sample`") && f.contains("\"y\""), "{f}");
}
