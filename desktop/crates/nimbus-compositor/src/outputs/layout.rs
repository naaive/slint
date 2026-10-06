// SPDX-License-Identifier: MIT

//! The layout the configuration asks for, validation, and the configuration entries that record a layout.

use super::{Layout, OutputError, OutputState, head_info};
use nimbus_config::geometry::{self, Rect};
use nimbus_config::{Config, OutputConfig, OutputId, OutputMode};
use smithay::output::{Mode, Output};
use smithay::reexports::wayland_server::protocol::wl_output;
use smithay::utils::{Logical, Point, Size, Transform};
use std::ops::RangeInclusive;

pub const SCALE_RANGE: RangeInclusive<f64> = 0.25..=8.0;
/// How far from the origin a display may sit, which keeps geometry far from integer overflow.
const MAX_COORDINATE: i32 = 1 << 20;
/// How far a requested refresh rate may be from a supported one, in millihertz.
const REFRESH_TOLERANCE: i32 = 1000;

/// How a display identifies itself, owned.
pub struct Identity {
    connector: String,
    make: String,
    model: String,
    serial: String,
}

impl Identity {
    pub fn of(output: &Output) -> Self {
        let physical = output.physical_properties();
        Self {
            connector: output.name(),
            make: physical.make,
            model: physical.model,
            serial: head_info(output).serial,
        }
    }

    pub fn id(&self) -> OutputId<'_> {
        OutputId {
            connector: &self.connector,
            make: &self.make,
            model: &self.model,
            serial: &self.serial,
        }
    }
}

/// `scale` rounded to the 1/120 steps that wp-fractional-scale can express.
pub fn round_scale(scale: f64) -> f64 {
    (scale * 120.0).round() / 120.0
}

fn valid_scale(scale: f64) -> Option<f64> {
    SCALE_RANGE.contains(&scale).then(|| round_scale(scale))
}

/// The size of an output in the global layout.
pub fn logical_size(state: &OutputState) -> Size<i32, Logical> {
    state.transform.transform_size(state.mode.size).to_f64().to_logical(state.scale).to_i32_ceil()
}

fn rect(state: &OutputState) -> Rect {
    let size = logical_size(state);
    Rect { x: state.position.x, y: state.position.y, w: size.w, h: size.h }
}

/// The supported mode of `output` with this size and a refresh rate near `refresh_mhz`;
/// a `refresh_mhz` of 0 picks the fastest.
pub fn find_mode(output: &Output, width: i32, height: i32, refresh_mhz: i32) -> Option<Mode> {
    let sized = output.modes().into_iter().filter(|m| m.size == (width, height).into());
    if refresh_mhz <= 0 {
        return sized.max_by_key(|m| m.refresh);
    }
    sized
        .map(|m| ((m.refresh - refresh_mhz).abs(), m))
        .filter(|&(distance, _)| distance <= REFRESH_TOLERANCE)
        .min_by_key(|&(distance, _)| distance)
        .map(|(_, m)| m)
}

/// The layout for `heads` that `config` asks for: each display's entry, with defaults for what it leaves out.
///
/// At least one display stays on.
/// Displays without a position go to the right of the others.
/// When the stored positions leave displays apart from the rest, as after unplugging the middle one of three,
/// they move to the nearest edge of the others, so the pointer can reach each of them.
pub fn resolve(heads: &[Output], config: &Config) -> Layout {
    let default_scale = valid_scale(config.appearance.scale).unwrap_or(1.0);
    let mut entries: Vec<(Output, OutputState, bool)> = heads
        .iter()
        .filter_map(|output| {
            let identity = Identity::of(output);
            let entry = config.output(identity.id());
            let default_mode = output.preferred_mode().or_else(|| output.current_mode())?;
            let mode = entry
                .and_then(|e| e.mode)
                .and_then(|m| {
                    let size = |n: u32| i32::try_from(n).ok();
                    let refresh = i32::try_from(m.refresh_mhz).ok()?;
                    find_mode(output, size(m.width)?, size(m.height)?, refresh)
                })
                .unwrap_or(default_mode);
            let state = OutputState {
                enabled: entry.is_none_or(|e| e.enabled),
                mode,
                position: entry
                    .and_then(|e| e.position)
                    .map_or_else(Point::default, |[x, y]| (x, y).into()),
                transform: entry
                    .and_then(|e| e.transform)
                    .map_or(head_info(output).native_transform, transform_from_config),
                scale: entry.and_then(|e| e.scale).and_then(valid_scale).unwrap_or(default_scale),
            };
            let placed = entry.and_then(|e| e.position).is_some();
            Some((output.clone(), state, placed))
        })
        .collect();
    if !entries.iter().any(|(_, state, _)| state.enabled)
        && let Some((_, state, _)) = entries.first_mut()
    {
        state.enabled = true;
    }
    place(&mut entries);
    let mut layout: Layout =
        entries.into_iter().map(|(output, state, _)| (output, state)).collect();
    update_rects(&mut layout, geometry::join);
    layout
}

