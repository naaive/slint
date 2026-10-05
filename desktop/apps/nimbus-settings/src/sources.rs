// SPDX-License-Identifier: MIT

//! Where the app's data comes from: the real system, or fixed sample data for screenshots and tests.
//! Every method may block, so callers run them through [`crate::dispatch::Dispatch`].

use std::path::{Path, PathBuf};

use chrono::{NaiveDate, NaiveDateTime};
use image::RgbaImage;
use nimbus_ipc::OutputInfo;

use crate::about::{Probe, SystemInfo};
use crate::wallpapers::{self, Wallpaper};
use crate::xkb;

pub trait Sources: Send + Sync + 'static {
    fn wallpapers(&self) -> Vec<Wallpaper>;
    fn thumbnail(&self, path: &Path) -> Option<RgbaImage>;
    fn font_families(&self) -> Vec<String>;
    fn xkb_rules(&self) -> xkb::Rules;
    fn system_info(&self) -> SystemInfo;
    /// The distribution logo named by `info`, about 128 pixels tall.
    fn logo(&self, info: &SystemInfo) -> Option<RgbaImage>;
    fn outputs(&self) -> Result<Vec<OutputInfo>, String>;
    /// The time used for clock previews.
    fn now(&self) -> NaiveDateTime;
}

/// The most wallpapers listed, to bound scanning time and memory.
pub const WALLPAPER_LIMIT: usize = 300;
const LOGO_SIZE: u32 = 256;

pub struct SystemSources {
    pub wallpaper_dirs: Vec<PathBuf>,
    pub cache_dir: Option<PathBuf>,
}

impl Default for SystemSources {
    fn default() -> Self {
        Self { wallpaper_dirs: wallpapers::default_dirs(), cache_dir: dirs::cache_dir() }
    }
}

impl Sources for SystemSources {
    fn wallpapers(&self) -> Vec<Wallpaper> {
        wallpapers::scan(&self.wallpaper_dirs, WALLPAPER_LIMIT)
    }

    fn thumbnail(&self, path: &Path) -> Option<RgbaImage> {
        wallpapers::thumbnail(path, self.cache_dir.as_deref())
    }

    fn font_families(&self) -> Vec<String> {
        crate::fonts::installed_families()
    }

    fn xkb_rules(&self) -> xkb::Rules {
        xkb::Rules::load(Path::new(xkb::EVDEV_LST))
    }

    fn system_info(&self) -> SystemInfo {
        Probe { root: "/".into() }.collect()
    }

    fn logo(&self, info: &SystemInfo) -> Option<RgbaImage> {
        let path = crate::about::find_logo(Path::new("/"), &info.os_logo)?;
        crate::imaging::load_fitting(&path, LOGO_SIZE, LOGO_SIZE)
    }

    fn outputs(&self) -> Result<Vec<OutputInfo>, String> {
        crate::displays::fetch().map_err(|e| e.to_string())
    }

    fn now(&self) -> NaiveDateTime {
        chrono::Local::now().naive_local()
    }
}

/// Deterministic data that doesn't depend on the machine.
#[derive(Default)]
pub struct SampleSources;

const SAMPLE_WALLPAPERS: [(&str, [u8; 3], [u8; 3]); 8] = [
    ("Aurora", [38, 52, 110], [180, 70, 150]),
    ("Dunes", [214, 150, 90], [120, 60, 50]),
    ("Fjord", [30, 90, 120], [170, 210, 220]),
    ("Meadow", [70, 140, 70], [210, 220, 120]),
    ("Nebula", [20, 18, 40], [120, 60, 180]),
    ("Peaks", [60, 70, 90], [230, 230, 240]),
    ("Sunset", [250, 120, 60], [90, 40, 110]),
    ("Tide", [20, 60, 90], [60, 180, 170]),
];

