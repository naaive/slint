// SPDX-License-Identifier: MIT

//! A real session: DRM/KMS outputs on the primary GPU through GBM and EGL, libinput, and libseat.
//!
//! Follows the structure of Smithay's `anvil` reference compositor, restricted to a single GPU.

use crate::capture;
use crate::outputs::edid::Edid;
use crate::outputs::{HeadDescription, OutputBackend, OutputError, OutputState};
use crate::render::{self, CLEAR_COLOR, OutputRenderElement, SceneOptions};
use crate::state::{Nimbus, State};
use anyhow::{Context, anyhow};
use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode, NodeType,
};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::import_surface_tree;
use smithay::backend::renderer::{ImportDma, ImportEgl};
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{UdevBackend as UdevMonitor, UdevEvent, all_gpus, primary_gpu};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::output::{Mode, Output};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::control::{
    self, Device as ControlDevice, ModeTypeFlags, connector, crtc,
};
use smithay::reexports::input::{self as libinput, Libinput};
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{DeviceFd, Monotonic, Time, Transform};
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use smithay::wayland::presentation::Refresh;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

/// Scan-out formats in order of preference.
const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Abgr8888];

type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;
type Feedback = Option<OutputPresentationFeedback>;

/// An enabled head, driven by a CRTC.
struct Surface {
    output: Output,
    drm_output: DrmOutput<Allocator, Exporter, Feedback, DrmDeviceFd>,
    connector: connector::Handle,
    mode: control::Mode,
    waiting_for_vblank: bool,
    /// `waiting_for_vblank` was set by an estimated-vblank timer rather than a queued page flip.
    estimated_vblank: bool,
    /// The CRTC is off (DPMS) because the output is; see [`power_off`].
    off: bool,
}

/// The refresh interval of `output`, or 60 Hz when its mode doesn't say.
fn frame_interval(output: &Output) -> Duration {
    output
        .current_mode()
        .and_then(|m| u64::try_from(m.refresh).ok())
        .filter(|&mhz| mhz > 0)
        .map_or(Duration::from_micros(16_667), |mhz| Duration::from_nanos(1_000_000_000_000 / mhz))
}

/// A connected connector, enabled or not.
struct Head {
    output: Output,
    info: connector::Info,
}

struct Gpu {
    node: DrmNode,
    manager: DrmOutputManager<Allocator, Exporter, Feedback, DrmDeviceFd>,
    renderer: GlesRenderer,
    heads: HashMap<connector::Handle, Head>,
    surfaces: HashMap<crtc::Handle, Surface>,
    notifier_token: RegistrationToken,
}

/// The steps that turn the current CRTC setup into a layout's.
#[derive(Default)]
struct Plan {
    disable: Vec<crtc::Handle>,
    modes: Vec<(crtc::Handle, control::Mode)>,
    enable: Vec<(connector::Handle, crtc::Handle, control::Mode)>,
}

/// The steps of a [`Plan`] that were carried out, to roll them back.
#[derive(Default)]
struct Undo {
    disabled: Vec<(connector::Handle, crtc::Handle, control::Mode)>,
    modes: Vec<(crtc::Handle, control::Mode)>,
    enabled: Vec<crtc::Handle>,
}

impl Gpu {
    fn plan(&self, layout: &[(Output, OutputState)]) -> Result<Plan, OutputError> {
        let device = self.manager.device();
        let resources = device
            .resource_handles()
            .map_err(|e| OutputError::Failed(format!("cannot read DRM resources: {e}")))?;
        let mut plan = Plan::default();
        let mut kept = HashSet::new();
        let mut new = Vec::new();
        for (output, state) in layout {
            let Some((&connector, head)) = self.heads.iter().find(|(_, h)| &h.output == output)
            else {
                return Err(OutputError::Invalid(format!("{} isn't connected", output.name())));
            };
            let current = self
                .surfaces
                .iter()
                .find(|(_, s)| s.connector == connector)
                .map(|(&c, s)| (c, s.mode));
            if !state.enabled {
                plan.disable.extend(current.map(|(crtc, _)| crtc));
                continue;
            }
            let mode = head
                .info
                .modes()
                .iter()
                .copied()
                .find(|&m| Mode::from(m) == state.mode)
                .ok_or_else(|| {
                    OutputError::Invalid(format!("{} doesn't support this mode", output.name()))
                })?;
            match current {
                Some((crtc, current)) => {
                    kept.insert(crtc);
                    if current != mode {
                        plan.modes.push((crtc, mode));
                    }
                }
                None => new.push((connector, head, mode)),
            }
        }
        for (connector, head, mode) in new {
            let crtc = head
                .info
                .encoders()
                .iter()
                .filter_map(|&encoder| device.get_encoder(encoder).ok())
                .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
                .find(|crtc| !kept.contains(crtc))
                .ok_or_else(|| {
                    OutputError::Failed(format!(
                        "no display controller is free for {}",
                        head.output.name()
                    ))
                })?;
            kept.insert(crtc);
            plan.enable.push((connector, crtc, mode));
        }
        Ok(plan)
    }

