// SPDX-License-Identifier: MIT

//! A real session: DRM/KMS outputs on the primary GPU through GBM and EGL, libinput, and libseat.
//!
//! Follows the structure of Smithay's `anvil` reference compositor, restricted to a single GPU.

use crate::render::{self, CLEAR_COLOR, Capture, OutputRenderElement, SceneOptions};
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
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::import_surface_tree;
use smithay::backend::renderer::{ImportDma, ImportEgl};
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{UdevBackend as UdevMonitor, UdevEvent, all_gpus, primary_gpu};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::control::{Device as ControlDevice, ModeTypeFlags, connector, crtc};
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

struct Surface {
    output: Output,
    drm_output: DrmOutput<Allocator, Exporter, Feedback, DrmDeviceFd>,
    connector: connector::Handle,
    waiting_for_vblank: bool,
}

struct Gpu {
    node: DrmNode,
    manager: DrmOutputManager<Allocator, Exporter, Feedback, DrmDeviceFd>,
    renderer: GlesRenderer,
    surfaces: HashMap<crtc::Handle, Surface>,
    notifier_token: RegistrationToken,
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

        self.gpu = Some(Gpu { node, manager, renderer, surfaces: HashMap::new(), notifier_token });
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
            for surface in gpu.surfaces.values() {
                nimbus.remove_output(&surface.output);
            }
            self.handle.remove(gpu.notifier_token);
            if let Some(global) = nimbus.dmabuf_global.take() {
                nimbus.dmabuf_state.destroy_global::<State>(&nimbus.display_handle, global);
            }
        }
        tracing::warn!("the primary GPU was removed");
    }

    /// Creates outputs for newly connected connectors and removes those of disconnected ones.
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
            .filter(|info| info.state() == connector::State::Connected)
            .collect();

        let gone: Vec<crtc::Handle> = gpu
            .surfaces
            .iter()
            .filter(|(_, s)| !connected.iter().any(|c| c.handle() == s.connector))
            .map(|(crtc, _)| *crtc)
            .collect();
        for crtc in gone {
            if let Some(surface) = gpu.surfaces.remove(&crtc) {
                nimbus.remove_output(&surface.output);
            }
        }

        let mut used: HashSet<crtc::Handle> = gpu.surfaces.keys().copied().collect();
        for info in connected {
            if gpu.surfaces.values().any(|s| s.connector == info.handle()) {
                continue;
            }
            let device = gpu.manager.device();
            let crtc = info.encoders().iter().filter_map(|&e| device.get_encoder(e).ok()).find_map(
                |encoder| {
                    resources
                        .filter_crtcs(encoder.possible_crtcs())
                        .into_iter()
                        .find(|c| !used.contains(c))
                },
            );
            let Some(crtc) = crtc else {
                tracing::warn!(connector = ?info.handle(), "no free CRTC for connector");
                continue;
            };
            let Some(&drm_mode) = info
                .modes()
                .iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .or_else(|| info.modes().first())
            else {
                continue;
            };
            let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
            let (w_mm, h_mm) = info.size().unwrap_or((0, 0));
            let output = Output::new(
                name.clone(),
                PhysicalProperties {
                    size: (i32::try_from(w_mm).unwrap_or(0), i32::try_from(h_mm).unwrap_or(0))
                        .into(),
                    subpixel: Subpixel::Unknown,
                    make: "Unknown".into(),
                    model: name.clone(),
                },
            );
            for &mode in info.modes() {
                output.add_mode(Mode::from(mode));
            }
            let mode = Mode::from(drm_mode);
            output.set_preferred(mode);
            output.change_current_state(Some(mode), Some(Transform::Normal), None, None);

            let drm_output =
                match gpu.manager.initialize_output::<_, OutputRenderElement<GlesRenderer>>(
                    crtc,
                    drm_mode,
                    &[info.handle()],
                    &output,
                    None,
                    &mut gpu.renderer,
                    &DrmOutputRenderElements::default(),
                ) {
                    Ok(drm_output) => drm_output,
                    Err(err) => {
                        tracing::warn!(output = %name, "cannot drive output: {err}");
                        continue;
                    }
                };
            used.insert(crtc);
            nimbus.add_output(output.clone());
            nimbus.queue_redraw(&output);
            gpu.surfaces.insert(
                crtc,
                Surface { output, drm_output, connector: info.handle(), waiting_for_vblank: false },
            );
        }
    }

    pub fn render(&mut self, nimbus: &mut Nimbus) {
        if !self.active {
            return;
        }
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        for surface in gpu.surfaces.values_mut() {
            let name = surface.output.name();
            if surface.waiting_for_vblank || !nimbus.pending_redraws.remove(&name) {
                continue;
            }
            if let Err(err) = render_surface(&mut gpu.renderer, surface, nimbus) {
                tracing::warn!(output = %name, "rendering failed: {err:#}");
            }
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
            }
        }
        self.active = true;
        self.scan_connectors(nimbus);
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

    pub fn capture(&mut self, nimbus: &Nimbus, output: &Output) -> anyhow::Result<Capture> {
        let gpu = self.gpu.as_mut().context("no GPU")?;
        let mode = output.current_mode().context("the output has no mode")?;
        let elements = render::output_elements(
            &mut gpu.renderer,
            nimbus,
            output,
            SceneOptions { cursor: false },
        );
        render::render_to_memory::<_, GlesTexture>(
            &mut gpu.renderer,
            mode.size,
            output.current_scale().fractional_scale(),
            &elements,
        )
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

fn render_surface(
    renderer: &mut GlesRenderer,
    surface: &mut Surface,
    nimbus: &mut Nimbus,
) -> anyhow::Result<()> {
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
        return Ok(());
    }
    let feedback = render::take_presentation_feedback(&output, nimbus, &frame.states);
    surface.drm_output.queue_frame(Some(feedback)).map_err(|e| anyhow!("{e}"))?;
    surface.waiting_for_vblank = true;
    Ok(())
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
