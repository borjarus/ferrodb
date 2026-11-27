//! A typed scalar shared by editing, export, import and copying: read from
//! Arrow cells and BSON values, converted to the type of a target column,
//! written as text, JSON, BSON or an SQL literal.

use std::fmt::Write as _;

use arrow_array::{Array, cast::AsArray, types::*};
use arrow_schema::{DataType, TimeUnit};
use bson::Bson;
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

const EPOCH_DAYS_FROM_CE: i32 = 719_163;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    /// Exact decimal as written (`-12.50`).
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    /// Days since 1970-01-01.
    Date(i32),
    /// Microseconds since midnight.
    Time(i64),
    /// Microseconds since the Unix epoch; `tz` = an instant (UTC).
    Timestamp {
        micros: i64,
        tz: bool,
    },
    /// Nested data: documents, arrays, JSON columns.
    Json(serde_json::Value),
    /// BSON values without an SQL counterpart (ObjectId, regex, MinKey…);
    /// kept as they are between document sources.
    Bson(Bson),
}

/// Type a value is converted to: the type of a target column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    Bool,
    Int,
    Float,
    Decimal,
    Text,
    Bytes,
    Date,
    Time,
    Timestamp {
        tz: bool,
    },
    Json,
    /// Keep whatever comes (documents, unknown column types).
    Any,
}

impl ValueType {
    pub fn from_arrow(t: &DataType) -> Self {
        match t {
            DataType::Boolean => Self::Bool,
            DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64 => Self::Int,
            DataType::Float16 | DataType::Float32 | DataType::Float64 => Self::Float,
            DataType::Decimal32(..)
            | DataType::Decimal64(..)
            | DataType::Decimal128(..)
            | DataType::Decimal256(..) => Self::Decimal,
            DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => Self::Text,
            DataType::Binary | DataType::LargeBinary | DataType::BinaryView => Self::Bytes,
            DataType::Date32 | DataType::Date64 => Self::Date,
            DataType::Time32(_) | DataType::Time64(_) => Self::Time,
            DataType::Timestamp(_, tz) => Self::Timestamp { tz: tz.is_some() },
            DataType::Struct(_)
            | DataType::List(_)
            | DataType::LargeList(_)
            | DataType::Map(..) => Self::Json,
            _ => Self::Any,
        }
    }

    /// From a database column type name (`integer`, `varchar(20)`,
    /// `timestamp with time zone`, `jsonb`, `tinyint(1)`…).
    pub fn from_sql(type_name: &str) -> Self {
        let t = type_name.trim().to_ascii_lowercase();
        let base = t.split('(').next().unwrap_or("").trim();
        let base = base.trim_end_matches("[]");
        if t.ends_with("[]") {
            return Self::Text;
        }
        match base {
            "bool" | "boolean" | "bit" => Self::Bool,
            "smallint" | "int2" | "integer" | "int" | "int4" | "bigint" | "int8" | "tinyint"
            | "mediumint" | "serial" | "bigserial" | "smallserial" | "year" => Self::Int,
            "real" | "float4" | "float8" | "double" | "double precision" | "float" => Self::Float,
            "numeric" | "decimal" | "money" | "number" => Self::Decimal,
            "bytea" | "blob" | "binary" | "varbinary" | "tinyblob" | "mediumblob" | "longblob" => {
                Self::Bytes
            }
            "date" => Self::Date,
            "time" | "time without time zone" | "timetz" | "time with time zone" => Self::Time,
            "timestamptz" | "timestamp with time zone" => Self::Timestamp { tz: true },
            "timestamp" | "timestamp without time zone" | "datetime" => {
                Self::Timestamp { tz: false }
            }
            "json" | "jsonb" => Self::Json,
            _ if base.starts_with("int") && base.contains("unsigned") => Self::Int,
            _ if base.contains("int") && !base.contains("interval") && !base.contains("point") => {
                // SQLite affinity: any type containing "INT".
                Self::Int
            }
            _ if base.contains("char") || base.contains("text") || base.contains("clob") => {
                Self::Text
            }
            _ if base.contains("real") || base.contains("floa") || base.contains("doub") => {
                Self::Float
            }
            _ => Self::Text,
        }
    }

