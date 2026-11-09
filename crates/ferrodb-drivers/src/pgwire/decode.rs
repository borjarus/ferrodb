//! PostgreSQL binary result format → Arrow cells.
//!
//! Every column is read as raw bytes ([`Raw`]) and decoded here by type OID,
//! so no value is ever rejected by a `FromSql` impl and no precision is lost
//! (plan, section 5.6). Types without a natural Arrow representation become
//! text rendered the way `psql` shows them.

use std::{borrow::Cow, error::Error as StdError, fmt::Write as _};

use tokio_postgres::types::{FromSql, Kind, Type};

use crate::columns::{Cell, ColumnSpec, ColumnType};

/// Any value as its raw binary representation.
pub(crate) struct Raw<'a>(pub &'a [u8]);

impl<'a> FromSql<'a> for Raw<'a> {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn StdError + Sync + Send>> {
        Ok(Raw(raw))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

/// Built-in type OIDs (`pg_type.oid`).
mod oid {
    pub const BOOL: u32 = 16;
    pub const BYTEA: u32 = 17;
    pub const CHAR: u32 = 18;
    pub const NAME: u32 = 19;
    pub const INT8: u32 = 20;
    pub const INT2: u32 = 21;
    pub const INT4: u32 = 23;
    pub const REGPROC: u32 = 24;
    pub const TEXT: u32 = 25;
    pub const OID: u32 = 26;
    pub const XID: u32 = 28;
    pub const CID: u32 = 29;
    pub const JSON: u32 = 114;
    pub const XML: u32 = 142;
    pub const POINT: u32 = 600;
    pub const LSEG: u32 = 601;
    pub const PATH: u32 = 602;
    pub const BOX: u32 = 603;
    pub const POLYGON: u32 = 604;
    pub const LINE: u32 = 628;
    pub const CIDR: u32 = 650;
    pub const FLOAT4: u32 = 700;
    pub const FLOAT8: u32 = 701;
    pub const UNKNOWN: u32 = 705;
    pub const CIRCLE: u32 = 718;
    pub const MACADDR8: u32 = 774;
    pub const MONEY: u32 = 790;
    pub const MACADDR: u32 = 829;
    pub const INET: u32 = 869;
    pub const BPCHAR: u32 = 1042;
    pub const VARCHAR: u32 = 1043;
    pub const DATE: u32 = 1082;
    pub const TIME: u32 = 1083;
    pub const TIMESTAMP: u32 = 1114;
    pub const TIMESTAMPTZ: u32 = 1184;
    pub const INTERVAL: u32 = 1186;
    pub const TIMETZ: u32 = 1266;
    pub const BIT: u32 = 1560;
    pub const VARBIT: u32 = 1562;
    pub const NUMERIC: u32 = 1700;
    pub const REGPROCEDURE: u32 = 2202;
    pub const REGOPER: u32 = 2203;
    pub const REGOPERATOR: u32 = 2204;
    pub const REGCLASS: u32 = 2205;
    pub const REGTYPE: u32 = 2206;
    pub const UUID: u32 = 2950;
    pub const PG_LSN: u32 = 3220;
    pub const JSONB: u32 = 3802;
    pub const REGNAMESPACE: u32 = 4089;
    pub const REGROLE: u32 = 4096;
    pub const JSONPATH: u32 = 4072;
    pub const XID8: u32 = 5069;
}

/// Days between 1970-01-01 and 2000-01-01 (the PostgreSQL epoch).
pub(crate) const PG_EPOCH_DAYS: i32 = 10_957;
/// Microseconds between the Unix and PostgreSQL epochs (946684800 s).
pub(crate) const PG_EPOCH_MICROS: i64 = 946_684_800_000_000;

/// How one result column is decoded.
#[derive(Clone, Debug)]
pub(crate) enum Plan {
    Bool,
    Int2,
    Int4,
    Int8,
    /// Unsigned 32-bit (oid) widened to Int64.
    Oid,
    Float4,
    Float8,
    /// numeric with a known precision ≤ 38: pushed into a Decimal column.
    Decimal,
    Bytea,
    Date,
    Time,
    Timestamp,
    /// Everything else, rendered as text.
    Text(Type),
}

/// Base type of a domain (domains share the binary format of their base).
fn base(ty: &Type) -> &Type {
    match ty.kind() {
        Kind::Domain(inner) => base(inner),
        _ => ty,
    }
}

/// `numeric(p,s)` from a type modifier (`None` for unconstrained numeric).
pub(crate) fn numeric_typmod(typmod: i32) -> Option<(i32, i32)> {
    if typmod < 4 {
        return None;
    }
    let m = typmod - 4;
    let precision = (m >> 16) & 0xffff;
    // PostgreSQL 15+ stores the scale as an 11-bit signed number.
    let scale = (((m & 0x7ff) ^ 1024) - 1024) as i16 as i32;
    Some((precision, scale))
}

