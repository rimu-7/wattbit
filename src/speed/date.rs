//! Local calendar dates for usage records, without a date library.
//!
//! Usage is bucketed by local calendar day. The agent runs for months, so
//! the date handling is small and explicit: `localtime_r` provides the
//! current day, and Howard Hinnant's civil-time algorithms convert between
//! calendar dates and day numbers for arithmetic.

use serde::{Deserialize, Serialize};

/// A local calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LocalDate {
    year: i32,
    month: u32,
    day: u32,
}

impl LocalDate {
    /// Builds a date, rejecting impossible ones such as 2026-02-30.
    pub fn new(year: i32, month: u32, day: u32) -> Option<Self> {
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        let round_trip = Self::from_days(days_from_civil(year, month, day));
        (round_trip == Self { year, month, day }).then_some(round_trip)
    }

    /// The current date in the local time zone.
    pub fn today() -> Self {
        Self::from_unix(now_unix())
    }

    /// The local date at `unix_secs` (seconds since the Unix epoch).
    pub fn from_unix(unix_secs: i64) -> Self {
        // SAFETY: a zeroed `tm` is a valid starting value.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `localtime_r` has no preconditions; `tm` is a valid
        // out-pointer.
        let converted = unsafe { libc::localtime_r(&unix_secs, &mut tm) };
        if converted.is_null() {
            return Self {
                year: 1970,
                month: 1,
                day: 1,
            };
        }
        Self {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
        }
    }

    /// Parses an ISO `YYYY-MM-DD` date.
    pub fn parse(iso: &str) -> Option<Self> {
        let bytes = iso.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let year: i32 = iso[0..4].parse().ok()?;
        let month: u32 = iso[5..7].parse().ok()?;
        let day: u32 = iso[8..10].parse().ok()?;
        Self::new(year, month, day)
    }

    /// The date as `YYYY-MM-DD`.
    pub fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Days since the Unix epoch (1970-01-01), negative before it.
    pub fn days(self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// The inverse of [`LocalDate::days`].
    pub fn from_days(days: i64) -> Self {
        let (year, month, day) = civil_from_days(days);
        Self { year, month, day }
    }

    /// A date `days` days after this one (negative moves backwards).
    pub fn add_days(self, days: i64) -> Self {
        Self::from_days(self.days() + days)
    }
}

impl std::fmt::Display for LocalDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.iso())
    }
}

/// Seconds since the Unix epoch.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM:SS` in the local time zone.
pub fn format_local(unix_secs: i64) -> String {
    // SAFETY: a zeroed `tm` is a valid starting value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` has no preconditions; `tm` is a valid
    // out-pointer.
    let converted = unsafe { libc::localtime_r(&unix_secs, &mut tm) };
    if converted.is_null() {
        return unix_secs.to_string();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Days from the civil date `y-m-d` to 1970-01-01 (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let year_of_era = y - era * 400; // [0, 399]
    let month = i64::from(m);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(d) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year; // [0, 146096]
    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`] (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let y = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153; // [0, 11]
    let d = day_of_year - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = mp + if mp < 10 { 3 } else { -9 }; // [1, 12]
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_round_trips() {
        let epoch = LocalDate::from_days(0);
        assert_eq!(epoch.iso(), "1970-01-01");
        assert_eq!(epoch.days(), 0);
    }

    #[test]
    fn leap_years_are_handled() {
        let leap = LocalDate::new(2024, 2, 29).unwrap();
        assert_eq!(leap.add_days(1).iso(), "2024-03-01");
        assert_eq!(leap.add_days(-1).iso(), "2024-02-28");
        assert_eq!(LocalDate::new(2023, 2, 29), None);
        assert_eq!(LocalDate::new(2026, 2, 29), None);
        assert_eq!(LocalDate::new(2026, 4, 31), None);
    }

    #[test]
    fn parsing_rejects_malformed_input() {
        assert_eq!(LocalDate::parse("2026-10-18").unwrap().iso(), "2026-10-18");
        for bad in [
            "",
            "2026-1-18",
            "2026-10-18T00",
            "20261018",
            "2026/10/18",
            "2026-13-01",
            "2026-00-10",
            "2026-10-00",
            "abcd-10-18",
        ] {
            assert_eq!(LocalDate::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn arithmetic_matches_known_offsets() {
        let base = LocalDate::new(2026, 3, 1).unwrap();
        assert_eq!(base.add_days(-1).iso(), "2026-02-28");
        assert_eq!(base.add_days(-30).iso(), "2026-01-30");
        assert_eq!(base.add_days(365).iso(), "2027-03-01");
        // Year boundary and leap day inside the window.
        let base = LocalDate::new(2024, 1, 15).unwrap();
        assert_eq!(base.add_days(-31).iso(), "2023-12-15");
    }

    #[test]
    fn civil_day_numbers_are_stable() {
        // Values cross-checked against `date -j -f %Y-%m-%d +%s` and Python's
        // `datetime.date`.
        assert_eq!(LocalDate::new(2026, 10, 18).unwrap().days(), 20_744);
        assert_eq!(LocalDate::new(1969, 12, 31).unwrap().days(), -1);
        assert_eq!(LocalDate::new(1970, 1, 2).unwrap().days(), 1);
    }

    #[test]
    fn formatting_is_iso_shaped() {
        let date = LocalDate::from_days(20_744);
        assert_eq!(format_local(0).len(), 19);
        assert!(LocalDate::parse(&date.iso()).is_some());
    }
}