    /// Lower-case name shown in import previews.
    pub fn label(self) -> &'static str {
        match self {
            Self::Bool => "boolean",
            Self::Int => "integer",
            Self::Float => "float",
            Self::Decimal => "decimal",
            Self::Text => "text",
            Self::Bytes => "binary",
            Self::Date => "date",
            Self::Time => "time",
            Self::Timestamp { tz: true } => "timestamptz",
            Self::Timestamp { tz: false } => "timestamp",
            Self::Json => "json",
            Self::Any => "any",
        }
    }
}

// ------------------------------------------------------------------ Arrow

/// The value of one Arrow cell.
pub fn value_at(array: &dyn Array, row: usize) -> Value {
    if array.is_null(row) {
        return Value::Null;
    }
    match array.data_type() {
        DataType::Null => Value::Null,
        DataType::Boolean => Value::Bool(array.as_boolean().value(row)),
        DataType::Int8 => Value::Int(array.as_primitive::<Int8Type>().value(row).into()),
        DataType::Int16 => Value::Int(array.as_primitive::<Int16Type>().value(row).into()),
        DataType::Int32 => Value::Int(array.as_primitive::<Int32Type>().value(row).into()),
        DataType::Int64 => Value::Int(array.as_primitive::<Int64Type>().value(row)),
        DataType::UInt8 => Value::Int(array.as_primitive::<UInt8Type>().value(row).into()),
        DataType::UInt16 => Value::Int(array.as_primitive::<UInt16Type>().value(row).into()),
        DataType::UInt32 => Value::Int(array.as_primitive::<UInt32Type>().value(row).into()),
        DataType::UInt64 => {
            let v = array.as_primitive::<UInt64Type>().value(row);
            i64::try_from(v).map_or(Value::UInt(v), Value::Int)
        }
        DataType::Float16 => Value::Float(f64::from(
            array.as_primitive::<Float16Type>().value(row).to_f32(),
        )),
        DataType::Float32 => {
            Value::Float(f64::from(array.as_primitive::<Float32Type>().value(row)))
        }
        DataType::Float64 => Value::Float(array.as_primitive::<Float64Type>().value(row)),
        DataType::Decimal128(_, scale) => Value::Decimal(format_decimal(
            array.as_primitive::<Decimal128Type>().value(row),
            *scale,
        )),
        DataType::Decimal64(_, scale) => Value::Decimal(format_decimal(
            array.as_primitive::<Decimal64Type>().value(row).into(),
            *scale,
        )),
        DataType::Decimal32(_, scale) => Value::Decimal(format_decimal(
            array.as_primitive::<Decimal32Type>().value(row).into(),
            *scale,
        )),
        DataType::Utf8 => Value::Text(array.as_string::<i32>().value(row).to_owned()),
        DataType::LargeUtf8 => Value::Text(array.as_string::<i64>().value(row).to_owned()),
        DataType::Utf8View => Value::Text(array.as_string_view().value(row).to_owned()),
        DataType::Binary => Value::Bytes(array.as_binary::<i32>().value(row).to_vec()),
        DataType::LargeBinary => Value::Bytes(array.as_binary::<i64>().value(row).to_vec()),
        DataType::BinaryView => Value::Bytes(array.as_binary_view().value(row).to_vec()),
        DataType::Date32 => Value::Date(array.as_primitive::<Date32Type>().value(row)),
        DataType::Date64 => {
            Value::Date((array.as_primitive::<Date64Type>().value(row) / 86_400_000) as i32)
        }
        DataType::Time32(TimeUnit::Second) => {
            Value::Time(i64::from(array.as_primitive::<Time32SecondType>().value(row)) * 1_000_000)
        }
        DataType::Time32(_) => {
            Value::Time(i64::from(array.as_primitive::<Time32MillisecondType>().value(row)) * 1_000)
        }
        DataType::Time64(TimeUnit::Nanosecond) => {
            Value::Time(array.as_primitive::<Time64NanosecondType>().value(row) / 1_000)
        }
        DataType::Time64(_) => {
            Value::Time(array.as_primitive::<Time64MicrosecondType>().value(row))
        }
        DataType::Timestamp(unit, tz) => {
            let micros = match unit {
                TimeUnit::Second => array
                    .as_primitive::<TimestampSecondType>()
                    .value(row)
                    .saturating_mul(1_000_000),
                TimeUnit::Millisecond => array
                    .as_primitive::<TimestampMillisecondType>()
                    .value(row)
                    .saturating_mul(1_000),
                TimeUnit::Microsecond => {
                    array.as_primitive::<TimestampMicrosecondType>().value(row)
                }
                TimeUnit::Nanosecond => {
                    array.as_primitive::<TimestampNanosecondType>().value(row) / 1_000
                }
            };
            Value::Timestamp {
                micros,
                tz: tz.is_some(),
            }
        }
        other => Value::Text(format!("<{other}>")),
    }
}

