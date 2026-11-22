//! Tabular edits → CQL (plan, section 5.6, Cassandra): `UPDATE … SET …
//! WHERE <full primary key>`, `INSERT` with every key column, `DELETE …
//! WHERE <full primary key>`. Cassandra writes are upserts and report no
//! affected rows, so a row is only addressed by its complete primary key
//! (partition and clustering columns): results without it are not editable
//! (the core checks that before planning). Key columns cannot be changed
//! (that would be another row); counters cannot be set.
//!
//! Literals follow the column's CQL type: numbers, `uuid` / `timeuuid` and
//! `duration` unquoted, text / `inet` / dates quoted, `blob` as `0x…`, and
//! collections, tuples, UDTs and vectors from their JSON text (as the driver
//! shows them) to `[…]`, `{…}`, `{k: v}`, `(…)`.

use ferrodb_api::{Change, ChangeSet};

use crate::{
    edit::Planned,
    value::{Value, ValueType, float_text, format_date, format_time, hex},
};

/// The table behind a result.
#[derive(Clone, Debug, PartialEq)]
pub struct CqlTable {
    /// Qualified, quoted table name (`ks.t`).
    pub table: String,
    /// Per result column: the table column (`None` = computed).
    pub columns: Vec<Option<String>>,
    /// Per result column: its CQL type (`int`, `map<text, int>`).
    pub types: Vec<String>,
    /// Result columns of the full primary key (partition + clustering).
    pub key: Vec<usize>,
}

/// A parsed CQL type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CqlType {
    Simple(String),
    List(Box<CqlType>),
    Set(Box<CqlType>),
    Map(Box<CqlType>, Box<CqlType>),
    Tuple(Vec<CqlType>),
    Vector(Box<CqlType>),
    /// A user-defined type (fields unknown here).
    Udt(String),
}

const SIMPLE: &[&str] = &[
    "ascii",
    "bigint",
    "blob",
    "boolean",
    "counter",
    "date",
    "decimal",
    "double",
    "duration",
    "float",
    "inet",
    "int",
    "smallint",
    "text",
    "time",
    "timestamp",
    "timeuuid",
    "tinyint",
    "uuid",
    "varchar",
    "varint",
];

