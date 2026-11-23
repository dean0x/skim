//! Gregorian calendar conversions without a date dependency.

use crate::analytics::SECONDS_PER_DAY;

/// The input is not a date in the supported UTC range.
#[derive(Debug)]
pub(in crate::cmd) struct InvalidUtcDate;

/// Parse exactly `YYYY-MM-DD` as Unix seconds at 00:00 UTC.
///
/// The CLI accepts only years 1970 through 9999. This parser uses the actual
/// month length because its result controls which commits the caller reads.
#[must_use = "use the UTC cutoff or handle the invalid date"]
pub(in crate::cmd) fn parse_utc_date(date: &str) -> Result<u64, InvalidUtcDate> {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(InvalidUtcDate);
    }

    let year = digits(bytes, 0, 4).ok_or(InvalidUtcDate)?;
    let month = digits(bytes, 5, 2).ok_or(InvalidUtcDate)?;
    let day = digits(bytes, 8, 2).ok_or(InvalidUtcDate)?;
    if !(1970..=9999).contains(&year) {
        return Err(InvalidUtcDate);
    }

    let last_day = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return Err(InvalidUtcDate),
    };
    if !(1..=last_day).contains(&day) {
        return Err(InvalidUtcDate);
    }

    let days = u64::try_from(days_from_civil(year, month, day)).map_err(|_| InvalidUtcDate)?;
    Ok(days * SECONDS_PER_DAY)
}

/// Parse exactly `len` ASCII digits at `start`.
///
/// Accepts fields with one to four digits. Returns `None` for a short
/// slice or a non-digit.
pub(in crate::cmd) fn digits(bytes: &[u8], start: usize, len: usize) -> Option<i64> {
    if !(1..=4).contains(&len) {
        return None;
    }
    let end = start.checked_add(len)?;
    let slice = bytes.get(start..end)?;
    let mut acc: i64 = 0;
    for &byte in slice {
        if !byte.is_ascii_digit() {
            return None;
        }
        acc = acc * 10 + i64::from(byte - b'0');
    }
    Some(acc)
}

/// Days from 1970-01-01 to the given proleptic Gregorian civil date.
///
/// Uses Howard Hinnant's `days_from_civil` algorithm. The `y - 399`
/// adjustment makes truncating division give the required floor.
/// Callers supply four-digit years, months 1 to 12, and days 1 to 31.
/// This conversion does not check the length of each month.
#[must_use]
pub(in crate::cmd) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365] for valid dates
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Convert whole days from Unix seconds to (year, month, day).
///
/// Uses Howard Hinnant's `civil_from_days` algorithm:
/// <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>.
#[must_use]
pub(in crate::cmd) fn days_to_ymd(days_since_epoch: u64) -> (u64, u64, u64) {
    debug_assert!(days_since_epoch <= u64::MAX / SECONDS_PER_DAY);
    let z = days_since_epoch + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_days_from_civil_boundaries() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(1971, 1, 1), 365);
        assert_eq!(days_from_civil(1972, 1, 1), 730);
        assert_eq!(
            days_from_civil(1972, 3, 1) - days_from_civil(1972, 2, 29),
            1
        );
        assert_eq!(
            days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28),
            1
        );
        assert_eq!(
            days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28),
            2
        );
    }

    #[test]
    fn test_days_to_ymd_epoch_and_leap_day() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        assert_eq!(days_to_ymd(11_016), (2000, 2, 29));
        assert_eq!(days_to_ymd(11_017), (2000, 3, 1));
    }

    #[test]
    fn test_parse_utc_date_returns_exact_bounds_and_leap_days() {
        for (date, epoch) in [
            ("1970-01-01", 0),
            ("1972-02-29", 68_169_600),
            ("2000-02-29", 951_782_400),
            ("9999-12-31", 253_402_214_400),
        ] {
            assert_eq!(parse_utc_date(date).ok(), Some(epoch), "{date}");
        }
    }

    #[test]
    fn test_parse_utc_date_rejects_impossible_dates() {
        for date in [
            "1969-12-31",
            "10000-01-01",
            "2026-00-01",
            "2026-13-01",
            "2026-01-00",
            "2026-02-30",
            "2026-04-31",
            "2025-02-29",
            "2100-02-29",
        ] {
            assert!(parse_utc_date(date).is_err(), "{date}");
        }
    }

    #[test]
    fn test_parse_utc_date_requires_fixed_ascii_fields() {
        for date in [
            "2026/11/01",
            "2026-11-1",
            "2026-11-01 ",
            "2026-11-01T00:00:00Z",
            "2026-٢-01",
            "2026-11-+1",
        ] {
            assert!(parse_utc_date(date).is_err(), "{date}");
        }
    }
}
