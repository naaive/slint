// SPDX-License-Identifier: MIT

//! A nested session in a window of the host's Wayland or X11 session, for development.

use super::DEFAULT_REFRESH_MHZ;
use crate::capture;
use crate::outputs::{HeadDescription, OutputBackend, OutputError, OutputState};
use crate::render::{self, CLEAR_COLOR, SceneOptions};
use crate::state::{Nimbus, State};
use anyhow::anyhow;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{ImportDma, ImportEgl};
use smithay::backend::winit::{self, WinitEvent, WinitEventLoop, WinitGraphicsBackend};
use smithay::output::{Mode, Output};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::utils::{Monotonic, Physical, Size, Transform};
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use smithay::wayland::presentation::Refresh;
use std::time::{Duration, Instant};

pub const OUTPUT_NAME: &str = "winit";

/// The host window, created before Nimbus advertises its own `WAYLAND_DISPLAY`.
pub struct WinitWindow {
    graphics: WinitGraphicsBackend<GlesRenderer>,
    events: WinitEventLoop,
}

/// Opens the host window; this connects to the parent session, so it must run before Nimbus sets its environment.
pub fn open_window() -> anyhow::Result<WinitWindow> {
    let (graphics, events) = winit::init::<GlesRenderer>()
        .map_err(|e| anyhow!("cannot open a window in the host session: {e}"))?;
    Ok(WinitWindow { graphics, events })
}

pub struct WinitBackend {
    graphics: WinitGraphicsBackend<GlesRenderer>,
    output: Output,
    damage_tracker: OutputDamageTracker,
    frame_interval: Duration,
    next_frame: Instant,
    frames: u64,
}

impl WinitBackend {
    pub fn new(
        window: WinitWindow,
        nimbus: &mut Nimbus,
        handle: &LoopHandle<'static, State>,
    ) -> anyhow::Result<Self> {
        let WinitWindow { mut graphics, events } = window;
        graphics.window().set_title("Nimbus");
        // Nimbus draws its own cursor.
        graphics.window().set_cursor_visible(false);
        if let Err(err) = graphics.renderer().bind_wl_display(&nimbus.display_handle) {
            tracing::debug!("EGL hardware acceleration for clients is unavailable: {err}");
        }

        let formats = graphics.renderer().dmabuf_formats();
        let render_node =
            EGLDevice::device_for_display(graphics.renderer().egl_context().display())
                .ok()
                .and_then(|device| device.try_get_render_node().ok().flatten());
        let feedback = render_node.and_then(|node| {
            DmabufFeedbackBuilder::new(node.dev_id(), formats.clone()).build().ok()
        });
        nimbus.dmabuf_global = Some(match feedback {
            Some(feedback) => nimbus
                .dmabuf_state
                .create_global_with_default_feedback::<State>(&nimbus.display_handle, &feedback),
            None => nimbus.dmabuf_state.create_global::<State>(&nimbus.display_handle, formats),
        });

        let size = graphics.window_size();
        let mode = Mode { size, refresh: DEFAULT_REFRESH_MHZ };
        let output = HeadDescription {
            name: OUTPUT_NAME.into(),
            make: "Nimbus".into(),
            model: "Winit".into(),
            serial: String::new(),
            physical_size: (0, 0),
            modes: vec![mode],
            preferred: mode,
            // GL framebuffers are bottom-up.
            native_transform: Transform::Flipped180,
        }
        .into_output();
        nimbus.connect_head(output.clone());

        handle
            .insert_source(events, |event, _, state| state.handle_winit_event(event))
            .map_err(|e| anyhow!("cannot watch the host window: {e}"))?;

        Ok(Self {
            damage_tracker: OutputDamageTracker::from_output(&output),
            graphics,
            output,
            frame_interval: Duration::from_micros(
                1_000_000_000 / u64::from(DEFAULT_REFRESH_MHZ.unsigned_abs()),
            ),
            next_frame: Instant::now(),
            frames: 0,
        })
    }

    pub fn next_deadline(&self, nimbus: &Nimbus) -> Option<Instant> {
        nimbus.pending_redraws.contains(OUTPUT_NAME).then_some(self.next_frame)
    }