/// Database type name shown in the grid header, e.g. `numeric(10,2)` or
/// `int4[]`.
pub(crate) fn db_type_name(ty: &Type, typmod: i32) -> String {
    if let Kind::Array(elem) = ty.kind() {
        return format!("{}[]", db_type_name(elem, typmod));
    }
    let name = ty.name();
    match ty.oid() {
        oid::VARCHAR | oid::BPCHAR if typmod > 4 => format!("{name}({})", typmod - 4),
        oid::BIT | oid::VARBIT if typmod > 0 => format!("{name}({typmod})"),
        oid::NUMERIC => match numeric_typmod(typmod) {
            Some((p, s)) => format!("{name}({p},{s})"),
            None => name.to_owned(),
        },
        _ => name.to_owned(),
    }
}

/// Arrow column and decoding plan for a result column.
pub(crate) fn plan(name: &str, ty: &Type, typmod: i32) -> (ColumnSpec, Plan) {
    let b = base(ty);
    let (ct, plan) = match b.oid() {
        oid::BOOL => (ColumnType::Bool, Plan::Bool),
        oid::INT2 => (ColumnType::Int16, Plan::Int2),
        oid::INT4 => (ColumnType::Int32, Plan::Int4),
        oid::INT8 => (ColumnType::Int64, Plan::Int8),
        oid::OID => (ColumnType::Int64, Plan::Oid),
        oid::FLOAT4 => (ColumnType::Float32, Plan::Float4),
        oid::FLOAT8 => (ColumnType::Float64, Plan::Float8),
        oid::NUMERIC => match numeric_typmod(typmod) {
            Some((p, s)) if (1..=38).contains(&p) && (0..=p).contains(&s) => (
                ColumnType::Decimal {
                    precision: p as u8,
                    scale: s as i8,
                },
                Plan::Decimal,
            ),
            _ => (ColumnType::Text, Plan::Text(b.clone())),
        },
        oid::BYTEA => (ColumnType::Binary, Plan::Bytea),
        oid::DATE => (ColumnType::Date, Plan::Date),
        oid::TIME => (ColumnType::Time, Plan::Time),
        oid::TIMESTAMP => (ColumnType::Timestamp { tz: false }, Plan::Timestamp),
        oid::TIMESTAMPTZ => (ColumnType::Timestamp { tz: true }, Plan::Timestamp),
        _ => (ColumnType::Text, Plan::Text(b.clone())),
    };
    (ColumnSpec::new(name, ct, db_type_name(ty, typmod)), plan)
}

/// A value that no column type accepts: counted as lossy by the builder.
const LOSSY: Cell<'static> = Cell::Bytes(&[]);

/// Decodes one non-NULL value. `scratch` holds rendered text for the
/// lifetime of the returned cell.
pub(crate) fn cell<'a>(plan: &Plan, raw: &'a [u8], scratch: &'a mut String) -> Cell<'a> {
    let c = match plan {
        Plan::Bool => raw.first().map(|b| Cell::Bool(*b != 0)),
        Plan::Int2 => be::<2>(raw).map(|b| Cell::Int(i16::from_be_bytes(b) as i64)),
        Plan::Int4 => be::<4>(raw).map(|b| Cell::Int(i32::from_be_bytes(b) as i64)),
        Plan::Int8 => be::<8>(raw).map(|b| Cell::Int(i64::from_be_bytes(b))),
        Plan::Oid => be::<4>(raw).map(|b| Cell::Int(u32::from_be_bytes(b) as i64)),
        Plan::Float4 => be::<4>(raw).map(|b| Cell::Float(f32::from_be_bytes(b) as f64)),
        Plan::Float8 => be::<8>(raw).map(|b| Cell::Float(f64::from_be_bytes(b))),
        // NaN / Infinity do not fit a Decimal column: pushed as text they
        // fail to parse and are counted as lossy.
        Plan::Decimal => match numeric(raw) {
            Some(s) => {
                *scratch = s;
                Some(Cell::Text(scratch.as_str()))
            }
            None => None,
        },
        Plan::Bytea => Some(Cell::Bytes(raw)),
        Plan::Date => be::<4>(raw).map(|b| match i32::from_be_bytes(b) {
            i32::MAX | i32::MIN => LOSSY,
            d => d
                .checked_add(PG_EPOCH_DAYS)
                .map(Cell::Date)
                .unwrap_or(LOSSY),
        }),
        Plan::Time => be::<8>(raw).map(|b| Cell::Time(i64::from_be_bytes(b))),
        Plan::Timestamp => be::<8>(raw).map(|b| match i64::from_be_bytes(b) {
            i64::MAX | i64::MIN => LOSSY,
            t => t
                .checked_add(PG_EPOCH_MICROS)
                .map(Cell::Timestamp)
                .unwrap_or(LOSSY),
        }),
        Plan::Text(ty) => {
            return match text(ty, raw) {
                Cow::Borrowed(s) => Cell::Text(s),
                Cow::Owned(s) => {
                    *scratch = s;
                    Cell::Text(scratch.as_str())
                }
            };
        }
    };
    c.unwrap_or(LOSSY)
}

fn be<const N: usize>(raw: &[u8]) -> Option<[u8; N]> {
    raw.try_into().ok()
}

