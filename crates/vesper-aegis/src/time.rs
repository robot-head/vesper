//! Strict UTC timestamps for vendor collateral. Library code never reads a
//! clock: callers pass `now` as Unix-epoch nanoseconds.

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Whole seconds of a nanosecond Unix time.
pub fn unix_seconds(now_ns: u64) -> u64 {
    now_ns / NANOS_PER_SECOND
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` (optionally with a fractional second, as
/// Intel writes in some PCS responses) into Unix seconds. Anything else,
/// including offsets other than `Z`, out-of-range fields and dates before
/// 1970, is `None`.
pub fn iso8601_utc_to_unix(s: &str) -> Option<u64> {
    crate::port::vesper::iso8601_utc_to_unix(s.as_bytes())
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
