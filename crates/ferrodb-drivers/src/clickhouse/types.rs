//! ClickHouse writes `FORMAT ArrowStream` directly from its columns, so most
//! types arrive as the Arrow types the grid already reads (integers,
//! decimals, `Date32`, `DateTime64` as timestamps, `Array` as lists, `Map`
//! as maps, `Tuple` and `Nested` as structs, `LowCardinality` unwrapped,
//! `Nullable` as nullable fields). A few lose their meaning on the way and
//! are fixed here, column by column, without touching the others:
//!
//! | ClickHouse | Arrow from the server | shown as |
//! |---|---|---|
//! | `DateTime`, `DateTime('tz')` | `UInt32` (seconds) | timestamp (µs) in the column / server time zone |
//! | `Enum8` / `Enum16` | `Int8` / `Int16` | the enum label |
//! | `IPv4` | `UInt32` | dotted text |
//! | `IPv6` | `FixedSizeBinary(16)` | text |
//! | `Int128` … `UInt256` | `FixedSizeBinary(16 / 32)`, little endian | decimal text |
//! | `UUID` | `FixedSizeBinary(16)` + `arrow.uuid` | canonical text |
//! | `BFloat16` | opaque binary | `Float32` |
//! | `FixedString(n)` | `FixedSizeBinary(n)` | text when valid UTF-8, else binary |
//!
//! `UInt32`, `Int8`/`Int16` and 16/32-byte fixed binaries are ambiguous
//! (they are also plain integers or `FixedString`s): only then does the
//! driver ask the server for the column types (`DESCRIBE (query)`); the
//! types it returns are also kept as the column's database type.

use std::{collections::HashMap, sync::Arc};

use arrow_array::{
    Array, ArrayRef, BinaryArray, FixedSizeBinaryArray, Float32Array, Int8Array, Int16Array,
    RecordBatch, StringArray, TimestampMicrosecondArray, UInt32Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};

use crate::columns::DB_TYPE_KEY;

const EXTENSION_NAME: &str = "ARROW:extension:name";
const EXTENSION_METADATA: &str = "ARROW:extension:metadata";

/// What happens to one column.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Fix {
    Keep,
    /// Seconds → microseconds with a time zone.
    DateTime(Arc<str>),
    /// Enum code → label.
    Enum(HashMap<i16, String>),
    Ipv4,
    Ipv6,
    /// Little-endian wide integer (`signed`).
    WideInt {
        signed: bool,
    },
    Uuid,
    BFloat16,
    /// `FixedString`: text when every value is UTF-8.
    FixedString,
}

/// The fixes for a result and the schema the sink sees.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub fixes: Vec<Fix>,
    pub schema: SchemaRef,
}

/// Whether the raw schema needs the server's column types to be read
/// correctly.
pub(crate) fn needs_types(schema: &Schema) -> bool {
    schema.fields().iter().any(|f| {
        matches!(
            f.data_type(),
            DataType::UInt32 | DataType::Int8 | DataType::Int16
        ) || (matches!(f.data_type(), DataType::FixedSizeBinary(16 | 32))
            && f.metadata().get(EXTENSION_NAME).map(String::as_str) != Some("arrow.uuid"))
    })
}

/// `Nullable(LowCardinality(T))` → `T`.
pub(crate) fn base_type(t: &str) -> &str {
    let mut t = t.trim();
    loop {
        let inner = ["Nullable(", "LowCardinality("]
            .iter()
            .find_map(|p| t.strip_prefix(p).and_then(|r| r.strip_suffix(')')));
        match inner {
            Some(i) => t = i.trim(),
            None => return t,
        }
    }
}

/// Time zone argument of `DateTime('Europe/Warsaw')` / `DateTime64(3, 'UTC')`.
fn time_zone(t: &str) -> Option<&str> {
    let start = t.find('\'')?;
    let end = t[start + 1..].find('\'')? + start + 1;
    Some(&t[start + 1..end])
}