/// Puts the displays without a stored position to the right of the others.
fn place(entries: &mut [(Output, OutputState, bool)]) {
    let mut x = entries
        .iter()
        .filter(|(_, state, placed)| *placed && state.enabled)
        .map(|(_, state, _)| state.position.x + logical_size(state).w)
        .max()
        .unwrap_or(0);
    for (_, state, _) in entries.iter_mut().filter(|(_, state, placed)| !placed && state.enabled) {
        state.position = (x, 0).into();
        x += logical_size(state).w;
    }
}

/// Runs `edit` on the rectangles of the enabled displays, and moves the displays to match.
fn update_rects(layout: &mut [(Output, OutputState)], edit: impl FnOnce(&mut [Rect])) {
    let mut enabled: Vec<&mut OutputState> =
        layout.iter_mut().filter(|(_, state)| state.enabled).map(|(_, state)| state).collect();
    let mut rects: Vec<Rect> = enabled.iter().map(|state| rect(state)).collect();
    edit(&mut rects);
    for (state, rect) in enabled.iter_mut().zip(rects) {
        state.position = (rect.x, rect.y).into();
    }
}

/// Pushes apart displays that grew into each other since `before`, as when `appearance.scale` changes,
/// and joins those left apart; returns the displays that moved.
pub fn separate_resized(
    layout: &mut Layout,
    before: impl Fn(&Output) -> OutputState,
) -> Vec<Output> {
    // A display that kept its size counts as where it is, so that only resizes push displays apart.
    let old: Vec<Option<Rect>> = layout
        .iter()
        .filter(|(_, state)| state.enabled)
        .map(|(output, state)| {
            let old = before(output);
            let resized = logical_size(&old) != logical_size(state);
            old.enabled.then(|| if resized { rect(&old) } else { rect(state) })
        })
        .collect();
    let unmoved: Vec<_> = layout.iter().map(|(_, state)| state.position).collect();
    update_rects(layout, |rects| {
        geometry::separate(rects, &old);
        geometry::join(rects);
    });
    layout
        .iter()
        .zip(unmoved)
        .filter(|((_, state), position)| state.position != *position)
        .map(|((output, _), _)| output.clone())
        .collect()
}

/// Checks what every backend needs of a layout.
pub fn validate(layout: &[(Output, OutputState)]) -> Result<(), OutputError> {
    if !layout.iter().any(|(_, state)| state.enabled) {
        return Err(OutputError::Invalid("at least one display has to stay on".into()));
    }
    for (output, state) in layout.iter().filter(|(_, state)| state.enabled) {
        let name = output.name();
        if !SCALE_RANGE.contains(&state.scale) {
            return Err(OutputError::Invalid(format!(
                "scale {} of {name} is outside {} to {}",
                state.scale,
                SCALE_RANGE.start(),
                SCALE_RANGE.end()
            )));
        }
        if !output.modes().contains(&state.mode) {
            return Err(OutputError::Invalid(format!(
                "{name} doesn't support {}x{} at {} mHz",
                state.mode.size.w, state.mode.size.h, state.mode.refresh
            )));
        }
        if state.position.x.abs() > MAX_COORDINATE || state.position.y.abs() > MAX_COORDINATE {
            return Err(OutputError::Invalid(format!("{name} is too far from the origin")));
        }
    }
    let rects: Vec<Rect> =
        layout.iter().filter(|(_, state)| state.enabled).map(|(_, state)| rect(state)).collect();
    if !geometry::connected(&rects) {
        return Err(OutputError::Invalid(
            "each display has to share an edge with another, so the pointer can reach it".into(),
        ));
    }
    Ok(())
}

/// Which parts of a display's configuration a client set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Explicit {
    pub mode: bool,
    pub transform: bool,
    pub scale: bool,
}

/// The configuration entry that records `state`: whether the display is on, its position, and what `set` names.
/// Everything else keeps what `stored` has, or the display's default.
pub fn entry(
    output: &Output,
    state: &OutputState,
    stored: Option<&OutputConfig>,
    set: Explicit,
) -> OutputConfig {
    let identity = Identity::of(output);
    let mode = state.mode;
    let stored = stored.cloned().unwrap_or_default();
    OutputConfig {
        enabled: state.enabled,
        mode: if set.mode {
            Some(OutputMode {
                width: u32::try_from(mode.size.w).unwrap_or(0),
                height: u32::try_from(mode.size.h).unwrap_or(0),
                refresh_mhz: u32::try_from(mode.refresh).unwrap_or(0),
            })
        } else {
            stored.mode
        },
        position: Some([state.position.x, state.position.y]),
        scale: if set.scale { Some(state.scale) } else { stored.scale },
        transform: if set.transform {
            Some(transform_to_config(state.transform))
        } else {
            stored.transform
        },
        ..OutputConfig::new(identity.id())
    }
}

