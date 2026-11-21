//! CQL types → Arrow.
//!
//! Scalars map to their Arrow counterparts (`tinyint`/`smallint` →
//! `Int16`, `int` → `Int32`, `bigint`/`counter` → `Int64`, `float` /
//! `double`, `boolean`, `blob` → `Binary`, `date` → `Date32`, `time` →
//! `Time64(µs)`, `timestamp` → `Timestamp(µs, UTC)`). Values without a
//! fixed Arrow representation are text: `decimal` and `varint` (arbitrary
//! precision and per-value scale: an Arrow decimal column needs one
//! scale), `uuid` / `timeuuid` / `inet` (canonical text, editable), and
//! `duration` (CQL literal `1mo2d3h4m5s6ms7us8ns`).
//!
//! Collections, tuples, UDTs and vectors are JSON text (`[1, 2]`,
//! `{"k": "v"}`, `{"field": 1}`): one representation the grid, the value
//! editor, export and the CQL edit generator all understand, whatever the
//! nesting (Arrow nested types would need nested views everywhere). The
//! column keeps the CQL type (`list<int>`, `frozen<address>`) in its
//! metadata, so edits are turned back into CQL literals of that type.

use std::net::IpAddr;

use scylla::{
    frame::response::result::{CollectionType, ColumnType, NativeType},
    value::{CqlDecimal, CqlDuration, CqlValue, CqlVarint},
};
use serde_json::Value as J;

use crate::columns::{self, Cell, ColumnSpec};

/// The CQL spelling of a column type.
pub fn type_name(t: &ColumnType<'_>) -> String {
    match t {
        ColumnType::Native(n) => native_name(n).to_owned(),
        ColumnType::Collection { frozen, typ } => {
            let inner = match typ {
                CollectionType::List(e) => format!("list<{}>", type_name(e)),
                CollectionType::Set(e) => format!("set<{}>", type_name(e)),
                CollectionType::Map(k, v) => format!("map<{}, {}>", type_name(k), type_name(v)),
                _ => "collection".to_owned(),
            };
            if *frozen {
                format!("frozen<{inner}>")
            } else {
                inner
            }
        }
        ColumnType::Vector { typ, dimensions } => {
            format!("vector<{}, {dimensions}>", type_name(typ))
        }
        ColumnType::UserDefinedType { frozen, definition } => {
            if *frozen {
                format!("frozen<{}>", definition.name)
            } else {
                definition.name.to_string()
            }
        }
        ColumnType::Tuple(ts) => format!(
            "tuple<{}>",
            ts.iter().map(type_name).collect::<Vec<_>>().join(", ")
        ),
        _ => "unknown".to_owned(),
    }
}

fn native_name(n: &NativeType) -> &'static str {
    match n {
        NativeType::Ascii => "ascii",
        NativeType::Boolean => "boolean",
        NativeType::Blob => "blob",
        NativeType::Counter => "counter",
        NativeType::Date => "date",
        NativeType::Decimal => "decimal",
        NativeType::Double => "double",
        NativeType::Duration => "duration",
        NativeType::Float => "float",
        NativeType::Int => "int",
        NativeType::BigInt => "bigint",
        NativeType::Text => "text",
        NativeType::Timestamp => "timestamp",
        NativeType::Inet => "inet",
        NativeType::SmallInt => "smallint",
        NativeType::TinyInt => "tinyint",
        NativeType::Time => "time",
        NativeType::Timeuuid => "timeuuid",
        NativeType::Uuid => "uuid",
        NativeType::Varint => "varint",
        _ => "unknown",
    }
}

/// The Arrow column of a CQL type.
pub fn column(name: &str, t: &ColumnType<'_>) -> ColumnSpec {
    use columns::ColumnType as C;
    let ty = match t {
        ColumnType::Native(n) => match n {
            NativeType::Boolean => C::Bool,
            NativeType::TinyInt | NativeType::SmallInt => C::Int16,
            NativeType::Int => C::Int32,
            NativeType::BigInt | NativeType::Counter => C::Int64,
            NativeType::Float => C::Float32,
            NativeType::Double => C::Float64,
            NativeType::Blob => C::Binary,
            NativeType::Date => C::Date,
            NativeType::Time => C::Time,
            NativeType::Timestamp => C::Timestamp { tz: true },
            _ => C::Text,
        },
        _ => C::Text,
    };
    ColumnSpec::new(name, ty, type_name(t))
}

/// Days between -5877641-06-23 (CQL date 0) and 1970-01-01.
const DATE_EPOCH: i64 = 1 << 31;

/// `decimal` as text (`unscaled × 10^-scale`).
pub fn decimal_text(d: &CqlDecimal) -> String {
    let (bytes, scale) = d.as_signed_be_bytes_slice_and_exponent();
    let unscaled = varint_text(bytes);
    if scale <= 0 {
        if unscaled == "0" {
            return unscaled;
        }
        return format!("{unscaled}{}", "0".repeat((-scale) as usize));
    }
    let (neg, digits) = match unscaled.strip_prefix('-') {
        Some(d) => (true, d.to_owned()),
        None => (false, unscaled),
    };
    let scale = scale as usize;
    let digits = if digits.len() <= scale {
        format!("{}{digits}", "0".repeat(scale - digits.len() + 1))
    } else {
        digits
    };
    let (int, frac) = digits.split_at(digits.len() - scale);
    format!("{}{int}.{frac}", if neg { "-" } else { "" })
}

