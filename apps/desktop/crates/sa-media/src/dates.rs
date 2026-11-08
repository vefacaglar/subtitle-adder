//! Timestamps of `media-index.json`: .NET ticks (100 ns since 0001-01-01 UTC) in memory, written as .NET
//! `DateTimeOffset` text on Windows (`2026-11-02T17:06:17.5945068+00:00`, trailing fraction zeros trimmed) and as
//! Apple ISO 8601 on macOS (`2026-11-02T17:06:17.594Z`, milliseconds truncated).

use std::time::{SystemTime, UNIX_EPOCH};

/// Ticks per second.
pub const TICKS_PER_SECOND: i64 = 10_000_000;
/// Ticks of 1970-01-01T00:00:00Z.
pub const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
const TICKS_PER_DAY: i64 = 86_400 * TICKS_PER_SECOND;
/// `DateTimeOffset.MaxValue.UtcTicks`.
pub const MAX_TICKS: i64 = 3_155_378_975_999_999_999;

/// Days since 1970-01-01 of a proleptic Gregorian civil date (Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

struct Parts {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    /// 0..10_000_000
    fraction_ticks: i64,
}

fn split(ticks: i64) -> Parts {
    let ticks = ticks.clamp(0, MAX_TICKS);
    let rel = ticks - UNIX_EPOCH_TICKS;
    let days = rel.div_euclid(TICKS_PER_DAY);
    let in_day = rel.rem_euclid(TICKS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let secs = in_day / TICKS_PER_SECOND;
    Parts {
        year,
        month,
        day,
        hour: secs / 3600,
        minute: secs / 60 % 60,
        second: secs % 60,
        fraction_ticks: in_day % TICKS_PER_SECOND,
    }
}

/// System.Text.Json `DateTimeOffset` text at offset `+00:00`: `yyyy-MM-ddTHH:mm:ss[.fffffff]+00:00`, trailing
/// fraction zeros trimmed, no fraction when zero. Ticks 0 → `0001-01-01T00:00:00+00:00`.
pub fn format_dotnet(ticks: i64) -> String {
    let p = split(ticks);
    let mut out = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", p.year, p.month, p.day, p.hour, p.minute, p.second);
    if p.fraction_ticks != 0 {
        let digits = format!("{:07}", p.fraction_ticks);
        out.push('.');
        out.push_str(digits.trim_end_matches('0'));
    }
    out.push_str("+00:00");
    out
}

/// Apple `Date.ISO8601FormatStyle(includingFractionalSeconds: true)`: `yyyy-MM-ddTHH:mm:ss.SSSZ` in UTC, truncated to
/// milliseconds.
pub fn format_apple(ticks: i64) -> String {
    let p = split(ticks);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        p.year,
        p.month,
        p.day,
        p.hour,
        p.minute,
        p.second,
        p.fraction_ticks / 10_000
    )
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn digits(&mut self, n: usize) -> Option<i64> {
        if self.i + n > self.b.len() {
            return None;
        }
        let mut v = 0i64;
        for k in 0..n {
            let c = self.b[self.i + k];
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + i64::from(c - b'0');
        }
        self.i += n;
        Some(v)
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn done(&self) -> bool {
        self.i == self.b.len()
    }
}

enum Zone {
    Utc,
    Offset(i64),
    Missing,
}

/// The System.Text.Json `DateTimeOffset` reader (`dotnet`, verified against .NET 10) or the Foundation ISO 8601 reader
/// LibraryKit uses.
///
/// .NET: `yyyy-MM-dd`, optionally `THH:mm`, `THH:mm:ss` and `.` with up to 16 fraction digits (none is allowed;
/// digits past the seventh are truncated), then optionally `Z` or `+HH` / `+HH:MM` up to 14:00. Upper-case `T`/`Z`
/// only, no `,` fraction, no `+HHMM`.
fn parse_common(text: &str, dotnet: bool) -> Option<(Parts, Zone)> {
    let mut c = Cursor { b: text.as_bytes(), i: 0 };
    let year = c.digits(4)?;
    if !c.eat(b'-') {
        return None;
    }
    let month = c.digits(2)?;
    if !c.eat(b'-') {
        return None;
    }
    let day = c.digits(2)?;
    let (mut hour, mut minute, mut second, mut fraction_ticks) = (0, 0, 0, 0);
    if c.eat(b'T') {
        hour = c.digits(2)?;
        if !c.eat(b':') {
            return None;
        }
        minute = c.digits(2)?;
        if c.eat(b':') {
            second = c.digits(2)?;
            if c.eat(b'.') {
                let start = c.i;
                let mut scale = 1_000_000i64;
                while let Some(d) = c.peek().filter(u8::is_ascii_digit) {
                    if scale > 0 {
                        fraction_ticks += i64::from(d - b'0') * scale;
                        scale /= 10;
                    }
                    c.i += 1;
                }
                let digits = c.i - start;
                if (dotnet && digits > 16) || (!dotnet && digits == 0) {
                    return None;
                }
            }
        } else if !dotnet {
            return None;
        }
    } else if !dotnet || !c.done() {
        return None;
    }
    let zone = match c.peek() {
        None => Zone::Missing,
        Some(b'Z') => {
            c.i += 1;
            Zone::Utc
        }
        Some(b'z') if !dotnet => {
            c.i += 1;
            Zone::Utc
        }
        Some(sign @ (b'+' | b'-')) => {
            c.i += 1;
            let h = c.digits(2)?;
            let m = if c.done() {
                0
            } else if dotnet {
                if !c.eat(b':') {
                    return None;
                }
                c.digits(2)?
            } else {
                let _ = c.eat(b':');
                c.digits(2)?
            };
            if h > 23 || m > 59 || (dotnet && h * 60 + m > 14 * 60) {
                return None;
            }
            let off = (h * 60 + m) * 60;
            Zone::Offset(if sign == b'-' { -off } else { off })
        }
        Some(_) => return None,
    };
    if !c.done() || !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days_in_month = match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > days_in_month || year < 1 {
        return None;
    }
    Some((Parts { year, month, day, hour, minute, second, fraction_ticks }, zone))
}

fn to_ticks(p: &Parts, offset_seconds: i64) -> Option<i64> {
    let days = days_from_civil(p.year, p.month, p.day);
    let secs = days * 86_400 + p.hour * 3600 + p.minute * 60 + p.second - offset_seconds;
    let ticks = secs.checked_mul(TICKS_PER_SECOND)?.checked_add(UNIX_EPOCH_TICKS)?.checked_add(p.fraction_ticks)?;
    (0..=MAX_TICKS).contains(&ticks).then_some(ticks)
}

fn local_offset_seconds(p: &Parts) -> i64 {
    use chrono::{Local, NaiveDate, Offset, TimeZone};
    NaiveDate::from_ymd_opt(p.year as i32, p.month as u32, p.day as u32)
        .and_then(|d| d.and_hms_opt(p.hour as u32, p.minute as u32, p.second as u32))
        .and_then(|naive| Local.offset_from_local_datetime(&naive).earliest())
        .map(|o| i64::from(o.fix().local_minus_utc()))
        .unwrap_or(0)
}

/// System.Text.Json `DateTimeOffset` reader: `yyyy-MM-dd[THH:mm[:ss[.f…]]][Z|±HH[:MM]]` (see `parse_common`); a
/// missing offset is local time. Returns UTC ticks.
pub fn parse_dotnet(text: &str) -> Option<i64> {
    let (parts, zone) = parse_common(text, true)?;
    let offset = match zone {
        Zone::Utc => 0,
        Zone::Offset(o) => o,
        Zone::Missing => local_offset_seconds(&parts),
    };
    to_ticks(&parts, offset)
}

/// Foundation `ISO8601FormatStyle` reader as used by LibraryKit: `yyyy-MM-ddTHH:mm:ss[.f…]` followed by `Z` or a
/// numeric offset (a missing zone fails). Returns UTC ticks.
pub fn parse_apple(text: &str) -> Option<i64> {
    let (parts, zone) = parse_common(text, false)?;
    let offset = match zone {
        Zone::Utc => 0,
        Zone::Offset(o) => o,
        Zone::Missing => return None,
    };
    to_ticks(&parts, offset)
}

/// A `SystemTime` as UTC ticks (100 ns resolution; clamped to the .NET range).
pub fn system_time_ticks(t: SystemTime) -> i64 {
    let rel = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos() / 100).unwrap_or(i64::MAX - UNIX_EPOCH_TICKS),
        Err(e) => -i64::try_from(e.duration().as_nanos().div_ceil(100)).unwrap_or(UNIX_EPOCH_TICKS),
    };
    (UNIX_EPOCH_TICKS + rel).clamp(0, MAX_TICKS)
}