/// Reads big-endian values from a binary value.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (h, t) = self.0.split_at(n);
        self.0 = t;
        Some(h)
    }
    fn arr<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.arr::<1>()?[0])
    }
    fn i16(&mut self) -> Option<i16> {
        self.arr().map(i16::from_be_bytes)
    }
    fn u16(&mut self) -> Option<u16> {
        self.arr().map(u16::from_be_bytes)
    }
    fn i32(&mut self) -> Option<i32> {
        self.arr().map(i32::from_be_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.arr().map(u32::from_be_bytes)
    }
    fn i64(&mut self) -> Option<i64> {
        self.arr().map(i64::from_be_bytes)
    }
    fn f64(&mut self) -> Option<f64> {
        self.arr().map(f64::from_be_bytes)
    }
    /// A length-prefixed value; `Some(None)` is NULL.
    fn value(&mut self) -> Option<Option<&'a [u8]>> {
        let len = self.i32()?;
        if len < 0 {
            return Some(None);
        }
        self.take(len as usize).map(Some)
    }
}

/// `\x…` — PostgreSQL's hex output for bytes.
pub(crate) fn hex(raw: &[u8]) -> String {
    let mut s = String::with_capacity(2 + raw.len() * 2);
    s.push_str("\\x");
    for b in raw {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Text for a value whose type has no binary decoder: UTF-8 when it looks
/// like text, hex otherwise.
fn fallback(raw: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(raw) {
        Ok(s)
            if !s
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) =>
        {
            Cow::Borrowed(s)
        }
        _ => Cow::Owned(hex(raw)),
    }
}

fn utf8(raw: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(raw)
}

/// Renders a value as PostgreSQL text output (as `psql` shows it).
pub(crate) fn text<'a>(ty: &Type, raw: &'a [u8]) -> Cow<'a, str> {
    match ty.kind() {
        Kind::Domain(inner) => return text(inner, raw),
        Kind::Enum(_) => return utf8(raw),
        Kind::Array(elem) => return own(array(elem, raw), raw),
        Kind::Range(sub) => return own(range(sub, raw), raw),
        Kind::Multirange(sub) => return own(multirange(sub, raw), raw),
        Kind::Composite(fields) => {
            let types: Vec<Type> = fields.iter().map(|f| f.type_().clone()).collect();
            return own(record(Some(types.as_slice()), raw), raw);
        }
        _ => {}
    }
    let mut r = Reader(raw);
    let s: Option<String> = match ty.oid() {
        oid::TEXT
        | oid::VARCHAR
        | oid::BPCHAR
        | oid::NAME
        | oid::UNKNOWN
        | oid::XML
        | oid::JSON
        | oid::CHAR => return utf8(raw),
        oid::JSONB | oid::JSONPATH => {
            return match raw.split_first() {
                Some((1, rest)) => utf8(rest),
                _ => Cow::Owned(hex(raw)),
            };
        }
        oid::BOOL => raw
            .first()
            .map(|b| if *b != 0 { "t" } else { "f" }.to_owned()),
        oid::INT2 => r.i16().map(|v| v.to_string()),
        oid::INT4 => r.i32().map(|v| v.to_string()),
        oid::INT8 => r.i64().map(|v| v.to_string()),
        oid::MONEY => r.i64().map(money),
        oid::OID
        | oid::XID
        | oid::CID
        | oid::REGPROC
        | oid::REGPROCEDURE
        | oid::REGOPER
        | oid::REGOPERATOR
        | oid::REGCLASS
        | oid::REGTYPE
        | oid::REGNAMESPACE
        | oid::REGROLE => r.u32().map(|v| v.to_string()),
        oid::XID8 => r.arr().map(|b| u64::from_be_bytes(b).to_string()),
        oid::PG_LSN => r.arr().map(|b| {
            let v = u64::from_be_bytes(b);
            format!("{:X}/{:X}", v >> 32, v & 0xffff_ffff)
        }),
        oid::FLOAT4 => r.arr().map(|b| float(f32::from_be_bytes(b) as f64)),
        oid::FLOAT8 => r.f64().map(float),
        oid::NUMERIC => numeric(raw),
        oid::BYTEA => Some(hex(raw)),
        oid::DATE => r.i32().map(date),
        oid::TIME => r.i64().map(time),
        oid::TIMETZ => (|| Some(timetz(r.i64()?, r.i32()?)))(),
        oid::TIMESTAMP => r.i64().map(|t| timestamp(t, false)),
        oid::TIMESTAMPTZ => r.i64().map(|t| timestamp(t, true)),
        oid::INTERVAL => interval(raw),
        oid::UUID => uuid(raw),
        oid::INET | oid::CIDR => inet(raw),
        oid::MACADDR | oid::MACADDR8 if raw.len() == 6 || raw.len() == 8 => Some(
            raw.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":"),
        ),
        oid::BIT | oid::VARBIT => bits(raw),
        oid::POINT => point(&mut r),
        oid::LSEG => (|| Some(format!("[{},{}]", point(&mut r)?, point(&mut r)?)))(),
        oid::BOX => (|| Some(format!("{},{}", point(&mut r)?, point(&mut r)?)))(),
        oid::LINE => (|| {
            Some(format!(
                "{{{},{},{}}}",
                float(r.f64()?),
                float(r.f64()?),
                float(r.f64()?)
            ))
        })(),
        oid::CIRCLE => (|| Some(format!("<{},{}>", point(&mut r)?, float(r.f64()?))))(),
        oid::PATH => (|| {
            let closed = r.u8()? != 0;
            let pts = points(&mut r)?;
            Some(if closed {
                format!("({pts})")
            } else {
                format!("[{pts}]")
            })
        })(),
        oid::POLYGON => points(&mut r).map(|p| format!("({p})")),
        // Anonymous records carry their field types inline.
        2249 => record(None, raw),
        _ if ty.name() == "citext" => return utf8(raw),
        _ => return fallback(raw),
    };
    own(s, raw)
}

