// SPDX-License-Identifier: MIT

//! Where the app's data comes from: the real system, or fixed sample data for screenshots and tests.
//! Every method may block, so callers run them through [`crate::dispatch::Dispatch`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime};
use image::RgbaImage;
use nimbus_ipc::OutputInfo;
use nimbus_services::{BusAddress, bluez, nm, sound, timedate};

use crate::about::{Probe, SystemInfo};
use crate::default_apps::{AppDefaults, SampleDefaults, XdgDefaults};
use crate::displays::{DisplayControl, DisplayEvent, DisplayEvents, Head, HeadConfig, HeadMode};
use crate::wallpapers::{self, Wallpaper};
use crate::xkb;

/// Where a client of a system service, such as NetworkManager, reports its events.
pub type Events<E> = Box<dyn Fn(E) + Send + Sync>;

/// Sends commands to a client of a system service; the outcomes arrive as its events.
pub trait Control<C>: 'static {
    fn send(&self, command: C);
}

impl Control<nm::Command> for nm::Client {
    fn send(&self, command: nm::Command) {
        nm::Client::send(self, command);
    }
}

impl Control<bluez::Command> for bluez::Client {
    fn send(&self, command: bluez::Command) {
        bluez::Client::send(self, command);
    }
}

impl Control<sound::Command> for sound::Client {
    fn send(&self, command: sound::Command) {
        sound::Client::send(self, command);
    }
}

impl Control<timedate::Command> for timedate::Client {
    fn send(&self, command: timedate::Command) {
        timedate::Client::send(self, command);
    }
}

pub trait Sources: Send + Sync + 'static {
    fn wallpapers(&self) -> Vec<Wallpaper>;
    fn thumbnail(&self, path: &Path) -> Option<RgbaImage>;
    fn font_families(&self) -> Vec<String>;
    fn xkb_rules(&self) -> xkb::Rules;
    fn system_info(&self) -> SystemInfo;
    /// The distribution logo named by `info`, about 128 pixels tall.
    fn logo(&self, info: &SystemInfo) -> Option<RgbaImage>;
    /// Starts reporting displays to `events`, with a way to configure them;
    /// `None` when displays can only be listed with [`Sources::outputs`].
    fn display_control(&self, events: DisplayEvents) -> Option<Box<dyn DisplayControl>>;
    /// The outputs, read-only.
    fn outputs(&self) -> Result<Vec<OutputInfo>, String>;
    /// Starts reporting networks to `events`, with a way to manage them.
    fn network(&self, events: Events<nm::Event>) -> Box<dyn Control<nm::Command>>;
    /// Starts reporting Bluetooth devices and pairing requests to `events`, with a way to manage them.
    fn bluetooth(&self, events: Events<bluez::Event>) -> Box<dyn Control<bluez::Command>>;
    /// Starts reporting sound devices to `events`, with a way to change them.
    fn sound(&self, events: Events<sound::Event>) -> Box<dyn Control<sound::Command>>;
    /// Starts reporting the time zone and synchronization to `events`, with a way to change them.
    fn time(&self, events: Events<timedate::Event>) -> Box<dyn Control<timedate::Command>>;
    /// Where default applications are read and chosen.
    fn default_apps(&self) -> Arc<dyn AppDefaults>;
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

    fn display_control(&self, events: DisplayEvents) -> Option<Box<dyn DisplayControl>> {
        match crate::displays::wlr::WlrControl::spawn(events) {
            Ok(control) => Some(Box::new(control)),
            Err(error) => {
                tracing::error!("cannot start the display thread: {error}");
                None
            }
        }
    }

    fn outputs(&self) -> Result<Vec<OutputInfo>, String> {
        crate::displays::fetch().map_err(|e| e.to_string())
    }

    fn network(&self, events: Events<nm::Event>) -> Box<dyn Control<nm::Command>> {
        Box::new(nm::Client::spawn(BusAddress::Default, events))
    }

    fn bluetooth(&self, events: Events<bluez::Event>) -> Box<dyn Control<bluez::Command>> {
        Box::new(bluez::Client::spawn(BusAddress::Default, events))
    }

    fn sound(&self, events: Events<sound::Event>) -> Box<dyn Control<sound::Command>> {
        Box::new(sound::Client::spawn(events))
    }

    fn time(&self, events: Events<timedate::Event>) -> Box<dyn Control<timedate::Command>> {
        Box::new(timedate::Client::spawn(BusAddress::Default, events))
    }

    fn default_apps(&self) -> Arc<dyn AppDefaults> {
        Arc::new(XdgDefaults { lookup: nimbus_xdg::MimeLookup::from_env() })
    }

    fn now(&self) -> NaiveDateTime {
        chrono::Local::now().naive_local()
    }
}

