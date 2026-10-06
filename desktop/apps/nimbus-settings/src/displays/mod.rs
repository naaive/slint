// SPDX-License-Identifier: MIT

//! Displays: the compositor's heads, editing their configuration, and arranging them.
//!
//! The app configures displays through wlr-output-management, in [`wlr`].
//! Without it, it shows the outputs that the Nimbus control socket reports, read-only.

pub mod wlr;

use std::path::Path;

use nimbus_config::Transform;
use nimbus_config::geometry::{Rect, attach, bounds, follow_resize};
use nimbus_ipc::{Client, OutputInfo, Request, Response};

#[derive(Debug, thiserror::Error)]
pub enum DisplayError {
    #[error("Nimbus isn't running, so displays can't be listed")]
    NotRunning,
    #[error("the compositor didn't answer: {0}")]
    Ipc(#[from] nimbus_ipc::Error),
    #[error("the compositor sent an unexpected reply")]
    Unexpected,
}

fn query(mut client: Client) -> Result<Vec<OutputInfo>, DisplayError> {
    match client.request(&Request::GetState)? {
        Response::State(state) => Ok(state.outputs),
        _ => Err(DisplayError::Unexpected),
    }
}

/// Asks the running compositor for its outputs. Blocks on the socket.
pub fn fetch() -> Result<Vec<OutputInfo>, DisplayError> {
    let path = nimbus_ipc::socket_path().ok_or(DisplayError::NotRunning)?;
    fetch_from(&path)
}

pub fn fetch_from(path: &Path) -> Result<Vec<OutputInfo>, DisplayError> {
    match Client::connect_to(path) {
        Ok(client) => query(client),
        Err(nimbus_ipc::Error::Io(_)) => Err(DisplayError::NotRunning),
        Err(other) => Err(other.into()),
    }
}

/// A refresh rate in millihertz as `60 Hz` or `59.95 Hz`; empty when unknown.
pub fn format_refresh(mhz: i32) -> String {
    match u32::try_from(mhz) {
        Ok(mhz) if mhz > 0 => format!("{} Hz", nimbus_config::format_hz(mhz, 2)),
        _ => String::new(),
    }
}

pub fn format_scale(scale: f64) -> String {
    if !scale.is_finite() || scale <= 0.0 {
        return "100%".into();
    }
    format!("{}%", (scale * 100.0).round())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeadMode {
    pub width: i32,
    pub height: i32,
    /// In millihertz; 0 when unknown.
    pub refresh_mhz: i32,
    pub preferred: bool,
}

impl HeadMode {
    fn same(&self, other: &HeadMode) -> bool {
        (self.width, self.height, self.refresh_mhz)
            == (other.width, other.height, other.refresh_mhz)
    }
}

/// A display as the compositor reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Head {
    /// The connector, such as `DP-1`, which identifies the head.
    pub name: String,
    pub make: String,
    pub model: String,
    pub modes: Vec<HeadMode>,
    pub enabled: bool,
    pub current_mode: Option<HeadMode>,
    pub position: (i32, i32),
    pub transform: Transform,
    pub scale: f64,
}

impl Head {
    /// A read-only head for an output that the control socket reports.
    pub fn from_output(output: &OutputInfo, position: (i32, i32)) -> Self {
        let mode = HeadMode {
            width: output.width,
            height: output.height,
            refresh_mhz: i32::try_from(output.refresh_mhz).unwrap_or(0),
            preferred: true,
        };
        Self {
            name: output.name.clone(),
            make: String::new(),
            model: String::new(),
            modes: vec![mode],
            enabled: true,
            current_mode: Some(mode),
            position,
            transform: Transform::Normal,
            scale: if output.scale.is_finite() && output.scale > 0.0 { output.scale } else { 1.0 },
        }
    }

    /// What the page calls the display: a laptop's panel is built in,
    /// and any other display goes by its model, or its connector without one.
    pub fn title(&self) -> &str {
        const BUILT_IN: [&str; 3] = ["eDP-", "LVDS-", "DSI-"];
        if BUILT_IN.iter().any(|prefix| self.name.starts_with(prefix)) {
            "Built-in Display"
        } else if self.model.is_empty() || self.model == self.name {
            &self.name
        } else {
            &self.model
        }
    }

    pub fn config(&self) -> HeadConfig {
        HeadConfig {
            name: self.name.clone(),
            enabled: self.enabled,
            mode: self.current_mode.or_else(|| self.modes.iter().copied().find(|m| m.preferred)),
            position: self.position,
            transform: self.transform,
            scale: self.scale,
        }
    }

