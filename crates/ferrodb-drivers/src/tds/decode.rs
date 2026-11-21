//! TDS column metadata and values → Arrow (plan, section 5.6).
//!
//! | SQL Server | Arrow |
//! |---|---|
//! | `bit` | Boolean |
//! | `tinyint`, `smallint` / `int` / `bigint` | Int16 / Int32 / Int64 |
//! | `real` / `float` | Float32 / Float64 |
//! | `decimal(p,s)`, `numeric(p,s)` | Decimal128(p, s) |
//! | `money` / `smallmoney` | Decimal128(19, 4) / Decimal128(10, 4) |
//! | `date` | Date32 |
//! | `time(n)` | Time64 µs |
//! | `datetime`, `smalldatetime`, `datetime2(n)` | Timestamp µs |
//! | `datetimeoffset(n)` | Timestamp µs UTC (the instant; the offset is in the type name) |
//! | `char`, `varchar`, `nchar`, `nvarchar`, `text`, `ntext`, `xml`, `uniqueidentifier`, `sql_variant` | Utf8 |
//! | `binary`, `varbinary`, `image`, `timestamp`/`rowversion`, CLR types | Binary |
//!
//! Fractions finer than a microsecond (`datetime2(7)`) are truncated.

use tiberius::{ColumnData, FixedLenType, MetaDataColumn, TypeInfo, VarLenType};

use crate::columns::{Cell, ColumnSpec, ColumnType};

/// Days between 0001-01-01 and 1970-01-01.
const CE_TO_EPOCH_DAYS: i64 = 719_162;
/// Days between 1900-01-01 and 1970-01-01.
const SQL_EPOCH_DAYS: i64 = 25_567;
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// How one result column is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    Plain,
    /// Exact decimal with this scale (values are rescaled to it).
    Decimal(u8),
}

fn len_name(name: &str, len: usize, unicode: bool) -> String {
    if len == 0xFFFF || len > 8000 {
        format!("{name}(max)")
    } else {
        format!("{name}({})", if unicode { len / 2 } else { len })
    }
}

