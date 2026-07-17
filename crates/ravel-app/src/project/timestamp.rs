// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Wall-clock timestamps for project manifests, without a date/time crate.
//!
//! The manifest stamps `created_at` / `modified_at` as RFC 3339 strings. To
//! keep the dependency tree free of `time`/`chrono`, the Unix-epoch → civil
//! date conversion (Howard Hinnant's `civil_from_days`) is implemented here
//! directly. Only non-negative epoch seconds are supported, which covers
//! every real-world save.

use std::time::{SystemTime, UNIX_EPOCH};

/// The current time as an RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Falls back to the Unix epoch when the system clock is before 1970 (a
/// misconfigured clock must not panic a save).
pub fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_rfc3339(secs)
}

/// Format non-negative Unix epoch seconds as an RFC 3339 UTC timestamp.
fn format_rfc3339(secs: u64) -> String {
    let days = secs / 86_400;
    let secs_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days since 1970-01-01 to a `(year, month, day)` civil date.
/// Howard Hinnant's `civil_from_days` algorithm, valid for the full
/// non-negative input range.
fn civil_from_days(days: u64) -> (i64, u32, u32) {
    // Shift the epoch from 1970-01-01 to 0000-03-01 so that leap days fall
    // at the end of an era year.
    let z = days as i64 + 719_468;
    let era = z / 146_097;
    let doe = (z - era * 146_097) as u64; // day of era: [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero_is_unix_epoch() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn known_timestamps() {
        // Well-known epoch milestones.
        assert_eq!(format_rfc3339(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(format_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn leap_days_and_year_boundaries() {
        // 2000 is a leap year (divisible by 400), 2100 is not.
        assert_eq!(format_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_rfc3339(951_868_799), "2000-02-29T23:59:59Z");
        assert_eq!(format_rfc3339(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_609_459_200), "2021-01-01T00:00:00Z");
    }

    #[test]
    fn now_produces_a_plausible_timestamp() {
        let stamp = rfc3339_now();
        assert_eq!(stamp.len(), 20);
        assert!(stamp.ends_with('Z'));
        // Written in 2026; the clock must be past 2025.
        assert!(stamp.as_str() >= "2025-01-01T00:00:00Z");
    }
}