/// Now as UTC ticks.
pub fn now_ticks() -> i64 {
    system_time_ticks(SystemTime::now())
}

/// Ticks of a Unix timestamp in seconds (tests and fixtures).
pub fn unix_seconds_ticks(seconds: i64) -> i64 {
    UNIX_EPOCH_TICKS + seconds * TICKS_PER_SECOND
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_format_trims_fraction_and_uses_plus_zero() {
        assert_eq!(format_dotnet(0), "0001-01-01T00:00:00+00:00");
        let t = parse_dotnet("2026-11-07T10:00:00.1234567+00:00").unwrap();
        assert_eq!(format_dotnet(t), "2026-11-07T10:00:00.1234567+00:00");
        assert_eq!(format_dotnet(parse_dotnet("2026-11-07T10:00:00.12+00:00").unwrap()), "2026-11-07T10:00:00.12+00:00");
        assert_eq!(format_dotnet(parse_dotnet("2026-11-07T10:00:00Z").unwrap()), "2026-11-07T10:00:00+00:00");
        assert_eq!(format_dotnet(unix_seconds_ticks(0)), "1970-01-01T00:00:00+00:00");
    }

    #[test]
    fn apple_format_truncates_to_milliseconds() {
        let t = parse_apple("2026-01-02T03:04:05.1239Z").unwrap();
        assert_eq!(format_apple(t), "2026-01-02T03:04:05.123Z");
        assert_eq!(format_apple(unix_seconds_ticks(0)), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn offsets_convert_to_utc() {
        assert_eq!(parse_apple("2026-11-02T18:30:15Z"), parse_apple("2026-11-02T21:30:15+03:00"));
        assert_eq!(parse_dotnet("2026-01-02T03:04:05+03:00"), parse_dotnet("2026-01-02T00:04:05Z"));
        assert_eq!(parse_apple("2026-01-02T03:04:05.5Z").unwrap() - parse_apple("2026-01-02T03:04:05Z").unwrap(), 5_000_000);
    }

    #[test]
    fn invalid_dates_fail() {
        assert_eq!(parse_apple("yesterday"), None);
        assert_eq!(parse_apple("2026-13-02T00:00:00Z"), None);
        assert_eq!(parse_apple("2026-01-02T03:04:05"), None, "Apple requires a zone");
        assert!(parse_dotnet("2026-01-02T03:04:05").is_some(), ".NET reads a missing offset as local time");
        assert_eq!(parse_dotnet("2026-02-30T00:00:00Z"), None);
        assert_eq!(parse_dotnet(""), None);
    }

    /// What System.Text.Json (.NET 10) accepts for a `DateTimeOffset`, as UTC ticks (`None` = `JsonException`).
    #[test]
    fn dotnet_reader_matches_system_text_json() {
        let vectors: [(&str, Option<i64>); 18] = [
            ("2026-11-01T10:00Z", Some(639_291_240_000_000_000)),
            ("2026-11-01T10:00+03:00", Some(639_291_132_000_000_000)),
            ("2026-11-01t10:00:00Z", None),
            ("2026-11-01T10:00:00,5Z", None),
            ("2026-11-01T10:00:00.12345678901Z", Some(639_291_240_001_234_567)),
            ("2026-11-01T10:00:00.1234567890123456Z", Some(639_291_240_001_234_567)),
            ("2026-11-01T10:00:00.12345678901234567Z", None),
            ("2026-11-01T10:00:00+03", Some(639_291_132_000_000_000)),
            ("2026-11-01T10:00:00+0300", None),
            ("2026-11-01T10:00:00z", None),
            ("2026-11-01 10:00:00Z", None),
            ("2026-11-01T24:00:00Z", None),
            (" 2026-11-01T10:00:00Z", None),
            ("2026-11-01T10:00:00+14:00", Some(639_290_736_000_000_000)),
            ("2026-11-01T10:00:00+15:00", None),
            ("2026-11-01T10:00:00.Z", Some(639_291_240_000_000_000)),
            ("2026-11-01T10:00:00.99999999Z", Some(639_291_240_009_999_999)),
            ("2026-11-01T10:00:00-00:30", Some(639_291_258_000_000_000)),
        ];
        for (text, ticks) in vectors {
            assert_eq!(parse_dotnet(text), ticks, "{text}");
        }
        assert_eq!(parse_dotnet("0000-01-01T00:00:00Z"), None);
        assert_eq!(parse_dotnet("2026-11-01T10:00:00+03:0"), None);
        assert!(parse_dotnet("2026-11-01").is_some(), "a date alone is local midnight");
    }

    #[test]
    fn civil_round_trip() {
        for days in [-719_162, -1, 0, 1, 19_000, 2_932_896] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }
}
