// SPDX-License-Identifier: MIT

//! Panel clock formats: presets, validation, and previews.

use chrono::NaiveDateTime;
use chrono::format::{Item, StrftimeItems};

/// Preset formats with a short description, in the order the UI lists them.
pub const PRESETS: [(&str, &str); 6] = [
    ("%H:%M", "24-hour"),
    ("%I:%M %p", "12-hour"),
    ("%a %H:%M", "Weekday and time"),
    ("%a %d %b  %H:%M", "Weekday, date, and time"),
    ("%a %d %b  %I:%M %p", "Weekday, date, and 12-hour time"),
    ("%Y-%m-%d %H:%M", "ISO 8601"),
];

/// Whether `format` is a non-empty `strftime` format that `chrono` can render.
pub fn is_valid_format(format: &str) -> bool {
    !format.trim().is_empty() && !StrftimeItems::new(format).any(|item| matches!(item, Item::Error))
}

/// Renders `format` at `time`, or `None` when the format is invalid.
pub fn preview(format: &str, time: NaiveDateTime) -> Option<String> {
    // `chrono` panics when displaying an invalid format, so it's validated first.
    is_valid_format(format).then(|| time.format(format).to_string())
}

/// The index of `format` in [`PRESETS`], or `PRESETS.len()` for a custom format.
pub fn preset_index(format: &str) -> usize {
    PRESETS.iter().position(|(f, _)| *f == format).unwrap_or(PRESETS.len())
}

/// The `strftime` fields of the 12-hour clock.
const TWELVE_HOUR: [&str; 5] = ["%I", "%l", "%r", "%p", "%P"];

/// Whether `format` shows no 12-hour field.
pub fn is_24_hour(format: &str) -> bool {
    !TWELVE_HOUR.iter().any(|field| format.contains(field))
}

/// `format` with its hours on the 24-hour clock, or on the 12-hour clock with AM or PM after the time.
/// A format without hours stays as it is.
pub fn with_24_hour(format: &str, on: bool) -> String {
    if on == is_24_hour(format) {
        return format.to_owned();
    }
    if on {
        return format
            .replace(" %p", "")
            .replace(" %P", "")
            .replace("%p", "")
            .replace("%P", "")
            .replace("%I", "%H")
            .replace("%l", "%k")
            .replace("%r", "%T");
    }
    let switched =
        format.replace("%H", "%I").replace("%k", "%l").replace("%R", "%I:%M").replace("%T", "%r");
    if switched == format || switched.contains("%r") {
        return switched;
    }
    // AM or PM goes right after the minutes or seconds.
    let end = ["%M", "%S"]
        .iter()
        .filter_map(|field| switched.rfind(field).map(|at| at + field.len()))
        .max()
        .unwrap_or(switched.len());
    format!("{} %p{}", &switched[..end], &switched[end..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn sample() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 5).and_then(|d| d.and_hms_opt(14, 7, 0)).unwrap()
    }

    #[test]
    fn previews() {
        assert_eq!(preview("%H:%M", sample()).as_deref(), Some("14:07"));
        assert_eq!(preview("%I:%M %p", sample()).as_deref(), Some("02:07 PM"));
        assert_eq!(preview("%a %d %b  %H:%M", sample()).as_deref(), Some("Mon 05 Oct  14:07"));
        assert_eq!(preview("%Q", sample()), None);
        assert_eq!(preview("", sample()), None);
        assert_eq!(preview("%", sample()), None);
        for (format, _) in PRESETS {
            assert!(is_valid_format(format), "{format}");
        }
    }

    #[test]
    fn switches_between_12_and_24_hours() {
        for (twelve, twenty_four) in [
            ("%I:%M %p", "%H:%M"),
            ("%a %d %b  %I:%M %p", "%a %d %b  %H:%M"),
            ("%Y-%m-%d %I:%M %p", "%Y-%m-%d %H:%M"),
            ("%l:%M:%S %p today", "%k:%M:%S today"),
            ("%r", "%T"),
        ] {
            assert!(!is_24_hour(twelve) && is_24_hour(twenty_four), "{twelve}");
            assert_eq!(with_24_hour(twelve, true), twenty_four);
            assert_eq!(with_24_hour(twenty_four, false), twelve);
            assert_eq!(with_24_hour(twelve, false), twelve);
        }
        assert_eq!(with_24_hour("%R", false), "%I:%M %p");
        assert_eq!(with_24_hour("%a %d %b", false), "%a %d %b", "no hours to switch");
        // Each preset switches to its counterpart.
        assert_eq!(with_24_hour(PRESETS[0].0, false), PRESETS[1].0);
        assert_eq!(with_24_hour(PRESETS[3].0, false), PRESETS[4].0);
    }

    #[test]
    fn preset_lookup() {
        assert_eq!(preset_index("%H:%M"), 0);
        assert_eq!(preset_index(nimbus_config::Panel::default().clock_format.as_str()), 3);
        assert_eq!(preset_index("%H"), PRESETS.len());
    }
}
