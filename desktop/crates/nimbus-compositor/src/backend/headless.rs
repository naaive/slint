// SPDX-License-Identifier: MIT

//! Renders virtual outputs into memory with Pixman at a fixed frame clock, for tests and screenshots.

use super::DEFAULT_REFRESH_MHZ;
use crate::capture;
use crate::outputs::{HeadDescription, OutputBackend, OutputError, OutputState};
use crate::render::{self, CLEAR_COLOR, Capture, SceneOptions};
use crate::state::Nimbus;
use anyhow::{Context, anyhow};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::backend::renderer::{Bind, Offscreen};
use smithay::output::{Mode, Output};
use smithay::reexports::pixman::Image;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::utils::{Monotonic, Size, Transform};
use smithay::wayland::presentation::Refresh;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const OUTPUTS_ENV: &str = "NIMBUS_HEADLESS_OUTPUTS";
pub const SCREENSHOT_DIR_ENV: &str = "NIMBUS_HEADLESS_SCREENSHOT_DIR";
pub const SCREENSHOT_FRAMES_ENV: &str = "NIMBUS_HEADLESS_SCREENSHOT_FRAMES";
const DEFAULT_SIZE: (i32, i32) = (1920, 1080);
const DEFAULT_SCREENSHOT_FRAMES: u64 = 30;

/// Parses `1280x720,1920x1080`; invalid entries are skipped, and an empty result means one default output.
pub fn parse_outputs(spec: Option<&str>) -> Vec<(i32, i32)> {
    let mut sizes: Vec<(i32, i32)> = spec
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|entry| {
            let parsed = entry
                .split_once(['x', 'X'])
                .and_then(|(w, h)| {
                    Some((w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?))
                })
                .filter(|&(w, h)| (1..=16384).contains(&w) && (1..=16384).contains(&h));
            if parsed.is_none() {
                tracing::warn!("ignoring invalid {OUTPUTS_ENV} entry '{entry}'");
            }
            parsed
        })
        .collect();
    if sizes.is_empty() {
        sizes.push(DEFAULT_SIZE);
    }
    sizes
}

/// Writes one PNG per output after a number of frames.
#[derive(Debug, Clone)]
pub struct ScreenshotRequest {
    pub dir: PathBuf,
    pub after_frames: u64,
}

impl ScreenshotRequest {
    pub fn from_env(cli_dir: Option<PathBuf>) -> Option<Self> {
        let dir = cli_dir.or_else(|| {
            std::env::var_os(SCREENSHOT_DIR_ENV).filter(|d| !d.is_empty()).map(PathBuf::from)
        })?;
        let after_frames = std::env::var(SCREENSHOT_FRAMES_ENV)
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(DEFAULT_SCREENSHOT_FRAMES);
        Some(Self { dir, after_frames })
    }
}

struct HeadlessOutput {
    output: Output,
    /// Allocated at the output's mode size while it's enabled.
    buffer: Option<Image<'static, 'static>>,
    damage_tracker: OutputDamageTracker,
    frames: u64,
}

pub struct HeadlessBackend {
    renderer: PixmanRenderer,
    outputs: Vec<HeadlessOutput>,
    frame_interval: Duration,
    next_frame: Instant,
    ticks: u64,
    screenshot: Option<ScreenshotRequest>,
}

impl HeadlessBackend {
    /// Connects a head per entry of `sizes`; [`Nimbus::reconfigure_outputs`] enables them.
    pub fn new(
        nimbus: &mut Nimbus,
        sizes: &[(i32, i32)],
        screenshot: Option<ScreenshotRequest>,
    ) -> anyhow::Result<Self> {
        let renderer =
            PixmanRenderer::new().map_err(|e| anyhow!("cannot create the Pixman renderer: {e}"))?;
        let outputs = sizes
            .iter()
            .enumerate()
            .map(|(index, &(w, h))| {
                let mode = Mode { size: (w, h).into(), refresh: DEFAULT_REFRESH_MHZ };
                let output = HeadDescription {
                    name: format!("HEADLESS-{}", index + 1),
                    make: "Nimbus".into(),
                    model: "Headless".into(),
                    serial: String::new(),
                    physical_size: (0, 0),
                    modes: vec![mode],
                    preferred: mode,
                    native_transform: Transform::Normal,
                }
                .into_output();
                nimbus.connect_head(output.clone());
                HeadlessOutput {
                    damage_tracker: OutputDamageTracker::from_output(&output),
                    output,
                    buffer: None,
                    frames: 0,
                }
            })
            .collect();
        let frame_interval =
            Duration::from_micros(1_000_000_000 / u64::from(DEFAULT_REFRESH_MHZ.unsigned_abs()));
        if let Some(request) = &screenshot {
            tracing::info!(dir = %request.dir.display(), frames = request.after_frames, "headless screenshots requested");
        }
        Ok(Self {
            renderer,
            outputs,
            frame_interval,
            next_frame: Instant::now(),
            ticks: 0,
            screenshot,
        })
    }

