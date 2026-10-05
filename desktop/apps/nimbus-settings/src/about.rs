// SPDX-License-Identifier: MIT

//! System information for the About page, read from `/etc`, `/proc`, and `/sys`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const UNKNOWN: &str = "Unknown";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemInfo {
    pub os_name: String,
    /// The icon name from `os-release`'s `LOGO`, such as `fedora-logo-icon`.
    pub os_logo: String,
    /// The distribution's brand color from `ANSI_COLOR`, as `#rrggbb`, when it names one.
    pub os_color: Option<String>,
    pub hostname: String,
    pub kernel: String,
    pub cpu: String,
    pub memory: String,
    pub graphics: String,
    pub disk: String,
    pub desktop: String,
    pub windowing: String,
}

/// Parses `os-release` key-value lines, unquoting values.
pub fn parse_os_release(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), unquote(value.trim())))
        .collect()
}

fn unquote(value: &str) -> String {
    let quoted = value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')));
    if !quoted {
        return value.to_string();
    }
    let inner = &value[1..value.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Turns an `ANSI_COLOR` such as `0;38;2;60;110;180` into `#3c6eb4`; only 24-bit colors are understood.
pub fn ansi_truecolor(ansi: &str) -> Option<String> {
    let parts: Vec<&str> = ansi.split(';').collect();
    let at = parts.windows(2).position(|w| w == ["38", "2"])?;
    let channel = |i: usize| parts.get(at + 2 + i)?.parse::<u8>().ok();
    Some(format!("#{:02x}{:02x}{:02x}", channel(0)?, channel(1)?, channel(2)?))
}

/// The CPU model and logical core count, as in `Intel Core i7-1165G7 × 8`.
pub fn parse_cpuinfo(text: &str) -> String {
    let field = |name: &str| {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == name && !value.trim().is_empty()).then(|| value.trim().to_string())
        })
    };
    let model = field("model name")
        .or_else(|| field("Hardware"))
        .or_else(|| field("Model"))
        .or_else(|| field("cpu"));
    let cores = text
        .lines()
        .filter(|l| l.split(':').next().is_some_and(|k| k.trim() == "processor"))
        .count();
    let Some(model) = model else { return UNKNOWN.into() };
    let model = ["(R)", "(r)", "(TM)", "(tm)", "CPU "]
        .iter()
        .fold(model, |m, junk| m.replace(junk, " "))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cores > 1 { format!("{model} × {cores}") } else { model }
}

/// Total memory from `/proc/meminfo`, in GiB.
pub fn parse_meminfo(text: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
        .map_or_else(|| UNKNOWN.into(), |kib| format_binary(kib.saturating_mul(1024)))
}

/// Formats bytes in binary units, as in `15.5 GiB`.
pub fn format_binary(bytes: u64) -> String {
    format_units(bytes, 1024.0, &["B", "KiB", "MiB", "GiB", "TiB", "PiB"])
}

/// Formats bytes in decimal units, as drive makers do, as in `512.1 GB`.
pub fn format_decimal(bytes: u64) -> String {
    format_units(bytes, 1000.0, &["B", "kB", "MB", "GB", "TB", "PB"])
}

fn format_units(bytes: u64, base: f64, units: &[&str]) -> String {
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= base && unit + 1 < units.len() {
        value /= base;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", units[unit]) }
}

fn pci_vendor(id: &str) -> Option<&'static str> {
    Some(match id.trim().to_ascii_lowercase().as_str() {
        "0x8086" => "Intel",
        "0x1002" | "0x1022" => "AMD",
        "0x10de" => "NVIDIA",
        "0x1af4" => "Virtio",
        "0x15ad" => "VMware",
        "0x1234" => "QEMU",
        "0x80ee" => "VirtualBox",
        "0x5143" => "Qualcomm",
        _ => return None,
    })
}

/// Splits `lspci -mm` output into its quoted fields.
pub fn parse_lspci_mm(line: &str) -> Vec<String> {
    line.split('"').skip(1).step_by(2).map(str::to_string).collect()
}

/// The machine-readable source of system information; `/` on a real system.
pub struct Probe {
    pub root: PathBuf,
}