fn transform_from_config(transform: nimbus_config::Transform) -> Transform {
    wl_output::Transform::try_from(u32::from(transform)).map_or(Transform::Normal, Transform::from)
}

fn transform_to_config(transform: Transform) -> nimbus_config::Transform {
    nimbus_config::Transform::try_from(u32::from(wl_output::Transform::from(transform)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outputs::HeadDescription;

    fn mode(w: i32, h: i32, refresh: i32) -> Mode {
        Mode { size: (w, h).into(), refresh }
    }

    fn head(name: &str, serial: &str, modes: &[Mode]) -> Output {
        HeadDescription {
            name: name.into(),
            make: "Acme".into(),
            model: "Panel".into(),
            serial: serial.into(),
            physical_size: (0, 0),
            modes: modes.to_vec(),
            preferred: modes[0],
            native_transform: Transform::Normal,
        }
        .into_output()
    }

    fn positions(layout: &Layout) -> Vec<(i32, i32)> {
        layout.iter().map(|(_, s)| (s.position.x, s.position.y)).collect()
    }

    fn stored(config: &mut Config, output: &Output, edit: impl FnOnce(&mut OutputConfig)) {
        let identity = Identity::of(output);
        let mut entry = OutputConfig::new(identity.id());
        edit(&mut entry);
        config.set_output(entry);
    }

    #[test]
    fn unknown_displays_line_up_with_defaults() {
        let a = head("A", "", &[mode(1920, 1080, 60_000)]);
        let b = head("B", "", &[mode(2560, 1440, 144_000), mode(2560, 1440, 60_000)]);
        let mut config = Config::default();
        config.appearance.scale = 1.25;
        let layout = resolve(&[a, b], &config);
        assert_eq!(positions(&layout), [(0, 0), (1536, 0)]);
        assert!(layout.iter().all(|(_, s)| s.enabled && s.scale == 1.25));
        assert_eq!(layout[1].1.mode, mode(2560, 1440, 144_000), "the preferred mode");
    }

    #[test]
    fn stored_entries_apply() {
        let a = head("A", "1", &[mode(1920, 1080, 60_000)]);
        let b = head("B", "2", &[mode(1920, 1080, 60_000), mode(1280, 720, 59_940)]);
        let mut config = Config::default();
        stored(&mut config, &b, |e| {
            e.connector = "elsewhere".into();
            e.mode = Some("1280x720@60".parse().unwrap());
            e.position = Some([0, 0]);
            e.scale = Some(2.0);
            e.transform = Some(nimbus_config::Transform::Rotate90);
        });
        stored(&mut config, &a, |e| e.position = Some([360, 0]));
        let layout = resolve(&[a, b], &config);
        let b = layout[1].1;
        assert_eq!(b.mode, mode(1280, 720, 59_940), "matched within the refresh tolerance");
        assert_eq!((b.scale, b.transform), (2.0, Transform::_90));
        assert_eq!(logical_size(&b), (360, 640).into());
        assert_eq!(positions(&layout), [(360, 0), (0, 0)]);
    }

    #[test]
    fn a_stranded_display_is_lined_up_again() {
        let heads =
            [head("A", "", &[mode(100, 100, 60_000)]), head("C", "", &[mode(100, 100, 60_000)])];
        let mut config = Config::default();
        stored(&mut config, &heads[0], |e| e.position = Some([0, 0]));
        // B, at 100, was unplugged.
        stored(&mut config, &heads[1], |e| e.position = Some([200, 0]));
        assert_eq!(positions(&resolve(&heads, &config)), [(0, 0), (100, 0)]);
        // A stranded display moves to the nearest edge, not into a row.
        stored(&mut config, &heads[1], |e| e.position = Some([0, 300]));
        assert_eq!(positions(&resolve(&heads, &config)), [(0, 0), (0, 100)]);

        // Stacked displays touch, so they stay.
        stored(&mut config, &heads[1], |e| e.position = Some([50, 100]));
        assert_eq!(positions(&resolve(&heads, &config)), [(0, 0), (50, 100)]);
        // Touching at a corner isn't enough.
        stored(&mut config, &heads[1], |e| e.position = Some([100, 100]));
        assert_eq!(positions(&resolve(&heads, &config)), [(0, 0), (100, 99)]);
    }

    #[test]
    fn one_display_stays_on() {
        let a = head("A", "", &[mode(100, 100, 60_000)]);
        let mut config = Config::default();
        stored(&mut config, &a, |e| e.enabled = false);
        assert!(resolve(&[a], &config)[0].1.enabled);
    }

    #[test]
    fn entries_store_what_clients_set() {
        let a = head("A", "S", &[mode(1920, 1080, 60_000), mode(1280, 720, 60_000)]);
        let state = OutputState {
            enabled: true,
            mode: mode(1920, 1080, 60_000),
            position: (10, 20).into(),
            transform: Transform::Normal,
            scale: 1.5,
        };
        let entry = entry(&a, &state, None, Explicit::default());
        assert_eq!((entry.mode, entry.scale, entry.transform), (None, None, None));
        assert_eq!(entry.position, Some([10, 20]));
        assert_eq!((entry.connector.as_str(), entry.serial.as_str()), ("A", "S"));

        // A scale that a client set is stored, even when it matches `appearance.scale`.
        let scaled =
            super::entry(&a, &state, None, Explicit { scale: true, ..Explicit::default() });
        assert_eq!(scaled.scale, Some(1.5));
        // What the client leaves out keeps what's stored.
        let changed =
            OutputState { mode: mode(1280, 720, 60_000), transform: Transform::_270, ..state };
        let set = Explicit { mode: true, transform: true, scale: false };
        let entry = super::entry(&a, &changed, Some(&scaled), set);
        assert_eq!(entry.mode.map(|m| m.to_string()).as_deref(), Some("1280x720@60"));
        assert_eq!(entry.scale, Some(1.5));
        assert_eq!(entry.transform, Some(nimbus_config::Transform::Rotate270));

        let mut config = Config::default();
        config.set_output(entry);
        let layout = resolve(&[a], &config);
        assert_eq!(layout[0].1, changed, "round trip");
    }

    #[test]
    fn displays_that_grew_into_each_other_move_apart() {
        let heads: Vec<Output> = ["A", "B", "C"]
            .into_iter()
            .map(|name| head(name, "", &[mode(1920, 1080, 60_000)]))
            .collect();
        let mut config = Config::default();
        stored(&mut config, &heads[0], |e| e.position = Some([0, 0]));
        stored(&mut config, &heads[1], |e| e.position = Some([960, 0]));
        stored(&mut config, &heads[2], |e| e.position = Some([0, 540]));
        config.appearance.scale = 2.0;
        let before = resolve(&heads, &config);
        assert_eq!(positions(&before), [(0, 0), (960, 0), (0, 540)]);

        // At 100%, the displays would overlap where they were.
        config.appearance.scale = 1.0;
        let mut after = resolve(&heads, &config);
        let state = |output: &Output| before.iter().find(|(o, _)| o == output).unwrap().1;
        let moved = separate_resized(&mut after, state);
        assert_eq!(positions(&after), [(0, 0), (1920, 0), (0, 1080)]);
        assert_eq!(moved, heads[1..]);

        // Unchanged sizes move nothing.
        let mut same = before.clone();
        assert!(separate_resized(&mut same, state).is_empty());
        assert_eq!(positions(&same), positions(&before));
    }

    #[test]
    fn validation() {
        let a = head("A", "", &[mode(100, 100, 60_000)]);
        let good = OutputState {
            enabled: true,
            mode: mode(100, 100, 60_000),
            position: (0, 0).into(),
            transform: Transform::Normal,
            scale: 1.0,
        };
        assert!(validate(&[(a.clone(), good)]).is_ok());
        let b = head("B", "", &[mode(100, 100, 60_000)]);
        let beside = OutputState { position: (100, 0).into(), ..good };
        assert!(validate(&[(a.clone(), good), (b.clone(), beside)]).is_ok());
        let apart = OutputState { position: (101, 0).into(), ..good };
        assert!(validate(&[(a.clone(), good), (b, apart)]).is_err(), "displays apart");
        for bad in [
            OutputState { enabled: false, ..good },
            OutputState { scale: 0.0, ..good },
            OutputState { scale: f64::NAN, ..good },
            OutputState { mode: mode(200, 100, 60_000), ..good },
            OutputState { position: (i32::MAX, 0).into(), ..good },
        ] {
            assert!(validate(&[(a.clone(), bad)]).is_err(), "{bad:?}");
        }
        assert_eq!(find_mode(&a, 100, 100, 0), Some(good.mode));
        assert_eq!(find_mode(&a, 100, 100, 75_000), None);
    }
}