/// `12345` with scale 2 → `123.45`.
pub fn format_decimal(v: i128, scale: i8) -> String {
    if scale <= 0 {
        let mut s = v.to_string();
        for _ in 0..(-scale) {
            s.push('0');
        }
        return s;
    }
    let neg = v < 0;
    let digits = v.unsigned_abs().to_string();
    let scale = scale as usize;
    let (int, frac) = if digits.len() > scale {
        let (a, b) = digits.split_at(digits.len() - scale);
        (a.to_owned(), b.to_owned())
    } else {
        ("0".to_owned(), format!("{digits:0>scale$}"))
    };
    format!("{}{int}.{frac}", if neg { "-" } else { "" })
}

// ------------------------------------------------------------------ text

pub fn format_date(days: i32) -> String {
    days.checked_add(EPOCH_DAYS_FROM_CE)
        .and_then(NaiveDate::from_num_days_from_ce_opt)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| days.to_string())
}

pub fn format_time(micros: i64) -> String {
    // Outside a day (negative values used to wrap and overflow below).
    if !(0..86_400_000_000).contains(&micros) {
        return micros.to_string();
    }
    let secs = (micros / 1_000_000) as u32;
    let frac = (micros % 1_000_000) as u32;
    NaiveTime::from_num_seconds_from_midnight_opt(secs, frac * 1000)
        .map(|t| {
            if frac == 0 {
                t.format("%H:%M:%S").to_string()
            } else {
                t.format("%H:%M:%S%.6f").to_string()
            }
        })
        .unwrap_or_else(|| micros.to_string())
}

/// `2024-01-02 03:04:05[.123456]`; with `tz`, `+00:00` is appended.
pub fn format_timestamp(micros: i64, tz: bool) -> String {
    match DateTime::from_timestamp_micros(micros) {
        Some(dt) => {
            let base = if dt.nanosecond() == 0 {
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            } else {
                dt.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
            };
            if tz { format!("{base}+00:00") } else { base }
        }
        None => micros.to_string(),
    }
}

pub fn parse_date(t: &str) -> Option<i32> {
    let d = NaiveDate::parse_from_str(t.trim(), "%Y-%m-%d").ok()?;
    Some(d.num_days_from_ce() - EPOCH_DAYS_FROM_CE)
}

pub fn parse_time(t: &str) -> Option<i64> {
    let t = t.trim();
    let tm = NaiveTime::parse_from_str(t, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(t, "%H:%M"))
        .ok()?;
    Some(i64::from(tm.num_seconds_from_midnight()) * 1_000_000 + i64::from(tm.nanosecond() / 1000))
}

