// SPDX-License-Identifier: MIT

//! Screen brightness from `/sys/class/backlight`, set through logind or sysfs.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, sleep_until};
use zbus::Connection;

use crate::bus;
use crate::hub::{Update, Updates};

const SYSFS_ROOT: &str = "/sys/class/backlight";
const POLL: Duration = Duration::from_secs(1);
const REDISCOVER: Duration = Duration::from_secs(10);
const LOGIND: &str = "org.freedesktop.login1";
const SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const SESSION_INTERFACE: &str = "org.freedesktop.login1.Session";

#[derive(Debug)]
pub(crate) enum BacklightCommand {
    Set(f32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Device {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) max: u32,
}

fn read_u32(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Ranks backlight types the way the kernel documents them: firmware, then platform, then raw.
fn type_rank(kind: &str) -> u8 {
    match kind {
        "firmware" => 0,
        "platform" => 1,
        "raw" => 2,
        _ => 3,
    }
}

/// Finds the preferred backlight under `root`.
// sysfs attributes never block, so plain `std::fs` is fine on the runtime thread.
pub(crate) fn discover(root: &Path) -> Option<Device> {
    let mut candidates: Vec<(u8, Device)> = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let max = read_u32(&path.join("max_brightness")).filter(|max| *max > 0)?;
            let kind = fs::read_to_string(path.join("type")).unwrap_or_default();
            let name = entry.file_name().to_string_lossy().into_owned();
            Some((type_rank(kind.trim()), Device { name, path, max }))
        })
        .collect();
    candidates
        .sort_by(|(a_rank, a), (b_rank, b)| a_rank.cmp(b_rank).then_with(|| a.name.cmp(&b.name)));
    candidates.into_iter().next().map(|(_, device)| device)
}

pub(crate) fn read_level(device: &Device) -> Option<f32> {
    let raw = read_u32(&device.path.join("brightness"))?;
    Some((raw as f32 / device.max as f32).clamp(0.0, 1.0))
}

/// Converts `level` to a raw value; it never reaches zero, which turns some panels off entirely.
pub(crate) fn raw_value(level: f32, max: u32) -> u32 {
    let level = if level.is_finite() { level.clamp(0.0, 1.0) } else { 1.0 };
    ((level * max as f32).round() as u32).clamp(1.min(max), max)
}

struct Backlight {
    updates: Updates,
    system: Option<Connection>,
    root: PathBuf,
    device: Option<Device>,
    published: Option<Option<f32>>,
    logind_failed: bool,
    sysfs_failed: bool,
}

impl Backlight {
    fn publish(&mut self, level: Option<f32>) {
        if self.published != Some(level) {
            self.published = Some(level);
            self.updates.send(Update::Brightness(level));
        }
    }

    fn poll(&mut self) {
        if self.device.is_none() {
            self.device = discover(&self.root);
            if let Some(device) = &self.device {
                tracing::info!("Using backlight {}", device.name);
            }
        }
        let level = self.device.as_ref().and_then(read_level);
        if level.is_none() && self.device.take().is_some() {
            tracing::info!("Backlight disappeared");
        }
        self.publish(level);
    }

    async fn set(&mut self, level: f32) {
        let Some(device) = self.device.clone() else {
            tracing::debug!("No backlight; ignoring brightness change");
            return;
        };
        let raw = raw_value(level, device.max);
        if self.set_through_logind(&device, raw).await || self.set_through_sysfs(&device, raw) {
            self.publish(Some(raw as f32 / device.max as f32));
        }
    }

    async fn set_through_logind(&mut self, device: &Device, raw: u32) -> bool {
        let Some(conn) = &self.system else {
            return false;
        };
        let body = ("backlight", device.name.as_str(), raw);
        match bus::call(conn, LOGIND, SESSION_PATH, SESSION_INTERFACE, "SetBrightness", &body).await
        {
            Ok(_) => true,
            Err(err) => {
                if !std::mem::replace(&mut self.logind_failed, true) {
                    tracing::info!("logind can't set the brightness, trying sysfs: {err}");
                }
                false
            }
        }
    }