/// Deterministic data that doesn't depend on the machine.
#[derive(Default)]
pub struct SampleSources {
    default_apps: Arc<SampleDefaults>,
}

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

    fn display_control(&self, events: DisplayEvents) -> Option<Box<dyn DisplayControl>> {
        let control = SampleDisplays { heads: std::sync::Mutex::new(sample_heads()), events };
        control.report();
        Some(Box::new(control))
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

    fn network(&self, events: Events<nm::Event>) -> Box<dyn Control<nm::Command>> {
        Box::new(crate::network::SampleNetwork::new(events))
    }

    fn bluetooth(&self, events: Events<bluez::Event>) -> Box<dyn Control<bluez::Command>> {
        Box::new(crate::bluetooth::SampleBluetooth::new(events))
    }

    fn sound(&self, events: Events<sound::Event>) -> Box<dyn Control<sound::Command>> {
        Box::new(crate::sound::SampleSound::new(events))
    }

    fn time(&self, events: Events<timedate::Event>) -> Box<dyn Control<timedate::Command>> {
        Box::new(crate::date_time::SampleTime::new(events))
    }

    fn default_apps(&self) -> Arc<dyn AppDefaults> {
        self.default_apps.clone()
    }

    fn now(&self) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 5)
            .and_then(|d| d.and_hms_opt(9, 41, 0))
            .unwrap_or_default()
    }
}

fn sample_heads() -> Vec<Head> {
    let mode =
        |width, height, refresh_mhz, preferred| HeadMode { width, height, refresh_mhz, preferred };
    let laptop = vec![mode(2880, 1800, 120_000, true), mode(2880, 1800, 60_000, false)];
    let monitor = vec![
        mode(3840, 2160, 59_997, true),
        mode(3840, 2160, 30_000, false),
        mode(2560, 1440, 59_951, false),
        mode(1920, 1080, 60_000, false),
    ];
    vec![
        Head {
            name: "eDP-1".into(),
            make: "BOE".into(),
            model: "0x0BCA".into(),
            current_mode: Some(laptop[0]),
            modes: laptop,
            enabled: true,
            position: (0, 360),
            transform: nimbus_config::Transform::Normal,
            scale: 2.0,
        },
        Head {
            name: "DP-2".into(),
            make: "DEL".into(),
            model: "DELL U2720Q".into(),
            current_mode: Some(monitor[0]),
            modes: monitor,
            enabled: true,
            position: (1440, 0),
            transform: nimbus_config::Transform::Normal,
            scale: 1.5,
        },
    ]
}

/// Displays that take any configuration, reporting synchronously like a compositor would.
struct SampleDisplays {
    heads: std::sync::Mutex<Vec<Head>>,
    events: DisplayEvents,
}

impl SampleDisplays {
    fn report(&self) {
        let heads = self.heads.lock().map(|heads| heads.clone()).unwrap_or_default();
        (self.events)(DisplayEvent::Heads(heads));
    }
}

impl DisplayControl for SampleDisplays {
    fn apply(&self, configuration: Vec<HeadConfig>) {
        if let Ok(mut heads) = self.heads.lock() {
            for head in heads.iter_mut() {
                if let Some(config) = configuration.iter().find(|c| c.name == head.name) {
                    head.enabled = config.enabled;
                    head.current_mode = config.mode;
                    head.position = config.position;
                    head.transform = config.transform;
                    head.scale = config.scale;
                }
            }
        }
        self.report();
        (self.events)(DisplayEvent::Applied(Ok(())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_data_is_complete() {
        let sample = SampleSources::default();
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