    /// The distinct mode sizes, largest first.
    pub fn resolutions(&self) -> Vec<(i32, i32)> {
        let mut sizes: Vec<(i32, i32)> = self.modes.iter().map(|m| (m.width, m.height)).collect();
        sizes.sort_by_key(|&(w, h)| std::cmp::Reverse((i64::from(w) * i64::from(h), w)));
        sizes.dedup();
        sizes
    }

    /// The modes of one size, fastest first.
    pub fn refresh_rates(&self, (width, height): (i32, i32)) -> Vec<HeadMode> {
        let mut modes: Vec<HeadMode> =
            self.modes.iter().copied().filter(|m| (m.width, m.height) == (width, height)).collect();
        modes.sort_by_key(|m| std::cmp::Reverse(m.refresh_mhz));
        modes.dedup_by(|a, b| a.same(b));
        modes
    }
}

/// Lines up outputs from the control socket from left to right, as the compositor does without a stored layout.
pub fn heads_from_outputs(outputs: &[OutputInfo]) -> Vec<Head> {
    let mut x = 0;
    outputs
        .iter()
        .map(|output| {
            let head = Head::from_output(output, (x, 0));
            x += logical_size(&head.config()).0;
            head
        })
        .collect()
}

/// The configuration of one head, as the page edits it and applies it.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadConfig {
    pub name: String,
    pub enabled: bool,
    /// `None` for a head without modes.
    pub mode: Option<HeadMode>,
    pub position: (i32, i32),
    pub transform: Transform,
    pub scale: f64,
}

/// What the display backend reports.
#[derive(Clone, Debug, PartialEq)]
pub enum DisplayEvent {
    /// Every head, after each change.
    Heads(Vec<Head>),
    /// The outcome of the last [`DisplayControl::apply`].
    Applied(Result<(), ApplyError>),
    /// Displays can't be configured, for the reason given.
    Unavailable(String),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("Nimbus couldn't apply these display settings")]
    Failed,
    #[error("The displays changed in the meantime. Try again.")]
    Outdated,
}

/// Where [`DisplayEvent`]s go; it's called on the backend's thread.
pub type DisplayEvents = Box<dyn Fn(DisplayEvent) + Send + Sync>;

/// A connection that configures displays.
pub trait DisplayControl {
    /// Applies a configuration of every head; the outcome arrives as [`DisplayEvent::Applied`].
    fn apply(&self, configuration: Vec<HeadConfig>);
}

