// SPDX-License-Identifier: GPL-3.0-only
//! RFC 3339 UTC timestamps (`2026-11-12T10:00:00Z`) from Unix milliseconds.

pub fn rfc3339(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Parse an RFC 3339 timestamp (`YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)`, `t`/space
/// separator accepted) into nanoseconds since the Unix epoch. Used to compare journal
/// times written by different packages (with and without fractional seconds, other
/// offsets). `None` when the text is not of that shape.
pub fn parse_rfc3339_ns(s: &str) -> Option<i128> {
    let b = s.trim().as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = b.get(r)?;
        if part.is_empty() || !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d, h, mi, sec) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut frac_ns: i64 = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return None;
        }
        let digits = &s.trim()[start..i];
        let mut padded: String = digits.chars().take(9).collect();
        while padded.len() < 9 {
            padded.push('0');
        }
        frac_ns = padded.parse().ok()?;
    }
    let offset_s: i64 = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() && b[i + 3] == b':' => {
            let o = num(i + 1..i + 3)? * 3600 + num(i + 4..i + 6)? * 60;
            if *sign == b'+' { o } else { -o }
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo as u32, d as u32);
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset_s;
    Some(secs as i128 * 1_000_000_000 + frac_ns as i128)
}

/// Inverse of `civil_from_days` (Howard Hinnant, public domain).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Howard Hinnant's days-to-civil algorithm (public domain).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_dates() {
        assert_eq!(super::rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::rfc3339(1_793_354_400_000), "2026-10-30T10:00:00Z");
        assert_eq!(super::rfc3339(951_782_400_000), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn parses_rfc3339_forms() {
        use super::parse_rfc3339_ns as p;
        assert_eq!(p("2026-10-30T10:00:00Z"), Some(1_793_354_400_000_000_000));
        assert_eq!(p("2000-02-29T00:00:00Z"), Some(951_782_400_000_000_000));
        assert_eq!(p("2026-10-30T10:00:00.5Z"), Some(1_793_354_400_500_000_000));
        assert_eq!(p("2026-10-30T12:00:00+02:00"), p("2026-10-30T10:00:00Z"));
        // Fractional form sorts after the whole second it belongs to.
        assert!(p("2026-10-30T10:00:00.100Z") > p("2026-10-30T10:00:00Z"));
        assert!(p("2026-10-30T10:00:00.9Z") < p("2026-10-30T10:00:01Z"));
        assert_eq!(p("t1"), None);
        assert_eq!(p("2026-10-30T10:00:00"), None);
    }
}