impl Probe {
    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(path.trim_start_matches('/'))).ok()
    }

    /// Collects everything. Blocks on file reads, `statvfs`, and `lspci`.
    pub fn collect(&self) -> SystemInfo {
        let os = self
            .read("etc/os-release")
            .or_else(|| self.read("usr/lib/os-release"))
            .unwrap_or_default();
        let os = parse_os_release(&os);
        let os_name = os
            .get("PRETTY_NAME")
            .or_else(|| os.get("NAME"))
            .cloned()
            .unwrap_or_else(|| "Linux".into());
        let line =
            |path: &str| self.read(path).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        SystemInfo {
            os_name,
            os_logo: os.get("LOGO").cloned().unwrap_or_default(),
            os_color: os.get("ANSI_COLOR").and_then(|c| ansi_truecolor(c)),
            hostname: line("proc/sys/kernel/hostname")
                .or_else(|| line("etc/hostname"))
                .unwrap_or_else(|| UNKNOWN.into()),
            kernel: line("proc/sys/kernel/osrelease")
                .map_or_else(|| UNKNOWN.into(), |k| format!("Linux {k}")),
            cpu: self.read("proc/cpuinfo").map_or_else(|| UNKNOWN.into(), |t| parse_cpuinfo(&t)),
            memory: self.read("proc/meminfo").map_or_else(|| UNKNOWN.into(), |t| parse_meminfo(&t)),
            graphics: self.graphics(),
            disk: self.disk(),
            desktop: format!("Nimbus {}", env!("CARGO_PKG_VERSION")),
            windowing: "Wayland".into(),
        }
    }

    fn disk(&self) -> String {
        match rustix::fs::statvfs(&self.root) {
            Ok(stats) => format_decimal(stats.f_blocks.saturating_mul(stats.f_frsize)),
            Err(error) => {
                tracing::debug!("statvfs failed: {error}");
                UNKNOWN.into()
            }
        }
    }

    fn graphics(&self) -> String {
        let drm = self.root.join("sys/class/drm");
        let Ok(entries) = std::fs::read_dir(&drm) else { return UNKNOWN.into() };
        let mut cards: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_prefix("card"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
            .collect();
        cards.sort();
        let mut names: Vec<String> = Vec::new();
        for card in cards {
            if let Some(name) = self.describe_card(&card.join("device"))
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
        if names.is_empty() { UNKNOWN.into() } else { names.join(" / ") }
    }

    fn describe_card(&self, device: &Path) -> Option<String> {
        let uevent = std::fs::read_to_string(device.join("uevent")).unwrap_or_default();
        let value = |key: &str| {
            uevent.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(str::to_string)
        };
        let driver = value("DRIVER");
        if let Some(slot) = value("PCI_SLOT_NAME")
            && self.root == Path::new("/")
            && let Some(name) = lspci_name(&slot)
        {
            return Some(name);
        }
        let vendor =
            std::fs::read_to_string(device.join("vendor")).ok().and_then(|v| pci_vendor(&v));
        match (vendor, driver) {
            (Some(vendor), Some(driver)) => Some(format!("{vendor} ({driver})")),
            (Some(vendor), None) => Some(format!("{vendor} graphics")),
            (None, Some(driver)) => Some(driver),
            (None, None) => None,
        }
    }
}

/// Finds the icon file for an `os-release` `LOGO` name in the hicolor theme or `/usr/share/pixmaps`.
pub fn find_logo(root: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let hicolor = root.join("usr/share/icons/hicolor");
    let sizes = ["scalable", "512x512", "256x256", "128x128", "96x96", "64x64", "48x48"];
    let dirs = sizes
        .iter()
        .map(|size| hicolor.join(size).join("apps"))
        .chain([root.join("usr/share/pixmaps")]);
    dirs.flat_map(|dir| ["svg", "png"].map(|ext| dir.join(format!("{name}.{ext}"))))
        .find(|path| path.is_file())
}

fn lspci_name(slot: &str) -> Option<String> {
    let output = Command::new("lspci")
        .args(["-mm", "-s", slot])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let fields = parse_lspci_mm(String::from_utf8_lossy(&output.stdout).lines().next()?);
    let vendor =
        fields.get(1)?.replace(" Corporation", "").replace(", Inc.", "").replace(" Inc.", "");
    let vendor =
        vendor.split_whitespace().next().unwrap_or_default().trim_matches(['[', ']']).to_string();
    let device = fields.get(2)?;
    Some(format!("{vendor} {device}").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release() {
        let fields = parse_os_release(
            "# comment\nNAME=Fedora\nPRETTY_NAME=\"Fedora Linux 41 (Workstation \\\"Edition\\\")\"\nLOGO='fedora-logo-icon'\nANSI_COLOR=\"0;38;2;60;110;180\"\nbroken\n",
        );
        assert_eq!(fields["NAME"], "Fedora");
        assert_eq!(fields["PRETTY_NAME"], "Fedora Linux 41 (Workstation \"Edition\")");
        assert_eq!(fields["LOGO"], "fedora-logo-icon");
        assert_eq!(ansi_truecolor(&fields["ANSI_COLOR"]).as_deref(), Some("#3c6eb4"));
        assert_eq!(ansi_truecolor("1;31"), None);
        assert_eq!(ansi_truecolor("38;2;300;0;0"), None);
    }

    #[test]
    fn cpu_and_memory() {
        let cpuinfo = "processor\t: 0\nmodel name\t: Intel(R) Core(TM) i7-1165G7 CPU @ 2.80GHz\n\nprocessor\t: 1\nmodel name\t: Intel(R) Core(TM) i7-1165G7 CPU @ 2.80GHz\n";
        assert_eq!(parse_cpuinfo(cpuinfo), "Intel Core i7-1165G7 @ 2.80GHz × 2");
        assert_eq!(parse_cpuinfo("processor : 0\nHardware : BCM2835\n"), "BCM2835");
        assert_eq!(parse_cpuinfo(""), UNKNOWN);
        assert_eq!(parse_meminfo("MemTotal:       16249284 kB\nMemFree: 1 kB\n"), "15.5 GiB");
        assert_eq!(parse_meminfo("garbage"), UNKNOWN);
        assert_eq!(format_decimal(512_110_190_592), "512.1 GB");
        assert_eq!(format_binary(512), "512 B");
        assert_eq!(format_binary(u64::MAX), "16384.0 PiB");
    }

    #[test]
    fn lspci_fields() {
        let line = r#"00:02.0 "VGA compatible controller" "Intel Corporation" "Alder Lake-P GT2 [Iris Xe Graphics]" -r0c "Lenovo" "Device 3f1a""#;
        let fields = parse_lspci_mm(line);
        assert_eq!(fields[1], "Intel Corporation");
        assert_eq!(fields[2], "Alder Lake-P GT2 [Iris Xe Graphics]");
    }

    #[test]
    fn fixture_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("usr/lib/os-release", "NAME=\"Nimbus OS\"\nLOGO=nimbus-logo\n");
        write("proc/sys/kernel/hostname", "workstation\n");
        write("proc/sys/kernel/osrelease", "6.11.4-arch1-1\n");
        write("proc/meminfo", "MemTotal: 8000000 kB\n");
        write("sys/class/drm/card0/device/vendor", "0x1002\n");
        write("sys/class/drm/card0/device/uevent", "DRIVER=amdgpu\nPCI_SLOT_NAME=0000:03:00.0\n");
        write("sys/class/drm/card0-eDP-1/status", "connected\n");
        write("sys/class/drm/card1/device/uevent", "DRIVER=simpledrm\n");
        let info = Probe { root: root.to_path_buf() }.collect();
        assert_eq!(info.os_name, "Nimbus OS");
        assert_eq!(info.os_logo, "nimbus-logo");
        assert_eq!(info.hostname, "workstation");
        assert_eq!(info.kernel, "Linux 6.11.4-arch1-1");
        assert_eq!(info.cpu, UNKNOWN);
        assert_eq!(info.memory, "7.6 GiB");
        assert_eq!(info.graphics, "AMD (amdgpu) / simpledrm");
        assert_ne!(info.disk, UNKNOWN);
        assert_eq!(info.windowing, "Wayland");
        assert!(info.desktop.starts_with("Nimbus "));

        assert_eq!(find_logo(root, "nimbus-logo"), None);
        write("usr/share/pixmaps/nimbus-logo.png", "");
        write("usr/share/icons/hicolor/128x128/apps/nimbus-logo.png", "");
        assert_eq!(
            find_logo(root, "nimbus-logo"),
            Some(root.join("usr/share/icons/hicolor/128x128/apps/nimbus-logo.png"))
        );
        assert_eq!(find_logo(root, "../etc"), None);

        let empty = tempfile::tempdir().unwrap();
        let info = Probe { root: empty.path().to_path_buf() }.collect();
        assert_eq!(info.os_name, "Linux");
        assert_eq!(info.graphics, UNKNOWN);
    }
}