/// Two's-complement big-endian bytes as a decimal integer.
pub fn varint_text(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "0".into();
    }
    if bytes.len() <= 16 {
        let mut v: i128 = if bytes[0] & 0x80 != 0 { -1 } else { 0 };
        for b in bytes {
            v = (v << 8) | i128::from(*b);
        }
        return v.to_string();
    }
    // Long values: magnitude by repeated division (base 10^9).
    let neg = bytes[0] & 0x80 != 0;
    let mut mag: Vec<u8> = if neg {
        // Two's complement negation.
        let mut m: Vec<u8> = bytes.iter().map(|b| !b).collect();
        for b in m.iter_mut().rev() {
            let (r, carry) = b.overflowing_add(1);
            *b = r;
            if !carry {
                break;
            }
        }
        m
    } else {
        bytes.to_vec()
    };
    let mut parts = Vec::new();
    while mag.iter().any(|b| *b != 0) {
        let mut rem: u64 = 0;
        for b in mag.iter_mut() {
            let cur = (rem << 8) | u64::from(*b);
            *b = (cur / 1_000_000_000) as u8;
            rem = cur % 1_000_000_000;
        }
        parts.push(rem);
    }
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    let mut it = parts.iter().rev();
    if let Some(first) = it.next() {
        s.push_str(&first.to_string());
    }
    for p in it {
        s.push_str(&format!("{p:09}"));
    }
    s
}

fn varint(v: &CqlVarint) -> String {
    varint_text(v.as_signed_bytes_be_slice())
}