    /// Carries out `plan`, disabling first to free CRTCs and bandwidth, and records each step in `undo`.
    fn execute(&mut self, plan: &Plan, undo: &mut Undo) -> Result<(), OutputError> {
        for crtc in &plan.disable {
            if let Some(surface) = self.surfaces.remove(crtc) {
                undo.disabled.push((surface.connector, *crtc, surface.mode));
            }
        }
        for &(crtc, mode) in &plan.modes {
            let surface = self
                .surfaces
                .get_mut(&crtc)
                .ok_or_else(|| OutputError::Failed("the CRTC vanished".into()))?;
            surface
                .drm_output
                .use_mode(
                    mode,
                    &mut self.renderer,
                    &DrmOutputRenderElements::<_, OutputRenderElement<GlesRenderer>>::default(),
                )
                .map_err(|e| {
                    OutputError::Failed(format!(
                        "cannot change the mode of {}: {e}",
                        surface.output.name()
                    ))
                })?;
            undo.modes.push((crtc, surface.mode));
            surface.mode = mode;
        }
        for &(connector, crtc, mode) in &plan.enable {
            self.enable(connector, crtc, mode)?;
            undo.enabled.push(crtc);
        }
        Ok(())
    }

    fn rollback(&mut self, undo: Undo) {
        for crtc in undo.enabled {
            self.surfaces.remove(&crtc);
        }
        for (crtc, mode) in undo.modes {
            if let Some(surface) = self.surfaces.get_mut(&crtc) {
                let restored = surface.drm_output.use_mode(
                    mode,
                    &mut self.renderer,
                    &DrmOutputRenderElements::<_, OutputRenderElement<GlesRenderer>>::default(),
                );
                match restored {
                    Ok(()) => surface.mode = mode,
                    Err(err) => {
                        tracing::error!(output = %surface.output.name(), "cannot restore the mode: {err}")
                    }
                }
            }
        }
        for (connector, crtc, mode) in undo.disabled {
            if let Err(err) = self.enable(connector, crtc, mode) {
                tracing::error!("cannot turn a display back on: {err}");
            }
        }
    }

    fn enable(
        &mut self,
        connector: connector::Handle,
        crtc: crtc::Handle,
        mode: control::Mode,
    ) -> Result<(), OutputError> {
        let output = self
            .heads
            .get(&connector)
            .map(|head| head.output.clone())
            .ok_or_else(|| OutputError::Failed("the display was disconnected".into()))?;
        // The head isn't advertised while it's disabled, so this reaches no client;
        // it gives the first frame, rendered right away, the right size.
        output.change_current_state(Some(Mode::from(mode)), None, None, None);
        let drm_output = self
            .manager
            .initialize_output::<_, OutputRenderElement<GlesRenderer>>(
                crtc,
                mode,
                &[connector],
                &output,
                None,
                &mut self.renderer,
                &DrmOutputRenderElements::default(),
            )
            .map_err(|e| OutputError::Failed(format!("cannot drive {}: {e}", output.name())))?;
        self.surfaces.insert(
            crtc,
            Surface {
                output,
                drm_output,
                connector,
                mode,
                waiting_for_vblank: false,
                estimated_vblank: false,
                off: false,
            },
        );
        Ok(())
    }
}

pub struct UdevBackend {
    session: LibSeatSession,
    libinput: Libinput,
    input_devices: Vec<libinput::Device>,
    input_config: nimbus_config::Input,
    primary: DrmNode,
    gpu: Option<Gpu>,
    active: bool,
    handle: LoopHandle<'static, State>,
}