/// Parses `map<text, frozen<list<int>>>` (`frozen<>` is dropped).
pub fn parse_type(s: &str) -> CqlType {
    fn split_args(s: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let (mut depth, mut start) = (0, 0);
        for (i, c) in s.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(s[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        out.push(s[start..].trim());
        out
    }
    let s = s.trim();
    let Some(open) = s.find('<') else {
        let lower = s.to_ascii_lowercase();
        return if SIMPLE.contains(&lower.as_str()) {
            CqlType::Simple(lower)
        } else {
            CqlType::Udt(s.to_owned())
        };
    };
    let name = s[..open].trim().to_ascii_lowercase();
    let inner = s[open + 1..]
        .trim_end()
        .strip_suffix('>')
        .unwrap_or(&s[open + 1..]);
    let args = split_args(inner);
    match name.as_str() {
        "frozen" => parse_type(inner),
        "list" => CqlType::List(Box::new(parse_type(args[0]))),
        "set" => CqlType::Set(Box::new(parse_type(args[0]))),
        "map" if args.len() == 2 => {
            CqlType::Map(Box::new(parse_type(args[0])), Box::new(parse_type(args[1])))
        }
        "tuple" => CqlType::Tuple(args.iter().map(|a| parse_type(a)).collect()),
        "vector" => CqlType::Vector(Box::new(parse_type(args[0]))),
        _ => CqlType::Udt(s.to_owned()),
    }
}

fn string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn is_number(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && (t.parse::<f64>().is_ok()
            || t.trim_start_matches(['-', '+'])
                .bytes()
                .all(|b| b.is_ascii_digit()))
        && !t.eq_ignore_ascii_case("nan")
        && !t.to_ascii_lowercase().contains("inf")
}

fn simple(ty: &str, v: &Value) -> Result<String, String> {
    if *v == Value::Null {
        return Ok("null".into());
    }
    let text = || v.to_text().unwrap_or_default();
    Ok(match ty {
        "int" | "bigint" | "smallint" | "tinyint" | "varint" | "counter" | "decimal" | "float"
        | "double" => {
            let t = match v {
                Value::Float(f) if f.is_nan() => return Ok("NaN".into()),
                Value::Float(f) if f.is_infinite() => {
                    return Ok(if *f > 0.0 { "Infinity" } else { "-Infinity" }.into());
                }
                Value::Float(f) => float_text(*f),
                Value::Json(serde_json::Value::Number(n)) => n.to_string(),
                _ => text(),
            };
            if !is_number(&t) {
                return Err(format!("{t:?} is not a number ({ty})"));
            }
            t.trim().to_owned()
        }
        "boolean" => match v {
            Value::Bool(b) => b.to_string(),
            _ => match text().trim().to_ascii_lowercase().as_str() {
                "true" | "t" | "1" => "true".into(),
                "false" | "f" | "0" => "false".into(),
                other => return Err(format!("{other:?} is not a boolean")),
            },
        },
        "uuid" | "timeuuid" => {
            let t = text();
            let t = t.trim();
            let ok = t.len() == 36
                && t.chars().enumerate().all(|(i, c)| {
                    if [8, 13, 18, 23].contains(&i) {
                        c == '-'
                    } else {
                        c.is_ascii_hexdigit()
                    }
                });
            if !ok {
                return Err(format!("{t:?} is not a UUID"));
            }
            t.to_owned()
        }
        "duration" => {
            let t = text();
            let t = t.trim();
            if t.is_empty()
                || !t
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':')
            {
                return Err(format!("{t:?} is not a duration"));
            }
            t.to_owned()
        }
        "blob" => match v {
            Value::Bytes(b) => format!("0x{}", hex(b)),
            _ => {
                let t = text();
                let t = t.trim();
                match t.strip_prefix("0x").or_else(|| t.strip_prefix("\\x")) {
                    Some(h) if h.bytes().all(|b| b.is_ascii_hexdigit()) => format!("0x{h}"),
                    _ => format!("0x{}", hex(t.as_bytes())),
                }
            }
        },
        "date" => match v {
            Value::Date(d) => string(&format_date(*d)),
            _ => string(&text()),
        },
        "time" => match v {
            Value::Time(t) => string(&format_time(*t)),
            _ => string(&text()),
        },
        "timestamp" => match v {
            // Milliseconds since the epoch: exact, whatever the time zone.
            Value::Timestamp { micros, .. } => micros.div_euclid(1000).to_string(),
            Value::Int(ms) => ms.to_string(),
            _ => string(&text()),
        },
        _ => string(&text()),
    })
}

/// A JSON value as a CQL literal of type `ty`.
fn from_json(ty: &CqlType, j: &serde_json::Value) -> Result<String, String> {
    use serde_json::Value as J;
    if j.is_null() {
        return Ok("null".into());
    }
    let items = |j: &J| -> Result<Vec<J>, String> {
        j.as_array()
            .cloned()
            .ok_or_else(|| format!("expected a JSON array, got {j}"))
    };
    Ok(match ty {
        CqlType::Simple(s) => {
            let v = match j {
                J::String(t) => Value::Text(t.clone()),
                J::Bool(b) => Value::Bool(*b),
                J::Number(n) => Value::Json(J::Number(n.clone())),
                other => Value::Text(other.to_string()),
            };
            simple(s, &v)?
        }
        CqlType::List(e) | CqlType::Vector(e) => format!(
            "[{}]",
            items(j)?
                .iter()
                .map(|x| from_json(e, x))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        CqlType::Set(e) => format!(
            "{{{}}}",
            items(j)?
                .iter()
                .map(|x| from_json(e, x))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        CqlType::Tuple(ts) => {
            let v = items(j)?;
            if v.len() != ts.len() {
                return Err(format!("expected {} tuple elements", ts.len()));
            }
            format!(
                "({})",
                ts.iter()
                    .zip(&v)
                    .map(|(t, x)| from_json(t, x))
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            )
        }
        CqlType::Map(k, v) => {
            let obj = j
                .as_object()
                .ok_or_else(|| format!("expected a JSON object, got {j}"))?;
            format!(
                "{{{}}}",
                sorted(obj)
                    .into_iter()
                    .map(|(key, val)| {
                        // JSON keys are strings: numbers etc. are parsed back.
                        let kj = match k.as_ref() {
                            CqlType::Simple(s) if s != "text" && s != "ascii" && s != "varchar" => {
                                serde_json::from_str(key).unwrap_or(J::String(key.clone()))
                            }
                            _ => J::String(key.clone()),
                        };
                        Ok(format!("{}: {}", from_json(k, &kj)?, from_json(v, val)?))
                    })
                    .collect::<Result<Vec<_>, String>>()?
                    .join(", ")
            )
        }
        CqlType::Udt(_) => {
            let obj = j
                .as_object()
                .ok_or_else(|| format!("expected a JSON object, got {j}"))?;
            format!(
                "{{{}}}",
                sorted(obj)
                    .into_iter()
                    .map(|(f, val)| Ok(format!("{}: {}", ident(f), untyped(val)?)))
                    .collect::<Result<Vec<_>, String>>()?
                    .join(", ")
            )
        }
    })
}

/// A JSON value without a known CQL type (UDT fields).
fn untyped(j: &serde_json::Value) -> Result<String, String> {
    use serde_json::Value as J;
    Ok(match j {
        J::Null => "null".into(),
        J::Bool(b) => b.to_string(),
        J::Number(n) => n.to_string(),
        J::String(s) => string(s),
        J::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(untyped)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        J::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| Ok(format!("{}: {}", ident(k), untyped(v)?)))
                .collect::<Result<Vec<_>, String>>()?
                .join(", ")
        ),
    })
}

/// A CQL identifier, quoted unless lower-case.
pub fn ident(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// A value as a literal of CQL type `ty` (collections from JSON text).
pub fn literal(ty: &str, v: &Value) -> Result<String, String> {
    match parse_type(ty) {
        CqlType::Simple(s) => simple(&s, v),
        other => match v {
            Value::Null => Ok("null".into()),
            Value::Json(j) => from_json(&other, j),
            Value::Text(t) => {
                let j: serde_json::Value =
                    serde_json::from_str(t).map_err(|e| format!("{ty}: not JSON ({e}): {t}"))?;
                from_json(&other, &j)
            }
            v => Err(format!("{ty}: unexpected value {v:?}")),
        },
    }
}

/// The value type edited cells of a CQL column are read as.
pub fn value_type(ty: &str) -> ValueType {
    match parse_type(ty) {
        CqlType::Simple(s) => match s.as_str() {
            "int" | "bigint" | "smallint" | "tinyint" | "counter" => ValueType::Int,
            "float" | "double" => ValueType::Float,
            "boolean" => ValueType::Bool,
            "blob" => ValueType::Bytes,
            "date" => ValueType::Date,
            "time" => ValueType::Time,
            "timestamp" => ValueType::Timestamp { tz: true },
            // varint, decimal: arbitrary precision, kept as written.
            _ => ValueType::Text,
        },
        _ => ValueType::Any,
    }
}

/// Plans `changes` as CQL. `original(row, column)` gives the value a cell
/// had when it was read.
pub fn plan(
    t: &CqlTable,
    changes: &ChangeSet,
    original: impl Fn(usize, usize) -> Option<Value>,
) -> Result<Vec<Planned>, String> {
    if t.key.is_empty() {
        return Err("the result does not contain the full primary key".into());
    }
    let column = |c: usize| -> Result<&str, String> {
        t.columns
            .get(c)
            .ok_or_else(|| format!("column {} is out of range", c + 1))?
            .as_deref()
            .ok_or_else(|| format!("column {} is computed and cannot be edited", c + 1))
    };
    let ty = |c: usize| t.types.get(c).map(String::as_str).unwrap_or("text");
    let lit = |c: usize, v: &Value| -> Result<String, String> {
        literal(ty(c), v).map_err(|e| format!("{}: {e}", column(c).unwrap_or("?")))
    };
    let edited = |c: usize, json: &str| -> Result<String, String> {
        let v = Value::from_cell_json(json, value_type(ty(c)))
            .map_err(|e| format!("{}: {e}", column(c).unwrap_or("?")))?;
        lit(c, &v)
    };
    let where_key = |row: usize| -> Result<String, String> {
        t.key
            .iter()
            .map(|&k| {
                let v = original(row, k)
                    .ok_or_else(|| format!("row {} is no longer available for editing", row + 1))?;
                if v == Value::Null {
                    return Err(format!(
                        "row {}: key column {} is null",
                        row + 1,
                        column(k)?
                    ));
                }
                Ok(format!("{} = {}", ident(column(k)?), lit(k, &v)?))
            })
            .collect::<Result<Vec<_>, String>>()
            .map(|v| v.join(" AND "))
    };
    let mut updates: std::collections::BTreeMap<usize, Vec<(usize, String)>> = Default::default();
    let mut out = Vec::new();
    let mut inserts = Vec::new();
    let mut deletes = Vec::new();
    for ch in &changes.changes {
        match ch {
            Change::SetCell {
                row,
                column: c,
                value,
            } => {
                column(*c)?;
                if t.key.contains(c) {
                    return Err(format!(
                        "{} is part of the primary key: delete the row and insert a new one",
                        column(*c)?
                    ));
                }
                if ty(*c) == "counter" {
                    return Err(format!(
                        "{} is a counter: change it with UPDATE … SET c = c + n",
                        column(*c)?
                    ));
                }
                let set = updates.entry(*row).or_default();
                set.retain(|(x, _)| x != c);
                set.push((*c, edited(*c, value)?));
            }
            Change::InsertRow { values } => {
                let mut cols = Vec::new();
                let mut vals = Vec::new();
                for (c, v) in values.iter().enumerate() {
                    let Some(v) = v else { continue };
                    cols.push(ident(column(c)?));
                    vals.push(edited(c, v)?);
                }
                for &k in &t.key {
                    if values.get(k).is_none_or(|v| v.is_none()) {
                        return Err(format!(
                            "a new row needs every primary key column ({} is empty)",
                            column(k)?
                        ));
                    }
                }
                inserts.push(format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    t.table,
                    cols.join(", "),
                    vals.join(", ")
                ));
            }
            Change::DeleteRow { row } => deletes.push(*row),
            other => return Err(format!("{other:?} is not a table change")),
        }
    }
    for (row, set) in updates {
        if deletes.contains(&row) {
            continue;
        }
        let assignments = set
            .iter()
            .map(|(c, v)| Ok(format!("{} = {v}", ident(column(*c)?))))
            .collect::<Result<Vec<_>, String>>()?
            .join(", ");
        out.push(Planned {
            sql: format!(
                "UPDATE {} SET {assignments} WHERE {}",
                t.table,
                where_key(row)?
            ),
            expect_one: false,
        });
    }
    out.extend(inserts.into_iter().map(|sql| Planned {
        sql,
        expect_one: false,
    }));
    for row in deletes {
        out.push(Planned {
            sql: format!("DELETE FROM {} WHERE {}", t.table, where_key(row)?),
            expect_one: false,
        });
    }
    Ok(out)
}

/// Object members in key order, whether or not `serde_json` preserves the
/// input order (its `preserve_order` feature is unified across the
/// workspace).
fn sorted(obj: &serde_json::Map<String, serde_json::Value>) -> Vec<(&String, &serde_json::Value)> {
    let mut v: Vec<_> = obj.iter().collect();
    v.sort_by(|a, b| a.0.cmp(b.0));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_and_literals() {
        assert_eq!(
            parse_type("map<text, frozen<list<int>>>"),
            CqlType::Map(
                Box::new(CqlType::Simple("text".into())),
                Box::new(CqlType::List(Box::new(CqlType::Simple("int".into()))))
            )
        );
        assert_eq!(
            parse_type("frozen<address>"),
            CqlType::Udt("address".into())
        );
        assert_eq!(
            literal("text", &Value::Text("it's".into())).unwrap(),
            "'it''s'"
        );
        assert_eq!(literal("int", &Value::Int(5)).unwrap(), "5");
        assert!(literal("int", &Value::Text("x".into())).is_err());
        assert_eq!(
            literal(
                "uuid",
                &Value::Text("123e4567-e89b-12d3-a456-426614174000".into())
            )
            .unwrap(),
            "123e4567-e89b-12d3-a456-426614174000"
        );
        assert!(literal("uuid", &Value::Text("1; DROP".into())).is_err());
        assert_eq!(
            literal("blob", &Value::Bytes(vec![1, 255])).unwrap(),
            "0x01ff"
        );
        assert_eq!(literal("date", &Value::Date(1)).unwrap(), "'1970-01-02'");
        assert_eq!(
            literal(
                "map<int, text>",
                &Value::Text(r#"{"1":"a","2":"b"}"#.into())
            )
            .unwrap(),
            "{1: 'a', 2: 'b'}"
        );
        assert_eq!(
            literal("set<text>", &Value::Text(r#"["x","y"]"#.into())).unwrap(),
            "{'x', 'y'}"
        );
        assert_eq!(
            literal("tuple<int, text>", &Value::Text(r#"[1,"a"]"#.into())).unwrap(),
            "(1, 'a')"
        );
        assert_eq!(
            literal(
                "frozen<address>",
                &Value::Text(r#"{"city":"Kraków","Zip":1}"#.into())
            )
            .unwrap(),
            "{\"Zip\": 1, city: 'Kraków'}"
        );
        assert_eq!(
            literal("duration", &Value::Text("1h30m".into())).unwrap(),
            "1h30m"
        );
        assert!(literal("duration", &Value::Text("1h; x".into())).is_err());
    }

    #[test]
    fn plans_by_full_primary_key() {
        let t = CqlTable {
            table: "ks.events".into(),
            columns: vec![
                Some("sensor".into()),
                Some("ts".into()),
                Some("v".into()),
                None,
            ],
            types: vec!["int".into(), "int".into(), "text".into(), "int".into()],
            key: vec![0, 1],
        };
        let orig = |r: usize, c: usize| match (r, c) {
            (0, 0) => Some(Value::Int(1)),
            (0, 1) => Some(Value::Int(10)),
            (1, 0) => Some(Value::Int(2)),
            (1, 1) => Some(Value::Int(20)),
            _ => None,
        };
        let changes = ChangeSet {
            changes: vec![
                Change::SetCell {
                    row: 0,
                    column: 2,
                    value: "\"new\"".into(),
                },
                Change::InsertRow {
                    values: vec![
                        Some("3".into()),
                        Some("30".into()),
                        Some("\"x\"".into()),
                        None,
                    ],
                },
                Change::DeleteRow { row: 1 },
            ],
        };
        let p: Vec<String> = plan(&t, &changes, orig)
            .unwrap()
            .into_iter()
            .map(|p| p.sql)
            .collect();
        assert_eq!(
            p,
            vec![
                "UPDATE ks.events SET v = 'new' WHERE sensor = 1 AND ts = 10",
                "INSERT INTO ks.events (sensor, ts, v) VALUES (3, 30, 'x')",
                "DELETE FROM ks.events WHERE sensor = 2 AND ts = 20",
            ]
        );
        let key_edit = ChangeSet {
            changes: vec![Change::SetCell {
                row: 0,
                column: 1,
                value: "11".into(),
            }],
        };
        assert!(
            plan(&t, &key_edit, orig)
                .unwrap_err()
                .contains("primary key")
        );
        let partial = ChangeSet {
            changes: vec![Change::InsertRow {
                values: vec![Some("3".into()), None, None, None],
            }],
        };
        assert!(plan(&t, &partial, orig).unwrap_err().contains("ts"));
        let no_key = CqlTable {
            key: vec![],
            ..t.clone()
        };
        assert!(plan(&no_key, &changes, orig).is_err());
    }
}