/// Arrow column and database type name of a result column.
pub(crate) fn column(c: &MetaDataColumn<'_>) -> (ColumnSpec, Plan) {
    use ColumnType as T;
    let (ty, db, plan) = match &c.base.ty {
        TypeInfo::FixedLen(f) => match f {
            FixedLenType::Int1 => (T::Int16, "tinyint".into(), Plan::Plain),
            FixedLenType::Bit => (T::Bool, "bit".into(), Plan::Plain),
            FixedLenType::Int2 => (T::Int16, "smallint".into(), Plan::Plain),
            FixedLenType::Int4 => (T::Int32, "int".into(), Plan::Plain),
            FixedLenType::Int8 => (T::Int64, "bigint".into(), Plan::Plain),
            FixedLenType::Float4 => (T::Float32, "real".into(), Plan::Plain),
            FixedLenType::Float8 => (T::Float64, "float".into(), Plan::Plain),
            FixedLenType::Money => (
                T::Decimal {
                    precision: 19,
                    scale: 4,
                },
                "money".into(),
                Plan::Decimal(4),
            ),
            FixedLenType::Money4 => (
                T::Decimal {
                    precision: 10,
                    scale: 4,
                },
                "smallmoney".into(),
                Plan::Decimal(4),
            ),
            FixedLenType::Datetime => (T::Timestamp { tz: false }, "datetime".into(), Plan::Plain),
            FixedLenType::Datetime4 => (
                T::Timestamp { tz: false },
                "smalldatetime".into(),
                Plan::Plain,
            ),
            FixedLenType::Null => (T::Text, "null".into(), Plan::Plain),
        },
        TypeInfo::VarLenSized(cx) => {
            let len = cx.len();
            match cx.r#type() {
                VarLenType::Intn => match len {
                    1 => (T::Int16, "tinyint".into(), Plan::Plain),
                    2 => (T::Int16, "smallint".into(), Plan::Plain),
                    4 => (T::Int32, "int".into(), Plan::Plain),
                    _ => (T::Int64, "bigint".into(), Plan::Plain),
                },
                VarLenType::Bitn => (T::Bool, "bit".into(), Plan::Plain),
                VarLenType::Floatn => match len {
                    4 => (T::Float32, "real".into(), Plan::Plain),
                    _ => (T::Float64, "float".into(), Plan::Plain),
                },
                VarLenType::Money => {
                    if len == 4 {
                        (
                            T::Decimal {
                                precision: 10,
                                scale: 4,
                            },
                            "smallmoney".into(),
                            Plan::Decimal(4),
                        )
                    } else {
                        (
                            T::Decimal {
                                precision: 19,
                                scale: 4,
                            },
                            "money".into(),
                            Plan::Decimal(4),
                        )
                    }
                }
                VarLenType::Datetimen => (
                    T::Timestamp { tz: false },
                    if len == 4 {
                        "smalldatetime"
                    } else {
                        "datetime"
                    }
                    .into(),
                    Plan::Plain,
                ),
                VarLenType::Daten => (T::Date, "date".into(), Plan::Plain),
                VarLenType::Timen => (T::Time, format!("time({len})"), Plan::Plain),
                VarLenType::Datetime2 => (
                    T::Timestamp { tz: false },
                    format!("datetime2({len})"),
                    Plan::Plain,
                ),
                VarLenType::DatetimeOffsetn => (
                    T::Timestamp { tz: true },
                    format!("datetimeoffset({len})"),
                    Plan::Plain,
                ),
                VarLenType::Guid => (T::Text, "uniqueidentifier".into(), Plan::Plain),
                VarLenType::BigVarChar => (T::Text, len_name("varchar", len, false), Plan::Plain),
                VarLenType::BigChar => (T::Text, len_name("char", len, false), Plan::Plain),
                VarLenType::NVarchar => (T::Text, len_name("nvarchar", len, true), Plan::Plain),
                VarLenType::NChar => (T::Text, len_name("nchar", len, true), Plan::Plain),
                VarLenType::Text => (T::Text, "text".into(), Plan::Plain),
                VarLenType::NText => (T::Text, "ntext".into(), Plan::Plain),
                VarLenType::BigVarBin => {
                    (T::Binary, len_name("varbinary", len, false), Plan::Plain)
                }
                VarLenType::BigBinary => (T::Binary, len_name("binary", len, false), Plan::Plain),
                VarLenType::Image => (T::Binary, "image".into(), Plan::Plain),
                VarLenType::Xml => (T::Text, "xml".into(), Plan::Plain),
                VarLenType::Udt => (T::Binary, "udt".into(), Plan::Plain),
                VarLenType::SSVariant => (T::Text, "sql_variant".into(), Plan::Plain),
                other => (T::Text, format!("{other:?}").to_lowercase(), Plan::Plain),
            }
        }
        TypeInfo::VarLenSizedPrecision {
            ty,
            precision,
            scale,
            ..
        } => {
            let name = if *ty == VarLenType::Numericn {
                "numeric"
            } else {
                "decimal"
            };
            let p = (*precision).clamp(1, 38);
            let s = (*scale).min(p);
            (
                T::Decimal {
                    precision: p,
                    scale: s as i8,
                },
                format!("{name}({precision},{scale})"),
                Plan::Decimal(s),
            )
        }
        TypeInfo::Xml { .. } => (T::Text, "xml".into(), Plan::Plain),
    };
    (ColumnSpec::new(c.col_name.as_ref(), ty, db), plan)
}

/// Unscaled `value` (with `from` digits) rescaled to `to` digits; `None`
/// when digits would be lost.
fn rescale(value: i128, from: u8, to: u8) -> Option<i128> {
    if from == to {
        Some(value)
    } else if from < to {
        value.checked_mul(10i128.checked_pow(u32::from(to - from))?)
    } else {
        let d = 10i128.checked_pow(u32::from(from - to))?;
        (value % d == 0).then_some(value / d)
    }
}

/// `uniqueidentifier` the way SQL Server prints it.
pub(crate) fn guid_text(u: &tiberius::Uuid) -> String {
    u.hyphenated().to_string().to_uppercase()
}

fn time_micros(t: tiberius::time::Time) -> i64 {
    let inc = t.increments() as i64;
    match t.scale() {
        s @ 0..=6 => inc * 10i64.pow(6 - u32::from(s)),
        s => inc / 10i64.pow(u32::from(s) - 6),
    }
}

fn datetime2_micros(dt: tiberius::time::DateTime2) -> i64 {
    (dt.date().days() as i64 - CE_TO_EPOCH_DAYS) * MICROS_PER_DAY + time_micros(dt.time())
}

