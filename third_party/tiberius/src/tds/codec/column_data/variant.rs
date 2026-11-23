//! FERRODB PATCH: `sql_variant` values (MS-TDS 2.2.5.5.4), rendered as text
//! the way SQL Server converts them to `nvarchar`.

use crate::{sql_read_bytes::SqlReadBytes, tds::Collation, ColumnData};

pub(crate) async fn decode<R>(src: &mut R) -> crate::Result<ColumnData<'static>>
where
    R: SqlReadBytes + Unpin,
{
    let total = src.read_u32_le().await? as usize;
    if total < 2 {
        return Ok(ColumnData::String(None));
    }
    let ty = src.read_u8().await?;
    let nprops = src.read_u8().await? as usize;
    let mut props = Vec::with_capacity(nprops);
    for _ in 0..nprops {
        props.push(src.read_u8().await?);
    }
    let n = total.saturating_sub(2 + nprops);
    let mut data = Vec::with_capacity(n);
    for _ in 0..n {
        data.push(src.read_u8().await?);
    }
    Ok(ColumnData::String(Some(render(ty, &props, &data).into())))
}

fn le<const N: usize>(d: &[u8]) -> [u8; N] {
    let mut b = [0u8; N];
    for (i, x) in d.iter().take(N).enumerate() {
        b[i] = *x;
    }
    b
}

fn uint_le(d: &[u8]) -> u128 {
    d.iter()
        .rev()
        .fold(0u128, |acc, b| (acc << 8) | u128::from(*b))
}

/// `value / 10^scale` as text.
fn scaled(value: i128, scale: u32) -> String {
    if scale == 0 {
        return value.to_string();
    }
    let neg = value < 0;
    let digits = value.unsigned_abs().to_string();
    let scale = scale as usize;
    let padded = if digits.len() <= scale {
        format!("{}{}", "0".repeat(scale + 1 - digits.len()), digits)
    } else {
        digits
    };
    let (int, frac) = padded.split_at(padded.len() - scale);
    format!("{}{int}.{frac}", if neg { "-" } else { "" })
}

/// Civil date of `days` since 0001-01-01.
fn civil(days: i64) -> (i64, u32, u32) {
    // Days since 1970-01-01, then Howard Hinnant's algorithm.
    let z = days - 719_162 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn date(days: i64) -> String {
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Time of day from `ticks` of 10^-`scale` seconds.
fn time(ticks: u64, scale: u32) -> String {
    let unit = 10u64.pow(scale);
    let secs = ticks / unit;
    let frac = ticks % unit;
    let base = format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    );
    if scale == 0 {
        base
    } else {
        format!("{base}.{frac:0width$}", width = scale as usize)
    }
}

fn time_len(scale: u32) -> usize {
    match scale {
        0..=2 => 3,
        3..=4 => 4,
        _ => 5,
    }
}

fn hex(d: &[u8]) -> String {
    let mut s = String::from("0x");
    for b in d {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

fn render(ty: u8, props: &[u8], d: &[u8]) -> String {
    match ty {
        0x30 => d.first().copied().unwrap_or(0).to_string(),
        0x32 => (d.first().copied().unwrap_or(0) != 0).to_string(),
        0x34 => i16::from_le_bytes(le(d)).to_string(),
        0x38 => i32::from_le_bytes(le(d)).to_string(),
        0x7F => i64::from_le_bytes(le(d)).to_string(),
        0x3B => f32::from_le_bytes(le(d)).to_string(),
        0x3E => f64::from_le_bytes(le(d)).to_string(),
        // decimal / numeric: precision, scale; sign byte + magnitude.
        0x6A | 0x6C => {
            let scale = props.get(1).copied().unwrap_or(0) as u32;
            let positive = d.first().copied().unwrap_or(1) == 1;
            let v = uint_le(d.get(1..).unwrap_or_default()) as i128;
            scaled(if positive { v } else { -v }, scale)
        }
        0x3C => {
            let high = i32::from_le_bytes(le(d)) as i64;
            let low = u32::from_le_bytes(le(d.get(4..).unwrap_or_default())) as i64;
            scaled(((high << 32) | low) as i128, 4)
        }
        0x7A => scaled(i32::from_le_bytes(le(d)) as i128, 4),
        // varchar / char: collation (5 bytes) + max length.
        0xA7 | 0xAF => {
            let info = u32::from_le_bytes(le(props));
            let sort = props.get(4).copied().unwrap_or(0);
            match Collation::new(info, sort).encoding() {
                Ok(enc) => enc.decode(d).0.into_owned(),
                Err(_) => String::from_utf8_lossy(d).into_owned(),
            }
        }
        0xE7 | 0xEF => {
            let units: Vec<u16> = d
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        0x24 if d.len() == 16 => format!(
            "{:08X}-{:04X}-{:04X}-{}-{}",
            u32::from_le_bytes(le(d)),
            u16::from_le_bytes(le(&d[4..])),
            u16::from_le_bytes(le(&d[6..])),
            hex(&d[8..10]).trim_start_matches("0x"),
            hex(&d[10..16]).trim_start_matches("0x")
        ),
        0x28 => date(uint_le(d) as i64),
        0x29 => {
            let scale = props.first().copied().unwrap_or(7) as u32;
            time(uint_le(d) as u64, scale)
        }
        0x2A | 0x2B => {
            let scale = props.first().copied().unwrap_or(7) as u32;
            let tl = time_len(scale).min(d.len());
            let t = time(uint_le(&d[..tl]) as u64, scale);
            let days = uint_le(d.get(tl..tl + 3).unwrap_or_default()) as i64;
            let mut s = format!("{} {t}", date(days));
            if ty == 0x2B {
                // Stored in UTC; the offset in minutes follows.
                let off = i16::from_le_bytes(le(d.get(tl + 3..).unwrap_or_default()));
                s.push_str(&format!(
                    " {}{:02}:{:02} (UTC time)",
                    if off < 0 { '-' } else { '+' },
                    off.unsigned_abs() / 60,
                    off.unsigned_abs() % 60
                ));
            }
            s
        }
        // datetime: days since 1900-01-01, 1/300 s since midnight.
        0x3D => {
            let days = i32::from_le_bytes(le(d)) as i64;
            let ticks = u32::from_le_bytes(le(d.get(4..).unwrap_or_default())) as u64;
            let millis = (ticks * 10 + 1) / 3;
            format!("{} {}", date(693_595 + days), time(millis, 3))
        }
        0x3A => {
            let days = u16::from_le_bytes(le(d)) as i64;
            let mins = u16::from_le_bytes(le(d.get(2..).unwrap_or_default())) as u64;
            format!("{} {}", date(693_595 + days), time(mins * 60, 0))
        }
        _ => hex(d),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_base_types() {
        assert_eq!(render(0x38, &[], &42i32.to_le_bytes()), "42");
        assert_eq!(render(0x6C, &[10, 2], &[1, 0xE2, 0x04, 0, 0]), "12.50");
        assert_eq!(render(0x6C, &[10, 2], &[0, 5, 0, 0, 0]), "-0.05");
        assert_eq!(render(0x28, &[], &[0x5B, 0x95, 0x0A]), "1900-01-01");
        assert_eq!(render(0xE7, &[0; 7], &[b'h', 0, b'i', 0]), "hi");
        assert_eq!(render(0xA5, &[], &[1, 255]), "0x01FF");
        assert_eq!(
            render(0x3D, &[], &[0, 0, 0, 0, 0, 0, 0, 0]),
            "1900-01-01 00:00:00.000"
        );
    }
}