fn own(s: Option<String>, raw: &[u8]) -> Cow<'static, str> {
    Cow::Owned(s.unwrap_or_else(|| hex(raw)))
}

/// float8out: shortest round-trip digits; `NaN`, `Infinity`.
pub(crate) fn float(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else {
        v.to_string()
    }
}

/// money as a plain decimal with two fractional digits (the server's
/// `lc_monetary` is unknown here).
fn money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let a = cents.unsigned_abs();
    format!("{sign}{}.{:02}", a / 100, a % 100)
}

fn point(r: &mut Reader<'_>) -> Option<String> {
    Some(format!("({},{})", float(r.f64()?), float(r.f64()?)))
}

fn points(r: &mut Reader<'_>) -> Option<String> {
    let n = r.i32()?;
    let mut out = Vec::new();
    for _ in 0..n.max(0) {
        out.push(point(r)?);
    }
    Some(out.join(","))
}

fn bits(raw: &[u8]) -> Option<String> {
    let mut r = Reader(raw);
    let n = r.i32()?.max(0) as usize;
    let bytes = r.0;
    if bytes.len() * 8 < n {
        return None;
    }
    Some(
        (0..n)
            .map(|i| {
                if bytes[i / 8] & (0x80 >> (i % 8)) != 0 {
                    '1'
                } else {
                    '0'
                }
            })
            .collect(),
    )
}