/// Opens the libseat session; its seat name is needed before the rest of the compositor starts.
pub fn open_session() -> anyhow::Result<(LibSeatSession, LibSeatSessionNotifier)> {
    LibSeatSession::new()
        .map_err(|e| anyhow!("cannot open a libseat session (is seatd or logind running?): {e}"))
}

impl UdevBackend {
    pub fn new(
        session: LibSeatSession,
        notifier: LibSeatSessionNotifier,
        nimbus: &mut Nimbus,
        handle: &LoopHandle<'static, State>,
    ) -> anyhow::Result<Self> {
        let seat = session.seat();
        let primary = primary_gpu(&seat)
            .ok()
            .flatten()
            .and_then(|path| DrmNode::from_path(path).ok())
            .or_else(|| {
                all_gpus(&seat).ok()?.into_iter().find_map(|path| DrmNode::from_path(path).ok())
            })
            .context("no GPU found on this seat")?;
        let primary =
            primary.node_with_type(NodeType::Primary).and_then(Result::ok).unwrap_or(primary);
        tracing::info!(gpu = ?primary.dev_path(), "using the primary GPU");

        let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(
            session.clone().into(),
        );
        libinput
            .udev_assign_seat(&seat)
            .map_err(|()| anyhow!("libinput cannot assign seat {seat}"))?;
        handle
            .insert_source(LibinputInputBackend::new(libinput.clone()), |event, _, state| {
                if let crate::backend::Backend::Udev(udev) = &mut state.backend {
                    match &event {
                        InputEvent::DeviceAdded { device } => udev.add_input_device(device.clone()),
                        InputEvent::DeviceRemoved { device } => {
                            udev.input_devices.retain(|d| d != device)
                        }
                        _ => {}
                    }
                }
                state.process_input_event(event);
            })
            .map_err(|e| anyhow!("cannot watch libinput: {e}"))?;

        handle
            .insert_source(notifier, |event, &mut (), state| state.handle_session_event(event))
            .map_err(|e| anyhow!("cannot watch the session: {e}"))?;

        let mut backend = Self {
            session,
            libinput,
            input_devices: Vec::new(),
            input_config: nimbus.config.current().input.clone(),
            primary,
            gpu: None,
            active: true,
            handle: handle.clone(),
        };

        let monitor = UdevMonitor::new(&seat).context("cannot monitor udev")?;
        for (device_id, path) in monitor.device_list() {
            if let Err(err) = backend.device_added(nimbus, device_id, path) {
                tracing::warn!(path = %path.display(), "cannot use GPU: {err:#}");
            }
        }
        if backend.gpu.is_none() {
            return Err(anyhow!(
                "the primary GPU {:?} could not be initialized",
                primary.dev_path()
            ));
        }
        handle
            .insert_source(monitor, |event, _, state| state.handle_udev_event(event))
            .map_err(|e| anyhow!("cannot watch udev: {e}"))?;
        Ok(backend)
    }

    fn device_added(
        &mut self,
        nimbus: &mut Nimbus,
        device_id: DevId,
        path: &Path,
    ) -> anyhow::Result<()> {
        let node = DrmNode::from_dev_id(device_id)?;
        if node != self.primary || self.gpu.is_some() {
            return Ok(());
        }
        let fd = self
            .session
            .open(path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)
            .map_err(|e| anyhow!("cannot open {}: {e}", path.display()))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (drm, notifier) = DrmDevice::new(fd.clone(), true)?;
        let gbm = GbmDevice::new(fd)?;
        // SAFETY: the GBM device outlives the display: both live in `Gpu` and drop together.
        let display = unsafe { EGLDisplay::new(gbm.clone()) }?;
        let context = EGLContext::new(&display)?;
        // SAFETY: the context was just created and is used only on this thread.
        let mut renderer = unsafe { GlesRenderer::new(context) }?;
        if let Err(err) = renderer.bind_wl_display(&nimbus.display_handle) {
            tracing::debug!("EGL hardware acceleration for clients is unavailable: {err}");
        }

        let render_node =
            node.node_with_type(NodeType::Render).and_then(Result::ok).unwrap_or(node);
        let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
        let allocator =
            GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(render_node));
        let manager = DrmOutputManager::new(
            drm,
            allocator,
            exporter,
            Some(gbm),
            COLOR_FORMATS,
            render_formats.iter().copied(),
        );

