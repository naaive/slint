// SPDX-License-Identifier: MIT

//! Human-readable sizes, rates, and durations.

use std::time::Duration;

const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/// Formats a byte count with binary units, such as `1.5 GiB`.
pub fn bytes(value: u64) -> String {
    scaled(value as f64)
}

fn scaled(value: f64) -> String {
    let mut value = if value.is_finite() { value.max(0.0) } else { 0.0 };
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[0])
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Formats bytes per second, such as `12 MiB/s`.
pub fn rate(bytes_per_second: f64) -> String {
    format!("{}/s", scaled(bytes_per_second))
}

/// Formats a percentage with one decimal below 10, such as `4.2%` or `37%`.
pub fn percent(value: f32) -> String {
    let value = if value.is_finite() { value.max(0.0) } else { 0.0 };
    if value < 10.0 { format!("{value:.1}%") } else { format!("{value:.0}%") }
}

/// Formats an uptime, such as `3 days, 4 h`, `2 h 05 min`, or `7 min`.
pub fn uptime(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes} min"),
        (0, h) => format!("{h} h {minutes:02} min"),
        (1, h) => format!("1 day, {h} h"),
        (d, h) => format!("{d} days, {h} h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_sizes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(200 * 1024 * 1024), "200 MiB");
        assert_eq!(bytes(u64::MAX), "16 EiB");
    }

    #[test]
    fn rates_and_percentages() {
        assert_eq!(rate(2048.0), "2.0 KiB/s");
        assert_eq!(rate(f64::NAN), "0 B/s");
        assert_eq!(percent(4.25), "4.2%");
        assert_eq!(percent(37.4), "37%");
        assert_eq!(percent(-1.0), "0.0%");
    }

    #[test]
    fn uptimes() {
        assert_eq!(uptime(Duration::from_secs(420)), "7 min");
        assert_eq!(uptime(Duration::from_secs(2 * 3600 + 300)), "2 h 05 min");
        assert_eq!(uptime(Duration::from_secs(86400 + 3600)), "1 day, 1 h");
        assert_eq!(uptime(Duration::from_secs(3 * 86400 + 4 * 3600)), "3 days, 4 h");
    }
}
