//! Driver values → Arrow (plan, section 5.6: one result format for every
//! access layer). Drivers describe columns with [`ColumnSpec`], push
//! [`Cell`]s row by row into a [`BatchBuilder`] and flush record batches of
//! at most `max_rows` rows or every 16 ms, whichever comes first.
//!
//! The database type name is kept in the field metadata under
//! [`DB_TYPE_KEY`] so the grid can show `varchar(20)` or `jsonb` instead of
//! the Arrow type.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use arrow_array::{
    ArrayRef, RecordBatch,
    builder::{
        ArrayBuilder, BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder,
        Float32Builder, Float64Builder, Int16Builder, Int32Builder, Int64Builder, StringBuilder,
        Time64MicrosecondBuilder, TimestampMicrosecondBuilder, UInt64Builder,
    },
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};

/// Field metadata key holding the database type name.
pub const DB_TYPE_KEY: &str = "ferrodb.db_type";

/// Flush interval for partially filled batches (ADR-002: 16 ms).
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// Arrow representation chosen for a database column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnType {
    Bool,
    Int16,
    Int32,
    Int64,
    UInt64,
    Float32,
    Float64,
    /// Exact decimal; values are pushed as [`Cell::Decimal`] scaled by `scale`
    /// or as text.
    Decimal {
        precision: u8,
        scale: i8,
    },
    Text,
    Binary,
    Date,
    /// Time of day, microseconds.
    Time,
    /// Microseconds since the Unix epoch; `tz` = with time zone (UTC).
    Timestamp {
        tz: bool,
    },
}