fn uuid(raw: &[u8]) -> Option<String> {
    if raw.len() != 16 {
        return None;
    }
    let h: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

fn inet(raw: &[u8]) -> Option<String> {
    let mut r = Reader(raw);
    let family = r.u8()?;
    let bits = r.u8()?;
    let is_cidr = r.u8()? != 0;
    let n = r.u8()? as usize;
    let addr = r.take(n)?;
    let (ip, full) = match (family, n) {
        (2, 4) => (
            std::net::Ipv4Addr::from(<[u8; 4]>::try_from(addr).ok()?).to_string(),
            32,
        ),
        (3, 16) => (
            std::net::Ipv6Addr::from(<[u8; 16]>::try_from(addr).ok()?).to_string(),
            128,
        ),
        _ => return None,
    };
    Some(if is_cidr || bits != full {
        format!("{ip}/{bits}")
    } else {
        ip
    })
}

/// Binary numeric → exact decimal string (`NaN`, `Infinity`, `-Infinity`).
pub(crate) fn numeric(raw: &[u8]) -> Option<String> {
    let mut r = Reader(raw);
    let ndigits = r.u16()? as usize;
    let weight = r.i16()? as i32;
    let sign = r.u16()?;
    let dscale = r.u16()? as usize;
    let digits: Vec<i16> = (0..ndigits).map(|_| r.i16()).collect::<Option<_>>()?;
    let mut s = String::new();
    match sign {
        0x0000 => {}
        0x4000 => s.push('-'),
        0xC000 => return Some("NaN".into()),
        0xD000 => return Some("Infinity".into()),
        0xF000 => return Some("-Infinity".into()),
        _ => return None,
    }
    let digit = |i: i32| -> i16 {
        if i >= 0 {
            digits.get(i as usize).copied().unwrap_or(0)
        } else {
            0
        }
    };
    if weight < 0 {
        s.push('0');
    } else {
        for i in 0..=weight {
            if i == 0 {
                let _ = write!(s, "{}", digit(i));
            } else {
                let _ = write!(s, "{:04}", digit(i));
            }
        }
    }
    if dscale > 0 {
        s.push('.');
        let start = s.len();
        let mut i = weight + 1;
        while s.len() - start < dscale {
            let _ = write!(s, "{:04}", digit(i));
            i += 1;
        }
        s.truncate(start + dscale);
    }
    Some(s)
}

/// `YYYY-MM-DD`, with ` BC` for years ≤ 0 as PostgreSQL prints them.
fn ymd(d: chrono::NaiveDate) -> (String, bool) {
    use chrono::Datelike as _;
    let y = d.year();
    let bc = y <= 0;
    let y = if bc { 1 - y } else { y };
    (format!("{y:04}-{:02}-{:02}", d.month(), d.day()), bc)
}

const UNIX_EPOCH_CE_DAYS: i32 = 719_163;

/// PostgreSQL date (days since 2000-01-01) as text.
pub(crate) fn date(days: i32) -> String {
    match days {
        i32::MAX => return "infinity".into(),
        i32::MIN => return "-infinity".into(),
        _ => {}
    }
    match days
        .checked_add(PG_EPOCH_DAYS + UNIX_EPOCH_CE_DAYS)
        .and_then(chrono::NaiveDate::from_num_days_from_ce_opt)
    {
        Some(d) => {
            let (s, bc) = ymd(d);
            if bc { format!("{s} BC") } else { s }
        }
        None => days.to_string(),
    }
}

/// `HH:MM:SS[.ffffff]` (trailing zeros trimmed); hours may reach 24.
pub(crate) fn time(micros: i64) -> String {
    let secs = micros.div_euclid(1_000_000);
    let frac = micros.rem_euclid(1_000_000);
    let mut s = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    push_frac(&mut s, frac);
    s
}

fn push_frac(s: &mut String, frac: i64) {
    if frac != 0 {
        let f = format!("{frac:06}");
        s.push('.');
        s.push_str(f.trim_end_matches('0'));
    }
}

/// `zone` is seconds *west* of UTC (the wire format).
fn timetz(micros: i64, zone: i32) -> String {
    let mut s = time(micros);
    push_offset(&mut s, -zone);
    s
}

fn push_offset(s: &mut String, east: i32) {
    let sign = if east < 0 { '-' } else { '+' };
    let a = east.unsigned_abs();
    let _ = write!(s, "{sign}{:02}", a / 3600);
    if !a.is_multiple_of(3600) {
        let _ = write!(s, ":{:02}", a / 60 % 60);
        if !a.is_multiple_of(60) {
            let _ = write!(s, ":{:02}", a % 60);
        }
    }
}

/// PostgreSQL timestamp (µs since 2000-01-01) as text; `tz` values are
/// shown in UTC (`+00`).
pub(crate) fn timestamp(micros: i64, tz: bool) -> String {
    match micros {
        i64::MAX => return "infinity".into(),
        i64::MIN => return "-infinity".into(),
        _ => {}
    }
    let Some(dt) = micros
        .checked_add(PG_EPOCH_MICROS)
        .and_then(chrono::DateTime::from_timestamp_micros)
    else {
        return micros.to_string();
    };
    let dt = dt.naive_utc();
    let (d, bc) = ymd(dt.date());
    let unix = micros + PG_EPOCH_MICROS;
    let mut s = format!("{d} {}", time(unix.rem_euclid(86_400_000_000)));
    if tz {
        s.push_str("+00");
    }
    if bc {
        s.push_str(" BC");
    }
    s
}

fn interval(raw: &[u8]) -> Option<String> {
    let mut r = Reader(raw);
    let micros = r.i64()?;
    let days = r.i32()?;
    let months = r.i32()?;
    Some(format_interval(months, days, micros))
}

/// Interval in PostgreSQL's default `IntervalStyle` (`postgres`), e.g.
/// `1 year 2 mons 3 days 04:05:06.5`.
pub(crate) fn format_interval(months: i32, days: i32, micros: i64) -> String {
    if (months, days, micros) == (i32::MAX, i32::MAX, i64::MAX) {
        return "infinity".into();
    }
    if (months, days, micros) == (i32::MIN, i32::MIN, i64::MIN) {
        return "-infinity".into();
    }
    let mut s = String::new();
    let mut is_zero = true;
    let mut is_before = false;
    let mut part = |s: &mut String, v: i64, unit: &str| {
        if v == 0 {
            return;
        }
        let _ = write!(
            s,
            "{}{}{v} {unit}{}",
            if is_zero { "" } else { " " },
            if is_before && v > 0 { "+" } else { "" },
            if v != 1 { "s" } else { "" }
        );
        is_before = v < 0;
        is_zero = false;
    };
    part(&mut s, (months / 12) as i64, "year");
    part(&mut s, (months % 12) as i64, "mon");
    part(&mut s, days as i64, "day");
    if is_zero || micros != 0 {
        let hours = micros / 3_600_000_000;
        let rem = micros % 3_600_000_000;
        let mins = rem / 60_000_000;
        let rem = rem % 60_000_000;
        let secs = rem / 1_000_000;
        let frac = rem % 1_000_000;
        let _ = write!(
            s,
            "{}{}{:02}:{:02}:{:02}",
            if is_zero { "" } else { " " },
            if micros < 0 {
                "-"
            } else if is_before {
                "+"
            } else {
                ""
            },
            hours.unsigned_abs(),
            mins.unsigned_abs(),
            secs.unsigned_abs()
        );
        push_frac(&mut s, frac.abs());
    }
    s
}

/// Array in PostgreSQL literal syntax: `{1,2,NULL}`, `{{1,2},{3,4}}`,
/// `[0:1]={a,b}` for non-default lower bounds.
pub(crate) fn array(elem: &Type, raw: &[u8]) -> Option<String> {
    let mut r = Reader(raw);
    let ndim = r.i32()?;
    let _has_nulls = r.i32()?;
    let _elem_oid = r.u32()?;
    if ndim <= 0 {
        return Some("{}".into());
    }
    let mut dims = Vec::with_capacity(ndim as usize);
    for _ in 0..ndim {
        let len = r.i32()?;
        let lower = r.i32()?;
        dims.push((len.max(0) as usize, lower));
    }
    let delim = if base(elem).oid() == oid::BOX {
        ';'
    } else {
        ','
    };
    let mut s = String::new();
    if dims.iter().any(|(_, lb)| *lb != 1) {
        for (len, lb) in &dims {
            let _ = write!(s, "[{lb}:{}]", *lb as i64 + *len as i64 - 1);
        }
        s.push('=');
    }
    fn level(
        s: &mut String,
        r: &mut Reader<'_>,
        dims: &[(usize, i32)],
        elem: &Type,
        delim: char,
    ) -> Option<()> {
        s.push('{');
        for i in 0..dims[0].0 {
            if i > 0 {
                s.push(delim);
            }
            if dims.len() > 1 {
                level(s, r, &dims[1..], elem, delim)?;
            } else {
                match r.value()? {
                    None => s.push_str("NULL"),
                    Some(v) => quote_array_elem(s, &text(elem, v), delim),
                }
            }
        }
        s.push('}');
        Some(())
    }
    level(&mut s, &mut r, &dims, elem, delim)?;
    Some(s)
}

fn quote_array_elem(s: &mut String, v: &str, delim: char) {
    let needs = v.is_empty()
        || v.eq_ignore_ascii_case("NULL")
        || v.chars()
            .any(|c| matches!(c, '{' | '}' | '"' | '\\') || c == delim || c.is_ascii_whitespace());
    if !needs {
        s.push_str(v);
        return;
    }
    s.push('"');
    for c in v.chars() {
        if c == '"' || c == '\\' {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
}

/// Composite / record value: `(1,"a b",)`. `types` come from the column
/// type; anonymous records use the OIDs on the wire.
fn record(types: Option<&[Type]>, raw: &[u8]) -> Option<String> {
    let mut r = Reader(raw);
    let n = r.i32()?;
    let mut s = String::from("(");
    for i in 0..n.max(0) as usize {
        if i > 0 {
            s.push(',');
        }
        let field_oid = r.u32()?;
        let Some(v) = r.value()? else { continue };
        let ty = types
            .and_then(|t| t.get(i).cloned())
            .or_else(|| Type::from_oid(field_oid));
        let t = match &ty {
            Some(ty) => text(ty, v),
            None => fallback(v),
        };
        let needs = t.is_empty()
            || t.chars()
                .any(|c| matches!(c, '(' | ')' | ',' | '"' | '\\') || c.is_ascii_whitespace());
        if needs {
            s.push('"');
            for c in t.chars() {
                if c == '"' || c == '\\' {
                    s.push(c);
                }
                s.push(c);
            }
            s.push('"');
        } else {
            s.push_str(&t);
        }
    }
    s.push(')');
    Some(s)
}

fn range(sub: &Type, raw: &[u8]) -> Option<String> {
    const EMPTY: u8 = 0x01;
    const LB_INC: u8 = 0x02;
    const UB_INC: u8 = 0x04;
    const LB_INF: u8 = 0x08;
    const UB_INF: u8 = 0x10;
    let mut r = Reader(raw);
    let flags = r.u8()?;
    if flags & EMPTY != 0 {
        return Some("empty".into());
    }
    let mut s = String::new();
    s.push(if flags & LB_INC != 0 { '[' } else { '(' });
    let bound = |s: &mut String, r: &mut Reader<'_>| -> Option<()> {
        let v = r.value()??;
        let t = text(sub, v);
        let needs = t.is_empty()
            || t.chars().any(|c| {
                matches!(c, '(' | ')' | '[' | ']' | ',' | '"' | '\\') || c.is_ascii_whitespace()
            });
        if needs {
            s.push('"');
            for c in t.chars() {
                if c == '"' || c == '\\' {
                    s.push(c);
                }
                s.push(c);
            }
            s.push('"');
        } else {
            s.push_str(&t);
        }
        Some(())
    };
    if flags & LB_INF == 0 {
        bound(&mut s, &mut r)?;
    }
    s.push(',');
    if flags & UB_INF == 0 {
        bound(&mut s, &mut r)?;
    }
    s.push(if flags & UB_INC != 0 { ']' } else { ')' });
    Some(s)
}

fn multirange(sub: &Type, raw: &[u8]) -> Option<String> {
    let mut r = Reader(raw);
    let n = r.i32()?;
    let mut parts = Vec::new();
    for _ in 0..n.max(0) {
        let v = r.value()??;
        parts.push(range(sub, v)?);
    }
    Some(format!("{{{}}}", parts.join(",")))
}

/// Converts a PostgreSQL error position (1-based character index) into a
/// byte range of `text` covering the token at that position.
pub(crate) fn error_range(text: &str, position: u32) -> Option<std::ops::Range<usize>> {
    if position == 0 {
        return None;
    }
    let Some((start, first)) = text.char_indices().nth(position as usize - 1) else {
        // "at end of input": point at the last character.
        let (last, _) = text.char_indices().next_back()?;
        return Some(last..text.len());
    };
    let rest = &text[start..];
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let len = if first == '"' {
        rest[1..]
            .find('"')
            .map(|i| i + 2)
            .unwrap_or(first.len_utf8())
    } else if is_word(first) {
        rest.char_indices()
            .find(|(_, c)| !is_word(*c))
            .map(|(i, _)| i)
            .unwrap_or(rest.len())
    } else {
        first.len_utf8()
    };
    Some(start..start + len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes a decimal string in the binary numeric format.
    fn enc_numeric(weight: i16, sign: u16, dscale: u16, digits: &[i16]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend((digits.len() as u16).to_be_bytes());
        v.extend(weight.to_be_bytes());
        v.extend(sign.to_be_bytes());
        v.extend(dscale.to_be_bytes());
        for d in digits {
            v.extend(d.to_be_bytes());
        }
        v
    }

    #[test]
    fn numeric_binary_to_string() {
        // 12345.678 = [1, 2345, 6780], weight 1, dscale 3
        assert_eq!(
            numeric(&enc_numeric(1, 0, 3, &[1, 2345, 6780])).unwrap(),
            "12345.678"
        );
        assert_eq!(
            numeric(&enc_numeric(1, 0x4000, 3, &[1, 2345, 6780])).unwrap(),
            "-12345.678"
        );
        // 0.00001 = [1000] weight -2, dscale 5
        assert_eq!(numeric(&enc_numeric(-2, 0, 5, &[1000])).unwrap(), "0.00001");
        // 0 with scale 2
        assert_eq!(numeric(&enc_numeric(0, 0, 2, &[])).unwrap(), "0.00");
        // 100000000 = [1] weight 2 (trailing zero groups stripped)
        assert_eq!(numeric(&enc_numeric(2, 0, 0, &[1])).unwrap(), "100000000");
        assert_eq!(numeric(&enc_numeric(0, 0xC000, 0, &[])).unwrap(), "NaN");
        assert_eq!(
            numeric(&enc_numeric(0, 0xD000, 0, &[])).unwrap(),
            "Infinity"
        );
        assert_eq!(
            numeric(&enc_numeric(0, 0xF000, 0, &[])).unwrap(),
            "-Infinity"
        );
        // 40 significant digits survive exactly.
        let digits = [1234, 5678, 9012, 3456, 7890, 1234, 5678, 9012, 3456, 7890];
        assert_eq!(
            numeric(&enc_numeric(4, 0, 20, &digits)).unwrap(),
            "12345678901234567890.12345678901234567890"
        );
        assert!(numeric(&[0, 1]).is_none());
    }

    #[test]
    fn numeric_into_decimal_column() {
        assert_eq!(numeric_typmod(((10 << 16) | 2) + 4), Some((10, 2)));
        assert_eq!(numeric_typmod(-1), None);
        let (spec, p) = plan("n", &Type::NUMERIC, ((10 << 16) | 2) + 4);
        assert_eq!(
            spec.ty,
            ColumnType::Decimal {
                precision: 10,
                scale: 2
            }
        );
        assert_eq!(spec.db_type, "numeric(10,2)");
        let mut scratch = String::new();
        let raw = enc_numeric(0, 0x4000, 2, &[12, 3400]);
        assert_eq!(cell(&p, &raw, &mut scratch), Cell::Text("-12.34"));
        let (spec, _) = plan("n", &Type::NUMERIC, -1);
        assert_eq!(spec.ty, ColumnType::Text);
        let (spec, _) = plan("n", &Type::NUMERIC, ((50 << 16) | 2) + 4);
        assert_eq!(spec.ty, ColumnType::Text);
    }

    #[test]
    fn interval_rendering() {
        assert_eq!(
            format_interval(14, 3, 4 * 3_600_000_000 + 5 * 60_000_000 + 6_500_000),
            "1 year 2 mons 3 days 04:05:06.5"
        );
        assert_eq!(format_interval(0, 0, 0), "00:00:00");
        assert_eq!(format_interval(0, 1, 0), "1 day");
        assert_eq!(format_interval(0, -1, 0), "-1 days");
        assert_eq!(format_interval(-14, 0, 0), "-1 years -2 mons");
        assert_eq!(format_interval(0, -1, 3_600_000_000), "-1 days +01:00:00");
        assert_eq!(format_interval(0, 0, -90_000_000), "-00:01:30");
        assert_eq!(format_interval(0, 0, 100 * 3_600_000_000), "100:00:00");
        assert_eq!(format_interval(i32::MAX, i32::MAX, i64::MAX), "infinity");
    }

    fn enc_array(elem_oid: u32, dims: &[(i32, i32)], elems: &[Option<&[u8]>]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend((dims.len() as i32).to_be_bytes());
        v.extend(0i32.to_be_bytes());
        v.extend(elem_oid.to_be_bytes());
        for (len, lb) in dims {
            v.extend(len.to_be_bytes());
            v.extend(lb.to_be_bytes());
        }
        for e in elems {
            match e {
                None => v.extend((-1i32).to_be_bytes()),
                Some(b) => {
                    v.extend((b.len() as i32).to_be_bytes());
                    v.extend(*b);
                }
            }
        }
        v
    }

    #[test]
    fn array_rendering() {
        let one = 1i32.to_be_bytes();
        let two = 2i32.to_be_bytes();
        let raw = enc_array(23, &[(3, 1)], &[Some(&one), Some(&two), None]);
        assert_eq!(array(&Type::INT4, &raw).unwrap(), "{1,2,NULL}");
        let raw = enc_array(
            23,
            &[(2, 1), (2, 1)],
            &[Some(&one), Some(&two), Some(&two), None],
        );
        assert_eq!(array(&Type::INT4, &raw).unwrap(), "{{1,2},{2,NULL}}");
        let raw = enc_array(
            25,
            &[(5, 1)],
            &[
                Some(b"a"),
                Some(b"b c"),
                Some(b""),
                Some(b"NULL"),
                Some(b"x\"y\\"),
            ],
        );
        assert_eq!(
            array(&Type::TEXT, &raw).unwrap(),
            r#"{a,"b c","","NULL","x\"y\\"}"#
        );
        let raw = enc_array(23, &[(1, 0)], &[Some(&one)]);
        assert_eq!(array(&Type::INT4, &raw).unwrap(), "[0:0]={1}");
        assert_eq!(array(&Type::INT4, &enc_array(23, &[], &[])).unwrap(), "{}");
        // Through the generic renderer, as a column would be.
        let raw = enc_array(23, &[(2, 1)], &[Some(&one), Some(&two)]);
        assert_eq!(text(&Type::INT4_ARRAY, &raw), "{1,2}");
        assert_eq!(db_type_name(&Type::INT4_ARRAY, -1), "int4[]");
        assert_eq!(db_type_name(&Type::VARCHAR_ARRAY, 14), "varchar(10)[]");
    }

    #[test]
    fn timestamp_epochs() {
        let mut scratch = String::new();
        // 2000-01-01 00:00:00 = 0 on the wire = 946684800 s Unix.
        let (_, p) = plan("t", &Type::TIMESTAMPTZ, -1);
        assert_eq!(
            cell(&p, &0i64.to_be_bytes(), &mut scratch),
            Cell::Timestamp(946_684_800_000_000)
        );
        let (_, p) = plan("d", &Type::DATE, -1);
        assert_eq!(
            cell(&p, &0i32.to_be_bytes(), &mut scratch),
            Cell::Date(10_957)
        );
        assert_eq!(cell(&p, &i32::MAX.to_be_bytes(), &mut scratch), LOSSY);
        assert_eq!(date(-1), "1999-12-31");
        assert_eq!(date(i32::MIN), "-infinity");
        assert_eq!(timestamp(1_500_000, false), "2000-01-01 00:00:01.5");
        assert_eq!(timestamp(-1, true), "1999-12-31 23:59:59.999999+00");
        // 0044-03-15 BC (proleptic year -43)
        let d = chrono::NaiveDate::from_ymd_opt(-43, 3, 15).unwrap();
        let days = d.signed_duration_since(chrono::NaiveDate::from_ymd_opt(2000, 1, 1).unwrap());
        assert_eq!(date(days.num_days() as i32), "0044-03-15 BC");
        assert_eq!(timetz(3_600_000_000, -3600 * 5 - 1800), "01:00:00+05:30");
        assert_eq!(timetz(0, 3600), "00:00:00-01");
    }

    #[test]
    fn misc_text_renderings() {
        let u = [
            0xa0, 0xee, 0xbc, 0x99, 0x9c, 0x0b, 0x4e, 0xf8, 0xbb, 0x6d, 0x6b, 0xb9, 0xbd, 0x38,
            0x0a, 0x11,
        ];
        assert_eq!(
            text(&Type::UUID, &u),
            "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"
        );
        assert_eq!(text(&Type::JSONB, b"\x01{\"a\": 1}"), "{\"a\": 1}");
        assert_eq!(text(&Type::BYTEA, &[0xde, 0xad]), "\\xdead");
        assert_eq!(text(&Type::INET, &[2, 32, 0, 4, 10, 0, 0, 1]), "10.0.0.1");
        assert_eq!(text(&Type::CIDR, &[2, 8, 1, 4, 10, 0, 0, 0]), "10.0.0.0/8");
        assert_eq!(text(&Type::MONEY, &(-1234i64).to_be_bytes()), "-12.34");
        let mut p = Vec::new();
        p.extend(1.5f64.to_be_bytes());
        p.extend((-2.0f64).to_be_bytes());
        assert_eq!(text(&Type::POINT, &p), "(1.5,-2)");
        assert_eq!(text(&Type::BIT, &[0, 0, 0, 3, 0b1010_0000]), "101");
    }

    #[test]
    fn error_position_to_byte_range() {
        let t = "select * from nope";
        assert_eq!(error_range(t, 15), Some(14..18));
        assert_eq!(&t[14..18], "nope");
        // Non-ASCII before the error: positions count characters.
        let t = "select 'żółć', * from brak";
        let r = error_range(t, 23).unwrap();
        assert_eq!(&t[r], "brak");
        let t = "select ąę from \"tab ł\"";
        let r = error_range(t, 8).unwrap();
        assert_eq!(&t[r], "ąę");
        let r = error_range(t, 16).unwrap();
        assert_eq!(&t[r], "\"tab ł\"");
        // A symbol is one character, however many bytes it takes.
        let t = "select 1 € 2";
        let r = error_range(t, 10).unwrap();
        assert_eq!(&t[r], "€");
        // End of input: the last character.
        let t = "select (ł";
        let r = error_range(t, 10).unwrap();
        assert_eq!(&t[r], "ł");
        assert_eq!(error_range("", 1), None);
        assert_eq!(error_range("x", 0), None);
    }
}