        let notifier_token = self
            .handle
            .insert_source(notifier, move |event, metadata, state| match event {
                DrmEvent::VBlank(crtc) => state.handle_vblank(crtc, metadata.take()),
                DrmEvent::Error(err) => tracing::error!("DRM error: {err}"),
            })
            .map_err(|e| anyhow!("cannot watch the DRM device: {e}"))?;

        let dmabuf_formats = renderer.dmabuf_formats();
        match DmabufFeedbackBuilder::new(render_node.dev_id(), dmabuf_formats.clone()).build() {
            Ok(feedback) => {
                nimbus.dmabuf_global =
                    Some(nimbus.dmabuf_state.create_global_with_default_feedback::<State>(
                        &nimbus.display_handle,
                        &feedback,
                    ));
            }
            Err(err) => {
                tracing::debug!("dmabuf feedback unavailable: {err}");
                nimbus.dmabuf_global = Some(
                    nimbus
                        .dmabuf_state
                        .create_global::<State>(&nimbus.display_handle, dmabuf_formats),
                );
            }
        }

        self.gpu = Some(Gpu {
            node,
            manager,
            renderer,
            heads: HashMap::new(),
            surfaces: HashMap::new(),
            notifier_token,
        });
        self.scan_connectors(nimbus);
        Ok(())
    }

    fn device_removed(&mut self, nimbus: &mut Nimbus, device_id: DevId) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        if gpu.node.dev_id() != device_id {
            return;
        }
        if let Some(gpu) = self.gpu.take() {
            for head in gpu.heads.values() {
                nimbus.disconnect_head(&head.output);
            }
            self.handle.remove(gpu.notifier_token);
            if let Some(global) = nimbus.dmabuf_global.take() {
                nimbus.dmabuf_state.destroy_global::<State>(&nimbus.display_handle, global);
            }
        }
        tracing::warn!("the primary GPU was removed");
    }

    /// Connects heads for newly connected connectors, disconnects those of unplugged ones,
    /// and applies the configured layout when that changed anything.
    fn scan_connectors(&mut self, nimbus: &mut Nimbus) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let device = gpu.manager.device();
        let resources = match device.resource_handles() {
            Ok(resources) => resources,
            Err(err) => {
                tracing::warn!("cannot read DRM resources: {err}");
                return;
            }
        };
        let connected: Vec<connector::Info> = resources
            .connectors()
            .iter()
            .filter_map(|&handle| device.get_connector(handle, true).ok())
            .filter(|info| info.state() == connector::State::Connected && !info.modes().is_empty())
            .collect();

        let gone: Vec<connector::Handle> = gpu
            .heads
            .keys()
            .filter(|&&handle| !connected.iter().any(|c| c.handle() == handle))
            .copied()
            .collect();
        let mut changed = !gone.is_empty();
        for handle in gone {
            gpu.surfaces.retain(|_, surface| surface.connector != handle);
            if let Some(head) = gpu.heads.remove(&handle) {
                nimbus.disconnect_head(&head.output);
            }
        }
        for info in connected {
            if gpu.heads.contains_key(&info.handle()) {
                continue;
            }
            let output = describe(gpu.manager.device(), &info);
            nimbus.connect_head(output.clone());
            gpu.heads.insert(info.handle(), Head { output, info });
            changed = true;
        }
        if changed {
            nimbus.reconfigure_outputs(self);
        }
    }

    pub fn render(&mut self, nimbus: &mut Nimbus) {
        if !self.active {
            return;
        }
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let mut idle = Vec::new();
        for (&crtc, surface) in &mut gpu.surfaces {
            let name = surface.output.name();
            if !nimbus.output_powered(&surface.output) {
                nimbus.pending_redraws.remove(&name);
                if !surface.off {
                    power_off(surface, nimbus);
                }
                continue;
            }
            if surface.waiting_for_vblank || !nimbus.pending_redraws.remove(&name) {
                continue;
            }
            surface.off = false;
            match render_surface(&mut gpu.renderer, surface, nimbus) {
                Ok(true) => {}
                Ok(false) => idle.push((crtc, frame_interval(&surface.output))),
                Err(err) => {
                    tracing::warn!(output = %name, "rendering failed: {err:#}");
                    // Clients still get their frame callbacks, and the frame is retried.
                    let time = nimbus.clock.now();
                    render::post_repaint(
                        &surface.output,
                        &RenderElementStates::default(),
                        nimbus,
                        time.into(),
                    );
                    nimbus.pending_redraws.insert(name);
                    idle.push((crtc, frame_interval(&surface.output)));
                }
            }
        }
        for (crtc, interval) in idle {
            self.estimate_vblank(crtc, interval);
        }
    }

    /// Holds the output's next frame back for one refresh interval without a page flip,
    /// so frame callbacks for frames with nothing to present stay paced at the refresh rate.
    fn estimate_vblank(&mut self, crtc: crtc::Handle, interval: Duration) {
        let Some(surface) = self.gpu.as_mut().and_then(|gpu| gpu.surfaces.get_mut(&crtc)) else {
            return;
        };
        surface.waiting_for_vblank = true;
        surface.estimated_vblank = true;
        let inserted =
            self.handle.insert_source(Timer::from_duration(interval), move |_, _, state| {
                if let Some((udev, _)) = state.udev_parts()
                    && let Some(surface) =
                        udev.gpu.as_mut().and_then(|gpu| gpu.surfaces.get_mut(&crtc))
                    && surface.estimated_vblank
                {
                    surface.estimated_vblank = false;
                    surface.waiting_for_vblank = false;
                }
                TimeoutAction::Drop
            });
        if let Err(err) = inserted {
            tracing::warn!("cannot schedule the next frame: {err}");
            surface.waiting_for_vblank = false;
            surface.estimated_vblank = false;
        }
    }

    fn vblank(
        &mut self,
        nimbus: &mut Nimbus,
        crtc: crtc::Handle,
        metadata: Option<DrmEventMetadata>,
    ) {
        let Some(surface) = self.gpu.as_mut().and_then(|gpu| gpu.surfaces.get_mut(&crtc)) else {
            return;
        };
        surface.waiting_for_vblank = false;
        nimbus.presenting.remove(&surface.output.name());
        match surface.drm_output.frame_submitted() {
            Ok(Some(Some(mut feedback))) => {
                let refresh = surface
                    .output
                    .current_mode()
                    .and_then(|m| u64::try_from(m.refresh).ok())
                    .filter(|&r| r > 0)
                    .map_or(Refresh::Unknown, |mhz| {
                        Refresh::Fixed(Duration::from_nanos(1_000_000_000_000 / mhz))
                    });
                let (time, sequence, flags) = match metadata {
                    Some(DrmEventMetadata { time: DrmEventTime::Monotonic(time), sequence }) => (
                        Time::<Monotonic>::from(time),
                        u64::from(sequence),
                        wp_presentation_feedback::Kind::Vsync
                            | wp_presentation_feedback::Kind::HwClock
                            | wp_presentation_feedback::Kind::HwCompletion,
                    ),
                    Some(DrmEventMetadata { sequence, .. }) => (
                        nimbus.clock.now(),
                        u64::from(sequence),
                        wp_presentation_feedback::Kind::Vsync,
                    ),
                    None => (nimbus.clock.now(), 0, wp_presentation_feedback::Kind::Vsync),
                };
                feedback.presented::<_, Monotonic>(time, refresh, sequence, flags);
            }
            Ok(_) => {}
            Err(err) => tracing::warn!("frame submission failed: {err}"),
        }
    }

    fn pause(&mut self) {
        self.active = false;
        self.libinput.suspend();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.manager.pause();
        }
    }

    fn resume(&mut self, nimbus: &mut Nimbus) {
        if let Err(()) = self.libinput.resume() {
            tracing::error!("cannot resume libinput");
        }
        if let Some(gpu) = self.gpu.as_mut() {
            if let Err(err) = gpu.manager.activate(false) {
                tracing::error!("cannot reactivate the DRM device: {err}");
            }
            for surface in gpu.surfaces.values_mut() {
                surface.waiting_for_vblank = false;
                surface.estimated_vblank = false;
                surface.off = false;
            }
        }
        nimbus.presenting.clear();
        self.active = true;
        self.scan_connectors(nimbus);
        // The configuration may have changed while the session was in the background.
        nimbus.reconfigure_outputs(self);
        nimbus.queue_redraw_all();
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        self.gpu.as_mut().is_some_and(|gpu| gpu.renderer.import_dmabuf(dmabuf, None).is_ok())
    }

    pub fn early_import(&mut self, surface: &WlSurface) {
        if let Some(gpu) = self.gpu.as_mut()
            && let Err(err) = import_surface_tree(&mut gpu.renderer, surface)
        {
            tracing::debug!("early buffer import failed: {err}");
        }
    }

    pub fn change_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            tracing::warn!("cannot switch to VT {vt}: {err}");
        }
    }

    pub fn capture(
        &mut self,
        nimbus: &Nimbus,
        job: capture::Job<'_>,
    ) -> anyhow::Result<Option<capture::Rendered>> {
        let gpu = self.gpu.as_mut().context("no GPU")?;
        capture::render::<_, GlesTexture>(&mut gpu.renderer, nimbus, job)
    }

    pub fn capture_dmabuf(&mut self) -> Option<capture::DmabufConstraints> {
        let gpu = self.gpu.as_ref()?;
        let formats = gpu.renderer.egl_context().dmabuf_render_formats().iter().copied();
        capture::DmabufConstraints::new(gpu.node.dev_id(), formats)
    }

    pub fn apply_input_config(&mut self, input: &nimbus_config::Input) {
        self.input_config = input.clone();
        let config = self.input_config.clone();
        for device in &mut self.input_devices {
            configure_device(device, &config);
        }
    }

    fn add_input_device(&mut self, mut device: libinput::Device) {
        configure_device(&mut device, &self.input_config);
        self.input_devices.push(device);
    }
}

