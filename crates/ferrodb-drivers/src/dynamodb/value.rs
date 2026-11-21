//! DynamoDB attribute values ↔ BSON, and the DynamoDB JSON of the AWS API
//! and CLI (`{"S": "x"}`, `{"N": "1"}`…).
//!
//! | DynamoDB | BSON |
//! |---|---|
//! | `S` | string |
//! | `N` | int32 / int64 for integers, decimal128 otherwise (exact); a string when 34 digits are not enough |
//! | `B` | binary (generic) |
//! | `BOOL` | boolean |
//! | `NULL` | null |
//! | `M` | document (keys sorted: DynamoDB maps have no order) |
//! | `L` | array |
//! | `SS` / `NS` / `BS` | `{ "$SS": [...] }`, `{ "$NS": [...] }`, `{ "$BS": [...] }` |
//!
//! BSON has no set type: a one-field document named after the DynamoDB
//! type keeps a set a set when the document is edited and written back.

use std::collections::HashMap;

use aws_sdk_dynamodb::{primitives::Blob, types::AttributeValue};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use bson::{Binary, Bson, Decimal128, Document, spec::BinarySubtype};
use serde_json::{Map, Value as Json};

use crate::{Error, Result};

/// Field name of the set wrappers.
pub const STRING_SET: &str = "$SS";
pub const NUMBER_SET: &str = "$NS";
pub const BINARY_SET: &str = "$BS";

fn bad(msg: impl Into<String>) -> Error {
    Error::Other(format!("DynamoDB value: {}", msg.into()))
}

/// A DynamoDB number as BSON, keeping its precision.
pub fn number_to_bson(n: &str) -> Bson {
    let t = n.trim();
    let integer = {
        let digits = t.strip_prefix('-').unwrap_or(t);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    };
    if integer && let Ok(v) = t.parse::<i64>() {
        return match i32::try_from(v) {
            Ok(small) => Bson::Int32(small),
            Err(_) => Bson::Int64(v),
        };
    }
    match t.parse::<Decimal128>() {
        Ok(d) => Bson::Decimal128(d),
        // More than 34 significant digits: the text is the only exact form.
        Err(_) => Bson::String(t.to_owned()),
    }
}

/// The text of a BSON number for `N`; `None` for other types.
pub fn bson_number(v: &Bson) -> Option<Result<String>> {
    Some(match v {
        Bson::Int32(i) => Ok(i.to_string()),
        Bson::Int64(i) => Ok(i.to_string()),
        Bson::Double(d) if d.is_finite() => Ok(format_double(*d)),
        Bson::Double(d) => Err(bad(format!("{d} is not a DynamoDB number"))),
        Bson::Decimal128(d) => Ok(d.to_string()),
        _ => return None,
    })
}

/// Shortest text that reads back as the same double.
fn format_double(d: f64) -> String {
    if d.fract() == 0.0 && d.abs() < 1e15 {
        format!("{d:.0}")
    } else {
        format!("{d}")
    }
}

fn binary(bytes: Vec<u8>) -> Bson {
    Bson::Binary(Binary {
        subtype: BinarySubtype::Generic,
        bytes,
    })
}

/// Attribute value → BSON.
pub fn to_bson(v: &AttributeValue) -> Bson {
    match v {
        AttributeValue::S(s) => Bson::String(s.clone()),
        AttributeValue::N(n) => number_to_bson(n),
        AttributeValue::B(b) => binary(b.as_ref().to_vec()),
        AttributeValue::Bool(b) => Bson::Boolean(*b),
        AttributeValue::Null(_) => Bson::Null,
        AttributeValue::M(m) => Bson::Document(map_to_doc(m, &[])),
        AttributeValue::L(l) => Bson::Array(l.iter().map(to_bson).collect()),
        AttributeValue::Ss(s) => set(
            STRING_SET,
            s.iter().map(|x| Bson::String(x.clone())).collect(),
        ),
        AttributeValue::Ns(n) => set(NUMBER_SET, n.iter().map(|x| number_to_bson(x)).collect()),
        AttributeValue::Bs(b) => set(
            BINARY_SET,
            b.iter().map(|x| binary(x.as_ref().to_vec())).collect(),
        ),
        _ => Bson::Null,
    }
}

fn set(name: &str, items: Vec<Bson>) -> Bson {
    let mut d = Document::new();
    d.insert(name, items);
    Bson::Document(d)
}

/// An item (or map) as a document: `first` attributes (the key) in that
/// order, then the rest sorted by name.
pub fn map_to_doc(m: &HashMap<String, AttributeValue>, first: &[String]) -> Document {
    let mut d = Document::new();
    for k in first {
        if let Some(v) = m.get(k) {
            d.insert(k.clone(), to_bson(v));
        }
    }
    let mut rest: Vec<&String> = m.keys().filter(|k| !first.contains(k)).collect();
    rest.sort();
    for k in rest {
        d.insert(k.clone(), to_bson(&m[k]));
    }
    d
}

