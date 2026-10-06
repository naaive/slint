// SPDX-License-Identifier: MIT

//! The layout the configuration asks for, validation, and the configuration entries that record a layout.

use super::{Layout, OutputError, OutputState, head_info};
use nimbus_config::{Config, OutputConfig, OutputId, OutputMode};
use smithay::output::{Mode, Output};
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};
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

fn rect(state: &OutputState) -> Rectangle<i32, Logical> {
    Rectangle::new(state.position, logical_size(state))
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
/// When the stored positions leave a display apart from the rest, as after unplugging the middle one of three,
/// the displays are lined up from left to right in their stored order, so the pointer can reach each of them.
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
    entries.into_iter().map(|(output, state, _)| (output, state)).collect()
}

fn place(entries: &mut [(Output, OutputState, bool)]) {
    let mut placed: Vec<&mut OutputState> = entries
        .iter_mut()
        .filter(|(_, state, placed)| *placed && state.enabled)
        .map(|(_, state, _)| state)
        .collect();
    let rects: Vec<_> = placed.iter().map(|state| rect(state)).collect();
    if !connected(&rects) {
        placed.sort_by_key(|state| (state.position.x, state.position.y));
        let mut x = 0;
        for state in placed {
            state.position = (x, 0).into();
            x += logical_size(state).w;
        }
    }
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

/// Whether the rectangles form one group, each sharing an edge with or overlapping another.
fn connected(rects: &[Rectangle<i32, Logical>]) -> bool {
    let adjacent = |a: &Rectangle<i32, Logical>, b: &Rectangle<i32, Logical>| {
        let overlap = |a0: i32, a1: i32, b0: i32, b1: i32| a1.min(b1) - a0.max(b0);
        let x = overlap(a.loc.x, a.loc.x + a.size.w, b.loc.x, b.loc.x + b.size.w);
        let y = overlap(a.loc.y, a.loc.y + a.size.h, b.loc.y, b.loc.y + b.size.h);
        x >= 0 && y >= 0 && (x > 0 || y > 0)
    };
    let Some(first) = rects.first() else {
        return true;
    };
    let mut reached = vec![first];
    let mut rest: Vec<_> = rects[1..].iter().collect();
    while let Some(index) = rest.iter().position(|r| reached.iter().any(|q| adjacent(q, r))) {
        reached.push(rest.swap_remove(index));
    }
    rest.is_empty()
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
    Ok(())
}

/// The configuration entry that records `state`, leaving out what matches the display's defaults.
pub fn entry(output: &Output, state: &OutputState, default_scale: f64) -> OutputConfig {
    let identity = Identity::of(output);
    let mode = state.mode;
    let default_scale = valid_scale(default_scale).unwrap_or(1.0);
    OutputConfig {
        enabled: state.enabled,
        mode: (output.preferred_mode() != Some(mode)).then(|| OutputMode {
            width: u32::try_from(mode.size.w).unwrap_or(0),
            height: u32::try_from(mode.size.h).unwrap_or(0),
            refresh_mhz: u32::try_from(mode.refresh).unwrap_or(0),
        }),
        position: Some([state.position.x, state.position.y]),
        scale: (state.scale != default_scale).then_some(state.scale),
        transform: (state.transform != head_info(output).native_transform)
            .then(|| transform_to_config(state.transform)),
        ..OutputConfig::new(identity.id())
    }
}

fn transform_from_config(transform: nimbus_config::Transform) -> Transform {
    use nimbus_config::Transform as T;
    match transform {
        T::Normal => Transform::Normal,
        T::Rotate90 => Transform::_90,
        T::Rotate180 => Transform::_180,
        T::Rotate270 => Transform::_270,
        T::Flipped => Transform::Flipped,
        T::Flipped90 => Transform::Flipped90,
        T::Flipped180 => Transform::Flipped180,
        T::Flipped270 => Transform::Flipped270,
    }
}

fn transform_to_config(transform: Transform) -> nimbus_config::Transform {
    use nimbus_config::Transform as T;
    match transform {
        Transform::Normal => T::Normal,
        Transform::_90 => T::Rotate90,
        Transform::_180 => T::Rotate180,
        Transform::_270 => T::Rotate270,
        Transform::Flipped => T::Flipped,
        Transform::Flipped90 => T::Flipped90,
        Transform::Flipped180 => T::Flipped180,
        Transform::Flipped270 => T::Flipped270,
    }
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

        // Stacked displays touch, so they stay.
        stored(&mut config, &heads[1], |e| e.position = Some([50, 100]));
        assert_eq!(positions(&resolve(&heads, &config)), [(0, 0), (50, 100)]);
        // Touching at a corner isn't enough.
        stored(&mut config, &heads[1], |e| e.position = Some([100, 100]));
        assert_eq!(positions(&resolve(&heads, &config)), [(0, 0), (100, 0)]);
    }

    #[test]
    fn one_display_stays_on() {
        let a = head("A", "", &[mode(100, 100, 60_000)]);
        let mut config = Config::default();
        stored(&mut config, &a, |e| e.enabled = false);
        assert!(resolve(&[a], &config)[0].1.enabled);
    }

    #[test]
    fn entries_leave_out_defaults() {
        let a = head("A", "S", &[mode(1920, 1080, 60_000), mode(1280, 720, 60_000)]);
        let state = OutputState {
            enabled: true,
            mode: mode(1920, 1080, 60_000),
            position: (10, 20).into(),
            transform: Transform::Normal,
            scale: 1.5,
        };
        let entry = entry(&a, &state, 1.5);
        assert_eq!((entry.mode, entry.scale, entry.transform), (None, None, None));
        assert_eq!(entry.position, Some([10, 20]));
        assert_eq!((entry.connector.as_str(), entry.serial.as_str()), ("A", "S"));

        let changed =
            OutputState { mode: mode(1280, 720, 60_000), transform: Transform::_270, ..state };
        let entry = super::entry(&a, &changed, 1.0);
        assert_eq!(entry.mode.map(|m| m.to_string()).as_deref(), Some("1280x720@60"));
        assert_eq!(entry.scale, Some(1.5));
        assert_eq!(entry.transform, Some(nimbus_config::Transform::Rotate270));

        let mut config = Config::default();
        config.set_output(entry);
        let layout = resolve(&[a], &config);
        assert_eq!(layout[0].1, changed, "round trip");
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