/// Applies a plan, or checks that the displays exist, take their modes, and have CRTCs with `test`.
/// The kernel has the last word only when a plan is carried out, which rolls back what it did on failure.
impl OutputBackend for UdevBackend {
    fn apply_outputs(
        &mut self,
        layout: &[(Output, OutputState)],
        test: bool,
    ) -> Result<(), OutputError> {
        let gpu = self.gpu.as_mut().ok_or_else(|| OutputError::Failed("there's no GPU".into()))?;
        let plan = gpu.plan(layout)?;
        if test {
            return Ok(());
        }
        if !self.active {
            return Err(OutputError::Failed("the session is in the background".into()));
        }
        let mut undo = Undo::default();
        if let Err(err) = gpu.execute(&plan, &mut undo) {
            gpu.rollback(undo);
            return Err(err);
        }
        if !plan.disable.is_empty()
            && let Err(err) =
                gpu.manager.try_to_restore_modifiers::<_, OutputRenderElement<GlesRenderer>>(
                    &mut gpu.renderer,
                    &DrmOutputRenderElements::default(),
                )
        {
            tracing::debug!("cannot restore explicit modifiers: {err}");
        }
        Ok(())
    }
}

/// A head for `info`, named and described from its EDID where there is one.
fn describe(device: &DrmDevice, info: &connector::Info) -> Output {
    let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
    let edid = read_edid(device, info.handle());
    let (make, model, serial) = match edid {
        Some(edid) => (edid.make, edid.model, edid.serial),
        None => ("Unknown".into(), name.clone(), String::new()),
    };
    let modes: Vec<Mode> = info.modes().iter().copied().map(Mode::from).collect();
    let preferred = info
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .map_or(modes[0], |&m| Mode::from(m));
    let (w_mm, h_mm) = info.size().unwrap_or((0, 0));
    HeadDescription {
        name,
        make,
        model,
        serial,
        physical_size: (i32::try_from(w_mm).unwrap_or(0), i32::try_from(h_mm).unwrap_or(0)),
        modes,
        preferred,
        native_transform: Transform::Normal,
    }
    .into_output()
}

