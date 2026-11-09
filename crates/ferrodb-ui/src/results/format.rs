//! Display formatting of result values: Arrow cells, BSON values, keys.
//! Pure functions (unit-tested), shared by the grid, document and key views.

use std::time::Duration;

use ferrodb_api::{
    KeyType,
    arrow_array::{
        Array, ArrayRef, BinaryArray, BooleanArray, Date32Array, Decimal128Array, Float32Array,
        Float64Array, Int8Array, Int16Array, Int32Array, Int64Array, LargeBinaryArray,
        LargeStringArray, StringArray, Time64MicrosecondArray, TimestampMicrosecondArray,
        TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt8Array,
        UInt16Array, UInt32Array, UInt64Array,
    },
    arrow_schema::{self, DataType, TimeUnit},
    bson::{Bson, Document, RawDocumentBuf},
};

/// Text shown for SQL `NULL`.
pub const NULL: &str = "<null>";

/// Kind of a column, used to pick the in-place editor and aggregates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Integer,
    Float,
    Bool,
    Text,
    Timestamp,
    Date,
    Binary,
    Other,
}

pub fn value_kind(dt: &DataType) -> ValueKind {
    use DataType::*;
    match dt {
        Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 => ValueKind::Integer,
        Float16 | Float32 | Float64 | Decimal128(..) | Decimal256(..) => ValueKind::Float,
        Boolean => ValueKind::Bool,
        Utf8 | LargeUtf8 | Utf8View => ValueKind::Text,
        Timestamp(..) => ValueKind::Timestamp,
        Date32 | Date64 => ValueKind::Date,
        Binary | LargeBinary | BinaryView | FixedSizeBinary(_) => ValueKind::Binary,
        _ => ValueKind::Other,
    }
}

/// Field metadata key with the database type name (set by real drivers,
/// `ferrodb_drivers::columns::DB_TYPE_KEY`).
pub const DB_TYPE_KEY: &str = "ferrodb.db_type";

/// Column header type: the database's own type name when the driver
/// reported it (`varchar(20)`, `jsonb`), otherwise derived from Arrow.
pub fn field_type_label(f: &arrow_schema::Field) -> String {
    match f.metadata().get(DB_TYPE_KEY) {
        Some(t) if !t.is_empty() => t.clone(),
        _ => type_label(f.data_type()),
    }
}

/// Short SQL-ish type name for column headers (`int8`, `text`, `timestamp`…).
pub fn type_label(dt: &DataType) -> String {
    use DataType::*;
    match dt {
        Int8 => "int1".into(),
        Int16 => "int2".into(),
        Int32 => "int4".into(),
        Int64 => "int8".into(),
        UInt8 | UInt16 | UInt32 | UInt64 => "uint".into(),
        Float32 => "float4".into(),
        Float64 => "float8".into(),
        Decimal128(p, s) | Decimal256(p, s) => format!("numeric({p},{s})"),
        Boolean => "bool".into(),
        Utf8 | LargeUtf8 | Utf8View => "text".into(),
        Timestamp(_, None) => "timestamp".into(),
        Timestamp(_, Some(_)) => "timestamptz".into(),
        Date32 | Date64 => "date".into(),
        Time32(_) | Time64(_) => "time".into(),
        Binary | LargeBinary | BinaryView | FixedSizeBinary(_) => "bytea".into(),
        other => other.to_string().to_lowercase(),
    }
}

fn timestamp(v: i64, unit: TimeUnit) -> String {
    let (secs, nanos) = match unit {
        TimeUnit::Second => (v, 0),
        TimeUnit::Millisecond => (v.div_euclid(1000), (v.rem_euclid(1000) * 1_000_000) as u32),
        TimeUnit::Microsecond => (
            v.div_euclid(1_000_000),
            (v.rem_euclid(1_000_000) * 1000) as u32,
        ),
        TimeUnit::Nanosecond => (
            v.div_euclid(1_000_000_000),
            v.rem_euclid(1_000_000_000) as u32,
        ),
    };
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|d| d.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_else(|| v.to_string())
}

macro_rules! downcast {
    ($a:expr, $t:ty) => {
        $a.as_any()
            .downcast_ref::<$t>()
            .expect("array type matches data type")
    };
}