impl ColumnType {
    pub fn arrow(self) -> DataType {
        match self {
            Self::Bool => DataType::Boolean,
            Self::Int16 => DataType::Int16,
            Self::Int32 => DataType::Int32,
            Self::Int64 => DataType::Int64,
            Self::UInt64 => DataType::UInt64,
            Self::Float32 => DataType::Float32,
            Self::Float64 => DataType::Float64,
            Self::Decimal { precision, scale } => DataType::Decimal128(precision, scale),
            Self::Text => DataType::Utf8,
            Self::Binary => DataType::Binary,
            Self::Date => DataType::Date32,
            Self::Time => DataType::Time64(TimeUnit::Microsecond),
            Self::Timestamp { tz } => {
                DataType::Timestamp(TimeUnit::Microsecond, tz.then(|| Arc::from("UTC")))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: String,
    pub ty: ColumnType,
    /// Database type name, e.g. `int4`, `varchar(20)`, `DECIMAL(10,2)`.
    pub db_type: String,
}

impl ColumnSpec {
    pub fn new(name: impl Into<String>, ty: ColumnType, db_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ty,
            db_type: db_type.into(),
        }
    }
}

/// Arrow schema for `cols`. All fields are nullable: drivers rarely know.
/// Duplicate column names (`select 1, 1`) are kept as they are.
pub fn schema(cols: &[ColumnSpec]) -> SchemaRef {
    Arc::new(Schema::new(
        cols.iter()
            .map(|c| {
                Field::new(&c.name, c.ty.arrow(), true)
                    .with_metadata(HashMap::from([(DB_TYPE_KEY.to_owned(), c.db_type.clone())]))
            })
            .collect::<Vec<_>>(),
    ))
}

/// One value as read from the wire.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cell<'a> {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    /// Unscaled decimal value (scale of the target column).
    Decimal(i128),
    Text(&'a str),
    Bytes(&'a [u8]),
    /// Days since 1970-01-01.
    Date(i32),
    /// Microseconds since midnight.
    Time(i64),
    /// Microseconds since the Unix epoch.
    Timestamp(i64),
}

enum Builder {
    Bool(BooleanBuilder),
    Int16(Int16Builder),
    Int32(Int32Builder),
    Int64(Int64Builder),
    UInt64(UInt64Builder),
    Float32(Float32Builder),
    Float64(Float64Builder),
    Decimal(Decimal128Builder, i8),
    Text(StringBuilder),
    Binary(BinaryBuilder),
    Date(Date32Builder),
    Time(Time64MicrosecondBuilder),
    Timestamp(TimestampMicrosecondBuilder),
}

impl Builder {
    fn new(dt: &DataType) -> Self {
        match dt {
            DataType::Boolean => Self::Bool(BooleanBuilder::new()),
            DataType::Int16 => Self::Int16(Int16Builder::new()),
            DataType::Int32 => Self::Int32(Int32Builder::new()),
            DataType::Int64 => Self::Int64(Int64Builder::new()),
            DataType::UInt64 => Self::UInt64(UInt64Builder::new()),
            DataType::Float32 => Self::Float32(Float32Builder::new()),
            DataType::Float64 => Self::Float64(Float64Builder::new()),
            DataType::Decimal128(p, s) => Self::Decimal(
                Decimal128Builder::new()
                    .with_precision_and_scale(*p, *s)
                    .unwrap_or_default(),
                *s,
            ),
            DataType::Binary => Self::Binary(BinaryBuilder::new()),
            DataType::Date32 => Self::Date(Date32Builder::new()),
            DataType::Time64(_) => Self::Time(Time64MicrosecondBuilder::new()),
            DataType::Timestamp(_, tz) => {
                Self::Timestamp(TimestampMicrosecondBuilder::new().with_timezone_opt(tz.clone()))
            }
            _ => Self::Text(StringBuilder::new()),
        }
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            Self::Bool(b) => Arc::new(b.finish()),
            Self::Int16(b) => Arc::new(b.finish()),
            Self::Int32(b) => Arc::new(b.finish()),
            Self::Int64(b) => Arc::new(b.finish()),
            Self::UInt64(b) => Arc::new(b.finish()),
            Self::Float32(b) => Arc::new(b.finish()),
            Self::Float64(b) => Arc::new(b.finish()),
            Self::Decimal(b, _) => Arc::new(b.finish()),
            Self::Text(b) => Arc::new(b.finish()),
            Self::Binary(b) => Arc::new(b.finish()),
            Self::Date(b) => Arc::new(b.finish()),
            Self::Time(b) => Arc::new(b.finish()),
            Self::Timestamp(b) => Arc::new(b.finish()),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Bool(b) => b.len(),
            Self::Int16(b) => b.len(),
            Self::Int32(b) => b.len(),
            Self::Int64(b) => b.len(),
            Self::UInt64(b) => b.len(),
            Self::Float32(b) => b.len(),
            Self::Float64(b) => b.len(),
            Self::Decimal(b, _) => b.len(),
            Self::Text(b) => b.len(),
            Self::Binary(b) => b.len(),
            Self::Date(b) => b.len(),
            Self::Time(b) => b.len(),
            Self::Timestamp(b) => b.len(),
        }
    }

    /// Appends `cell`, coercing between representations where the meaning
    /// is preserved. Returns `false` (and appends NULL) when it cannot.
    fn push(&mut self, cell: Cell<'_>) -> bool {
        use Cell as C;
        macro_rules! put {
            ($b:expr, $v:expr) => {
                match $v {
                    Some(v) => {
                        $b.append_value(v);
                        true
                    }
                    None => {
                        $b.append_null();
                        false
                    }
                }
            };
        }
        if cell == C::Null {
            self.push_null();
            return true;
        }
        match self {
            Self::Bool(b) => put!(
                b,
                match cell {
                    C::Bool(v) => Some(v),
                    C::Int(v) => Some(v != 0),
                    C::UInt(v) => Some(v != 0),
                    C::Text(t) => parse_bool(t),
                    _ => None,
                }
            ),
            Self::Int16(b) => put!(b, as_i64(cell).and_then(|v| i16::try_from(v).ok())),
            Self::Int32(b) => put!(b, as_i64(cell).and_then(|v| i32::try_from(v).ok())),
            Self::Int64(b) => put!(b, as_i64(cell)),
            Self::UInt64(b) => put!(
                b,
                match cell {
                    C::UInt(v) => Some(v),
                    C::Int(v) => u64::try_from(v).ok(),
                    C::Text(t) => t.trim().parse().ok(),
                    _ => None,
                }
            ),
            Self::Float32(b) => put!(b, as_f64(cell).map(|v| v as f32)),
            Self::Float64(b) => put!(b, as_f64(cell)),
            Self::Decimal(b, scale) => {
                let s = *scale;
                put!(
                    b,
                    match cell {
                        C::Decimal(v) => Some(v),
                        C::Int(v) => 10i128
                            .checked_pow(s.max(0) as u32)
                            .and_then(|p| (v as i128).checked_mul(p)),
                        C::UInt(v) => 10i128
                            .checked_pow(s.max(0) as u32)
                            .and_then(|p| (v as i128).checked_mul(p)),
                        C::Text(t) => parse_decimal(t, s),
                        _ => None,
                    }
                )
            }
            Self::Text(b) => {
                match cell {
                    C::Text(t) => b.append_value(t),
                    C::Bytes(x) => b.append_value(String::from_utf8_lossy(x)),
                    C::Bool(v) => b.append_value(if v { "true" } else { "false" }),
                    C::Int(v) => b.append_value(v.to_string()),
                    C::UInt(v) => b.append_value(v.to_string()),
                    C::Float(v) => b.append_value(v.to_string()),
                    C::Decimal(v) => b.append_value(v.to_string()),
                    C::Date(d) => b.append_value(format_date(d)),
                    C::Time(t) => b.append_value(format_time(t)),
                    C::Timestamp(t) => b.append_value(format_timestamp(t)),
                    C::Null => b.append_null(),
                }
                true
            }
            Self::Binary(b) => put!(
                b,
                match cell {
                    C::Bytes(x) => Some(x),
                    C::Text(t) => Some(t.as_bytes()),
                    _ => None,
                }
            ),
            Self::Date(b) => put!(
                b,
                match cell {
                    C::Date(d) => Some(d),
                    C::Timestamp(t) => i32::try_from(t.div_euclid(86_400_000_000)).ok(),
                    C::Text(t) => parse_date(t),
                    _ => None,
                }
            ),
            Self::Time(b) => put!(
                b,
                match cell {
                    C::Time(t) => Some(t),
                    C::Text(t) => parse_time(t),
                    _ => None,
                }
            ),
            Self::Timestamp(b) => put!(
                b,
                match cell {
                    C::Timestamp(t) => Some(t),
                    C::Date(d) => Some(d as i64 * 86_400_000_000),
                    C::Text(t) => parse_timestamp(t),
                    _ => None,
                }
            ),
        }
    }

    fn push_null(&mut self) {
        match self {
            Self::Bool(b) => b.append_null(),
            Self::Int16(b) => b.append_null(),
            Self::Int32(b) => b.append_null(),
            Self::Int64(b) => b.append_null(),
            Self::UInt64(b) => b.append_null(),
            Self::Float32(b) => b.append_null(),
            Self::Float64(b) => b.append_null(),
            Self::Decimal(b, _) => b.append_null(),
            Self::Text(b) => b.append_null(),
            Self::Binary(b) => b.append_null(),
            Self::Date(b) => b.append_null(),
            Self::Time(b) => b.append_null(),
            Self::Timestamp(b) => b.append_null(),
        }
    }
}

fn as_i64(cell: Cell<'_>) -> Option<i64> {
    match cell {
        Cell::Int(v) => Some(v),
        Cell::UInt(v) => i64::try_from(v).ok(),
        Cell::Bool(v) => Some(v as i64),
        Cell::Float(v) if v.fract() == 0.0 && v.abs() < 9.2e18 => Some(v as i64),
        Cell::Text(t) => t.trim().parse().ok(),
        _ => None,
    }
}

fn as_f64(cell: Cell<'_>) -> Option<f64> {
    match cell {
        Cell::Float(v) => Some(v),
        Cell::Int(v) => Some(v as f64),
        Cell::UInt(v) => Some(v as f64),
        Cell::Text(t) => t.trim().parse().ok(),
        _ => None,
    }
}

fn parse_bool(t: &str) -> Option<bool> {
    match t.trim().to_ascii_lowercase().as_str() {
        "t" | "true" | "1" | "y" | "yes" | "on" => Some(true),
        "f" | "false" | "0" | "n" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Parses `-123.4500` into an unscaled i128 with `scale` digits.
pub fn parse_decimal(t: &str, scale: i8) -> Option<i128> {
    let t = t.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (int, frac) = t.split_once('.').unwrap_or((t, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    if !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let scale = scale.max(0) as usize;
    let mut digits = String::with_capacity(int.len() + scale);
    digits.push_str(int);
    if frac.len() >= scale {
        // Extra digits must be zeros, otherwise precision would be lost.
        if frac[scale..].bytes().any(|b| b != b'0') {
            return None;
        }
        digits.push_str(&frac[..scale]);
    } else {
        digits.push_str(frac);
        digits.extend(std::iter::repeat_n('0', scale - frac.len()));
    }
    let v: i128 = if digits.is_empty() {
        0
    } else {
        digits.parse().ok()?
    };
    Some(if neg { -v } else { v })
}

const EPOCH_DAYS_FROM_CE: i32 = 719_163;

pub fn format_date(days: i32) -> String {
    chrono::NaiveDate::from_num_days_from_ce_opt(days + EPOCH_DAYS_FROM_CE)
        .map(|d| d.to_string())
        .unwrap_or_else(|| days.to_string())
}

pub fn format_time(micros: i64) -> String {
    chrono::NaiveTime::from_num_seconds_from_midnight_opt(
        (micros / 1_000_000) as u32,
        ((micros % 1_000_000) * 1000) as u32,
    )
    .map(|t| t.format("%H:%M:%S%.f").to_string())
    .unwrap_or_else(|| micros.to_string())
}

pub fn format_timestamp(micros: i64) -> String {
    chrono::DateTime::from_timestamp_micros(micros)
        .map(|d| d.naive_utc().format("%Y-%m-%d %H:%M:%S%.f").to_string())
        .unwrap_or_else(|| micros.to_string())
}

pub fn parse_date(t: &str) -> Option<i32> {
    chrono::NaiveDate::parse_from_str(t.trim(), "%Y-%m-%d")
        .ok()
        .map(|d| d.num_days_from_ce() - EPOCH_DAYS_FROM_CE)
}

use chrono::{Datelike as _, Timelike as _};

pub fn parse_time(t: &str) -> Option<i64> {
    let t = chrono::NaiveTime::parse_from_str(t.trim(), "%H:%M:%S%.f").ok()?;
    Some(t.num_seconds_from_midnight() as i64 * 1_000_000 + (t.nanosecond() / 1000) as i64)
}

/// `YYYY-MM-DD[ T]HH:MM:SS[.f][±HH[:MM]|Z]`; values without offset are UTC.
pub fn parse_timestamp(t: &str) -> Option<i64> {
    let t = t.trim();
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(&t.replacen(' ', "T", 1)) {
        return Some(d.timestamp_micros());
    }
    for f in ["%Y-%m-%d %H:%M:%S%.f%#z", "%Y-%m-%d %H:%M:%S%#z"] {
        if let Ok(d) = chrono::DateTime::parse_from_str(t, f) {
            return Some(d.timestamp_micros());
        }
    }
    for f in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(d) = chrono::NaiveDateTime::parse_from_str(t, f) {
            return Some(d.and_utc().timestamp_micros());
        }
    }
    parse_date(t).map(|d| d as i64 * 86_400_000_000)
}

/// Accumulates rows into record batches.
pub struct BatchBuilder {
    schema: SchemaRef,
    builders: Vec<Builder>,
    rows: usize,
    max_rows: usize,
    started: Instant,
    /// Values that could not be represented in their column (pushed as NULL).
    lossy: u64,
}

impl BatchBuilder {
    pub fn new(schema: SchemaRef, max_rows: usize) -> Self {
        let builders = schema
            .fields()
            .iter()
            .map(|f| Builder::new(f.data_type()))
            .collect();
        Self {
            schema,
            builders,
            rows: 0,
            max_rows: max_rows.max(1),
            started: Instant::now(),
            lossy: 0,
        }
    }

    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    /// Sets column `col` of the current row. Every column must be pushed
    /// exactly once per row, in any order is not supported: left to right.
    pub fn push(&mut self, col: usize, cell: Cell<'_>) {
        if !self.builders[col].push(cell) {
            self.lossy += 1;
        }
    }

    /// Completes the current row (pads missing columns with NULL).
    pub fn end_row(&mut self) {
        if self.rows == 0 {
            self.started = Instant::now();
        }
        self.rows += 1;
        for b in &mut self.builders {
            while b.len() < self.rows {
                b.push_null();
            }
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Values replaced by NULL because they did not fit their column.
    pub fn lossy(&self) -> u64 {
        self.lossy
    }

    /// Batch is full or has been filling for longer than [`FLUSH_INTERVAL`].
    pub fn should_flush(&self) -> bool {
        self.rows >= self.max_rows || (self.rows > 0 && self.started.elapsed() >= FLUSH_INTERVAL)
    }

    /// The rows accumulated so far, `None` when there are none.
    pub fn flush(&mut self) -> Option<RecordBatch> {
        if self.rows == 0 {
            return None;
        }
        let cols: Vec<ArrayRef> = self.builders.iter_mut().map(Builder::finish).collect();
        self.rows = 0;
        let batch = if cols.is_empty() {
            RecordBatch::new_empty(self.schema.clone())
        } else {
            RecordBatch::try_new(self.schema.clone(), cols).expect("builders match the schema")
        };
        Some(batch)
    }
}

#[cfg(test)]
mod tests {
    use arrow_array::{Array, Decimal128Array, Int32Array, StringArray, TimestampMicrosecondArray};

    use super::*;

    #[test]
    fn builds_typed_batches_with_coercion() {
        let s = schema(&[
            ColumnSpec::new("i", ColumnType::Int32, "int4"),
            ColumnSpec::new(
                "d",
                ColumnType::Decimal {
                    precision: 10,
                    scale: 2,
                },
                "numeric(10,2)",
            ),
            ColumnSpec::new("t", ColumnType::Text, "text"),
            ColumnSpec::new("ts", ColumnType::Timestamp { tz: true }, "timestamptz"),
        ]);
        assert_eq!(s.field(1).metadata()[DB_TYPE_KEY], "numeric(10,2)");
        let mut b = BatchBuilder::new(s, 1000);
        b.push(0, Cell::Int(7));
        b.push(1, Cell::Text("12.5"));
        b.push(2, Cell::Int(3));
        b.push(3, Cell::Text("2026-11-09 10:00:00+01"));
        b.end_row();
        b.push(0, Cell::Text("not a number"));
        b.push(1, Cell::Decimal(-101));
        b.push(2, Cell::Null);
        b.end_row();
        assert_eq!(b.lossy(), 1);
        let batch = b.flush().unwrap();
        assert_eq!(batch.num_rows(), 2);
        let i = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        assert_eq!((i.value(0), i.is_null(1)), (7, true));
        let d = batch
            .column(1)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap();
        assert_eq!((d.value(0), d.value(1)), (1250, -101));
        let t = batch
            .column(2)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!((t.value(0), t.is_null(1)), ("3", true));
        let ts = batch
            .column(3)
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert_eq!(ts.value(0), 1_794_214_800_000_000);
        assert!(ts.is_null(1));
        assert!(b.flush().is_none());
    }

    #[test]
    fn decimals_parse_exactly() {
        assert_eq!(parse_decimal("123.45", 2), Some(12345));
        assert_eq!(parse_decimal("-0.5", 3), Some(-500));
        assert_eq!(parse_decimal("1.230", 2), Some(123));
        assert_eq!(parse_decimal("1.234", 2), None);
        assert_eq!(parse_decimal("abc", 2), None);
        assert_eq!(parse_decimal(".5", 1), Some(5));
    }

    #[test]
    fn dates_and_times_roundtrip() {
        assert_eq!(parse_date("1970-01-02"), Some(1));
        assert_eq!(format_date(1), "1970-01-02");
        assert_eq!(parse_time("01:00:00.5"), Some(3_600_500_000));
        assert_eq!(format_time(3_600_500_000), "01:00:00.500");
        assert_eq!(parse_timestamp("1970-01-01 00:00:01"), Some(1_000_000));
        assert_eq!(format_timestamp(1_000_000), "1970-01-01 00:00:01");
    }
}