fn read_edid(device: &DrmDevice, connector: connector::Handle) -> Option<Edid> {
    let properties = device.get_properties(connector).ok()?;
    let (handles, values) = properties.as_props_and_values();
    let blob = handles.iter().zip(values).find_map(|(&handle, &value)| {
        let info = device.get_property(handle).ok()?;
        (info.name().to_str() == Ok("EDID")).then_some(value)
    })?;
    Edid::parse(&device.get_property_blob(blob).ok()?)
}

/// `dev_t`, which is 64 bits on every Linux target.
type DevId = u64;

fn configure_device(device: &mut libinput::Device, input: &nimbus_config::Input) {
    if device.config_tap_finger_count() > 0
        && let Err(err) = device.config_tap_set_enabled(input.tap_to_click)
    {
        tracing::debug!(device = device.name(), "cannot set tap-to-click: {err:?}");
    }
    if device.config_scroll_has_natural_scroll()
        && let Err(err) = device.config_scroll_set_natural_scroll_enabled(input.natural_scroll)
    {
        tracing::debug!(device = device.name(), "cannot set natural scrolling: {err:?}");
    }
    if device.config_accel_is_available()
        && let Err(err) = device.config_accel_set_speed(input.pointer_speed.clamp(-1.0, 1.0))
    {
        tracing::debug!(device = device.name(), "cannot set pointer speed: {err:?}");
    }
}