/// Formats one Arrow cell; `None` for SQL `NULL`.
pub fn arrow_cell(a: &ArrayRef, i: usize) -> Option<String> {
    if a.is_null(i) {
        return None;
    }
    use DataType::*;
    Some(match a.data_type() {
        Int8 => downcast!(a, Int8Array).value(i).to_string(),
        Int16 => downcast!(a, Int16Array).value(i).to_string(),
        Int32 => downcast!(a, Int32Array).value(i).to_string(),
        Int64 => downcast!(a, Int64Array).value(i).to_string(),
        UInt8 => downcast!(a, UInt8Array).value(i).to_string(),
        UInt16 => downcast!(a, UInt16Array).value(i).to_string(),
        UInt32 => downcast!(a, UInt32Array).value(i).to_string(),
        UInt64 => downcast!(a, UInt64Array).value(i).to_string(),
        Float32 => downcast!(a, Float32Array).value(i).to_string(),
        Float64 => {
            let v = downcast!(a, Float64Array).value(i);
            if v.fract() == 0.0 && v.abs() < 1e15 {
                format!("{v:.1}")
            } else {
                v.to_string()
            }
        }
        Boolean => downcast!(a, BooleanArray).value(i).to_string(),
        Utf8 => downcast!(a, StringArray).value(i).to_owned(),
        LargeUtf8 => downcast!(a, LargeStringArray).value(i).to_owned(),
        Timestamp(TimeUnit::Second, _) => timestamp(
            downcast!(a, TimestampSecondArray).value(i),
            TimeUnit::Second,
        ),
        Timestamp(TimeUnit::Millisecond, _) => timestamp(
            downcast!(a, TimestampMillisecondArray).value(i),
            TimeUnit::Millisecond,
        ),
        Timestamp(TimeUnit::Microsecond, _) => timestamp(
            downcast!(a, TimestampMicrosecondArray).value(i),
            TimeUnit::Microsecond,
        ),
        Timestamp(TimeUnit::Nanosecond, _) => timestamp(
            downcast!(a, TimestampNanosecondArray).value(i),
            TimeUnit::Nanosecond,
        ),
        Date32 => {
            let days = downcast!(a, Date32Array).value(i);
            chrono::NaiveDate::from_num_days_from_ce_opt(days + 719_163)
                .map(|d| d.to_string())
                .unwrap_or_else(|| days.to_string())
        }
        Binary => {
            let b = downcast!(a, BinaryArray).value(i);
            format!("0x{}", hex(&b[..b.len().min(32)]))
        }
        LargeBinary => {
            let b = downcast!(a, LargeBinaryArray).value(i);
            format!("0x{}", hex(&b[..b.len().min(32)]))
        }
        Decimal128(_, scale) => decimal(downcast!(a, Decimal128Array).value(i), *scale),
        Time64(TimeUnit::Microsecond) => {
            let us = downcast!(a, Time64MicrosecondArray).value(i);
            chrono::NaiveTime::from_num_seconds_from_midnight_opt(
                (us / 1_000_000) as u32,
                ((us % 1_000_000) * 1000) as u32,
            )
            .map(|t| t.format("%H:%M:%S%.f").to_string())
            .unwrap_or_else(|| us.to_string())
        }
        other => format!("<{other}>"),
    })
}

/// Numeric value of a cell (aggregates); `None` for NULL / non-numeric.
pub fn arrow_number(a: &ArrayRef, i: usize) -> Option<f64> {
    if a.is_null(i) {
        return None;
    }
    use DataType::*;
    Some(match a.data_type() {
        Int8 => downcast!(a, Int8Array).value(i) as f64,
        Int16 => downcast!(a, Int16Array).value(i) as f64,
        Int32 => downcast!(a, Int32Array).value(i) as f64,
        Int64 => downcast!(a, Int64Array).value(i) as f64,
        UInt8 => downcast!(a, UInt8Array).value(i) as f64,
        UInt16 => downcast!(a, UInt16Array).value(i) as f64,
        UInt32 => downcast!(a, UInt32Array).value(i) as f64,
        UInt64 => downcast!(a, UInt64Array).value(i) as f64,
        Float32 => downcast!(a, Float32Array).value(i) as f64,
        Float64 => downcast!(a, Float64Array).value(i),
        Decimal128(_, scale) => {
            downcast!(a, Decimal128Array).value(i) as f64 / 10f64.powi(*scale as i32)
        }
        _ => return None,
    })
}