    pub fn next_deadline(&self, nimbus: &Nimbus) -> Option<Instant> {
        (!nimbus.pending_redraws.is_empty() || self.screenshot.is_some()).then_some(self.next_frame)
    }

    pub fn render(&mut self, nimbus: &mut Nimbus) {
        let now = Instant::now();
        if now < self.next_frame {
            return;
        }
        // Skip missed ticks instead of rendering a burst.
        while self.next_frame <= now {
            self.next_frame += self.frame_interval;
        }
        self.ticks += 1;
        let time = nimbus.clock.now();
        for index in 0..self.outputs.len() {
            let name = self.outputs[index].output.name();
            if nimbus.pending_redraws.remove(&name)
                && let Err(err) = self.render_output(nimbus, index, time.into())
            {
                tracing::warn!(output = %name, "rendering failed: {err:#}");
            }
        }
        if let Some(request) = self.screenshot.clone().filter(|r| self.ticks >= r.after_frames) {
            self.screenshot = None;
            for output in nimbus.outputs().cloned().collect::<Vec<_>>() {
                let path = request.dir.join(format!("{}.png", output.name()));
                match self.screenshot(nimbus, &output).and_then(|capture| capture.save_png(&path)) {
                    Ok(()) => tracing::info!(path = %path.display(), "wrote headless screenshot"),
                    Err(err) => tracing::warn!("headless screenshot failed: {err:#}"),
                }
            }
        }
    }

    fn render_output(
        &mut self,
        nimbus: &mut Nimbus,
        index: usize,
        time: Duration,
    ) -> anyhow::Result<()> {
        let out = &mut self.outputs[index];
        let output = out.output.clone();
        let elements = render::output_elements(
            &mut self.renderer,
            nimbus,
            &output,
            SceneOptions { cursor: false },
        );
        let age = if out.frames == 0 { 0 } else { 1 };
        let buffer = out.buffer.as_mut().context("the output has no buffer")?;
        let mut framebuffer = self.renderer.bind(buffer).map_err(|e| anyhow!("{e}"))?;
        let result = out
            .damage_tracker
            .render_output(&mut self.renderer, &mut framebuffer, age, &elements, CLEAR_COLOR)
            .map_err(|e| anyhow!("{e:?}"))?;
        out.frames += 1;
        let states = result.states;
        render::post_repaint(&output, &states, nimbus, time);
        let mut feedback = render::take_presentation_feedback(&output, nimbus, &states);
        feedback.presented::<_, Monotonic>(
            nimbus.clock.now(),
            Refresh::Fixed(self.frame_interval),
            out.frames,
            wp_presentation_feedback::Kind::empty(),
        );
        Ok(())
    }

    pub fn capture(
        &mut self,
        nimbus: &Nimbus,
        job: capture::Job<'_>,
    ) -> anyhow::Result<Option<capture::Rendered>> {
        capture::render::<_, Image<'static, 'static>>(&mut self.renderer, nimbus, job)
    }

    fn screenshot(&mut self, nimbus: &Nimbus, output: &Output) -> anyhow::Result<Capture> {
        capture::screenshot(nimbus, output, |job| self.capture(nimbus, job))
    }
}

/// Virtual outputs take any of their modes and transforms, and can be turned off.
impl OutputBackend for HeadlessBackend {
    fn apply_outputs(
        &mut self,
        layout: &[(Output, OutputState)],
        test: bool,
    ) -> Result<(), OutputError> {
        let mut buffers = Vec::new();
        for (index, out) in self.outputs.iter().enumerate() {
            let Some((_, state)) = layout.iter().find(|(o, s)| o == &out.output && s.enabled)
            else {
                continue;
            };
            let (w, h) = (state.mode.size.w, state.mode.size.h);
            let fits = out
                .buffer
                .as_ref()
                .is_some_and(|b| (b.width(), b.height()) == (w as usize, h as usize));
            if !fits && !test {
                let buffer: Image<'static, 'static> =
                    self.renderer.create_buffer(Fourcc::Abgr8888, Size::from((w, h))).map_err(
                        |e| OutputError::Failed(format!("cannot allocate a {w}x{h} buffer: {e}")),
                    )?;
                buffers.push((index, buffer));
            }
        }
        for (index, buffer) in buffers {
            self.outputs[index].buffer = Some(buffer);
            self.outputs[index].frames = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_output_lists() {
        assert_eq!(parse_outputs(Some("1280x720,1920X1080")), vec![(1280, 720), (1920, 1080)]);
        assert_eq!(parse_outputs(Some(" 800 x 600 , ")), vec![(800, 600)]);
    }

    #[test]
    fn invalid_or_missing_lists_fall_back_to_one_output() {
        assert_eq!(parse_outputs(None), vec![DEFAULT_SIZE]);
        assert_eq!(parse_outputs(Some("")), vec![DEFAULT_SIZE]);
        assert_eq!(parse_outputs(Some("huge,0x10,-5x5,99999x1")), vec![DEFAULT_SIZE]);
        assert_eq!(parse_outputs(Some("abc,640x480")), vec![(640, 480)]);
    }
}
