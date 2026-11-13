//! Strict UTC timestamps for vendor collateral. Library code never reads a
//! clock: callers pass `now` as Unix-epoch nanoseconds.

/// Whole seconds of a nanosecond Unix time.
pub fn unix_seconds(now_ns: u64) -> u64 {
    now_ns / 1_000_000_000
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` (optionally with a fractional second, as
/// Intel writes in some PCS responses) into Unix seconds. Anything else,
/// including offsets other than `Z`, out-of-range fields and dates before
/// 1970, is `None`.
pub fn iso8601_utc_to_unix(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u64> {
        let mut v = 0u64;
        for &c in &b[r] {
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + u64::from(c - b'0');
        }
        Some(v)
    };
    let year = num(0..4)?;
    let month = num(5..7)?;
    let day = num(8..10)?;
    let hour = num(11..13)?;
    let minute = num(14..16)?;
    let second = num(17..19)?;
    let tail = &b[19..];
    let ok_tail = match tail {
        [b'Z'] => true,
        [b'.', digits @ .., b'Z'] => !digits.is_empty() && digits.iter().all(u8::is_ascii_digit),
        _ => false,
    };
    if !ok_tail
        || year < 1970
        || !(1..=12).contains(&month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let mdays = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day == 0 || day > mdays[(month - 1) as usize] {
        return None;
    }
    let days = days_from_civil(year as i64, month as i64, day as i64);
    let days = u64::try_from(days).ok()?;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(iso8601_utc_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            iso8601_utc_to_unix("2025-06-19T10:16:03Z"),
            Some(1_750_328_163)
        );
        assert_eq!(
            iso8601_utc_to_unix("2024-02-29T23:59:59.123Z"),
            Some(1_709_251_199)
        );
        for bad in [
            "2023-02-29T00:00:00Z",
            "2025-06-19T10:16:03",
            "2025-06-19T10:16:03+00:00",
            "2025-13-01T00:00:00Z",
            "1969-12-31T23:59:59Z",
            "2025-06-19T24:00:00Z",
            "2025-06-19T10:16:03.Z",
            "２025-06-19T10:16:03Z",
        ] {
            assert_eq!(iso8601_utc_to_unix(bad), None, "{bad}");
        }
    }
}