/// Exact text of an unscaled decimal (`12345`, scale 2 → `123.45`).
pub fn decimal(v: i128, scale: i8) -> String {
    if scale <= 0 {
        return (v * 10i128.pow((-scale) as u32)).to_string();
    }
    let s = v.unsigned_abs().to_string();
    let scale = scale as usize;
    let s = format!("{s:0>width$}", width = scale + 1);
    let (int, frac) = s.split_at(s.len() - scale);
    format!("{}{int}.{frac}", if v < 0 { "-" } else { "" })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// JSON encoding of an edited cell value for [`ferrodb_api::Change::SetCell`]
/// (`None` = NULL). Validates the text against the column kind.
pub fn encode_cell(kind: ValueKind, text: Option<&str>) -> Result<String, String> {
    let Some(text) = text else {
        return Ok("null".into());
    };
    match kind {
        ValueKind::Integer => text
            .trim()
            .parse::<i64>()
            .map(|v| v.to_string())
            .map_err(|_| format!("not an integer: {text}")),
        ValueKind::Float => text
            .trim()
            .parse::<f64>()
            .map(|v| v.to_string())
            .map_err(|_| format!("not a number: {text}")),
        ValueKind::Bool => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => Ok("true".into()),
            "false" | "f" | "0" | "no" => Ok("false".into()),
            _ => Err(format!("not a boolean: {text}")),
        },
        ValueKind::Timestamp => {
            chrono::NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%d %H:%M:%S%.f")
                .or_else(|_| {
                    chrono::NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%d %H:%M:%S")
                })
                .map(|_| serde_json::Value::String(text.trim().into()).to_string())
                .map_err(|_| format!("expected YYYY-MM-DD HH:MM:SS: {text}"))
        }
        ValueKind::Date => chrono::NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
            .map(|_| serde_json::Value::String(text.trim().into()).to_string())
            .map_err(|_| format!("expected YYYY-MM-DD: {text}")),
        _ => Ok(serde_json::Value::String(text.into()).to_string()),
    }
}

// ------------------------------------------------------------------ BSON

/// BSON type name as shown in the document browser (`ObjectId`, `Int32`…).
pub fn bson_type(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "Double",
        Bson::String(_) => "String",
        Bson::Array(_) => "Array",
        Bson::Document(_) => "Object",
        Bson::Boolean(_) => "Boolean",
        Bson::Null => "Null",
        Bson::RegularExpression(_) => "Regex",
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => "Code",
        Bson::Int32(_) => "Int32",
        Bson::Int64(_) => "Int64",
        Bson::Timestamp(_) => "Timestamp",
        Bson::Binary(_) => "Binary",
        Bson::ObjectId(_) => "ObjectId",
        Bson::DateTime(_) => "Date",
        Bson::Symbol(_) => "Symbol",
        Bson::Decimal128(_) => "Decimal128",
        Bson::Undefined => "Undefined",
        Bson::MaxKey => "MaxKey",
        Bson::MinKey => "MinKey",
        Bson::DbPointer(_) => "DBPointer",
    }
}

