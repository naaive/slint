// SPDX-License-Identifier: MIT

//! Human-readable sizes, counts, dates, and permissions.

use std::time::SystemTime;

use chrono::{DateTime, Datelike as _, Local, NaiveDateTime};

/// A size with decimal units, as GNOME and KDE show them: "512 bytes", "1.2 kB", "3.4 GB".
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["kB", "MB", "GB", "TB", "PB", "EB"];
    if bytes == 1 {
        return "1 byte".to_string();
    }
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub fn item_count(count: u64) -> String {
    if count == 1 { "1 item".to_string() } else { format!("{count} items") }
}

pub fn local_time(time: SystemTime) -> NaiveDateTime {
    DateTime::<Local>::from(time).naive_local()
}

pub fn now() -> NaiveDateTime {
    Local::now().naive_local()
}

/// A short, relative date for list columns: "14:05" today, "Yesterday", the weekday within a week,
/// "12 Mar" this year, and "12 Mar 2023" otherwise.
pub fn short_date(time: NaiveDateTime, now: NaiveDateTime) -> String {
    let days = (now.date() - time.date()).num_days();
    match days {
        0 => time.format("%H:%M").to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => time.format("%A").to_string(),
        _ if time.year() == now.year() && days > 0 => time.format("%-d %b").to_string(),
        _ => time.format("%-d %b %Y").to_string(),
    }
}

/// A complete date and time for the properties dialog, such as "Tue 12 Mar 2024, 14:05".
pub fn long_date(time: NaiveDateTime) -> String {
    time.format("%a %-d %b %Y, %H:%M").to_string()
}

/// Permission bits as `ls` shows them, such as "rwxr-xr-x", including setuid, setgid, and sticky bits.
pub fn permissions(mode: u32) -> String {
    let bit = |mask: u32, c: char| if mode & mask != 0 { c } else { '-' };
    let special = |exec: u32, special: u32, set: char, unset: char| match (
        mode & exec != 0,
        mode & special != 0,
    ) {
        (true, true) => set,
        (false, true) => unset,
        (true, false) => 'x',
        (false, false) => '-',
    };
    [
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        special(0o100, 0o4000, 's', 'S'),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        special(0o010, 0o2000, 's', 'S'),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        special(0o001, 0o1000, 't', 'T'),
    ]
    .into_iter()
    .collect()
}

/// What one class of users may do, in words, such as "Read and write".
pub fn access(mode: u32, shift: u32, is_dir: bool) -> &'static str {
    let bits = (mode >> shift) & 0o7;
    let (read, write, exec) = (bits & 4 != 0, bits & 2 != 0, bits & 1 != 0);
    match (is_dir, read, write, exec) {
        (true, true, true, true) => "Create and delete files",
        (true, true, false, true) => "Access files",
        (true, true, _, false) => "List files only",
        (true, false, _, _) => "None",
        (false, true, true, _) => "Read and write",
        (false, true, false, _) => "Read-only",
        (false, false, true, _) => "Write-only",
        (false, false, false, _) => "None",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .and_then(|date| date.and_hms_opt(h, min, 0))
            .expect("valid date")
    }

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 bytes");
        assert_eq!(size(1), "1 byte");
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1000), "1.0 kB");
        assert_eq!(size(1234), "1.2 kB");
        assert_eq!(size(999_949), "999.9 kB");
        assert_eq!(size(999_999), "1.0 MB");
        assert_eq!(size(4_500_000_000), "4.5 GB");
        assert_eq!(size(u64::MAX), "18.4 EB");
    }

    #[test]
    fn counts() {
        assert_eq!(item_count(0), "0 items");
        assert_eq!(item_count(1), "1 item");
        assert_eq!(item_count(12), "12 items");
    }

    #[test]
    fn relative_dates() {
        let now = at(2026, 10, 5, 15, 0);
        assert_eq!(short_date(at(2026, 10, 5, 9, 7), now), "09:07");
        assert_eq!(short_date(at(2026, 10, 4, 23, 59), now), "Yesterday");
        assert_eq!(short_date(at(2026, 10, 1, 8, 0), now), "Thursday");
        assert_eq!(short_date(at(2026, 3, 12, 8, 0), now), "12 Mar");
        assert_eq!(short_date(at(2023, 3, 12, 8, 0), now), "12 Mar 2023");
        assert_eq!(short_date(at(2026, 12, 24, 8, 0), now), "24 Dec 2026");
        assert_eq!(long_date(at(2024, 3, 12, 14, 5)), "Tue 12 Mar 2024, 14:05");
    }

    #[test]
    fn permission_strings() {
        assert_eq!(permissions(0o755), "rwxr-xr-x");
        assert_eq!(permissions(0o644), "rw-r--r--");
        assert_eq!(permissions(0o4755), "rwsr-xr-x");
        assert_eq!(permissions(0o2644), "rw-r-Sr--");
        assert_eq!(permissions(0o1777), "rwxrwxrwt");
        assert_eq!(permissions(0o1776), "rwxrwxrwT");
        assert_eq!(permissions(0), "---------");
    }

    #[test]
    fn access_words() {
        assert_eq!(access(0o640, 6, false), "Read and write");
        assert_eq!(access(0o640, 3, false), "Read-only");
        assert_eq!(access(0o640, 0, false), "None");
        assert_eq!(access(0o200, 6, false), "Write-only");
        assert_eq!(access(0o755, 6, true), "Create and delete files");
        assert_eq!(access(0o755, 3, true), "Access files");
        assert_eq!(access(0o744, 0, true), "List files only");
        assert_eq!(access(0o700, 0, true), "None");
    }
}