/// One value. `scratch` holds rendered text for the returned cell.
pub(crate) fn cell<'a>(
    plan: Plan,
    v: &'a ColumnData<'static>,
    scratch: &'a mut String,
) -> Cell<'a> {
    fn text(scratch: &mut String, s: String) -> Cell<'_> {
        *scratch = s;
        Cell::Text(scratch.as_str())
    }
    match v {
        ColumnData::U8(Some(x)) => Cell::Int(i64::from(*x)),
        ColumnData::I16(Some(x)) => Cell::Int(i64::from(*x)),
        ColumnData::I32(Some(x)) => Cell::Int(i64::from(*x)),
        ColumnData::I64(Some(x)) => Cell::Int(*x),
        ColumnData::F32(Some(x)) => Cell::Float(f64::from(*x)),
        ColumnData::F64(Some(x)) => Cell::Float(*x),
        ColumnData::Bit(Some(x)) => Cell::Bool(*x),
        ColumnData::String(Some(s)) => Cell::Text(s.as_ref()),
        ColumnData::Guid(Some(u)) => text(scratch, guid_text(u)),
        ColumnData::Binary(Some(b)) => Cell::Bytes(b.as_ref()),
        ColumnData::Numeric(Some(n)) => match plan {
            Plan::Decimal(s) => match rescale(n.value(), n.scale(), s) {
                Some(v) => Cell::Decimal(v),
                None => text(scratch, n.to_string()),
            },
            Plan::Plain => text(scratch, n.to_string()),
        },
        ColumnData::Xml(Some(x)) => text(scratch, x.to_string()),
        ColumnData::DateTime(Some(dt)) => Cell::Timestamp(
            (dt.days() as i64 - SQL_EPOCH_DAYS) * MICROS_PER_DAY
                // 1/300 s, shown by SQL Server rounded to milliseconds.
                + (dt.seconds_fragments() as i64 * 10 + 1) / 3 * 1000,
        ),
        ColumnData::SmallDateTime(Some(dt)) => Cell::Timestamp(
            (dt.days() as i64 - SQL_EPOCH_DAYS) * MICROS_PER_DAY
                + dt.seconds_fragments() as i64 * 60_000_000,
        ),
        ColumnData::Date(Some(d)) => Cell::Date((d.days() as i64 - CE_TO_EPOCH_DAYS) as i32),
        ColumnData::Time(Some(t)) => Cell::Time(time_micros(*t)),
        ColumnData::DateTime2(Some(dt)) => Cell::Timestamp(datetime2_micros(*dt)),
        // Stored as UTC plus the offset: the instant is the UTC value.
        ColumnData::DateTimeOffset(Some(dto)) => Cell::Timestamp(datetime2_micros(dto.datetime2())),
        _ => Cell::Null,
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use tiberius::{BaseMetaDataColumn, numeric::Numeric, time};

    use super::*;

    fn meta(ty: TypeInfo) -> MetaDataColumn<'static> {
        MetaDataColumn {
            base: BaseMetaDataColumn {
                flags: Default::default(),
                ty,
            },
            col_name: Cow::Borrowed("c"),
        }
    }

    #[test]
    fn column_types() {
        let (spec, plan) = column(&meta(TypeInfo::VarLenSizedPrecision {
            ty: VarLenType::Decimaln,
            size: 9,
            precision: 10,
            scale: 2,
        }));
        assert_eq!(
            (spec.ty, spec.db_type.as_str(), plan),
            (
                ColumnType::Decimal {
                    precision: 10,
                    scale: 2
                },
                "decimal(10,2)",
                Plan::Decimal(2)
            )
        );
        let (spec, _) = column(&meta(TypeInfo::VarLenSized(tiberius::VarLenContext::new(
            VarLenType::NVarchar,
            40,
            None,
        ))));
        assert_eq!(spec.db_type, "nvarchar(20)");
        let (spec, _) = column(&meta(TypeInfo::VarLenSized(tiberius::VarLenContext::new(
            VarLenType::BigVarBin,
            0xFFFF,
            None,
        ))));
        assert_eq!(
            (spec.ty, spec.db_type.as_str()),
            (ColumnType::Binary, "varbinary(max)")
        );
        let (spec, _) = column(&meta(TypeInfo::FixedLen(FixedLenType::Money)));
        assert_eq!(spec.db_type, "money");
    }

    #[test]
    fn values() {
        let mut s = String::new();
        let n = ColumnData::Numeric(Some(Numeric::new_with_scale(125, 1)));
        assert_eq!(cell(Plan::Decimal(2), &n, &mut s), Cell::Decimal(1250));
        // 1970-01-01 00:00:01 as datetime: 300 fragments.
        let dt = ColumnData::DateTime(Some(time::DateTime::new(25_567, 300)));
        assert_eq!(cell(Plan::Plain, &dt, &mut s), Cell::Timestamp(1_000_000));
        let d2 = ColumnData::DateTime2(Some(time::DateTime2::new(
            time::Date::new(719_162),
            time::Time::new(15, 7),
        )));
        assert_eq!(cell(Plan::Plain, &d2, &mut s), Cell::Timestamp(1));
        let g = ColumnData::Guid(Some(tiberius::Uuid::nil()));
        assert_eq!(
            cell(Plan::Plain, &g, &mut s),
            Cell::Text("00000000-0000-0000-0000-000000000000")
        );
        assert_eq!(
            cell(Plan::Plain, &ColumnData::I32(None), &mut s),
            Cell::Null
        );
        assert_eq!(rescale(12345, 4, 2), None);
    }
}