    /// Makes the window's new size the output's only mode.
    fn resized(&mut self, size: Size<i32, Physical>, nimbus: &mut Nimbus) {
        let mode = Mode { size, refresh: DEFAULT_REFRESH_MHZ };
        for old in self.output.modes() {
            self.output.delete_mode(old);
        }
        self.output.change_current_state(Some(mode), None, None, None);
        self.output.set_preferred(mode);
        nimbus.outputs_changed(&[]);
    }

    pub fn render(&mut self, nimbus: &mut Nimbus) {
        let now = Instant::now();
        if now < self.next_frame || !nimbus.pending_redraws.remove(OUTPUT_NAME) {
            return;
        }
        self.next_frame = now + self.frame_interval;
        if let Err(err) = self.render_frame(nimbus) {
            tracing::warn!("rendering the nested window failed: {err:#}");
        }
    }

    fn render_frame(&mut self, nimbus: &mut Nimbus) -> anyhow::Result<()> {
        let age = self.graphics.buffer_age().unwrap_or(0);
        let output = self.output.clone();
        let (damage, states) = {
            let (renderer, mut framebuffer) = self.graphics.bind().map_err(|e| anyhow!("{e}"))?;
            let elements =
                render::output_elements(renderer, nimbus, &output, SceneOptions { cursor: true });
            let result = self
                .damage_tracker
                .render_output(renderer, &mut framebuffer, age, &elements, CLEAR_COLOR)
                .map_err(|e| anyhow!("{e:?}"))?;
            (result.damage.cloned(), result.states)
        };
        self.graphics.submit(damage.as_deref()).map_err(|e| anyhow!("{e}"))?;
        self.frames += 1;
        let time = nimbus.clock.now();
        render::post_repaint(&output, &states, nimbus, time.into());
        let mut feedback = render::take_presentation_feedback(&output, nimbus, &states);
        feedback.presented::<_, Monotonic>(
            time,
            Refresh::Fixed(self.frame_interval),
            self.frames,
            wp_presentation_feedback::Kind::empty(),
        );
        Ok(())
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        self.graphics.renderer().import_dmabuf(dmabuf, None).is_ok()
    }

    pub fn capture(
        &mut self,
        nimbus: &Nimbus,
        job: capture::Job<'_>,
    ) -> anyhow::Result<Option<capture::Rendered>> {
        capture::render::<_, GlesTexture>(self.graphics.renderer(), nimbus, job)
    }

    pub fn capture_dmabuf(&mut self) -> Option<capture::DmabufConstraints> {
        let context = self.graphics.renderer().egl_context();
        let node = EGLDevice::device_for_display(context.display())
            .ok()?
            .try_get_render_node()
            .ok()
            .flatten()?;
        capture::DmabufConstraints::new(
            node.dev_id(),
            context.dmabuf_render_formats().iter().copied(),
        )
    }
}

/// The window's size sets the mode and the transform keeps GL's picture upright, so only position and scale change.
impl OutputBackend for WinitBackend {
    fn apply_outputs(
        &mut self,
        layout: &[(Output, OutputState)],
        _test: bool,
    ) -> Result<(), OutputError> {
        for (output, state) in layout {
            if !state.enabled {
                return Err(OutputError::Unsupported("turn off the nested window"));
            }
            if output.current_mode() != Some(state.mode) {
                return Err(OutputError::Unsupported("change the mode of the nested window"));
            }
            if output.current_transform() != state.transform {
                return Err(OutputError::Unsupported("rotate the nested window"));
            }
        }
        Ok(())
    }
}

impl State {
    fn handle_winit_event(&mut self, event: WinitEvent) {
        match event {
            WinitEvent::Resized { size, .. } => {
                if let crate::backend::Backend::Winit(backend) = &mut self.backend {
                    backend.resized(size, &mut self.nimbus);
                }
            }
            WinitEvent::Input(event) => self.process_input_event(event),
            WinitEvent::Redraw => self.nimbus.queue_redraw_all(),
            WinitEvent::CloseRequested => self.nimbus.stop(),
            WinitEvent::Focus(_) => {}
        }
    }
}
