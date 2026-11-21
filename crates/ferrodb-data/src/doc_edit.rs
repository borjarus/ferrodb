//! Document edits as SQL++ (Couchbase) and PartiQL (DynamoDB) statements
//! (plan, section 5.6). MongoDB edits are shell commands (`ferrodb-core`).
//!
//! | Change | SQL++ | PartiQL |
//! |---|---|---|
//! | set field | `UPDATE ks USE KEYS "k" SET a.b = v` | `UPDATE "t" SET "a"."b" = v WHERE "pk" = … AND "sk" = …` |
//! | unset field | `UPDATE ks USE KEYS "k" UNSET a.b` | `UPDATE "t" REMOVE "a"."b" WHERE …` |
//! | insert | `INSERT INTO ks (KEY, VALUE) VALUES ("k", {…})` | `INSERT INTO "t" VALUE {…}` |
//! | replace | `UPSERT INTO ks (KEY, VALUE) VALUES ("k", {…})` | `UPDATE "t" SET … REMOVE … WHERE …` |
//! | delete | `DELETE FROM ks USE KEYS "k"` | `DELETE FROM "t" WHERE …` |
//!
//! A Couchbase document's key travels as `_id` (the driver adds it to
//! `SELECT *` results); it is not part of the stored value. DynamoDB items
//! are addressed by their key attributes. PartiQL statements are run with
//! `?` parameters (values keep their DynamoDB types, binaries included);
//! the preview shows them with literals.

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use bson::{Bson, Document};
use serde_json::{Map, Value as Json};

/// A generated statement: the text shown in the operation preview and,
/// for PartiQL, the parameterized form that is executed.
#[derive(Clone, Debug, PartialEq)]
pub struct DocStatement {
    pub preview: String,
    /// Statement with `?` placeholders and its parameters in DynamoDB JSON
    /// (PartiQL); `None` = run `preview` as it is (SQL++).
    pub parameterized: Option<(String, Vec<Json>)>,
}

impl DocStatement {
    fn text(preview: String) -> Self {
        Self {
            preview,
            parameterized: None,
        }
    }
}

// ------------------------------------------------------------------ SQL++

/// `name` or `` `name` `` (SQL++ identifiers).
pub fn sqlpp_ident(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain && !SQLPP_RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        name.to_owned()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

const SQLPP_RESERVED: &[&str] = &[
    "all", "and", "any", "array", "as", "by", "case", "default", "delete", "desc", "distinct",
    "else", "end", "every", "exists", "false", "first", "from", "group", "having", "in", "index",
    "insert", "is", "join", "key", "keys", "let", "like", "limit", "missing", "not", "null",
    "offset", "on", "or", "order", "path", "select", "set", "some", "then", "to", "true", "unnest",
    "unset", "update", "upsert", "use", "user", "value", "values", "when", "where", "with",
];

/// Splits `` b.`s-1`.c `` into its parts.
pub fn split_keyspace(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' if quoted && chars.peek() == Some(&'`') => {
                chars.next();
                cur.push('`');
            }
            '`' => quoted = !quoted,
            '.' if !quoted => out.push(std::mem::take(&mut cur)),
            other => cur.push(other),
        }
    }
    out.push(cur);
    out
}

/// `a.b.0` → `` a.b[0] `` (numeric segments index arrays).
pub fn sqlpp_path(path: &str) -> String {
    let mut out = String::new();
    for (i, seg) in path.split('.').enumerate() {
        if i > 0 && !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()) {
            out.push_str(&format!("[{seg}]"));
        } else {
            if i > 0 {
                out.push('.');
            }
            out.push_str(&sqlpp_ident(seg));
        }
    }
    out
}