/// Turns the output's CRTC off (DPMS) and drops its pending frame; the next queued frame turns it on.
fn power_off(surface: &mut Surface, nimbus: &mut Nimbus) {
    let name = surface.output.name();
    if let Err(err) = surface.drm_output.with_compositor(|compositor| compositor.clear()) {
        tracing::warn!(output = %name, "cannot turn the display off: {err}");
    }
    surface.off = true;
    surface.waiting_for_vblank = false;
    surface.estimated_vblank = false;
    nimbus.presenting.remove(&name);
}

/// Renders and queues a frame; returns `false` when there was nothing new to present.
fn render_surface(
    renderer: &mut GlesRenderer,
    surface: &mut Surface,
    nimbus: &mut Nimbus,
) -> anyhow::Result<bool> {
    let output = surface.output.clone();
    let elements =
        render::output_elements(renderer, nimbus, &output, SceneOptions { cursor: true });
    let frame = surface
        .drm_output
        .render_frame(renderer, &elements, CLEAR_COLOR, FrameFlags::DEFAULT)
        .map_err(|e| anyhow!("{e}"))?;
    let time = nimbus.clock.now();
    render::post_repaint(&output, &frame.states, nimbus, time.into());
    if frame.is_empty {
        return Ok(false);
    }
    let feedback = render::take_presentation_feedback(&output, nimbus, &frame.states);
    surface.drm_output.queue_frame(Some(feedback)).map_err(|e| anyhow!("{e}"))?;
    surface.waiting_for_vblank = true;
    surface.estimated_vblank = false;
    nimbus.presenting.insert(output.name());
    Ok(true)
}

impl State {
    fn udev_parts(&mut self) -> Option<(&mut UdevBackend, &mut Nimbus)> {
        match &mut self.backend {
            crate::backend::Backend::Udev(udev) => Some((udev, &mut self.nimbus)),
            _ => None,
        }
    }

    fn handle_session_event(&mut self, event: SessionEvent) {
        let Some((udev, nimbus)) = self.udev_parts() else {
            return;
        };
        match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused");
                udev.pause();
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session resumed");
                udev.resume(nimbus);
            }
        }
    }

    fn handle_udev_event(&mut self, event: UdevEvent) {
        let Some((udev, nimbus)) = self.udev_parts() else {
            return;
        };
        match event {
            UdevEvent::Added { device_id, path } => {
                if let Err(err) = udev.device_added(nimbus, device_id, &path) {
                    tracing::warn!(path = %path.display(), "cannot use GPU: {err:#}");
                }
            }
            UdevEvent::Changed { device_id } => {
                if udev.gpu.as_ref().is_some_and(|gpu| gpu.node.dev_id() == device_id) {
                    udev.scan_connectors(nimbus);
                }
            }
            UdevEvent::Removed { device_id } => udev.device_removed(nimbus, device_id),
        }
    }

    fn handle_vblank(&mut self, crtc: crtc::Handle, metadata: Option<DrmEventMetadata>) {
        if let Some((udev, nimbus)) = self.udev_parts() {
            udev.vblank(nimbus, crtc, metadata);
        }
    }
}