/// `duration` as a CQL literal.
pub fn duration_text(d: &CqlDuration) -> String {
    let mut s = String::new();
    let neg = d.months < 0 || d.days < 0 || d.nanoseconds < 0;
    if neg {
        s.push('-');
    }
    let (months, days, mut ns) = (
        d.months.unsigned_abs(),
        d.days.unsigned_abs(),
        d.nanoseconds.unsigned_abs(),
    );
    if months >= 12 {
        s.push_str(&format!("{}y", months / 12));
    }
    if months % 12 > 0 {
        s.push_str(&format!("{}mo", months % 12));
    }
    if days > 0 {
        s.push_str(&format!("{days}d"));
    }
    for (unit, n) in [
        ("h", 3_600_000_000_000u64),
        ("m", 60_000_000_000),
        ("s", 1_000_000_000),
        ("ms", 1_000_000),
        ("us", 1_000),
        ("ns", 1),
    ] {
        if ns >= n {
            s.push_str(&format!("{}{unit}", ns / n));
            ns %= n;
        }
    }
    if s.is_empty() || s == "-" {
        "0s".into()
    } else {
        s
    }
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(2 + b.len() * 2);
    s.push_str("0x");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// A value as JSON (nested values, JSON text columns).
pub fn json(v: &CqlValue) -> J {
    match v {
        CqlValue::Ascii(s) | CqlValue::Text(s) => J::String(s.clone()),
        CqlValue::Boolean(b) => J::Bool(*b),
        CqlValue::Blob(b) => J::String(hex(b)),
        CqlValue::Counter(c) => J::from(c.0),
        CqlValue::Decimal(d) => J::String(decimal_text(d)),
        CqlValue::Date(d) => J::String(columns::format_date((i64::from(d.0) - DATE_EPOCH) as i32)),
        CqlValue::Double(d) => serde_json::Number::from_f64(*d)
            .map(J::Number)
            .unwrap_or_else(|| J::String(d.to_string())),
        CqlValue::Float(f) => serde_json::Number::from_f64(f64::from(*f))
            .map(J::Number)
            .unwrap_or_else(|| J::String(f.to_string())),
        CqlValue::Duration(d) => J::String(duration_text(d)),
        CqlValue::Empty => J::Null,
        CqlValue::Int(i) => J::from(*i),
        CqlValue::BigInt(i) => J::from(*i),
        CqlValue::SmallInt(i) => J::from(*i),
        CqlValue::TinyInt(i) => J::from(*i),
        CqlValue::Timestamp(t) => J::String(columns::format_timestamp(t.0.saturating_mul(1000))),
        CqlValue::Time(t) => J::String(columns::format_time(t.0 / 1000)),
        CqlValue::Inet(ip) => J::String(inet(ip)),
        CqlValue::Uuid(u) => J::String(u.to_string()),
        CqlValue::Timeuuid(u) => J::String(u.to_string()),
        CqlValue::Varint(v) => J::String(varint(v)),
        CqlValue::List(l) | CqlValue::Set(l) | CqlValue::Vector(l) => {
            J::Array(l.iter().map(json).collect())
        }
        CqlValue::Tuple(t) => J::Array(
            t.iter()
                .map(|x| x.as_ref().map(json).unwrap_or(J::Null))
                .collect(),
        ),
        CqlValue::Map(m) => {
            // JSON keys are strings; non-text keys use their text form.
            let mut obj = serde_json::Map::new();
            for (k, v) in m {
                let key = match json(k) {
                    J::String(s) => s,
                    other => other.to_string(),
                };
                obj.insert(key, json(v));
            }
            J::Object(obj)
        }
        CqlValue::UserDefinedType { fields, .. } => J::Object(
            fields
                .iter()
                .map(|(n, v)| (n.clone(), v.as_ref().map(json).unwrap_or(J::Null)))
                .collect(),
        ),
        _ => J::Null,
    }
}

fn inet(ip: &IpAddr) -> String {
    ip.to_string()
}

/// Pushes one value into column `col` of `b` (see the module docs).
pub fn push(b: &mut columns::BatchBuilder, col: usize, v: Option<&CqlValue>) {
    let Some(v) = v else {
        b.push(col, Cell::Null);
        return;
    };
    match v {
        CqlValue::Boolean(x) => b.push(col, Cell::Bool(*x)),
        CqlValue::TinyInt(x) => b.push(col, Cell::Int(i64::from(*x))),
        CqlValue::SmallInt(x) => b.push(col, Cell::Int(i64::from(*x))),
        CqlValue::Int(x) => b.push(col, Cell::Int(i64::from(*x))),
        CqlValue::BigInt(x) => b.push(col, Cell::Int(*x)),
        CqlValue::Counter(x) => b.push(col, Cell::Int(x.0)),
        CqlValue::Float(x) => b.push(col, Cell::Float(f64::from(*x))),
        CqlValue::Double(x) => b.push(col, Cell::Float(*x)),
        CqlValue::Blob(x) => b.push(col, Cell::Bytes(x)),
        CqlValue::Date(d) => b.push(col, Cell::Date((i64::from(d.0) - DATE_EPOCH) as i32)),
        CqlValue::Time(t) => b.push(col, Cell::Time(t.0 / 1000)),
        CqlValue::Timestamp(t) => b.push(col, Cell::Timestamp(t.0.saturating_mul(1000))),
        CqlValue::Ascii(s) | CqlValue::Text(s) => b.push(col, Cell::Text(s)),
        CqlValue::Empty => b.push(col, Cell::Null),
        other => {
            let text = match other {
                CqlValue::Decimal(d) => decimal_text(d),
                CqlValue::Varint(v) => varint(v),
                CqlValue::Duration(d) => duration_text(d),
                CqlValue::Inet(ip) => inet(ip),
                CqlValue::Uuid(u) => u.to_string(),
                CqlValue::Timeuuid(u) => u.to_string(),
                nested => json(nested).to_string(),
            };
            b.push(col, Cell::Text(&text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(varint_text(&[0x00, 0xff]), "255");
        assert_eq!(varint_text(&[0xff]), "-1");
        assert_eq!(varint_text(&[0x80, 0x00]), "-32768");
        let big = {
            let mut b = vec![0x01];
            b.extend([0u8; 20]);
            b
        };
        // 2^160
        assert_eq!(
            varint_text(&big),
            "1461501637330902918203684832716283019655932542976"
        );
        let mut neg = vec![0xff];
        neg.extend([0u8; 20]);
        // -(2^160)
        assert_eq!(
            varint_text(&neg),
            "-1461501637330902918203684832716283019655932542976"
        );
        let d = CqlDecimal::from_signed_be_bytes_slice_and_exponent(&[0x04, 0xd2], 2);
        assert_eq!(decimal_text(&d), "12.34");
        let d = CqlDecimal::from_signed_be_bytes_slice_and_exponent(&[0xfb, 0x2e], 4);
        assert_eq!(decimal_text(&d), "-0.1234");
        let d = CqlDecimal::from_signed_be_bytes_slice_and_exponent(&[0x05], -2);
        assert_eq!(decimal_text(&d), "500");
    }

    #[test]
    fn durations_and_json() {
        let d = CqlDuration {
            months: 14,
            days: 3,
            nanoseconds: 3_723_000_000_001,
        };
        assert_eq!(duration_text(&d), "1y2mo3d1h2m3s1ns");
        assert_eq!(
            duration_text(&CqlDuration {
                months: 0,
                days: 0,
                nanoseconds: 0
            }),
            "0s"
        );
        let v = CqlValue::Map(vec![
            (CqlValue::Int(1), CqlValue::Text("a".into())),
            (
                CqlValue::Int(2),
                CqlValue::List(vec![CqlValue::Boolean(true)]),
            ),
        ]);
        assert_eq!(json(&v).to_string(), r#"{"1":"a","2":[true]}"#);
        let u = CqlValue::UserDefinedType {
            keyspace: "ks".into(),
            name: "addr".into(),
            fields: vec![
                ("city".into(), Some(CqlValue::Text("Kraków".into()))),
                ("zip".into(), None),
            ],
        };
        assert_eq!(json(&u).to_string(), r#"{"city":"Kraków","zip":null}"#);
        assert_eq!(
            json(&CqlValue::Date(scylla::value::CqlDate((1 << 31) + 1))).to_string(),
            "\"1970-01-02\""
        );
    }
}