/// BSON as JSON text (a SQL++ value literal).
pub fn json_literal(v: &Bson) -> String {
    match v {
        Bson::Document(d) => format!(
            "{{{}}}",
            d.iter()
                .map(|(k, x)| format!("{}: {}", Json::String(k.clone()), json_literal(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Bson::Array(a) => format!(
            "[{}]",
            a.iter().map(json_literal).collect::<Vec<_>>().join(", ")
        ),
        other => to_json(other).to_string(),
    }
}

/// BSON → plain JSON (Couchbase stores JSON).
pub fn to_json(v: &Bson) -> Json {
    match v {
        Bson::Null | Bson::Undefined => Json::Null,
        Bson::Boolean(b) => Json::Bool(*b),
        Bson::Int32(i) => Json::from(*i),
        Bson::Int64(i) => Json::from(*i),
        Bson::Double(d) => serde_json::Number::from_f64(*d).map_or(Json::Null, Json::Number),
        Bson::Decimal128(d) => {
            let s = d.to_string();
            serde_json::from_str::<Json>(&s)
                .ok()
                .filter(Json::is_number)
                .unwrap_or(Json::String(s))
        }
        Bson::String(s) | Bson::Symbol(s) => Json::String(s.clone()),
        Bson::Binary(b) => Json::String(B64.encode(&b.bytes)),
        Bson::ObjectId(o) => Json::String(o.to_hex()),
        Bson::DateTime(t) => Json::String(
            t.try_to_rfc3339_string()
                .unwrap_or_else(|_| t.timestamp_millis().to_string()),
        ),
        Bson::Document(d) => Json::Object(d.iter().map(|(k, x)| (k.clone(), to_json(x))).collect()),
        Bson::Array(a) => Json::Array(a.iter().map(to_json).collect()),
        other => Json::String(other.to_string()),
    }
}

/// The document key as a string literal.
fn key_literal(key: &Bson) -> String {
    match key {
        Bson::String(s) => Json::String(s.clone()).to_string(),
        other => Json::String(other.to_string()).to_string(),
    }
}

pub fn sqlpp_set(keyspace: &str, key: &Bson, path: &str, value: &Bson) -> DocStatement {
    DocStatement::text(format!(
        "UPDATE {keyspace} USE KEYS {} SET {} = {}",
        key_literal(key),
        sqlpp_path(path),
        json_literal(value)
    ))
}

pub fn sqlpp_unset(keyspace: &str, key: &Bson, path: &str) -> DocStatement {
    DocStatement::text(format!(
        "UPDATE {keyspace} USE KEYS {} UNSET {}",
        key_literal(key),
        sqlpp_path(path)
    ))
}

/// `doc` without `_id`; the key is `_id` (required for an insert).
pub fn sqlpp_insert(keyspace: &str, doc: &Document) -> Result<DocStatement, String> {
    let key = doc
        .get("_id")
        .ok_or("a new Couchbase document needs its key in `_id`")?;
    let mut value = doc.clone();
    value.remove("_id");
    Ok(DocStatement::text(format!(
        "INSERT INTO {keyspace} (KEY, VALUE) VALUES ({}, {})",
        key_literal(key),
        json_literal(&Bson::Document(value))
    )))
}

pub fn sqlpp_replace(keyspace: &str, key: &Bson, doc: &Document) -> DocStatement {
    let mut value = doc.clone();
    value.remove("_id");
    DocStatement::text(format!(
        "UPSERT INTO {keyspace} (KEY, VALUE) VALUES ({}, {})",
        key_literal(key),
        json_literal(&Bson::Document(value))
    ))
}

pub fn sqlpp_delete(keyspace: &str, key: &Bson) -> DocStatement {
    DocStatement::text(format!(
        "DELETE FROM {keyspace} USE KEYS {}",
        key_literal(key)
    ))
}

/// `path = value` for the quick filter (`WHERE`).
pub fn sqlpp_condition(path: &str, value: &Bson) -> String {
    match value {
        Bson::Null => format!("{} IS NULL", sqlpp_path(path)),
        v => format!("{} = {}", sqlpp_path(path), json_literal(v)),
    }
}

// ------------------------------------------------------------------ PartiQL

/// Set wrappers of DynamoDB items (see the driver's `value` module).
const SETS: [(&str, &str); 3] = [("$SS", "SS"), ("$NS", "NS"), ("$BS", "BS")];

/// `"name"` (PartiQL identifiers are case-sensitive; always quoted).
pub fn partiql_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `a.b.0` → `"a"."b"[0]`.
pub fn partiql_path(path: &str) -> String {
    let mut out = String::new();
    for (i, seg) in path.split('.').enumerate() {
        if i > 0 && !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()) {
            out.push_str(&format!("[{seg}]"));
        } else {
            if i > 0 {
                out.push('.');
            }
            out.push_str(&partiql_ident(seg));
        }
    }
    out
}

fn set_of(d: &Document) -> Option<(&'static str, &Vec<Bson>)> {
    if d.len() != 1 {
        return None;
    }
    let (k, v) = d.iter().next()?;
    let (_, t) = SETS.iter().find(|(w, _)| w == k)?;
    match v {
        Bson::Array(a) => Some((t, a)),
        _ => None,
    }
}

fn number_text(v: &Bson) -> Option<String> {
    Some(match v {
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) if d.is_finite() => {
            if d.fract() == 0.0 && d.abs() < 1e15 {
                format!("{d:.0}")
            } else {
                d.to_string()
            }
        }
        Bson::Decimal128(d) => d.to_string(),
        _ => return None,
    })
}

/// BSON → DynamoDB JSON (`{"S": "x"}`), the parameters of a statement.
pub fn dynamo_json(v: &Bson) -> Result<Json, String> {
    let one = |t: &str, x: Json| {
        let mut m = Map::new();
        m.insert(t.to_owned(), x);
        Json::Object(m)
    };
    if let Some(n) = number_text(v) {
        return Ok(one("N", Json::String(n)));
    }
    Ok(match v {
        Bson::String(s) | Bson::Symbol(s) => one("S", Json::String(s.clone())),
        Bson::Boolean(b) => one("BOOL", Json::Bool(*b)),
        Bson::Null | Bson::Undefined => one("NULL", Json::Bool(true)),
        Bson::Binary(b) => one("B", Json::String(B64.encode(&b.bytes))),
        Bson::ObjectId(o) => one("S", Json::String(o.to_hex())),
        Bson::DateTime(t) => one(
            "S",
            Json::String(t.try_to_rfc3339_string().map_err(|e| e.to_string())?),
        ),
        Bson::Array(a) => one(
            "L",
            Json::Array(a.iter().map(dynamo_json).collect::<Result<_, _>>()?),
        ),
        Bson::Document(d) => match set_of(d) {
            Some((t, items)) => {
                if items.is_empty() {
                    return Err("DynamoDB sets cannot be empty".into());
                }
                let texts = items
                    .iter()
                    .map(|x| match (t, x) {
                        ("SS", Bson::String(s)) => Ok(Json::String(s.clone())),
                        ("NS", Bson::String(s)) => Ok(Json::String(s.clone())),
                        ("NS", n) => number_text(n)
                            .map(Json::String)
                            .ok_or_else(|| format!("{n} in a number set")),
                        ("BS", Bson::Binary(b)) => Ok(Json::String(B64.encode(&b.bytes))),
                        (_, other) => Err(format!("{other} in a {t} set")),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                one(t, Json::Array(texts))
            }
            None => one(
                "M",
                Json::Object(
                    d.iter()
                        .map(|(k, x)| Ok((k.clone(), dynamo_json(x)?)))
                        .collect::<Result<_, String>>()?,
                ),
            ),
        },
        other => return Err(format!("{other} has no DynamoDB type")),
    })
}

/// A PartiQL value literal (for the preview).
pub fn partiql_literal(v: &Bson) -> String {
    if let Some(n) = number_text(v) {
        return n;
    }
    let s = |x: &str| format!("'{}'", x.replace('\'', "''"));
    match v {
        Bson::String(x) | Bson::Symbol(x) => s(x),
        Bson::Boolean(b) => b.to_string(),
        Bson::Null | Bson::Undefined => "NULL".into(),
        // No binary literal in PartiQL: the statement runs with a parameter.
        Bson::Binary(b) => format!("?/* binary, {} bytes */", b.bytes.len()),
        Bson::ObjectId(o) => s(&o.to_hex()),
        Bson::DateTime(t) => s(&t.try_to_rfc3339_string().unwrap_or_default()),
        Bson::Array(a) => format!(
            "[{}]",
            a.iter().map(partiql_literal).collect::<Vec<_>>().join(", ")
        ),
        Bson::Document(d) => match set_of(d) {
            Some((_, items)) => format!(
                "<<{}>>",
                items
                    .iter()
                    .map(partiql_literal)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None => format!(
                "{{{}}}",
                d.iter()
                    .map(|(k, x)| format!("{}: {}", s(k), partiql_literal(x)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        other => s(&other.to_string()),
    }
}

/// Builds the preview and the parameterized text side by side.
#[derive(Default)]
struct Pq {
    preview: String,
    text: String,
    params: Vec<Json>,
}

impl Pq {
    fn raw(&mut self, s: &str) {
        self.preview.push_str(s);
        self.text.push_str(s);
    }

    fn value(&mut self, v: &Bson) -> Result<(), String> {
        self.preview.push_str(&partiql_literal(v));
        self.text.push('?');
        self.params.push(dynamo_json(v)?);
        Ok(())
    }

    fn key(&mut self, key: &[(String, Bson)]) -> Result<(), String> {
        self.raw(" WHERE ");
        for (i, (k, v)) in key.iter().enumerate() {
            if i > 0 {
                self.raw(" AND ");
            }
            self.raw(&format!("{} = ", partiql_ident(k)));
            self.value(v)?;
        }
        Ok(())
    }

    fn done(self) -> DocStatement {
        DocStatement {
            preview: self.preview,
            parameterized: Some((self.text, self.params)),
        }
    }
}

/// The key attributes of an item, in key order; an error when one is
/// missing (the item cannot be addressed).
pub fn dynamo_key(item: &Document, key: &[String]) -> Result<Vec<(String, Bson)>, String> {
    key.iter()
        .map(|k| {
            item.get(k)
                .cloned()
                .map(|v| (k.clone(), v))
                .ok_or_else(|| format!("the item has no key attribute `{k}`"))
        })
        .collect()
}

fn check_not_key(path: &str, key: &[(String, Bson)]) -> Result<(), String> {
    let top = path.split('.').next().unwrap_or(path);
    if key.iter().any(|(k, _)| k == top) {
        Err(format!("`{top}` is part of the key: it cannot be changed"))
    } else {
        Ok(())
    }
}

pub fn partiql_set(
    table: &str,
    key: &[(String, Bson)],
    path: &str,
    value: &Bson,
) -> Result<DocStatement, String> {
    check_not_key(path, key)?;
    let mut q = Pq::default();
    q.raw(&format!(
        "UPDATE {} SET {} = ",
        partiql_ident(table),
        partiql_path(path)
    ));
    q.value(value)?;
    q.key(key)?;
    Ok(q.done())
}

pub fn partiql_unset(
    table: &str,
    key: &[(String, Bson)],
    path: &str,
) -> Result<DocStatement, String> {
    check_not_key(path, key)?;
    let mut q = Pq::default();
    q.raw(&format!(
        "UPDATE {} REMOVE {}",
        partiql_ident(table),
        partiql_path(path)
    ));
    q.key(key)?;
    Ok(q.done())
}

pub fn partiql_insert(table: &str, item: &Document) -> Result<DocStatement, String> {
    if item.is_empty() {
        return Err("an empty item".into());
    }
    let mut q = Pq::default();
    q.raw(&format!("INSERT INTO {} VALUE {{", partiql_ident(table)));
    for (i, (k, v)) in item.iter().enumerate() {
        if i > 0 {
            q.raw(", ");
        }
        q.raw(&format!("'{}': ", k.replace('\'', "''")));
        q.value(v)?;
    }
    q.raw("}");
    Ok(q.done())
}

/// Replaces every attribute but the key: `SET` the new ones, `REMOVE`
/// the old ones that are gone (`old` = attribute names of the item).
pub fn partiql_replace(
    table: &str,
    key: &[(String, Bson)],
    old: &[String],
    item: &Document,
) -> Result<DocStatement, String> {
    for (k, v) in key {
        match item.get(k) {
            Some(n) if n != v => {
                return Err(format!("`{k}` is part of the key: it cannot be changed"));
            }
            _ => {}
        }
    }
    let is_key = |n: &str| key.iter().any(|(k, _)| k == n);
    let set: Vec<(&String, &Bson)> = item.iter().filter(|(k, _)| !is_key(k)).collect();
    let remove: Vec<&String> = old
        .iter()
        .filter(|n| !is_key(n) && !item.contains_key(n.as_str()))
        .collect();
    if set.is_empty() && remove.is_empty() {
        return Err("nothing to replace: the item has only key attributes".into());
    }
    let mut q = Pq::default();
    q.raw(&format!("UPDATE {}", partiql_ident(table)));
    for (i, (k, v)) in set.iter().enumerate() {
        q.raw(if i == 0 { " SET " } else { ", " });
        q.raw(&format!("{} = ", partiql_ident(k)));
        q.value(v)?;
    }
    for n in &remove {
        q.raw(&format!(" REMOVE {}", partiql_ident(n)));
    }
    q.key(key)?;
    Ok(q.done())
}

pub fn partiql_delete(table: &str, key: &[(String, Bson)]) -> Result<DocStatement, String> {
    let mut q = Pq::default();
    q.raw(&format!("DELETE FROM {}", partiql_ident(table)));
    q.key(key)?;
    Ok(q.done())
}

/// `path = value` for the quick filter (`WHERE`).
pub fn partiql_condition(path: &str, value: &Bson) -> String {
    match value {
        Bson::Null => format!("{} IS NULL", partiql_path(path)),
        v => format!("{} = {}", partiql_path(path), partiql_literal(v)),
    }
}

#[cfg(test)]
mod tests {
    use bson::{Binary, doc, spec::BinarySubtype};

    use super::*;

    #[test]
    fn sqlpp_statements() {
        let k = Bson::String("k1".into());
        assert_eq!(
            sqlpp_set(
                "tck.app.docs",
                &k,
                "address.city",
                &Bson::String("Kraków".into())
            )
            .preview,
            r#"UPDATE tck.app.docs USE KEYS "k1" SET address.city = "Kraków""#
        );
        assert_eq!(
            sqlpp_unset("tck.app.docs", &k, "items.0.value").preview,
            r#"UPDATE tck.app.docs USE KEYS "k1" UNSET items[0].`value`"#
        );
        let d = doc! { "_id": "k2", "a": 1, "b": { "c": [true, Bson::Null] }, "f": 1.5 };
        assert_eq!(
            sqlpp_insert("ks", &d).unwrap().preview,
            r#"INSERT INTO ks (KEY, VALUE) VALUES ("k2", {"a": 1, "b": {"c": [true, null]}, "f": 1.5})"#
        );
        assert!(sqlpp_insert("ks", &doc! { "a": 1 }).is_err());
        assert_eq!(
            sqlpp_replace("ks", &k, &doc! { "_id": "k1", "z": "x\"y" }).preview,
            r#"UPSERT INTO ks (KEY, VALUE) VALUES ("k1", {"z": "x\"y"})"#
        );
        assert_eq!(
            sqlpp_delete("ks", &k).preview,
            r#"DELETE FROM ks USE KEYS "k1""#
        );
        assert_eq!(sqlpp_condition("a.b", &Bson::Int32(3)), "a.b = 3");
        assert_eq!(
            split_keyspace("`travel-sample`.inv.`a``b`"),
            vec!["travel-sample", "inv", "a`b"]
        );
        assert_eq!(sqlpp_condition("user", &Bson::Null), "`user` IS NULL");
        let dec = "12345.6789".parse::<bson::Decimal128>().unwrap();
        assert_eq!(json_literal(&Bson::Decimal128(dec)), "12345.6789");
    }

    #[test]
    fn partiql_statements_with_parameters() {
        let key = dynamo_key(
            &doc! { "pk": "u#1", "sk": 7, "a": 1 },
            &["pk".into(), "sk".into()],
        )
        .unwrap();
        let s = partiql_set(
            "Orders",
            &key,
            "address.city",
            &Bson::String("O'Hare".into()),
        )
        .unwrap();
        assert_eq!(
            s.preview,
            r#"UPDATE "Orders" SET "address"."city" = 'O''Hare' WHERE "pk" = 'u#1' AND "sk" = 7"#
        );
        let (text, params) = s.parameterized.unwrap();
        assert_eq!(
            text,
            r#"UPDATE "Orders" SET "address"."city" = ? WHERE "pk" = ? AND "sk" = ?"#
        );
        assert_eq!(
            params,
            vec![
                serde_json::json!({"S": "O'Hare"}),
                serde_json::json!({"S": "u#1"}),
                serde_json::json!({"N": "7"})
            ]
        );
        assert!(partiql_set("Orders", &key, "sk", &Bson::Int32(1)).is_err());
        assert_eq!(
            partiql_unset("Orders", &key, "tags.0").unwrap().preview,
            r#"UPDATE "Orders" REMOVE "tags"[0] WHERE "pk" = 'u#1' AND "sk" = 7"#
        );
        let item = doc! { "pk": "u#2", "tags": { "$SS": ["a", "b"] }, "bin": Binary { subtype: BinarySubtype::Generic, bytes: vec![1, 2] } };
        let ins = partiql_insert("Orders", &item).unwrap();
        assert_eq!(
            ins.preview,
            r#"INSERT INTO "Orders" VALUE {'pk': 'u#2', 'tags': <<'a', 'b'>>, 'bin': ?/* binary, 2 bytes */}"#
        );
        let (text, params) = ins.parameterized.unwrap();
        assert_eq!(
            text,
            r#"INSERT INTO "Orders" VALUE {'pk': ?, 'tags': ?, 'bin': ?}"#
        );
        assert_eq!(params[1], serde_json::json!({"SS": ["a", "b"]}));
        assert_eq!(params[2], serde_json::json!({"B": "AQI="}));
        let rep = partiql_replace(
            "Orders",
            &key,
            &["pk".into(), "sk".into(), "a".into(), "old".into()],
            &doc! { "pk": "u#1", "sk": 7, "a": 2, "n": { "$NS": [1, 2.5] } },
        )
        .unwrap();
        assert_eq!(
            rep.preview,
            r#"UPDATE "Orders" SET "a" = 2, "n" = <<1, 2.5>> REMOVE "old" WHERE "pk" = 'u#1' AND "sk" = 7"#
        );
        assert_eq!(
            rep.parameterized.unwrap().1[1],
            serde_json::json!({"NS": ["1", "2.5"]})
        );
        assert!(partiql_replace("Orders", &key, &[], &doc! { "pk": "other" }).is_err());
        assert_eq!(
            partiql_delete("Orders", &key).unwrap().preview,
            r#"DELETE FROM "Orders" WHERE "pk" = 'u#1' AND "sk" = 7"#
        );
        assert!(dynamo_key(&doc! { "pk": 1 }, &["pk".into(), "sk".into()]).is_err());
        assert_eq!(
            partiql_condition("a.b", &Bson::Boolean(true)),
            r#""a"."b" = true"#
        );
        assert!(dynamo_json(&Bson::Document(doc! { "$SS": [] })).is_err());
    }

    #[test]
    fn edited_values_arrive_as_extended_json() {
        // The document view sends values in relaxed Extended JSON.
        let item = doc! { "s": { "$SS": ["x"] }, "d": "1.25".parse::<bson::Decimal128>().unwrap(), "i": 9_007_199_254_740_993_i64 };
        let ext = Bson::Document(item.clone()).into_relaxed_extjson();
        let back = Bson::try_from(ext).unwrap();
        assert_eq!(back, Bson::Document(item.clone()));
        assert_eq!(
            dynamo_json(&back).unwrap(),
            serde_json::json!({"M": {"s": {"SS": ["x"]}, "d": {"N": "1.25"}, "i": {"N": "9007199254740993"}}})
        );
    }
}
