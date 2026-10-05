// SPDX-License-Identifier: MIT

//! Clock text, the calendar's month grid, and relative notification times.

use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Datelike, Months, NaiveDate, TimeZone, Timelike, Weekday};

/// The panel clock format used when the configured one is invalid.
pub const FALLBACK_CLOCK_FORMAT: &str = "%a %d %b  %H:%M";

/// Formats `time` with a `chrono` format string, falling back to `fallback` when `format` is invalid.
pub fn format_time<Tz: TimeZone>(time: &DateTime<Tz>, format: &str, fallback: &str) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut text = String::new();
    // `to_string` would panic on an invalid format; `write!` reports it instead.
    if write!(text, "{}", time.format(format)).is_ok() {
        return text;
    }
    text.clear();
    if write!(text, "{}", time.format(fallback)).is_err() {
        text.clear();
    }
    text
}

/// How long until the next minute starts, plus a small margin so that a timer firing then sees the new minute.
pub fn until_next_minute<Tz: TimeZone>(now: &DateTime<Tz>) -> Duration {
    let elapsed = Duration::new(u64::from(now.second().min(59)), now.nanosecond() % 1_000_000_000);
    Duration::from_secs(60).saturating_sub(elapsed) + Duration::from_millis(20)
}

/// One cell of the month grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Day {
    pub date: NaiveDate,
    pub in_month: bool,
    pub today: bool,
    pub weekend: bool,
}

/// Returns six weeks of days, Monday first, covering the month of `year`/`month`.
/// Returns an empty grid for an invalid month.
pub fn month_grid(year: i32, month: u32, today: NaiveDate) -> Vec<Day> {
    let Some(first) = NaiveDate::from_ymd_opt(year, month, 1) else {
        return Vec::new();
    };
    let lead = u64::from(first.weekday().num_days_from_monday());
    let Some(start) = first.checked_sub_days(chrono::Days::new(lead)) else {
        return Vec::new();
    };
    start
        .iter_days()
        .take(42)
        .map(|date| Day {
            date,
            in_month: date.month() == month && date.year() == year,
            today: date == today,
            weekend: matches!(date.weekday(), Weekday::Sat | Weekday::Sun),
        })
        .collect()
}

/// Returns the month `delta` months after `year`/`month`, or the same month if that overflows.
pub fn add_months(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let Some(first) = NaiveDate::from_ymd_opt(year, month, 1) else {
        return (year, month);
    };
    let shifted = if delta >= 0 {
        first.checked_add_months(Months::new(delta.unsigned_abs()))
    } else {
        first.checked_sub_months(Months::new(delta.unsigned_abs()))
    };
    shifted.map_or((year, month), |date| (date.year(), date.month()))
}

/// A month title such as "October 2026".
pub fn month_title(year: i32, month: u32) -> String {
    NaiveDate::from_ymd_opt(year, month, 1)
        .map(|date| date.format("%B %Y").to_string())
        .unwrap_or_default()
}

/// How long ago `then` was, such as "now", "5 min ago", or "3 h ago".
pub fn relative_time(then: SystemTime, now: SystemTime) -> String {
    let elapsed = now.duration_since(then).unwrap_or_default().as_secs();
    match elapsed {
        0..60 => "now".into(),
        60..3_600 => format!("{} min ago", elapsed / 60),
        3_600..86_400 => format!("{} h ago", elapsed / 3_600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", elapsed / 86_400),
    }
}

/// A duration such as "2 h 5 min" or "45 min".
pub fn short_duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(h: u32, m: u32, s: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(3600)
            .and_then(|tz| tz.with_ymd_and_hms(2026, 10, 5, h, m, s).single())
            .expect("valid time")
    }

    #[test]
    fn formats_and_falls_back() {
        let time = at(9, 7, 0);
        assert_eq!(format_time(&time, "%H:%M", FALLBACK_CLOCK_FORMAT), "09:07");
        assert_eq!(format_time(&time, FALLBACK_CLOCK_FORMAT, "%H:%M"), "Mon 05 Oct  09:07");
        // `%Q` isn't a specifier, and a lone `%` is incomplete.
        assert_eq!(format_time(&time, "%Q", "%H:%M"), "09:07");
        assert_eq!(format_time(&time, "%H %", "%H:%M"), "09:07");
        assert_eq!(format_time(&time, "%Q", "%Q"), "");
    }

    #[test]
    fn next_minute_is_aligned() {
        assert_eq!(until_next_minute(&at(9, 7, 0)), Duration::from_millis(60_020));
        assert_eq!(until_next_minute(&at(9, 7, 59)), Duration::from_millis(1_020));
        assert_eq!(until_next_minute(&at(9, 7, 30)), Duration::from_millis(30_020));
    }

    #[test]
    fn month_grid_starts_on_monday() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 5).expect("valid date");
        let grid = month_grid(2026, 10, today);
        assert_eq!(grid.len(), 42);
        // October 2026 starts on a Thursday.
        assert_eq!(grid[0].date, NaiveDate::from_ymd_opt(2026, 9, 28).expect("valid date"));
        assert!(!grid[0].in_month);
        assert!(grid[3].in_month);
        assert_eq!(grid[3].date.day(), 1);
        assert!(grid[7].today && grid[7].date == today);
        assert_eq!(grid.iter().filter(|d| d.today).count(), 1);
        assert!(grid[5].weekend && grid[6].weekend && !grid[4].weekend);
        assert_eq!(grid.iter().filter(|d| d.in_month).count(), 31);
        assert!(month_grid(2026, 13, today).is_empty());
    }

    #[test]
    fn months_wrap_across_years() {
        assert_eq!(add_months(2026, 12, 1), (2027, 1));
        assert_eq!(add_months(2026, 1, -1), (2025, 12));
        assert_eq!(add_months(2026, 5, 0), (2026, 5));
        assert_eq!(add_months(2026, 5, -17), (2024, 12));
        assert_eq!(add_months(2026, 0, 1), (2026, 0));
        assert_eq!(month_title(2026, 10), "October 2026");
        assert_eq!(month_title(2026, 13), "");
    }

    #[test]
    fn relative_times() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ago = |secs| relative_time(now - Duration::from_secs(secs), now);
        assert_eq!(ago(5), "now");
        assert_eq!(ago(125), "2 min ago");
        assert_eq!(ago(7_300), "2 h ago");
        assert_eq!(ago(90_000), "yesterday");
        assert_eq!(ago(3 * 86_400), "3 days ago");
        assert_eq!(relative_time(now + Duration::from_secs(30), now), "now");
    }

    #[test]
    fn durations() {
        assert_eq!(short_duration(Duration::from_secs(45 * 60)), "45 min");
        assert_eq!(short_duration(Duration::from_secs(2 * 3600)), "2 h");
        assert_eq!(short_duration(Duration::from_secs(2 * 3600 + 5 * 60 + 30)), "2 h 5 min");
    }
}