/// ISO 8601 / SQL timestamps; an offset or `Z` is converted to UTC.
pub fn parse_timestamp(t: &str) -> Option<i64> {
    let t = t.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        return Some(dt.timestamp_micros());
    }
    for f in ["%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f%#z"] {
        if let Ok(dt) = DateTime::parse_from_str(t, f) {
            return Some(dt.timestamp_micros());
        }
    }
    for f in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(t, f) {
            return Some(dt.and_utc().timestamp_micros());
        }
    }
    parse_date(t).map(|d| i64::from(d) * 86_400_000_000)
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    let s = s
        .strip_prefix("\\x")
        .or_else(|| s.strip_prefix("0x"))
        .unwrap_or(s);
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn is_decimal(t: &str) -> bool {
    let t = t.strip_prefix(['-', '+']).unwrap_or(t);
    let mut parts = t.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next().unwrap_or("");
    !(int.is_empty() && frac.is_empty())
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Text form used by CSV, Markdown, HTML and previews; `None` = NULL.
    pub fn to_text(&self) -> Option<String> {
        Some(match self {
            Self::Null => return None,
            Self::Bool(b) => b.to_string(),
            Self::Int(i) => i.to_string(),
            Self::UInt(u) => u.to_string(),
            Self::Float(f) => float_text(*f),
            Self::Decimal(d) => d.clone(),
            Self::Text(s) => s.clone(),
            Self::Bytes(b) => format!("\\x{}", hex(b)),
            Self::Date(d) => format_date(*d),
            Self::Time(t) => format_time(*t),
            Self::Timestamp { micros, tz } => format_timestamp(*micros, *tz),
            Self::Json(j) => j.to_string(),
            Self::Bson(b) => match b {
                Bson::ObjectId(o) => o.to_hex(),
                other => other.clone().into_relaxed_extjson().to_string(),
            },
        })
    }

    /// Relaxed JSON (exact decimals are written as numbers).
    pub fn write_json(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Int(i) => {
                let _ = write!(out, "{i}");
            }
            Self::UInt(u) => {
                let _ = write!(out, "{u}");
            }
            Self::Float(f) if f.is_finite() => out.push_str(&float_text(*f)),
            Self::Float(_) => out.push_str("null"),
            Self::Decimal(d) if is_decimal(d) => out.push_str(d),
            Self::Json(j) => out.push_str(&j.to_string()),
            Self::Bson(b) => out.push_str(&b.clone().into_relaxed_extjson().to_string()),
            other => {
                let s = other.to_text().unwrap_or_default();
                out.push_str(&serde_json::Value::String(s).to_string());
            }
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut s = String::new();
        self.write_json(&mut s);
        serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)
    }

    /// The BSON value with the closest type (dates become UTC dates at
    /// millisecond precision, decimals Decimal128).
    pub fn to_bson(&self) -> Bson {
        match self {
            Self::Null => Bson::Null,
            Self::Bool(b) => Bson::Boolean(*b),
            Self::Int(i) => match i32::try_from(*i) {
                Ok(v) => Bson::Int32(v),
                Err(_) => Bson::Int64(*i),
            },
            Self::UInt(u) => i64::try_from(*u).map_or(Bson::Double(*u as f64), Bson::Int64),
            Self::Float(f) => Bson::Double(*f),
            Self::Decimal(d) => d
                .parse::<bson::Decimal128>()
                .map(Bson::Decimal128)
                .unwrap_or_else(|_| Bson::String(d.clone())),
            Self::Text(s) => Bson::String(s.clone()),
            Self::Bytes(b) => Bson::Binary(bson::Binary {
                subtype: bson::spec::BinarySubtype::Generic,
                bytes: b.clone(),
            }),
            Self::Date(d) => {
                Bson::DateTime(bson::DateTime::from_millis(i64::from(*d) * 86_400_000))
            }
            Self::Time(t) => Bson::String(format_time(*t)),
            Self::Timestamp { micros, .. } => {
                Bson::DateTime(bson::DateTime::from_millis(micros.div_euclid(1000)))
            }
            Self::Json(j) => {
                Bson::try_from(j.clone()).unwrap_or_else(|_| Bson::String(j.to_string()))
            }
            Self::Bson(b) => b.clone(),
        }
    }

    pub fn from_bson(b: &Bson) -> Value {
        match b {
            Bson::Null | Bson::Undefined => Value::Null,
            Bson::Boolean(v) => Value::Bool(*v),
            Bson::Int32(v) => Value::Int((*v).into()),
            Bson::Int64(v) => Value::Int(*v),
            Bson::Double(v) => Value::Float(*v),
            Bson::Decimal128(d) => Value::Decimal(d.to_string()),
            Bson::String(s) => Value::Text(s.clone()),
            Bson::Binary(bin) => Value::Bytes(bin.bytes.clone()),
            // Dates beyond ±292k years do not fit in microseconds: kept as
            // BSON (the multiplication used to overflow).
            Bson::DateTime(dt) => match dt.timestamp_millis().checked_mul(1000) {
                Some(micros) => Value::Timestamp { micros, tz: true },
                None => Value::Bson(b.clone()),
            },
            Bson::Document(_) | Bson::Array(_) => Value::Json(b.clone().into_relaxed_extjson()),
            other => Value::Bson(other.clone()),
        }
    }

    /// A cell edited in the grid: JSON (`null`, a number, `true`, a
    /// string) as produced by the UI, typed by the column.
    pub fn from_cell_json(json: &str, ty: ValueType) -> Result<Value, String> {
        let v: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("value {json}: {e}"))?;
        let raw = match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => match n.as_u64() {
                    Some(u) => Value::UInt(u),
                    None => Value::Decimal(n.to_string()),
                },
            },
            serde_json::Value::String(s) => Value::Text(s),
            other => Value::Json(other),
        };
        raw.convert(ty)
    }

    /// Converts to `ty` (import mapping, edited cells, copies).
    pub fn convert(self, ty: ValueType) -> Result<Value, String> {
        use ValueType as T;
        let bad = |v: &Value| format!("cannot convert {} to {}", v.describe(), ty.label());
        Ok(match (self, ty) {
            (Value::Null, _) => Value::Null,
            (v, T::Any) => v,
            // Text: parse.
            (Value::Text(s), T::Text) => Value::Text(s),
            (Value::Text(s), t) => {
                let tr = s.trim();
                match t {
                    T::Bool => match tr.to_ascii_lowercase().as_str() {
                        "true" | "t" | "1" | "yes" | "y" => Value::Bool(true),
                        "false" | "f" | "0" | "no" | "n" => Value::Bool(false),
                        _ => return Err(bad(&Value::Text(s))),
                    },
                    T::Int => match tr.parse::<i64>() {
                        Ok(i) => Value::Int(i),
                        Err(_) => match tr.parse::<f64>() {
                            Ok(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Value::Int(f as i64),
                            _ => return Err(bad(&Value::Text(s))),
                        },
                    },
                    T::Float => tr
                        .parse::<f64>()
                        .map(Value::Float)
                        .map_err(|_| bad(&Value::Text(s.clone())))?,
                    T::Decimal => {
                        if is_decimal(tr) {
                            Value::Decimal(tr.to_owned())
                        } else if let Ok(f) = tr.parse::<f64>() {
                            Value::Decimal(float_text(f))
                        } else {
                            return Err(bad(&Value::Text(s)));
                        }
                    }
                    T::Bytes => unhex(tr)
                        .map(Value::Bytes)
                        .unwrap_or_else(|| Value::Bytes(s.into_bytes())),
                    T::Date => match parse_date(tr) {
                        Some(d) => Value::Date(d),
                        None => match parse_timestamp(tr) {
                            Some(m) => Value::Date(m.div_euclid(86_400_000_000) as i32),
                            None => return Err(bad(&Value::Text(s))),
                        },
                    },
                    T::Time => parse_time(tr)
                        .map(Value::Time)
                        .ok_or_else(|| bad(&Value::Text(s.clone())))?,
                    T::Timestamp { tz } => parse_timestamp(tr)
                        .map(|micros| Value::Timestamp { micros, tz })
                        .ok_or_else(|| bad(&Value::Text(s.clone())))?,
                    T::Json => serde_json::from_str(tr)
                        .map(Value::Json)
                        .unwrap_or(Value::Json(serde_json::Value::String(s))),
                    T::Text | T::Any => unreachable!("handled above"),
                }
            }
            // Same kind.
            (v @ Value::Bool(_), T::Bool)
            | (v @ Value::Int(_), T::Int)
            | (v @ Value::Float(_), T::Float)
            | (v @ Value::Decimal(_), T::Decimal)
            | (v @ Value::Bytes(_), T::Bytes)
            | (v @ Value::Date(_), T::Date)
            | (v @ Value::Time(_), T::Time)
            | (v @ Value::Json(_), T::Json) => v,
            (Value::Timestamp { micros, .. }, T::Timestamp { tz }) => {
                Value::Timestamp { micros, tz }
            }
            // Numbers.
            (Value::Bool(b), T::Int) => Value::Int(b.into()),
            (Value::Bool(b), T::Float) => Value::Float(if b { 1.0 } else { 0.0 }),
            (Value::Bool(b), T::Decimal) => Value::Decimal(if b { "1" } else { "0" }.into()),
            (Value::Int(i), T::Bool) => Value::Bool(i != 0),
            (Value::Int(i), T::Float) => Value::Float(i as f64),
            (Value::Int(i), T::Decimal) => Value::Decimal(i.to_string()),
            (Value::UInt(u), T::Int) => {
                Value::Int(i64::try_from(u).map_err(|_| bad(&Value::UInt(u)))?)
            }
            (Value::UInt(u), T::Float) => Value::Float(u as f64),
            (Value::UInt(u), T::Decimal) => Value::Decimal(u.to_string()),
            (Value::Float(f), T::Int) if f.fract() == 0.0 && f.abs() < 9.0e18 => {
                Value::Int(f as i64)
            }
            (Value::Float(f), T::Decimal) => Value::Decimal(float_text(f)),
            (Value::Decimal(d), T::Float) => {
                Value::Float(d.parse().map_err(|_| bad(&Value::Decimal(d.clone())))?)
            }
            (Value::Decimal(d), T::Int) => match d.parse::<i64>() {
                Ok(i) => Value::Int(i),
                Err(_) => match d.parse::<f64>() {
                    Ok(f) if f.fract() == 0.0 => Value::Int(f as i64),
                    _ => return Err(bad(&Value::Decimal(d))),
                },
            },
            // Dates.
            (Value::Timestamp { micros, .. }, T::Date) => {
                Value::Date(micros.div_euclid(86_400_000_000) as i32)
            }
            (Value::Date(d), T::Timestamp { tz }) => match i64::from(d).checked_mul(86_400_000_000)
            {
                Some(micros) => Value::Timestamp { micros, tz },
                None => return Err(bad(&Value::Date(d))),
            },
            (Value::Timestamp { micros, .. }, T::Time) => {
                Value::Time(micros.rem_euclid(86_400_000_000))
            }
            // JSON and BSON.
            (Value::Json(serde_json::Value::String(s)), t) => Value::Text(s).convert(t)?,
            (Value::Json(j), T::Text) => Value::Text(j.to_string()),
            (Value::Bson(b), T::Text) => Value::Text(Value::Bson(b).to_text().unwrap_or_default()),
            (Value::Bson(b), T::Json) => Value::Json(b.into_relaxed_extjson()),
            (v, T::Json) => v.to_json().pipe(Value::Json),
            // Anything to text.
            (v, T::Text) => Value::Text(v.to_text().unwrap_or_default()),
            (v, t) => match v.to_text() {
                Some(s) if !matches!(v, Value::Json(_) | Value::Bson(_)) => {
                    Value::Text(s).convert(t)?
                }
                _ => return Err(bad(&v)),
            },
        })
    }

    fn describe(&self) -> String {
        let mut s = self.to_text().unwrap_or_else(|| "NULL".into());
        if s.len() > 40 {
            let mut end = 40;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            s.push('…');
        }
        format!("`{s}`")
    }

    /// The value handed to extension scripts: numbers and booleans as they
    /// are, everything else (decimals, dates, JSON, binary…) as its text.
    pub fn to_script_value(&self) -> ferrodb_api::ScriptValue {
        use ferrodb_api::ScriptValue as S;
        match self {
            Self::Null => S::Null,
            Self::Bool(b) => S::Bool(*b),
            Self::Int(i) => S::Int(*i),
            Self::Float(f) => S::Float(*f),
            other => other.to_text().map_or(S::Null, S::Text),
        }
    }

    /// Numeric value for aggregates and charts.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(i) => Some(*i as f64),
            Self::UInt(u) => Some(*u as f64),
            Self::Float(f) => Some(*f),
            Self::Decimal(d) => d.parse().ok(),
            Self::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

/// Shortest text that reads back as the same float.
pub fn float_text(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}").trim_end_matches(".0").to_owned()
    } else {
        f.to_string()
    }
}