impl Sources for SampleSources {
    fn wallpapers(&self) -> Vec<Wallpaper> {
        SAMPLE_WALLPAPERS
            .iter()
            .map(|(name, _, _)| Wallpaper {
                path: PathBuf::from(format!(
                    "/usr/share/backgrounds/nimbus/{}.jpg",
                    name.to_lowercase()
                )),
                name: (*name).into(),
            })
            .collect()
    }

    fn thumbnail(&self, path: &Path) -> Option<RgbaImage> {
        let stem = path.file_stem()?.to_str()?;
        let (_, from, to) =
            SAMPLE_WALLPAPERS.iter().find(|(name, _, _)| name.eq_ignore_ascii_case(stem))?;
        let (width, height) = (320u32, 200u32);
        Some(RgbaImage::from_fn(width, height, |x, y| {
            let t = (x as f32 / width as f32) * 0.4 + (y as f32 / height as f32) * 0.6;
            let hill = ((x as f32 / width as f32 * 6.0).sin() * 0.08 + 0.7) * height as f32;
            let shade = if (y as f32) > hill { 0.75 } else { 1.0 };
            let channel = |i: usize| {
                ((f32::from(from[i]) + (f32::from(to[i]) - f32::from(from[i])) * t) * shade) as u8
            };
            image::Rgba([channel(0), channel(1), channel(2), 255])
        }))
    }

    fn font_families(&self) -> Vec<String> {
        ["Cantarell", "DejaVu Sans", "Fira Sans", "Inter", "Noto Sans", "Source Sans 3", "Ubuntu"]
            .map(String::from)
            .to_vec()
    }

    fn xkb_rules(&self) -> xkb::Rules {
        xkb::Rules::builtin()
    }

    fn system_info(&self) -> SystemInfo {
        SystemInfo {
            os_name: "Nimbus OS 1.0".into(),
            os_logo: String::new(),
            os_color: Some("#3584e4".into()),
            hostname: "workstation".into(),
            kernel: "Linux 6.11.4".into(),
            cpu: "AMD Ryzen 7 7840U w/ Radeon 780M Graphics × 16".into(),
            memory: "30.6 GiB".into(),
            graphics: "AMD Radeon 780M".into(),
            disk: "1.0 TB".into(),
            desktop: format!("Nimbus {}", env!("CARGO_PKG_VERSION")),
            windowing: "Wayland".into(),
        }
    }

    fn logo(&self, _info: &SystemInfo) -> Option<RgbaImage> {
        None
    }

    fn outputs(&self) -> Result<Vec<OutputInfo>, String> {
        Ok(vec![
            OutputInfo {
                name: "eDP-1".into(),
                width: 2880,
                height: 1800,
                scale: 2.0,
                refresh_mhz: 120_000,
            },
            OutputInfo {
                name: "DP-2".into(),
                width: 3840,
                height: 2160,
                scale: 1.5,
                refresh_mhz: 59_997,
            },
        ])
    }

    fn now(&self) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 5)
            .and_then(|d| d.and_hms_opt(9, 41, 0))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_data_is_complete() {
        let sample = SampleSources;
        let wallpapers = sample.wallpapers();
        assert_eq!(wallpapers.len(), SAMPLE_WALLPAPERS.len());
        for wallpaper in &wallpapers {
            assert_eq!(sample.thumbnail(&wallpaper.path).map(|t| t.dimensions()), Some((320, 200)));
        }
        assert!(sample.thumbnail(Path::new("/elsewhere/x.jpg")).is_none());
        assert_eq!(sample.outputs().map(|o| o.len()), Ok(2));
        assert_eq!(sample.now().to_string(), "2026-10-05 09:41:00");
    }

    #[test]
    fn system_sources_degrade() {
        let dir = tempfile::tempdir().unwrap();
        let system =
            SystemSources { wallpaper_dirs: vec![dir.path().join("none")], cache_dir: None };
        assert!(system.wallpapers().is_empty());
        assert!(!system.font_families().is_empty());
        assert!(!system.xkb_rules().layouts.is_empty());
        assert!(!system.system_info().os_name.is_empty());
    }
}