/// BSON → attribute value (writes from the document editor).
pub fn from_bson(v: &Bson) -> Result<AttributeValue> {
    if let Some(n) = bson_number(v) {
        return Ok(AttributeValue::N(n?));
    }
    Ok(match v {
        Bson::String(s) => AttributeValue::S(s.clone()),
        Bson::Boolean(b) => AttributeValue::Bool(*b),
        Bson::Null | Bson::Undefined => AttributeValue::Null(true),
        Bson::Binary(b) => AttributeValue::B(Blob::new(b.bytes.clone())),
        Bson::Array(a) => AttributeValue::L(a.iter().map(from_bson).collect::<Result<_>>()?),
        Bson::Document(d) => match set_of(d) {
            Some((kind, items)) => from_set(kind, items)?,
            None => AttributeValue::M(
                d.iter()
                    .map(|(k, x)| Ok((k.clone(), from_bson(x)?)))
                    .collect::<Result<_>>()?,
            ),
        },
        Bson::ObjectId(o) => AttributeValue::S(o.to_hex()),
        Bson::DateTime(t) => AttributeValue::S(
            t.try_to_rfc3339_string()
                .map_err(|e| bad(format!("date: {e}")))?,
        ),
        Bson::Symbol(s) => AttributeValue::S(s.clone()),
        other => return Err(bad(format!("{other} has no DynamoDB type"))),
    })
}

/// `{ "$SS": [...] }` and friends.
pub fn set_of(d: &Document) -> Option<(&str, &Vec<Bson>)> {
    if d.len() != 1 {
        return None;
    }
    let (k, v) = d.iter().next()?;
    match (k.as_str(), v) {
        (STRING_SET | NUMBER_SET | BINARY_SET, Bson::Array(items)) => Some((k.as_str(), items)),
        _ => None,
    }
}

fn from_set(kind: &str, items: &[Bson]) -> Result<AttributeValue> {
    if items.is_empty() {
        return Err(bad("DynamoDB sets cannot be empty"));
    }
    Ok(match kind {
        STRING_SET => AttributeValue::Ss(
            items
                .iter()
                .map(|x| match x {
                    Bson::String(s) => Ok(s.clone()),
                    other => Err(bad(format!("{other} in a string set"))),
                })
                .collect::<Result<_>>()?,
        ),
        NUMBER_SET => AttributeValue::Ns(
            items
                .iter()
                .map(|x| match bson_number(x) {
                    Some(n) => n,
                    None => match x {
                        Bson::String(s) => Ok(s.clone()),
                        other => Err(bad(format!("{other} in a number set"))),
                    },
                })
                .collect::<Result<_>>()?,
        ),
        _ => AttributeValue::Bs(
            items
                .iter()
                .map(|x| match x {
                    Bson::Binary(b) => Ok(Blob::new(b.bytes.clone())),
                    other => Err(bad(format!("{other} in a binary set"))),
                })
                .collect::<Result<_>>()?,
        ),
    })
}

/// DynamoDB JSON (`{"S": "x"}`) → attribute value.
pub fn from_json(v: &Json) -> Result<AttributeValue> {
    let o = v
        .as_object()
        .filter(|o| o.len() == 1)
        .ok_or_else(|| bad(format!("expected {{\"<type>\": value}}, got {v}")))?;
    let (t, x) = o.iter().next().expect("one entry");
    let str_of = |x: &Json| -> Result<String> {
        match x {
            Json::String(s) => Ok(s.clone()),
            Json::Number(n) => Ok(n.to_string()),
            other => Err(bad(format!("{t}: expected a string, got {other}"))),
        }
    };
    let bytes = |x: &Json| -> Result<Blob> {
        let s = str_of(x)?;
        B64.decode(s.as_bytes())
            .map(Blob::new)
            .map_err(|e| bad(format!("{t}: base64: {e}")))
    };
    let list = |x: &Json| -> Result<Vec<Json>> {
        x.as_array()
            .cloned()
            .ok_or_else(|| bad(format!("{t}: expected an array")))
    };
    Ok(match t.as_str() {
        "S" => AttributeValue::S(str_of(x)?),
        "N" => AttributeValue::N(str_of(x)?),
        "B" => AttributeValue::B(bytes(x)?),
        "BOOL" => AttributeValue::Bool(
            x.as_bool()
                .ok_or_else(|| bad("BOOL: expected true or false"))?,
        ),
        "NULL" => AttributeValue::Null(true),
        "M" => AttributeValue::M(item_from_json(x)?),
        "L" => AttributeValue::L(list(x)?.iter().map(from_json).collect::<Result<_>>()?),
        "SS" => AttributeValue::Ss(list(x)?.iter().map(str_of).collect::<Result<_>>()?),
        "NS" => AttributeValue::Ns(list(x)?.iter().map(str_of).collect::<Result<_>>()?),
        "BS" => AttributeValue::Bs(list(x)?.iter().map(bytes).collect::<Result<_>>()?),
        other => return Err(bad(format!("unknown type `{other}`"))),
    })
}

