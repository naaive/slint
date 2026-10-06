// SPDX-License-Identifier: MIT

//! Time zone names from the tz database, for systems whose timedated lacks `ListTimezones`, which came in systemd 251.

use std::collections::BTreeSet;
use std::path::Path;

/// The zone and link names in `tzdata.zi` below `zoneinfo`, such as `/usr/share/zoneinfo`,
/// or the zones in `zone1970.tab` without it, sorted and with `UTC`; empty when neither is there.
pub fn zone_names(zoneinfo: &Path) -> Vec<String> {
    let mut names = BTreeSet::new();
    if let Ok(data) = std::fs::read_to_string(zoneinfo.join("tzdata.zi")) {
        for line in data.lines() {
            let mut fields = line.split_whitespace();
            match (fields.next(), fields.next(), fields.next()) {
                (Some("Z"), Some(zone), _) => names.insert(zone.to_owned()),
                (Some("L"), Some(_), Some(link)) => names.insert(link.to_owned()),
                _ => false,
            };
        }
    } else if let Ok(table) = std::fs::read_to_string(zoneinfo.join("zone1970.tab")) {
        let zones = table.lines().filter(|line| !line.starts_with('#'));
        names.extend(zones.filter_map(|line| line.split('\t').nth(2)).map(str::to_owned));
    }
    if !names.is_empty() {
        names.insert("UTC".to_owned());
    }
    names.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tzdata_or_the_zone_table() {
        let dir = tempfile::tempdir().unwrap();
        assert!(zone_names(dir.path()).is_empty());
        std::fs::write(
            dir.path().join("zone1970.tab"),
            "# comment\nDE,DK\t+5230+01322\tEurope/Berlin\nUS\t+404251-0740023\tAmerica/New_York\tEastern (most areas)\n",
        )
        .unwrap();
        assert_eq!(zone_names(dir.path()), ["America/New_York", "Europe/Berlin", "UTC"]);
        std::fs::write(
            dir.path().join("tzdata.zi"),
            "# version 2025b\nR d 1916 o - Ap 30 23 1 S\nZ Europe/Berlin 0:53:28 - LMT 1893 Ap\nL Europe/Berlin Arctic/Longyearbyen\nZ Etc/UTC 0 - UTC\n",
        )
        .unwrap();
        assert_eq!(
            zone_names(dir.path()),
            ["Arctic/Longyearbyen", "Etc/UTC", "Europe/Berlin", "UTC"]
        );
    }
}