/// The scales the page offers, besides a display's current one.
pub const SCALES: [f64; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

/// [`SCALES`] with `current` added in order when it's not among them.
pub fn scale_choices(current: f64) -> Vec<f64> {
    let mut scales = SCALES.to_vec();
    if !scales.iter().any(|s| (s - current).abs() < 1e-3) {
        scales.push(current);
        scales.sort_by(f64::total_cmp);
    }
    scales
}

/// The size of a head in the global layout, as the compositor computes it.
pub fn logical_size(config: &HeadConfig) -> (i32, i32) {
    let Some(mode) = config.mode else {
        return (0, 0);
    };
    let (w, h) = if config.transform.rotation() % 2 == 1 {
        (mode.height, mode.width)
    } else {
        (mode.width, mode.height)
    };
    let scale = if config.scale > 0.0 { config.scale } else { 1.0 };
    let logical = |n: i32| (f64::from(n) / scale).ceil() as i32;
    (logical(w), logical(h))
}

fn rect(config: &HeadConfig) -> Rect {
    let (w, h) = logical_size(config);
    Rect { x: config.position.0, y: config.position.1, w, h }
}

/// Each head's rectangle, or `None` for a head that's disabled or has no size.
fn enabled_rects(configs: &[HeadConfig]) -> Vec<Option<Rect>> {
    configs.iter().map(|c| c.enabled.then(|| rect(c)).filter(|r| r.w > 0 && r.h > 0)).collect()
}

/// The arrangement preview: each enabled head's rectangle as fractions of the arrangement's bounds,
/// and the bounds' width divided by their height.
pub fn arrangement(configs: &[HeadConfig]) -> (Vec<Option<[f32; 4]>>, f32) {
    let rects = enabled_rects(configs);
    let Some(b) = bounds(rects.iter().flatten()) else {
        return (vec![None; configs.len()], 1.0);
    };
    let (width, height) = (b.w as f32, b.h as f32);
    let fractions = rects
        .iter()
        .map(|r| {
            r.map(|r| {
                [
                    (r.x - b.x) as f32 / width,
                    (r.y - b.y) as f32 / height,
                    r.w as f32 / width,
                    r.h as f32 / height,
                ]
            })
        })
        .collect();
    (fractions, width / height)
}

/// Moves head `index` to `proposed`, a top-left corner as fractions of the arrangement's bounds,
/// attached to the nearest edge of another enabled head, and then moves the arrangement to the origin.
pub fn move_head(configs: &mut [HeadConfig], index: usize, proposed: (f32, f32)) {
    let rects = enabled_rects(configs);
    let Some(Some(moving)) = rects.get(index).copied() else {
        return;
    };
    let b = bounds(rects.iter().flatten()).unwrap_or(moving);
    let target = (
        b.x + (proposed.0 * b.w as f32).round() as i32,
        b.y + (proposed.1 * b.h as f32).round() as i32,
    );
    let others: Vec<Rect> =
        rects.iter().enumerate().filter(|&(i, _)| i != index).filter_map(|(_, r)| *r).collect();
    if let Some(position) = attach(moving, target, &others) {
        configs[index].position = position;
    }
    normalize(configs);
}

/// Keeps the arrangement together after head `index` changed from `before`.
///
/// Heads past its old right or bottom edge move by the change of its size,
/// and a head that was turned on goes to the right of the others.
pub fn make_room(configs: &mut [HeadConfig], index: usize, before: &HeadConfig) {
    let Some(after) = configs.get(index).cloned() else {
        return;
    };
    if after.enabled && !before.enabled {
        let others = configs.iter().enumerate().filter(|&(i, c)| i != index && c.enabled);
        let others: Vec<Rect> = others.map(|(_, c)| rect(c)).collect();
        configs[index].position = bounds(&others).map_or((0, 0), |b| (b.x + b.w, b.y));
    } else {
        let size = |c: &HeadConfig| if c.enabled { logical_size(c) } else { (0, 0) };
        let (w, h) = size(before);
        let before = Rect { x: before.position.0, y: before.position.1, w, h };
        let mut others: Vec<&mut HeadConfig> = configs
            .iter_mut()
            .enumerate()
            .filter(|(i, c)| *i != index && c.enabled)
            .map(|(_, c)| c)
            .collect();
        let mut rects: Vec<Rect> = others.iter().map(|c| rect(c)).collect();
        follow_resize(&mut rects, before, size(&after));
        for (config, rect) in others.iter_mut().zip(rects) {
            config.position = (rect.x, rect.y);
        }
    }
    normalize(configs);
}

/// Moves the enabled heads so that their bounds start at the origin.
pub fn normalize(configs: &mut [HeadConfig]) {
    let enabled = || configs.iter().filter(|c| c.enabled);
    let (Some(left), Some(top)) =
        (enabled().map(|c| c.position.0).min(), enabled().map(|c| c.position.1).min())
    else {
        return;
    };
    for config in configs.iter_mut().filter(|c| c.enabled) {
        config.position = (config.position.0 - left, config.position.1 - top);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_ipc::{CompositorState, read_message, write_message};

    fn mode(width: i32, height: i32, refresh_mhz: i32) -> HeadMode {
        HeadMode { width, height, refresh_mhz, preferred: false }
    }

    fn config(name: &str, size: (i32, i32), position: (i32, i32)) -> HeadConfig {
        HeadConfig {
            name: name.into(),
            enabled: true,
            mode: Some(mode(size.0, size.1, 60_000)),
            position,
            transform: Transform::Normal,
            scale: 1.0,
        }
    }

    #[test]
    fn formatting() {
        assert_eq!(format_refresh(60000), "60 Hz");
        assert_eq!(format_refresh(59951), "59.95 Hz");
        assert_eq!(format_refresh(143_900), "143.9 Hz");
        assert_eq!(format_refresh(0), "");
        assert_eq!(format_scale(1.25), "125%");
        assert_eq!(format_scale(f64::NAN), "100%");
    }

    #[test]
    fn heads_list_their_modes() {
        let head = Head {
            name: "DP-1".into(),
            make: "DEL".into(),
            model: "DELL U2720Q".into(),
            modes: vec![
                mode(1920, 1080, 60_000),
                mode(3840, 2160, 30_000),
                mode(3840, 2160, 60_000),
                mode(1920, 1080, 144_000),
            ],
            enabled: true,
            current_mode: None,
            position: (0, 0),
            transform: Transform::Normal,
            scale: 1.0,
        };
        assert_eq!(head.title(), "DELL U2720Q");
        assert_eq!(head.resolutions(), [(3840, 2160), (1920, 1080)]);
        let rates: Vec<i32> =
            head.refresh_rates((1920, 1080)).iter().map(|m| m.refresh_mhz).collect();
        assert_eq!(rates, [144_000, 60_000]);
        assert_eq!(scale_choices(1.5), SCALES);
        assert_eq!(scale_choices(1.1), [1.0, 1.1, 1.25, 1.5, 1.75, 2.0]);
    }

    #[test]
    fn rotation_swaps_the_logical_size() {
        let rotated = HeadConfig {
            transform: Transform::Rotate90,
            scale: 2.0,
            ..config("A", (1920, 1080), (0, 0))
        };
        assert_eq!(logical_size(&rotated), (540, 960));
    }

    #[test]
    fn outputs_line_up() {
        let outputs = [
            OutputInfo {
                name: "eDP-1".into(),
                width: 2880,
                height: 1800,
                scale: 2.0,
                refresh_mhz: 90_000,
            },
            OutputInfo {
                name: "DP-1".into(),
                width: 1920,
                height: 1080,
                scale: f64::NAN,
                refresh_mhz: 0,
            },
        ];
        let heads = heads_from_outputs(&outputs);
        assert_eq!((heads[1].position, heads[1].scale), ((1440, 0), 1.0));
        let (rects, aspect) = arrangement(&heads.iter().map(Head::config).collect::<Vec<_>>());
        assert!((aspect - 3360.0 / 1080.0).abs() < 1e-4);
        assert_eq!(rects[0], Some([0.0, 0.0, 1440.0 / 3360.0, 900.0 / 1080.0]));
        assert_eq!(arrangement(&[]).1, 1.0);
    }

    #[test]
    fn moved_heads_attach_to_an_edge() {
        let mut configs =
            vec![config("A", (1920, 1080), (0, 0)), config("B", (1280, 1024), (1920, 0))];
        // Dropped near the bottom-left of A: B goes below A, lined up with its left edge.
        move_head(&mut configs, 1, (0.01, 0.95));
        assert_eq!(configs[1].position, (0, 1080));
        // Dropped far to the left: B goes left of A, which then starts at the origin.
        move_head(&mut configs, 1, (-0.9, 0.0));
        assert_eq!(configs[1].position, (0, 0));
        assert_eq!(configs[0].position, (1280, 0));
        // A lone head stays where it is.
        let mut lone = vec![config("A", (100, 100), (0, 0))];
        move_head(&mut lone, 0, (0.5, 0.5));
        assert_eq!(lone[0].position, (0, 0));
    }

    #[test]
    fn changed_heads_make_room() {
        let mut configs = vec![
            config("A", (1920, 1080), (0, 0)),
            config("B", (1920, 1080), (1920, 0)),
            config("C", (1920, 1080), (0, 1080)),
        ];
        let before = configs[0].clone();
        configs[0].scale = 2.0;
        make_room(&mut configs, 0, &before);
        assert_eq!(configs[1].position, (960, 0), "B follows A's right edge");
        assert_eq!(configs[2].position, (0, 540), "C follows A's bottom edge");

        let before = configs[1].clone();
        configs[1].enabled = false;
        make_room(&mut configs, 1, &before);
        let before = configs[1].clone();
        configs[1].enabled = true;
        make_room(&mut configs, 1, &before);
        assert_eq!(configs[1].position, (1920, 0), "a head turned on goes to the right");
    }

    #[test]
    fn missing_socket_means_not_running() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(fetch_from(&dir.path().join("none.sock")), Err(DisplayError::NotRunning)));
    }

    #[test]
    fn queries_the_compositor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nimbus.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let request: Request = read_message(&mut reader).unwrap();
            assert_eq!(request, Request::GetState);
            let outputs = vec![OutputInfo {
                name: "HDMI-A-1".into(),
                width: 1920,
                height: 1080,
                scale: 1.0,
                refresh_mhz: 60000,
            }];
            write_message(
                &mut writer,
                &Response::State(CompositorState { outputs, ..Default::default() }),
            )
            .unwrap();
        });
        let outputs = fetch_from(&path).unwrap();
        assert_eq!(outputs[0].name, "HDMI-A-1");
        server.join().unwrap();
    }
}