/// One-line rendering of a value, faithful to its type
/// (`ObjectId("…")`, `NumberLong(5)`, `ISODate("…")`, `{ 3 fields }`).
pub fn bson_short(v: &Bson) -> String {
    match v {
        Bson::String(s) => format!("\"{}\"", truncate(s, 120)),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => format!("NumberLong({i})"),
        Bson::Double(d) => {
            if d.fract() == 0.0 && d.abs() < 1e15 {
                format!("{d:.1}")
            } else {
                d.to_string()
            }
        }
        Bson::Decimal128(d) => format!("NumberDecimal(\"{d}\")"),
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".into(),
        Bson::ObjectId(o) => format!("ObjectId(\"{o}\")"),
        Bson::DateTime(d) => format!(
            "ISODate(\"{}\")",
            d.try_to_rfc3339_string()
                .unwrap_or_else(|_| d.timestamp_millis().to_string())
        ),
        Bson::Array(a) => format!("[ {} items ]", a.len()),
        Bson::Document(d) => format!("{{ {} fields }}", d.len()),
        Bson::Binary(b) => format!("BinData({} bytes)", b.bytes.len()),
        other => truncate(&other.to_string(), 120),
    }
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// Decodes a raw document (lazily, on display).
pub fn document(raw: &RawDocumentBuf) -> Document {
    Document::try_from(raw.as_ref()).unwrap_or_default()
}

/// Relaxed Extended JSON, pretty-printed (JSON view, copy, edit dialog).
pub fn document_json(doc: &Document) -> String {
    let v = Bson::Document(doc.clone()).into_relaxed_extjson();
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

/// Extended JSON of one value (edit dialog, change set): relaxed, except
/// `Int64`, which relaxed JSON would turn into a plain (Int32) number.
pub fn value_json(v: &Bson) -> String {
    let json = match v {
        Bson::Int64(_) => v.clone().into_canonical_extjson(),
        _ => v.clone().into_relaxed_extjson(),
    };
    serde_json::to_string(&json).unwrap_or_default()
}

/// Parses an edited value (Extended JSON) back to BSON.
pub fn parse_value_json(text: &str) -> Result<Bson, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Bson::try_from(v).map_err(|e| e.to_string())
}

/// Value at a dotted path (`address.city`, `tags.0`).
pub fn bson_at<'a>(doc: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut parts = path.split('.');
    let mut cur = doc.get(parts.next()?)?;
    for p in parts {
        cur = match cur {
            Bson::Document(d) => d.get(p)?,
            Bson::Array(a) => a.get(p.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

// ------------------------------------------------------------------ keys

pub fn key_type_label(k: KeyType) -> &'static str {
    match k {
        KeyType::String => "string",
        KeyType::Hash => "hash",
        KeyType::List => "list",
        KeyType::Set => "set",
        KeyType::ZSet => "zset",
        KeyType::Stream => "stream",
        KeyType::Json => "json",
        KeyType::Other => "other",
    }
}

/// `1h 02m`, `45s`, `∞` (no expiry).
pub fn ttl(t: Option<Duration>) -> String {
    let Some(t) = t else { return "∞".into() };
    let s = t.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        3600..86_400 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d {:02}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// `512 B`, `4.0 KB`, `1.2 GB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use ferrodb_api::bson::doc;

    #[test]
    fn arrow_cells() {
        let a: ArrayRef = Arc::new(Int64Array::from(vec![Some(5), None]));
        assert_eq!(arrow_cell(&a, 0).as_deref(), Some("5"));
        assert_eq!(arrow_cell(&a, 1), None);
        let t: ArrayRef = Arc::new(TimestampMillisecondArray::from(vec![0]));
        assert_eq!(arrow_cell(&t, 0).unwrap(), "1970-01-01 00:00:00.000");
        let f: ArrayRef = Arc::new(Float64Array::from(vec![2.0, 2.5]));
        assert_eq!(arrow_cell(&f, 0).unwrap(), "2.0");
        assert_eq!(arrow_number(&f, 1), Some(2.5));
    }

    #[test]
    fn cell_encoding_validates_by_kind() {
        assert_eq!(encode_cell(ValueKind::Integer, Some(" 42 ")).unwrap(), "42");
        assert!(encode_cell(ValueKind::Integer, Some("x")).is_err());
        assert_eq!(encode_cell(ValueKind::Bool, Some("yes")).unwrap(), "true");
        assert_eq!(
            encode_cell(ValueKind::Text, Some("a\"b")).unwrap(),
            "\"a\\\"b\""
        );
        assert_eq!(encode_cell(ValueKind::Text, None).unwrap(), "null");
        assert!(encode_cell(ValueKind::Timestamp, Some("2026-01-02 03:04:05")).is_ok());
        assert!(encode_cell(ValueKind::Timestamp, Some("yesterday")).is_err());
    }

    #[test]
    fn bson_rendering_is_type_faithful() {
        let d = doc! { "n": 5i64, "i": 5i32, "a": { "b": [1, 2] } };
        assert_eq!(bson_short(d.get("n").unwrap()), "NumberLong(5)");
        assert_eq!(bson_short(d.get("i").unwrap()), "5");
        assert_eq!(bson_type(d.get("a").unwrap()), "Object");
        assert_eq!(bson_at(&d, "a.b.1"), Some(&Bson::Int32(2)));
        let back = parse_value_json(&value_json(d.get("n").unwrap())).unwrap();
        assert_eq!(bson_type(&back), "Int64");
    }

    #[test]
    fn ttl_and_sizes() {
        assert_eq!(ttl(None), "∞");
        assert_eq!(ttl(Some(Duration::from_secs(3725))), "1h 02m");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(4096), "4.0 KB");
    }
}