/// Labels of `Enum8('a' = 1, 'b''c' = 2)`.
pub(crate) fn enum_labels(t: &str) -> HashMap<i16, String> {
    let mut out = HashMap::new();
    let Some(body) = t.find('(').and_then(|i| t.rfind(')').map(|j| &t[i + 1..j])) else {
        return out;
    };
    let mut chars = body.chars().peekable();
    loop {
        // label
        while chars.peek().is_some_and(|c| *c != '\'') {
            chars.next();
        }
        if chars.next().is_none() {
            break;
        }
        let mut label = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        label.push(n);
                    }
                }
                '\'' if chars.peek() == Some(&'\'') => {
                    chars.next();
                    label.push('\'');
                }
                '\'' => break,
                c => label.push(c),
            }
        }
        // = number
        let mut num = String::new();
        for c in chars.by_ref() {
            match c {
                ',' => break,
                c if c == '-' || c.is_ascii_digit() => num.push(c),
                _ => {}
            }
        }
        if let Ok(n) = num.parse::<i16>() {
            out.insert(n, label);
        }
    }
    out
}

/// The fixes for `schema`, given the ClickHouse type of each column when
/// known (`types[i]`) and the server time zone.
pub(crate) fn plan(schema: &Schema, types: Option<&[String]>, server_tz: &str) -> Plan {
    let mut fixes = Vec::with_capacity(schema.fields().len());
    let mut fields = Vec::with_capacity(schema.fields().len());
    for (i, f) in schema.fields().iter().enumerate() {
        let ch = types.and_then(|t| t.get(i)).map(String::as_str);
        let base = ch.map(base_type).unwrap_or("");
        let ext = f.metadata().get(EXTENSION_NAME).map(String::as_str);
        let ext_meta = f.metadata().get(EXTENSION_METADATA).map(String::as_str);
        let fix = match (f.data_type(), base) {
            (DataType::UInt32, b) if b == "DateTime" || b.starts_with("DateTime(") => {
                Fix::DateTime(Arc::from(time_zone(b).unwrap_or(server_tz)))
            }
            (DataType::UInt32, "IPv4") => Fix::Ipv4,
            (DataType::Int8 | DataType::Int16, b) if b.starts_with("Enum") => {
                Fix::Enum(enum_labels(b))
            }
            (DataType::FixedSizeBinary(16), _) if ext == Some("arrow.uuid") || base == "UUID" => {
                Fix::Uuid
            }
            (DataType::FixedSizeBinary(16), "IPv6") => Fix::Ipv6,
            (DataType::FixedSizeBinary(16 | 32), "Int128" | "Int256") => {
                Fix::WideInt { signed: true }
            }
            (DataType::FixedSizeBinary(16 | 32), "UInt128" | "UInt256") => {
                Fix::WideInt { signed: false }
            }
            (DataType::FixedSizeBinary(_), b) if b.starts_with("FixedString") => Fix::FixedString,
            // Without the server types, a 16/32-byte binary that is not a
            // UUID stays binary; other widths can only be `FixedString`.
            (DataType::FixedSizeBinary(16 | 32), "") => Fix::Keep,
            (DataType::FixedSizeBinary(_), "") => Fix::FixedString,
            (DataType::Binary, _) if ext_meta == Some("BFloat16") || base == "BFloat16" => {
                Fix::BFloat16
            }
            _ => Fix::Keep,
        };
        let ty = match &fix {
            Fix::Keep => f.data_type().clone(),
            Fix::DateTime(tz) => DataType::Timestamp(TimeUnit::Microsecond, Some(tz.clone())),
            Fix::Enum(_) | Fix::Ipv4 | Fix::Ipv6 | Fix::WideInt { .. } | Fix::Uuid => {
                DataType::Utf8
            }
            Fix::BFloat16 => DataType::Float32,
            // Decided per batch would change the schema mid-result: text
            // (binary values that are not UTF-8 are shown escaped).
            Fix::FixedString => DataType::Utf8,
        };
        let mut meta: HashMap<String, String> = f
            .metadata()
            .iter()
            .filter(|(k, _)| fix == Fix::Keep || !k.starts_with("ARROW:extension"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Some(t) = ch {
            meta.insert(DB_TYPE_KEY.to_owned(), t.to_owned());
        }
        fields.push(Field::new(f.name(), ty, f.is_nullable()).with_metadata(meta));
        fixes.push(fix);
    }
    Plan {
        fixes,
        schema: Arc::new(Schema::new_with_metadata(fields, schema.metadata().clone())),
    }
}

impl Plan {
    pub fn is_identity(&self) -> bool {
        self.fixes.iter().all(|f| *f == Fix::Keep)
    }

    /// Applies the fixes to one batch of the raw stream.
    pub fn apply(&self, batch: &RecordBatch) -> Result<RecordBatch, String> {
        if self.is_identity() {
            return batch
                .clone()
                .with_schema(self.schema.clone())
                .map_err(|e| e.to_string());
        }
        let columns: Vec<ArrayRef> = batch
            .columns()
            .iter()
            .zip(&self.fixes)
            .map(|(c, fix)| convert(c, fix))
            .collect::<Result<_, _>>()?;
        RecordBatch::try_new(self.schema.clone(), columns).map_err(|e| e.to_string())
    }
}

fn convert(c: &ArrayRef, fix: &Fix) -> Result<ArrayRef, String> {
    let bad = || format!("unexpected Arrow type {} for {fix:?}", c.data_type());
    Ok(match fix {
        Fix::Keep => c.clone(),
        Fix::DateTime(tz) => {
            let a = c.as_any().downcast_ref::<UInt32Array>().ok_or_else(bad)?;
            Arc::new(
                a.iter()
                    .map(|v| v.map(|s| i64::from(s) * 1_000_000))
                    .collect::<TimestampMicrosecondArray>()
                    .with_timezone(tz.clone()),
            )
        }
        Fix::Ipv4 => {
            let a = c.as_any().downcast_ref::<UInt32Array>().ok_or_else(bad)?;
            Arc::new(
                a.iter()
                    .map(|v| v.map(|ip| std::net::Ipv4Addr::from(ip).to_string()))
                    .collect::<StringArray>(),
            )
        }
        Fix::Enum(labels) => {
            let codes: Vec<Option<i16>> = if let Some(a) = c.as_any().downcast_ref::<Int8Array>() {
                a.iter().map(|v| v.map(i16::from)).collect()
            } else if let Some(a) = c.as_any().downcast_ref::<Int16Array>() {
                a.iter().collect()
            } else {
                return Err(bad());
            };
            Arc::new(
                codes
                    .into_iter()
                    .map(|v| {
                        v.map(|code| {
                            labels
                                .get(&code)
                                .cloned()
                                .unwrap_or_else(|| code.to_string())
                        })
                    })
                    .collect::<StringArray>(),
            )
        }
        Fix::Ipv6 | Fix::WideInt { .. } | Fix::Uuid | Fix::FixedString => {
            let a = c
                .as_any()
                .downcast_ref::<FixedSizeBinaryArray>()
                .ok_or_else(bad)?;
            Arc::new(
                (0..a.len())
                    .map(|i| {
                        (!a.is_null(i)).then(|| {
                            let v = a.value(i);
                            match fix {
                                Fix::Ipv6 => ipv6_text(v),
                                Fix::WideInt { signed } => wide_int_text(v, *signed),
                                Fix::Uuid => uuid_text(v),
                                _ => fixed_string_text(v),
                            }
                        })
                    })
                    .collect::<StringArray>(),
            )
        }
        Fix::BFloat16 => {
            let a = c.as_any().downcast_ref::<BinaryArray>().ok_or_else(bad)?;
            Arc::new(
                a.iter()
                    .map(|v| {
                        v.and_then(|b| {
                            let bits = u16::from_le_bytes(b.get(..2)?.try_into().ok()?);
                            Some(f32::from_bits(u32::from(bits) << 16))
                        })
                    })
                    .collect::<Float32Array>(),
            )
        }
    })
}

fn ipv6_text(v: &[u8]) -> String {
    <[u8; 16]>::try_from(v)
        .map(|b| std::net::Ipv6Addr::from(b).to_string())
        .unwrap_or_default()
}

/// Canonical `8-4-4-4-12` text of a UUID in RFC byte order.
pub(crate) fn uuid_text(v: &[u8]) -> String {
    let h: String = v.iter().map(|b| format!("{b:02x}")).collect();
    if h.len() != 32 {
        return h;
    }
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// `FixedString` value without its trailing zero padding.
fn fixed_string_text(v: &[u8]) -> String {
    let end = v.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    String::from_utf8_lossy(&v[..end]).into_owned()
}

/// Decimal text of a little-endian two's complement integer of 16 or 32
/// bytes.
pub(crate) fn wide_int_text(v: &[u8], signed: bool) -> String {
    if v.len() == 16 {
        let b: [u8; 16] = v.try_into().unwrap_or([0; 16]);
        return if signed {
            i128::from_le_bytes(b).to_string()
        } else {
            u128::from_le_bytes(b).to_string()
        };
    }
    // 256 bits: little-endian u64 limbs, negated when the sign bit is set.
    let mut limbs: Vec<u64> = v
        .chunks(8)
        .map(|c| {
            let mut b = [0u8; 8];
            b[..c.len()].copy_from_slice(c);
            u64::from_le_bytes(b)
        })
        .collect();
    let negative = signed && limbs.last().is_some_and(|l| l >> 63 == 1);
    if negative {
        // two's complement: invert and add one
        let mut carry = true;
        for l in &mut limbs {
            *l = !*l;
            if carry {
                let (s, c) = l.overflowing_add(1);
                *l = s;
                carry = c;
            }
        }
    }
    let mut digits = Vec::new();
    while limbs.iter().any(|l| *l != 0) {
        let mut rem: u128 = 0;
        for l in limbs.iter_mut().rev() {
            let cur = (rem << 64) | u128::from(*l);
            *l = (cur / 10) as u64;
            rem = cur % 10;
        }
        digits.push(b'0' + rem as u8);
    }
    if digits.is_empty() {
        digits.push(b'0');
    }
    if negative {
        digits.push(b'-');
    }
    digits.reverse();
    String::from_utf8(digits).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_names() {
        assert_eq!(base_type("Nullable(DateTime('UTC'))"), "DateTime('UTC')");
        assert_eq!(base_type("LowCardinality(Nullable(String))"), "String");
        assert_eq!(
            time_zone("DateTime64(3, 'Europe/Warsaw')"),
            Some("Europe/Warsaw")
        );
        let e = enum_labels("Enum8('a' = 1, 'it''s' = -2, 'x\\'y' = 3)");
        assert_eq!(e[&1], "a");
        assert_eq!(e[&-2], "it's");
        assert_eq!(e[&3], "x'y");
    }

    #[test]
    fn wide_integers() {
        let mut v = [0u8; 32];
        v[0] = 5;
        assert_eq!(wide_int_text(&v, true), "5");
        let minus_one = [0xffu8; 32];
        assert_eq!(wide_int_text(&minus_one, true), "-1");
        assert_eq!(
            wide_int_text(&minus_one, false),
            "115792089237316195423570985008687907853269984665640564039457584007913129639935"
        );
        assert_eq!(wide_int_text(&(-7i128).to_le_bytes(), true), "-7");
        assert_eq!(
            uuid_text(&[
                0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0, 1, 2, 3, 4, 5, 6, 7
            ]),
            "12345678-9abc-def0-0001-020304050607"
        );
    }

    #[test]
    fn plan_converts_ambiguous_columns() {
        let raw = Schema::new(vec![
            Field::new("ts", DataType::UInt32, false),
            Field::new("e", DataType::Int8, false),
            Field::new("n", DataType::UInt32, true),
            Field::new("ip", DataType::UInt32, false),
        ]);
        assert!(needs_types(&raw));
        let types = vec![
            "DateTime".to_owned(),
            "Enum8('a' = 1, 'b' = 2)".to_owned(),
            "Nullable(UInt32)".to_owned(),
            "IPv4".to_owned(),
        ];
        let p = plan(&raw, Some(&types), "Europe/Warsaw");
        assert_eq!(
            p.schema.field(0).data_type(),
            &DataType::Timestamp(TimeUnit::Microsecond, Some("Europe/Warsaw".into()))
        );
        assert_eq!(p.schema.field(2).data_type(), &DataType::UInt32);
        assert_eq!(
            p.schema.field(2).metadata()[DB_TYPE_KEY],
            "Nullable(UInt32)"
        );
        let batch = RecordBatch::try_new(
            Arc::new(raw),
            vec![
                Arc::new(UInt32Array::from(vec![1_700_000_000])),
                Arc::new(Int8Array::from(vec![2])),
                Arc::new(UInt32Array::from(vec![None])),
                Arc::new(UInt32Array::from(vec![0x01020304])),
            ],
        )
        .unwrap();
        let out = p.apply(&batch).unwrap();
        let ts = out
            .column(0)
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert_eq!(ts.value(0), 1_700_000_000_000_000);
        let e = out
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(e.value(0), "b");
        let ip = out
            .column(3)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(ip.value(0), "1.2.3.4");
    }
}