/// `{"name": {"S": "x"}, …}` → item.
pub fn item_from_json(v: &Json) -> Result<HashMap<String, AttributeValue>> {
    v.as_object()
        .ok_or_else(|| bad(format!("expected an object of attributes, got {v}")))?
        .iter()
        .map(|(k, x)| Ok((k.clone(), from_json(x)?)))
        .collect()
}

/// Attribute value → DynamoDB JSON.
pub fn to_json(v: &AttributeValue) -> Json {
    let one = |t: &str, x: Json| {
        let mut m = Map::new();
        m.insert(t.to_owned(), x);
        Json::Object(m)
    };
    match v {
        AttributeValue::S(s) => one("S", Json::String(s.clone())),
        AttributeValue::N(n) => one("N", Json::String(n.clone())),
        AttributeValue::B(b) => one("B", Json::String(B64.encode(b.as_ref()))),
        AttributeValue::Bool(b) => one("BOOL", Json::Bool(*b)),
        AttributeValue::Null(_) => one("NULL", Json::Bool(true)),
        AttributeValue::M(m) => one("M", item_to_json(m)),
        AttributeValue::L(l) => one("L", Json::Array(l.iter().map(to_json).collect())),
        AttributeValue::Ss(s) => one(
            "SS",
            Json::Array(s.iter().cloned().map(Json::String).collect()),
        ),
        AttributeValue::Ns(s) => one(
            "NS",
            Json::Array(s.iter().cloned().map(Json::String).collect()),
        ),
        AttributeValue::Bs(s) => one(
            "BS",
            Json::Array(
                s.iter()
                    .map(|b| Json::String(B64.encode(b.as_ref())))
                    .collect(),
            ),
        ),
        _ => one("NULL", Json::Bool(true)),
    }
}

/// Item → DynamoDB JSON, attributes sorted by name.
pub fn item_to_json(m: &HashMap<String, AttributeValue>) -> Json {
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort();
    Json::Object(
        keys.into_iter()
            .map(|k| (k.clone(), to_json(&m[k])))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    #[test]
    fn numbers_keep_precision() {
        assert_eq!(number_to_bson("42"), Bson::Int32(42));
        assert_eq!(
            number_to_bson("9007199254740993"),
            Bson::Int64(9_007_199_254_740_993)
        );
        let d = number_to_bson("12345.6789");
        assert!(matches!(d, Bson::Decimal128(_)), "{d:?}");
        assert_eq!(bson_number(&d).unwrap().unwrap(), "12345.6789");
        // 38 digits: beyond decimal128, kept as text.
        let big = "12345678901234567890123456789012345678";
        assert_eq!(number_to_bson(big), Bson::String(big.into()));
        assert_eq!(bson_number(&Bson::Double(1.5)).unwrap().unwrap(), "1.5");
        assert_eq!(bson_number(&Bson::Double(3.0)).unwrap().unwrap(), "3");
        assert!(bson_number(&Bson::Double(f64::NAN)).unwrap().is_err());
    }

    #[test]
    fn every_type_round_trips() {
        let json: Json = serde_json::from_str(
            r#"{
              "pk": {"S": "user#1"}, "n": {"N": "-0.125"}, "big": {"N": "123456789012"},
              "b": {"B": "AAEC/w=="}, "ok": {"BOOL": true}, "nothing": {"NULL": true},
              "m": {"M": {"z": {"S": "last"}, "a": {"L": [{"N": "1"}, {"S": "two"}]}}},
              "ss": {"SS": ["a", "b"]}, "ns": {"NS": ["1", "2.5"]}, "bs": {"BS": ["AQ==", "Ag=="]}
            }"#,
        )
        .unwrap();
        let item = item_from_json(&json).unwrap();
        let d = map_to_doc(&item, &["pk".into()]);
        assert_eq!(d.keys().next().map(String::as_str), Some("pk"));
        assert_eq!(d.get_document("ss").unwrap(), &doc! { "$SS": ["a", "b"] });
        assert_eq!(
            d.get_document("m").unwrap().keys().collect::<Vec<_>>(),
            vec!["a", "z"]
        );
        // BSON → attribute values → DynamoDB JSON gives the input back.
        let back: HashMap<String, AttributeValue> = d
            .iter()
            .map(|(k, v)| (k.clone(), from_bson(v).unwrap()))
            .collect();
        assert_eq!(item_to_json(&back), item_to_json(&item));
        assert_eq!(back, item);
        assert!(from_bson(&Bson::Document(doc! { "$SS": [] })).is_err());
        assert!(from_json(&serde_json::json!({"X": 1})).is_err());
    }
}
