//! `ProfileDateCoding` (mac-librarykit §2.6): the .NET `DateTimeOffset` shape used in `profiles.json` and the vault,
//! over [`Ticks`] (100 ns since 0001-01-01 UTC, exactly the precision of the 7-digit fraction).

use crate::Ticks;

const TICKS_PER_SECOND: i64 = 10_000_000;

/// Unix seconds (floor) and the sub-second ticks of `t`.
fn split(t: Ticks) -> (i64, i64) {
    let since = t.0 - Ticks::UNIX_EPOCH;
    (since.div_euclid(TICKS_PER_SECOND), since.rem_euclid(TICKS_PER_SECOND))
}

/// Ticks of a Unix instant in seconds (tests and clocks).
pub fn from_unix_seconds(seconds: f64) -> Ticks {
    Ticks(Ticks::UNIX_EPOCH + (seconds * TICKS_PER_SECOND as f64).round() as i64)
}

/// `t` as Unix seconds.
pub fn to_unix_seconds(t: Ticks) -> f64 {
    (t.0 - Ticks::UNIX_EPOCH) as f64 / TICKS_PER_SECOND as f64
}

/// A duration in seconds as ticks.
pub fn seconds(s: f64) -> i64 {
    (s * TICKS_PER_SECOND as f64).round() as i64
}

/// `yyyy-MM-ddTHH:mm:ss.fffffff+00:00`.
pub fn to_string(t: Ticks) -> String {
    let (secs, ticks) = split(t);
    let (y, mo, d, h, mi, s) = fields(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ticks:07}+00:00")
}

/// `yyyyMMdd-HHmmss` in UTC (file names).
pub fn compact_timestamp(t: Ticks) -> String {
    let (secs, _) = split(t);
    let (y, mo, d, h, mi, s) = fields(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// Accepts `yyyy-MM-ddTHH:mm:ss[.f…][Z|z|±HH[:][mm]]`; no zone = UTC. The whole string must be consumed.
pub fn parse(text: &str) -> Option<Ticks> {
    let b = text.as_bytes();
    let mut i = 0usize;
    let number = |i: &mut usize, digits: usize| -> Option<i64> {
        if *i + digits > b.len() {
            return None;
        }
        let mut v = 0i64;
        for k in 0..digits {
            let c = b[*i + k];
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + i64::from(c - b'0');
        }
        *i += digits;
        Some(v)
    };
    let expect = |i: &mut usize, c: u8| -> bool {
        if *i < b.len() && b[*i] == c {
            *i += 1;
            true
        } else {
            false
        }
    };

    let year = number(&mut i, 4)?;
    if !expect(&mut i, b'-') {
        return None;
    }
    let month = number(&mut i, 2)?;
    if !expect(&mut i, b'-') {
        return None;
    }
    let day = number(&mut i, 2)?;
    if !expect(&mut i, b'T') {
        return None;
    }
    let hour = number(&mut i, 2)?;
    if !expect(&mut i, b':') {
        return None;
    }
    let minute = number(&mut i, 2)?;
    if !expect(&mut i, b':') {
        return None;
    }
    let second = number(&mut i, 2)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour >= 24 || minute >= 60 || second >= 61 {
        return None;
    }

    // Fraction: the first 7 digits are ticks, the 8th rounds (Swift kept the fraction as a Double).
    let mut fraction_ticks = 0i64;
    if expect(&mut i, b'.') {
        let mut digits = 0;
        let mut round_up = false;
        while i < b.len() && b[i].is_ascii_digit() {
            let d = i64::from(b[i] - b'0');
            if digits < 7 {
                fraction_ticks = fraction_ticks * 10 + d;
            } else if digits == 7 {
                round_up = d >= 5;
            }
            digits += 1;
            i += 1;
        }
        if digits == 0 {
            return None;
        }
        for _ in digits.min(7)..7 {
            fraction_ticks *= 10;
        }
        if round_up {
            fraction_ticks += 1;
        }
    }

    let mut offset_seconds = 0i64;
    if i < b.len() {
        match b[i] {
            b'Z' | b'z' => i += 1,
            b'+' | b'-' => {
                let sign = if b[i] == b'-' { -1 } else { 1 };
                i += 1;
                let hours = number(&mut i, 2)?;
                let _ = expect(&mut i, b':');
                let minutes = number(&mut i, 2).unwrap_or(0);
                offset_seconds = sign * (hours * 3600 + minutes * 60);
            }
            _ => return None,
        }
    }
    if i != b.len() {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Some(Ticks(Ticks::UNIX_EPOCH + secs * TICKS_PER_SECOND + fraction_ticks))
}

fn fields(epoch_seconds: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = epoch_seconds.div_euclid(86_400);
    let rest = epoch_seconds.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, rest / 3600, rest % 3600 / 60, rest % 60)
}

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_coding_round_trips_and_reads_dot_net_shapes() {
        let date = from_unix_seconds(1_800_000_123.4567);
        let text = to_string(date);
        assert!(text.ends_with("+00:00"));
        assert!(text.contains('.'));
        let back = parse(&text).unwrap();
        assert!((to_unix_seconds(back) - to_unix_seconds(date)).abs() < 1e-6);

        assert_eq!(to_string(Ticks(Ticks::UNIX_EPOCH)), "1970-01-01T00:00:00.0000000+00:00");
        assert_eq!(parse("2026-11-02T18:30:15Z"), parse("2026-11-02T21:30:15+03:00"));
        assert_eq!(parse("2026-11-02T18:30:15"), parse("2026-11-02T18:30:15Z"));
        assert_eq!(parse("2026-11-02T18:30:15.5Z").unwrap().0, parse("2026-11-02T18:30:15Z").unwrap().0 + 5_000_000);
        assert_eq!(parse("not a date"), None);
        assert_eq!(parse("2026-13-02T18:30:15Z"), None);
        assert_eq!(compact_timestamp(Ticks(Ticks::UNIX_EPOCH)), "19700101-000000");
    }

    #[test]
    fn lenient_zones_and_fractions() {
        let base = parse("2026-05-01T07:00:00Z").unwrap();
        for text in ["2026-05-01T10:00:00+03", "2026-05-01T10:00:00+0300", "2026-05-01T10:00:00+03:00", "2026-05-01T10:00:00+03:"] {
            assert_eq!(parse(text), Some(base), "{text}");
        }
        assert_eq!(parse("2026-05-01T10:20:30.1234567+00:00").unwrap().0 % 10_000_000, 1_234_567);
        assert_eq!(parse("2026-05-01T10:20:30.12345678Z").unwrap().0 % 10_000_000, 1_234_568);
        assert_eq!(parse("2026-05-01T10:20:30.Z"), None);
        assert_eq!(parse("2026-05-01t10:20:30Z"), None);
        assert_eq!(parse("2026-05-01T10:20:30Zx"), None);
        // Feb 31 normalises arithmetically.
        assert_eq!(parse("2026-02-31T00:00:00Z"), parse("2026-03-03T00:00:00Z"));
        assert_eq!(to_string(parse("1969-12-31T23:59:59.5Z").unwrap()), "1969-12-31T23:59:59.5000000+00:00");
    }
}
