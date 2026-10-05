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
    fn preset_lookup() {
        assert_eq!(preset_index("%H:%M"), 0);
        assert_eq!(preset_index(nimbus_config::Panel::default().clock_format.as_str()), 3);
        assert_eq!(preset_index("%H"), PRESETS.len());
    }
}
