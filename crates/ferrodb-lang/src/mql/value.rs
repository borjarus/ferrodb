//! Values in shell syntax ("relaxed" JSON) ↔ BSON.
//!
//! Accepted: objects with unquoted / quoted keys and trailing commas,
//! arrays, strings, numbers (integers in the `int32` range become `Int32`,
//! other numbers `Double`, as in mongosh), `true`/`false`/`null`/
//! `undefined`, `Infinity`, `NaN`, regular expressions and the helpers
//! `ObjectId`, `ISODate`, `new Date`, `Date`, `NumberDecimal`/`Decimal128`,
//! `NumberLong`/`Long`, `NumberInt`/`Int32`, `Double`, `Timestamp`, `UUID`,
//! `BinData`, `HexData`, `MinKey`, `MaxKey`, `RegExp`.

use std::ops::Range;

use bson::{
    Binary, Bson, DateTime, Decimal128, Document, Regex, Timestamp, oid::ObjectId,
    spec::BinarySubtype,
};

use super::lexer::{Tok, Token};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub range: Range<usize>,
}

impl ParseError {
    pub fn new(message: impl Into<String>, range: Range<usize>) -> Self {
        Self {
            message: message.into(),
            range,
        }
    }
}

/// Cursor over significant tokens (comments and newlines removed).
pub struct Tokens<'a> {
    pub toks: Vec<&'a Token>,
    pub pos: usize,
    /// End offset used for errors at the end of input.
    pub end: usize,
}

impl<'a> Tokens<'a> {
    pub fn new(all: &'a [Token], end: usize) -> Self {
        Self {
            toks: all
                .iter()
                .filter(|t| !matches!(t.tok, Tok::Comment | Tok::Newline))
                .collect(),
            pos: 0,
            end,
        }
    }