    fn set_through_sysfs(&mut self, device: &Device, raw: u32) -> bool {
        match fs::write(device.path.join("brightness"), raw.to_string()) {
            Ok(()) => true,
            Err(err) => {
                if !std::mem::replace(&mut self.sysfs_failed, true) {
                    tracing::info!("Can't set the brightness of {}: {err}", device.name);
                }
                false
            }
        }
    }
}

pub(crate) async fn run(
    system: Option<Connection>,
    updates: Updates,
    commands: UnboundedReceiver<BacklightCommand>,
) {
    run_at(PathBuf::from(SYSFS_ROOT), system, updates, commands).await;
}

async fn run_at(
    root: PathBuf,
    system: Option<Connection>,
    updates: Updates,
    mut commands: UnboundedReceiver<BacklightCommand>,
) {
    let mut backlight = Backlight {
        updates,
        system,
        root,
        device: None,
        published: None,
        logind_failed: false,
        sysfs_failed: false,
    };
    loop {
        backlight.poll();
        let next = Instant::now() + if backlight.device.is_some() { POLL } else { REDISCOVER };
        tokio::select! {
            command = commands.recv() => {
                let Some(BacklightCommand::Set(mut level)) = command else { return };
                while let Ok(BacklightCommand::Set(next)) = commands.try_recv() {
                    level = next;
                }
                backlight.set(level).await;
            }
            () = sleep_until(next) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::Update;

    fn add(root: &Path, name: &str, kind: &str, max: &str, brightness: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
        fs::write(dir.join("max_brightness"), format!("{max}\n")).unwrap();
        fs::write(dir.join("brightness"), format!("{brightness}\n")).unwrap();
    }

    #[test]
    fn prefers_firmware_then_platform_then_raw() {
        let root = tempfile::tempdir().unwrap();
        add(root.path(), "intel_backlight", "raw", "96000", "48000");
        add(root.path(), "thinkpad_screen", "platform", "15", "7");
        assert_eq!(discover(root.path()).unwrap().name, "thinkpad_screen");
        add(root.path(), "acpi_video0", "firmware", "100", "50");
        let device = discover(root.path()).unwrap();
        assert_eq!(device.name, "acpi_video0");
        assert_eq!(device.max, 100);
        assert_eq!(read_level(&device), Some(0.5));
    }

    #[test]
    fn skips_broken_devices() {
        let root = tempfile::tempdir().unwrap();
        add(root.path(), "zero", "firmware", "0", "0");
        add(root.path(), "garbage", "firmware", "lots", "1");
        assert_eq!(discover(root.path()), None);
        add(root.path(), "good", "raw", "10", "3");
        assert_eq!(discover(root.path()).unwrap().name, "good");
        assert_eq!(discover(&root.path().join("missing")), None);
    }

    #[test]
    fn raw_values() {
        assert_eq!(raw_value(0.5, 100), 50);
        assert_eq!(raw_value(0.0, 100), 1);
        assert_eq!(raw_value(2.0, 100), 100);
        assert_eq!(raw_value(f32::NAN, 100), 100);
        assert_eq!(raw_value(0.33, 15), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn polls_and_sets_through_sysfs() {
        let root = tempfile::tempdir().unwrap();
        add(root.path(), "panel", "raw", "200", "100");
        let (updates, mut received) = Updates::channel();
        let (commands_tx, commands) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run_at(root.path().to_owned(), None, updates, commands));

        assert!(
            matches!(received.recv().await, Some(Update::Brightness(Some(level))) if level == 0.5)
        );
        commands_tx.send(BacklightCommand::Set(0.1)).unwrap();
        commands_tx.send(BacklightCommand::Set(0.25)).unwrap();
        assert!(
            matches!(received.recv().await, Some(Update::Brightness(Some(level))) if level == 0.25)
        );
        assert_eq!(fs::read_to_string(root.path().join("panel/brightness")).unwrap(), "50");

        fs::remove_dir_all(root.path().join("panel")).unwrap();
        assert!(matches!(received.recv().await, Some(Update::Brightness(None))));
        drop(commands_tx);
        task.await.unwrap();
    }
}