/// Guesses the type of text cells (CSV import): the narrowest type every
/// non-empty sample parses as.
pub fn guess_type<'a>(samples: impl Iterator<Item = &'a str>) -> ValueType {
    let mut candidates = [
        ValueType::Int,
        ValueType::Float,
        ValueType::Bool,
        ValueType::Date,
        ValueType::Timestamp { tz: false },
    ]
    .to_vec();
    let mut any = false;
    for s in samples {
        let s = s.trim();
        if s.is_empty() {
            continue;
        }
        any = true;
        candidates.retain(|t| match t {
            ValueType::Int => s.parse::<i64>().is_ok(),
            ValueType::Float => s.parse::<f64>().is_ok(),
            ValueType::Bool => matches!(
                s.to_ascii_lowercase().as_str(),
                "true" | "false" | "t" | "f" | "yes" | "no"
            ),
            ValueType::Date => parse_date(s).is_some(),
            ValueType::Timestamp { .. } => s.len() > 10 && parse_timestamp(s).is_some(),
            _ => false,
        });
        if candidates.is_empty() {
            return ValueType::Text;
        }
    }
    if !any {
        return ValueType::Text;
    }
    candidates.first().copied().unwrap_or(ValueType::Text)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{
        ArrayRef, BinaryArray, Date32Array, Decimal128Array, Int64Array, StringArray,
        TimestampMicrosecondArray,
    };

    use super::*;

    #[test]
    fn arrow_cells() {
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(vec![Some(7), None])),
            Arc::new(
                Decimal128Array::from(vec![12345, -5])
                    .with_precision_and_scale(10, 2)
                    .unwrap(),
            ),
            Arc::new(StringArray::from(vec!["a", "b"])),
            Arc::new(Date32Array::from(vec![19723, 0])),
            Arc::new(
                TimestampMicrosecondArray::from(vec![1_704_164_645_000_001, 0])
                    .with_timezone("UTC"),
            ),
            Arc::new(BinaryArray::from(vec![&b"\x01\xff"[..], b""])),
        ];
        assert_eq!(value_at(&cols[0], 0), Value::Int(7));
        assert_eq!(value_at(&cols[0], 1), Value::Null);
        assert_eq!(value_at(&cols[1], 0), Value::Decimal("123.45".into()));
        assert_eq!(value_at(&cols[1], 1), Value::Decimal("-0.05".into()));
        assert_eq!(value_at(&cols[3], 0).to_text().unwrap(), "2024-01-01");
        assert_eq!(
            value_at(&cols[4], 0).to_text().unwrap(),
            "2024-01-02 03:04:05.000001+00:00"
        );
        assert_eq!(value_at(&cols[5], 0).to_text().unwrap(), "\\x01ff");
    }

    #[test]
    fn conversions() {
        assert_eq!(
            Value::Text(" 42 ".into()).convert(ValueType::Int),
            Ok(Value::Int(42))
        );
        assert_eq!(
            Value::Text("2024-01-02".into()).convert(ValueType::Date),
            Ok(Value::Date(19724))
        );
        assert_eq!(
            Value::Text("2024-01-02T03:04:05Z".into())
                .convert(ValueType::Timestamp { tz: true })
                .unwrap()
                .to_text()
                .unwrap(),
            "2024-01-02 03:04:05+00:00"
        );
        assert!(Value::Text("x".into()).convert(ValueType::Int).is_err());
        assert_eq!(
            Value::from_cell_json("\"12.5\"", ValueType::Decimal),
            Ok(Value::Decimal("12.5".into()))
        );
        assert_eq!(
            Value::from_cell_json("null", ValueType::Int),
            Ok(Value::Null)
        );
        assert_eq!(Value::Float(3.0).to_text().unwrap(), "3");
        assert_eq!(Value::Float(0.1).to_text().unwrap(), "0.1");
    }

    #[test]
    fn bson_round_trip() {
        let values = [
            Value::Bool(true),
            Value::Int(5),
            Value::Int(1 << 40),
            Value::Float(1.5),
            Value::Decimal("123.45".into()),
            Value::Text("zażółć".into()),
            Value::Bytes(vec![0, 1, 255]),
            Value::Timestamp {
                micros: 1_704_164_645_123_000,
                tz: true,
            },
        ];
        for v in values {
            assert_eq!(Value::from_bson(&v.to_bson()), v, "{v:?}");
        }
        // Dates come back as timestamps; the target type restores them.
        let d = Value::Date(19724);
        assert_eq!(
            Value::from_bson(&d.to_bson()).convert(ValueType::Date),
            Ok(d)
        );
    }

    #[test]
    fn guessing() {
        assert_eq!(guess_type(["1", "2", ""].into_iter()), ValueType::Int);
        assert_eq!(guess_type(["1", "2.5"].into_iter()), ValueType::Float);
        assert_eq!(guess_type(["2024-01-01"].into_iter()), ValueType::Date);
        assert_eq!(
            guess_type(["2024-01-01 10:00:00"].into_iter()),
            ValueType::Timestamp { tz: false }
        );
        assert_eq!(guess_type(["a", "1"].into_iter()), ValueType::Text);
        assert_eq!(ValueType::from_sql("numeric(10,2)"), ValueType::Decimal);
        assert_eq!(ValueType::from_sql("VARCHAR(20)"), ValueType::Text);
        assert_eq!(
            ValueType::from_sql("timestamp with time zone"),
            ValueType::Timestamp { tz: true }
        );
        assert_eq!(ValueType::from_sql("BIGINT UNSIGNED"), ValueType::Int);
    }
}