    pub fn peek(&self) -> Option<&'a Token> {
        self.toks.get(self.pos).copied()
    }

    pub fn peek_tok(&self) -> Option<&'a Tok> {
        self.peek().map(|t| &t.tok)
    }

    pub fn next(&mut self) -> Option<&'a Token> {
        let t = self.toks.get(self.pos).copied();
        self.pos += usize::from(t.is_some());
        t
    }

    pub fn here(&self) -> Range<usize> {
        self.peek()
            .map(|t| t.range.clone())
            .unwrap_or(self.end..self.end)
    }

    pub fn is_punct(&self, c: char) -> bool {
        self.peek_tok() == Some(&Tok::Punct(c))
    }

    pub fn eat(&mut self, c: char) -> bool {
        if self.is_punct(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub fn expect(&mut self, c: char) -> Result<Range<usize>, ParseError> {
        match self.peek() {
            Some(t) if t.tok == Tok::Punct(c) => {
                self.pos += 1;
                Ok(t.range.clone())
            }
            _ => Err(ParseError::new(format!("expected `{c}`"), self.here())),
        }
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }
}

fn number(text: &str, neg: bool, range: Range<usize>) -> Result<Bson, ParseError> {
    let clean = text.replace('_', "");
    let sign = if neg { -1.0 } else { 1.0 };
    if let Some(hex) = clean
        .strip_prefix("0x")
        .or_else(|| clean.strip_prefix("0X"))
    {
        let v = i64::from_str_radix(hex, 16)
            .map_err(|_| ParseError::new("invalid number", range.clone()))?;
        let v = if neg { -v } else { v };
        return Ok(i32::try_from(v).map_or(Bson::Double(v as f64), Bson::Int32));
    }
    if !clean.contains(['.', 'e', 'E'])
        && let Ok(v) = clean.parse::<i64>()
    {
        let v = if neg { -v } else { v };
        return Ok(i32::try_from(v).map_or(Bson::Double(v as f64), Bson::Int32));
    }
    clean
        .parse::<f64>()
        .map(|v| Bson::Double(v * sign))
        .map_err(|_| ParseError::new("invalid number", range))
}

/// One value.
pub fn parse_value(t: &mut Tokens<'_>) -> Result<Bson, ParseError> {
    let Some(tok) = t.next() else {
        return Err(ParseError::new("expected a value", t.here()));
    };
    let r = tok.range.clone();
    match &tok.tok {
        Tok::Punct('{') => parse_object_body(t, r.start).map(Bson::Document),
        Tok::Punct('[') => {
            let mut items = Vec::new();
            loop {
                if t.eat(']') {
                    break;
                }
                items.push(parse_value(t)?);
                if t.eat(',') {
                    continue;
                }
                t.expect(']')?;
                break;
            }
            Ok(Bson::Array(items))
        }
        Tok::Punct('-') => match t.next() {
            Some(Token {
                tok: Tok::Number(n),
                range,
            }) => number(n, true, range.clone()),
            Some(Token {
                tok: Tok::Ident(i), ..
            }) if i == "Infinity" => Ok(Bson::Double(f64::NEG_INFINITY)),
            _ => Err(ParseError::new("expected a number after `-`", r)),
        },
        Tok::Punct('+') => parse_value(t),
        Tok::Number(n) => number(n, false, r),
        Tok::Str(s) => Ok(Bson::String(s.clone())),
        Tok::Regex(p, f) => regex(p, f, r),
        Tok::Ident(id) => match id.as_str() {
            "true" => Ok(Bson::Boolean(true)),
            "false" => Ok(Bson::Boolean(false)),
            "null" => Ok(Bson::Null),
            "undefined" => Ok(Bson::Undefined),
            "Infinity" => Ok(Bson::Double(f64::INFINITY)),
            "NaN" => Ok(Bson::Double(f64::NAN)),
            "MinKey" => {
                optional_call(t)?;
                Ok(Bson::MinKey)
            }
            "MaxKey" => {
                optional_call(t)?;
                Ok(Bson::MaxKey)
            }
            "new" => match t.next() {
                Some(Token {
                    tok: Tok::Ident(h),
                    range,
                }) => helper(t, h, range.clone(), true),
                _ => Err(ParseError::new("expected a constructor after `new`", r)),
            },
            other => helper(t, other, r, false),
        },
        _ => Err(ParseError::new("expected a value", r)),
    }
}

/// A BSON regular expression (options sorted, as BSON requires).
pub fn regex(pattern: &str, flags: &str, r: Range<usize>) -> Result<Bson, ParseError> {
    let bad = |_| ParseError::new("regular expressions cannot contain NUL", r.clone());
    Ok(Bson::RegularExpression(Regex {
        pattern: pattern.to_owned().try_into().map_err(bad)?,
        options: sorted(flags).try_into().map_err(bad)?,
    }))
}

fn sorted(flags: &str) -> String {
    let mut f: Vec<char> = flags.chars().collect();
    f.sort_unstable();
    f.into_iter().collect()
}

fn optional_call(t: &mut Tokens<'_>) -> Result<(), ParseError> {
    if t.eat('(') {
        t.expect(')')?;
    }
    Ok(())
}

/// `{` already consumed.
pub fn parse_object_body(t: &mut Tokens<'_>, _start: usize) -> Result<Document, ParseError> {
    let mut d = Document::new();
    loop {
        if t.eat('}') {
            return Ok(d);
        }
        let key = match t.next() {
            Some(Token {
                tok: Tok::Ident(k) | Tok::Str(k) | Tok::Number(k),
                ..
            }) => k.clone(),
            _ => {
                return Err(ParseError::new("expected a field name", t.here()));
            }
        };
        // `{ a.b: 1 }` is not JS, but `{ "a.b": 1 }` is: keep it simple.
        t.expect(':')?;
        let v = parse_value(t)?;
        d.insert(key, v);
        if t.eat(',') {
            continue;
        }
        t.expect('}')?;
        return Ok(d);
    }
}

/// Arguments of a call (after `(`, up to and including `)`).
pub fn parse_args(t: &mut Tokens<'_>) -> Result<Vec<Bson>, ParseError> {
    let mut args = Vec::new();
    loop {
        if t.eat(')') {
            return Ok(args);
        }
        args.push(parse_value(t)?);
        if t.eat(',') {
            continue;
        }
        t.expect(')')?;
        return Ok(args);
    }
}

fn str_arg(args: &[Bson], i: usize) -> Option<&str> {
    args.get(i).and_then(Bson::as_str)
}

fn int_arg(args: &[Bson], i: usize) -> Option<i64> {
    match args.get(i)? {
        Bson::Int32(v) => Some(*v as i64),
        Bson::Int64(v) => Some(*v),
        Bson::Double(v) if v.fract() == 0.0 => Some(*v as i64),
        Bson::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn parse_date(s: &str) -> Option<DateTime> {
    if let Ok(d) = DateTime::parse_rfc3339_str(s) {
        return Some(d);
    }
    // `2024-01-02`, `2024-01-02T03:04:05` (UTC, like ISODate).
    let s = s.trim();
    let with_time = if s.len() == 10 {
        format!("{s}T00:00:00Z")
    } else if !s.ends_with('Z') && !s[10..].contains(['+', '-']) {
        format!("{s}Z")
    } else {
        s.to_owned()
    };
    DateTime::parse_rfc3339_str(&with_time).ok()
}

fn helper(t: &mut Tokens<'_>, name: &str, r: Range<usize>, _new: bool) -> Result<Bson, ParseError> {
    if !t.is_punct('(') {
        return Err(ParseError::new(format!("unknown value `{name}`"), r));
    }
    t.next();
    let args = parse_args(t)?;
    let bad = |what: &str| ParseError::new(format!("{name}: {what}"), r.clone());
    Ok(match name {
        "ObjectId" => match str_arg(&args, 0) {
            Some(hex) => Bson::ObjectId(
                ObjectId::parse_str(hex).map_err(|_| bad("expected 24 hexadecimal characters"))?,
            ),
            None if args.is_empty() => Bson::ObjectId(ObjectId::new()),
            None => return Err(bad("expected a string")),
        },
        "ISODate" | "Date" => match args.first() {
            None => Bson::DateTime(DateTime::now()),
            Some(Bson::String(s)) => {
                Bson::DateTime(parse_date(s).ok_or_else(|| bad("invalid date"))?)
            }
            Some(_) => Bson::DateTime(DateTime::from_millis(
                int_arg(&args, 0).ok_or_else(|| bad("invalid date"))?,
            )),
        },
        "NumberDecimal" | "Decimal128" => {
            let s = match args.first() {
                Some(Bson::String(s)) => s.clone(),
                Some(Bson::Int32(v)) => v.to_string(),
                Some(Bson::Double(v)) => v.to_string(),
                _ => return Err(bad("expected a string")),
            };
            Bson::Decimal128(
                s.parse::<Decimal128>()
                    .map_err(|_| bad("invalid decimal"))?,
            )
        }
        "NumberLong" | "Long" => {
            Bson::Int64(int_arg(&args, 0).ok_or_else(|| bad("expected an integer"))?)
        }
        "NumberInt" | "Int32" => Bson::Int32(
            int_arg(&args, 0)
                .and_then(|v| i32::try_from(v).ok())
                .ok_or_else(|| bad("expected a 32-bit integer"))?,
        ),
        "Double" => match args.first() {
            Some(Bson::Int32(v)) => Bson::Double(*v as f64),
            Some(Bson::Double(v)) => Bson::Double(*v),
            Some(Bson::String(s)) => Bson::Double(s.parse().map_err(|_| bad("invalid number"))?),
            _ => return Err(bad("expected a number")),
        },
        "Timestamp" => {
            let (time, increment) = match args.first() {
                Some(Bson::Document(d)) => (
                    d.get("t")
                        .and_then(Bson::as_i32)
                        .or_else(|| d.get_i64("t").ok().map(|v| v as i32))
                        .unwrap_or(0),
                    d.get("i")
                        .and_then(Bson::as_i32)
                        .or_else(|| d.get_i64("i").ok().map(|v| v as i32))
                        .unwrap_or(0),
                ),
                _ => (
                    int_arg(&args, 0).unwrap_or(0) as i32,
                    int_arg(&args, 1).unwrap_or(0) as i32,
                ),
            };
            Bson::Timestamp(Timestamp {
                time: time as u32,
                increment: increment as u32,
            })
        }
        "UUID" => {
            let s = str_arg(&args, 0).ok_or_else(|| bad("expected a string"))?;
            let hex: String = s.chars().filter(|c| *c != '-').collect();
            Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes: hex_bytes(&hex)
                    .filter(|b| b.len() == 16)
                    .ok_or_else(|| bad("invalid UUID"))?,
            })
        }
        "BinData" => {
            let sub = int_arg(&args, 0).ok_or_else(|| bad("expected a subtype"))? as u8;
            let b64 = str_arg(&args, 1).ok_or_else(|| bad("expected base64 data"))?;
            Bson::Binary(Binary {
                subtype: BinarySubtype::from(sub),
                bytes: base64_decode(b64).ok_or_else(|| bad("invalid base64"))?,
            })
        }
        "HexData" => {
            let sub = int_arg(&args, 0).ok_or_else(|| bad("expected a subtype"))? as u8;
            let hex = str_arg(&args, 1).ok_or_else(|| bad("expected hexadecimal data"))?;
            Bson::Binary(Binary {
                subtype: BinarySubtype::from(sub),
                bytes: hex_bytes(hex).ok_or_else(|| bad("invalid hexadecimal data"))?,
            })
        }
        "RegExp" => regex(
            str_arg(&args, 0).ok_or_else(|| bad("expected a pattern"))?,
            str_arg(&args, 1).unwrap_or_default(),
            r.clone(),
        )?,
        _ => return Err(ParseError::new(format!("unknown function `{name}`"), r)),
    })
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            break;
        }
        let v = B64.iter().position(|x| *x == c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn base64_encode(b: &[u8]) -> String {
    let mut s = String::new();
    for chunk in b.chunks(3) {
        let n = chunk.len();
        let v = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= n {
                s.push(B64[((v >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

// ------------------------------------------------------------------ printer

fn is_ident(k: &str) -> bool {
    let mut c = k.chars();
    c.next()
        .is_some_and(|f| f == '_' || f == '$' || f.is_ascii_alphabetic())
        && c.all(|x| x == '_' || x == '$' || x.is_ascii_alphanumeric())
}

pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn key(k: &str) -> String {
    if is_ident(k) { k.to_owned() } else { quote(k) }
}

/// A value on one line.
pub fn print_inline(v: &Bson) -> String {
    print(v, None, 0)
}

/// A value, broken over lines (two-space indent) when longer than `width`.
pub fn print_pretty(v: &Bson, width: usize) -> String {
    print(v, Some(width), 0)
}

fn print(v: &Bson, width: Option<usize>, level: usize) -> String {
    let flat = match v {
        Bson::Document(d) => {
            if d.is_empty() {
                return "{}".into();
            }
            let items: Vec<String> = d
                .iter()
                .map(|(k, x)| format!("{}: {}", key(k), print(x, None, 0)))
                .collect();
            format!("{{ {} }}", items.join(", "))
        }
        Bson::Array(a) => {
            if a.is_empty() {
                return "[]".into();
            }
            let items: Vec<String> = a.iter().map(|x| print(x, None, 0)).collect();
            format!("[{}]", items.join(", "))
        }
        other => return scalar(other),
    };
    let Some(w) = width else { return flat };
    if flat.len() + level * 2 <= w {
        return flat;
    }
    let pad = "  ".repeat(level + 1);
    let close = "  ".repeat(level);
    match v {
        Bson::Document(d) => {
            let items: Vec<String> = d
                .iter()
                .map(|(k, x)| format!("{pad}{}: {}", key(k), print(x, width, level + 1)))
                .collect();
            format!("{{\n{}\n{close}}}", items.join(",\n"))
        }
        Bson::Array(a) => {
            let items: Vec<String> = a
                .iter()
                .map(|x| format!("{pad}{}", print(x, width, level + 1)))
                .collect();
            format!("[\n{}\n{close}]", items.join(",\n"))
        }
        _ => flat,
    }
}

fn scalar(v: &Bson) -> String {
    match v {
        Bson::Double(d) if d.is_nan() => "NaN".into(),
        Bson::Double(d) if d.is_infinite() => {
            if *d > 0.0 {
                "Infinity".into()
            } else {
                "-Infinity".into()
            }
        }
        Bson::Double(d) if d.fract() == 0.0 && d.abs() < 1e15 => {
            if i32::try_from(*d as i64).is_ok() {
                format!("Double({d:.1})")
            } else {
                format!("{d:.1}")
            }
        }
        Bson::Double(d) => d.to_string(),
        Bson::String(s) => quote(s),
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".into(),
        Bson::Undefined => "undefined".into(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => format!("NumberLong(\"{i}\")"),
        Bson::Decimal128(d) => format!("NumberDecimal(\"{d}\")"),
        Bson::ObjectId(o) => format!("ObjectId(\"{}\")", o.to_hex()),
        Bson::DateTime(d) => match d.try_to_rfc3339_string() {
            Ok(s) => format!("ISODate(\"{s}\")"),
            Err(_) => format!("new Date({})", d.timestamp_millis()),
        },
        Bson::Timestamp(t) => format!("Timestamp({{ t: {}, i: {} }})", t.time, t.increment),
        Bson::RegularExpression(r) => {
            let p = r.pattern.to_string();
            if p.contains('\n') {
                format!("RegExp({}, {})", quote(&p), quote(r.options.as_str()))
            } else {
                format!(
                    "/{}/{}",
                    p.replace('/', "\\/").replace("\\\\/", "\\/"),
                    r.options.as_str()
                )
            }
        }
        Bson::Binary(b) if b.subtype == BinarySubtype::Uuid && b.bytes.len() == 16 => {
            let h: String = b.bytes.iter().map(|x| format!("{x:02x}")).collect();
            format!(
                "UUID(\"{}-{}-{}-{}-{}\")",
                &h[0..8],
                &h[8..12],
                &h[12..16],
                &h[16..20],
                &h[20..]
            )
        }
        Bson::Binary(b) => format!(
            "BinData({}, \"{}\")",
            u8::from(b.subtype),
            base64_encode(&b.bytes)
        ),
        Bson::MinKey => "MinKey()".into(),
        Bson::MaxKey => "MaxKey()".into(),
        other => format!("{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mql::lexer::lex;

    fn parse(s: &str) -> Bson {
        let (toks, errs) = lex(s);
        assert!(errs.is_empty(), "{errs:?}");
        let mut t = Tokens::new(&toks, s.len());
        let v = parse_value(&mut t).unwrap();
        assert!(t.at_end(), "trailing tokens in {s}");
        v
    }

    #[test]
    fn helpers_and_numbers() {
        let v = parse(
            "{a: 1, b: 2.5, c: 3000000000, 'd e': [true, null,], f: NumberLong(5), \
             g: NumberDecimal('1.10'), h: ObjectId('65a1b2c3d4e5f6a7b8c9d0e1'), \
             i: ISODate('2024-01-02T03:04:05Z'), j: /x/mi, k: UUID('0123456789abcdef0123456789abcdef'), \
             l: BinData(0, 'AQID'), m: Timestamp(1, 2), n: new Date('2024-01-02'), o: -4, p: MinKey}",
        );
        let d = v.as_document().unwrap();
        assert_eq!(d.get("a"), Some(&Bson::Int32(1)));
        assert_eq!(d.get("b"), Some(&Bson::Double(2.5)));
        assert_eq!(d.get("c"), Some(&Bson::Double(3e9)));
        assert_eq!(d.get_array("d e").unwrap().len(), 2);
        assert_eq!(d.get("f"), Some(&Bson::Int64(5)));
        assert_eq!(d.get("g").unwrap().to_string(), "1.10");
        assert_eq!(
            d.get_object_id("h").unwrap().to_hex(),
            "65a1b2c3d4e5f6a7b8c9d0e1"
        );
        assert_eq!(
            d.get_datetime("i").unwrap().timestamp_millis(),
            1_704_164_645_000
        );
        assert_eq!(d.get("j"), Some(&regex("x", "mi", 0..0).unwrap()));
        assert!(matches!(d.get("k"), Some(Bson::Binary(b)) if b.subtype == BinarySubtype::Uuid));
        assert!(matches!(d.get("l"), Some(Bson::Binary(b)) if b.bytes == [1, 2, 3]));
        assert_eq!(
            d.get("n")
                .unwrap()
                .as_datetime()
                .unwrap()
                .timestamp_millis(),
            1_704_153_600_000
        );
        assert_eq!(d.get("o"), Some(&Bson::Int32(-4)));
        assert_eq!(d.get("p"), Some(&Bson::MinKey));
    }

    #[test]
    fn print_round_trips() {
        let src = "{ a: 1, b: \"x\\\"y\", c: NumberLong(\"5\"), d: NumberDecimal(\"1.10\"), \
                   e: ObjectId(\"65a1b2c3d4e5f6a7b8c9d0e1\"), f: ISODate(\"2024-01-02T03:04:05Z\"), \
                   g: [1, Double(2.0), 2.5], \"h.i\": /a\\/b/i, j: BinData(0, \"AQID\"), \
                   k: UUID(\"01234567-89ab-cdef-0123-456789abcdef\"), l: Timestamp({ t: 1, i: 2 }), m: MaxKey() }";
        let v = parse(src);
        let printed = print_inline(&v);
        assert_eq!(parse(&printed), v, "{printed}");
        let pretty = print_pretty(&v, 40);
        assert!(pretty.contains("\n  a: 1"), "{pretty}");
        assert_eq!(parse(&pretty), v);
    }

    #[test]
    fn errors_have_positions() {
        let (toks, _) = lex("{a: ObjectId('zz')}");
        let mut t = Tokens::new(&toks, 19);
        let e = parse_value(&mut t).unwrap_err();
        assert!(e.message.contains("ObjectId"), "{e:?}");
        assert_eq!(e.range, 4..12);
        let (toks, _) = lex("{a 1}");
        let e = parse_value(&mut Tokens::new(&toks, 5)).unwrap_err();
        assert_eq!(e.message, "expected `:`");
    }
}
