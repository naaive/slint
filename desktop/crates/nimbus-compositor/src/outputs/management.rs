// SPDX-License-Identifier: MIT

//! The server side of wlr-output-management-unstable-v1, which display settings tools such as
//! Nimbus Settings, `wlr-randr`, and `kanshi` use.
//!
//! Each client is sent only what changed since its last `done`.
//! Applied configurations go through [`Nimbus::configure_outputs`](crate::state::Nimbus::configure_outputs)
//! like every other display change, and are saved to the configuration file.

use super::layout::{find_mode, round_scale};
use super::{Layout, OutputError, OutputState, head_info};
use crate::state::State;
use smithay::output::{Mode, Output};
use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::{
    zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, AdaptiveSyncState, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_output;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::{Logical, Point, Transform};
use std::sync::{Arc, Mutex};

const VERSION: u32 = 4;

/// What a head advertises, compared with what each client was last sent.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadSnapshot {
    name: String,
    description: String,
    make: String,
    model: String,
    serial: String,
    physical_size: (i32, i32),
    modes: Vec<Mode>,
    preferred: Option<Mode>,
    state: OutputState,
}

impl HeadSnapshot {
    pub fn new(output: &Output, state: OutputState) -> Self {
        let physical = output.physical_properties();
        Self {
            name: output.name(),
            description: output.description(),
            make: physical.make,
            model: physical.model,
            serial: head_info(output).serial,
            physical_size: (physical.size.w, physical.size.h),
            modes: output.modes(),
            preferred: output.preferred_mode(),
            state,
        }
    }

    /// Whether `other` is the same device, whose read-only properties a head object announces only once.
    fn same_device(&self, other: &Self) -> bool {
        (&self.name, &self.description, &self.make, &self.model, &self.serial, self.physical_size)
            == (
                &other.name,
                &other.description,
                &other.make,
                &other.model,
                &other.serial,
                other.physical_size,
            )
    }
}

pub struct OutputManagementState {
    serial: u32,
    heads: Vec<HeadSnapshot>,
    managers: Vec<Manager>,
}

impl OutputManagementState {
    pub fn new(display: &DisplayHandle) -> Self {
        display.create_global::<State, ZwlrOutputManagerV1, ()>(VERSION, ());
        Self { serial: 1, heads: Vec::new(), managers: Vec::new() }
    }

    /// Sends clients what changed, followed by `done` with a new serial.
    pub fn update(&mut self, display: &DisplayHandle, heads: Vec<HeadSnapshot>) {
        if heads == self.heads {
            return;
        }
        self.heads = heads;
        self.serial = self.serial.wrapping_add(1);
        for manager in &mut self.managers {
            manager.sync(display, &self.heads, self.serial);
        }
    }
}

/// One client's bound manager and the heads it was sent.
struct Manager {
    resource: ZwlrOutputManagerV1,
    heads: Vec<SentHead>,
}

impl Manager {
    fn sync(&mut self, display: &DisplayHandle, heads: &[HeadSnapshot], serial: u32) {
        self.heads.retain(|sent| {
            let present = heads.iter().any(|head| head.same_device(&sent.snapshot));
            if !present {
                sent.finish();
            }
            present
        });
        for head in heads {
            match self.heads.iter_mut().find(|sent| sent.snapshot.same_device(head)) {
                Some(sent) => sent.update(display, head),
                None => self.heads.extend(SentHead::create(display, &self.resource, head)),
            }
        }
        self.resource.done(serial);
    }
}

struct SentHead {
    resource: ZwlrOutputHeadV1,
    /// The mode objects, in the order of `snapshot.modes`.
    modes: Vec<ZwlrOutputModeV1>,
    snapshot: HeadSnapshot,
}

/// The user data of a mode object.
pub struct ModeData {
    head: String,
    mode: Mode,
}

impl SentHead {
    fn create(
        display: &DisplayHandle,
        manager: &ZwlrOutputManagerV1,
        head: &HeadSnapshot,
    ) -> Option<Self> {
        let client = manager.client()?;
        let resource = client
            .create_resource::<ZwlrOutputHeadV1, _, State>(
                display,
                manager.version(),
                head.name.clone(),
            )
            .ok()?;
        manager.head(&resource);
        resource.name(head.name.clone());
        resource.description(head.description.clone());
        if head.physical_size != (0, 0) {
            resource.physical_size(head.physical_size.0, head.physical_size.1);
        }
        if resource.version() >= 2 {
            for (text, send) in [
                (&head.make, ZwlrOutputHeadV1::make as fn(&ZwlrOutputHeadV1, String)),
                (&head.model, ZwlrOutputHeadV1::model),
                (&head.serial, ZwlrOutputHeadV1::serial_number),
            ] {
                if !text.is_empty() {
                    send(&resource, text.clone());
                }
            }
        }
        if resource.version() >= 4 {
            resource.adaptive_sync(AdaptiveSyncState::Disabled);
        }
        let mut sent = Self { resource, modes: Vec::new(), snapshot: head.clone() };
        sent.send_modes(display, &client);
        sent.send_state(None, true);
        Some(sent)
    }

    fn send_modes(&mut self, display: &DisplayHandle, client: &Client) {
        for &mode in &self.snapshot.modes {
            let data = ModeData { head: self.snapshot.name.clone(), mode };
            let Ok(resource) = client.create_resource::<ZwlrOutputModeV1, _, State>(
                display,
                self.resource.version(),
                data,
            ) else {
                continue;
            };
            self.resource.mode(&resource);
            resource.size(mode.size.w, mode.size.h);
            if mode.refresh > 0 {
                resource.refresh(mode.refresh);
            }
            if self.snapshot.preferred == Some(mode) {
                resource.preferred();
            }
            self.modes.push(resource);
        }
    }

    /// Sends the current state that differs from `old`, all of it for `None`.
    fn send_state(&self, old: Option<&OutputState>, modes_replaced: bool) {
        let new = &self.snapshot.state;
        if old.is_none_or(|old| old.enabled != new.enabled) {
            self.resource.enabled(i32::from(new.enabled));
        }
        if !new.enabled {
            return;
        }
        let old = old.filter(|old| old.enabled);
        if (modes_replaced || old.is_none_or(|old| old.mode != new.mode))
            && let Some(index) = self.snapshot.modes.iter().position(|&m| m == new.mode)
            && let Some(mode) = self.modes.get(index)
        {
            self.resource.current_mode(mode);
        }
        if old.is_none_or(|old| old.position != new.position) {
            self.resource.position(new.position.x, new.position.y);
        }
        if old.is_none_or(|old| old.transform != new.transform) {
            self.resource.transform(new.transform.into());
        }
        if old.is_none_or(|old| old.scale != new.scale) {
            self.resource.scale(new.scale);
        }
    }

    fn update(&mut self, display: &DisplayHandle, head: &HeadSnapshot) {
        if self.snapshot == *head {
            return;
        }
        let modes_replaced =
            self.snapshot.modes != head.modes || self.snapshot.preferred != head.preferred;
        let old = std::mem::replace(&mut self.snapshot, head.clone());
        if modes_replaced && let Some(client) = self.resource.client() {
            for mode in self.modes.drain(..) {
                mode.finished();
            }
            self.send_modes(display, &client);
        }
        self.send_state(Some(&old.state), modes_replaced);
    }

    fn finish(&self) {
        for mode in &self.modes {
            mode.finished();
        }
        self.resource.finished();
    }
}

impl GlobalDispatch<ZwlrOutputManagerV1, ()> for State {
    fn bind(
        state: &mut Self,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let resource = data_init.init(resource, ());
        let management = &mut state.nimbus.output_management;
        let mut manager = Manager { resource, heads: Vec::new() };
        manager.sync(display, &management.heads, management.serial);
        management.managers.push(manager);
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                data_init.init(id, ConfigurationData { serial, pending: Mutex::default() });
            }
            zwlr_output_manager_v1::Request::Stop => {
                state.nimbus.output_management.managers.retain(|m| &m.resource != resource);
                resource.finished();
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &ZwlrOutputManagerV1, _data: &()) {
        state.nimbus.output_management.managers.retain(|m| &m.resource != resource);
    }
}

impl Dispatch<ZwlrOutputHeadV1, String> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwlrOutputHeadV1,
        _request: zwlr_output_head_v1::Request,
        _data: &String,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ZwlrOutputModeV1, ModeData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwlrOutputModeV1,
        _request: zwlr_output_mode_v1::Request,
        _data: &ModeData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

/// The user data of a configuration object.
pub struct ConfigurationData {
    serial: u32,
    pending: Mutex<PendingConfiguration>,
}

#[derive(Default)]
struct PendingConfiguration {
    /// Each configured head, with its changes, or `None` to disable it.
    heads: Vec<(ZwlrOutputHeadV1, Option<Arc<Mutex<HeadChanges>>>)>,
    used: bool,
}

#[derive(Debug, Default)]
struct HeadChanges {
    mode: Option<RequestedMode>,
    position: Option<Point<i32, Logical>>,
    transform: Option<Transform>,
    scale: Option<f64>,
    adaptive_sync: Option<bool>,
}

#[derive(Clone, Copy, Debug)]
enum RequestedMode {
    Advertised(Mode),
    Custom { width: i32, height: i32, refresh: i32 },
}

impl HeadChanges {
    fn apply(&self, output: &Output, state: &mut OutputState) -> Result<(), OutputError> {
        state.enabled = true;
        match self.mode {
            Some(RequestedMode::Advertised(mode)) => state.mode = mode,
            Some(RequestedMode::Custom { width, height, refresh }) => {
                state.mode = find_mode(output, width, height, refresh).ok_or_else(|| {
                    OutputError::Invalid(format!(
                        "{} has no {width}x{height} mode at {refresh} mHz",
                        output.name()
                    ))
                })?;
            }
            None => {}
        }
        if let Some(position) = self.position {
            state.position = position;
        }
        if let Some(transform) = self.transform {
            state.transform = transform;
        }
        if let Some(scale) = self.scale {
            state.scale = round_scale(scale);
        }
        if self.adaptive_sync == Some(true) {
            return Err(OutputError::Unsupported("enable adaptive sync"));
        }
        Ok(())
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ConfigurationData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        data: &ConfigurationData,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_v1::{Error, Request};
        let mut pending = data.pending.lock().unwrap();
        let (head, changes) = match request {
            Request::EnableHead { id, head } => {
                let changes = Arc::new(Mutex::new(HeadChanges::default()));
                let name = head.data::<String>().cloned().unwrap_or_default();
                data_init.init(id, ConfigurationHeadData { head: name, changes: changes.clone() });
                (head, Some(changes))
            }
            Request::DisableHead { head } => (head, None),
            Request::Apply | Request::Test => {
                if std::mem::replace(&mut pending.used, true) {
                    resource.post_error(Error::AlreadyUsed, "the configuration was already used");
                    return;
                }
                let heads = std::mem::take(&mut pending.heads);
                drop(pending);
                let test = matches!(request, Request::Test);
                state.apply_output_configuration(resource, data.serial, &heads, test);
                return;
            }
            _ => return,
        };
        if pending.used {
            resource.post_error(Error::AlreadyUsed, "the configuration was already used");
        } else if pending.heads.iter().any(|(configured, _)| configured == &head) {
            resource.post_error(Error::AlreadyConfiguredHead, "the head is already configured");
        } else {
            pending.heads.push((head, changes));
        }
    }
}

/// The user data of a configuration head object.
pub struct ConfigurationHeadData {
    head: String,
    changes: Arc<Mutex<HeadChanges>>,
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ConfigurationHeadData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &ConfigurationHeadData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_head_v1::{Error, Request};
        let mut changes = data.changes.lock().unwrap();
        let already_set = match request {
            Request::SetMode { mode } => match mode.data::<ModeData>() {
                Some(mode) if mode.head == data.head => {
                    set_once(&mut changes.mode, RequestedMode::Advertised(mode.mode))
                }
                _ => {
                    resource.post_error(Error::InvalidMode, "the mode belongs to another head");
                    return;
                }
            },
            Request::SetCustomMode { width, height, refresh } => {
                if width <= 0 || height <= 0 || refresh < 0 {
                    resource.post_error(Error::InvalidCustomMode, "the custom mode is invalid");
                    return;
                }
                set_once(&mut changes.mode, RequestedMode::Custom { width, height, refresh })
            }
            Request::SetPosition { x, y } => set_once(&mut changes.position, (x, y).into()),
            Request::SetTransform { transform } => match transform_from_protocol(transform) {
                Some(transform) => set_once(&mut changes.transform, transform),
                None => {
                    resource.post_error(Error::InvalidTransform, "unknown transform");
                    return;
                }
            },
            Request::SetScale { scale } => {
                if scale.is_nan() || scale <= 0.0 {
                    resource.post_error(Error::InvalidScale, "the scale must be positive");
                    return;
                }
                set_once(&mut changes.scale, scale)
            }
            Request::SetAdaptiveSync { state } => match state {
                WEnum::Value(state) => {
                    set_once(&mut changes.adaptive_sync, state == AdaptiveSyncState::Enabled)
                }
                WEnum::Unknown(_) => {
                    resource
                        .post_error(Error::InvalidAdaptiveSyncState, "unknown adaptive sync state");
                    return;
                }
            },
            _ => false,
        };
        if already_set {
            resource.post_error(Error::AlreadySet, "the property was already set");
        }
    }
}

/// Stores `value` unless `slot` holds one; returns whether it did.
fn set_once<T>(slot: &mut Option<T>, value: T) -> bool {
    let already_set = slot.is_some();
    slot.get_or_insert(value);
    already_set
}

fn transform_from_protocol(transform: WEnum<wl_output::Transform>) -> Option<Transform> {
    use wl_output::Transform as T;
    Some(match transform.into_result().ok()? {
        T::Normal => Transform::Normal,
        T::_90 => Transform::_90,
        T::_180 => Transform::_180,
        T::_270 => Transform::_270,
        T::Flipped => Transform::Flipped,
        T::Flipped90 => Transform::Flipped90,
        T::Flipped180 => Transform::Flipped180,
        T::Flipped270 => Transform::Flipped270,
        _ => return None,
    })
}

impl State {
    fn apply_output_configuration(
        &mut self,
        configuration: &ZwlrOutputConfigurationV1,
        serial: u32,
        heads: &[(ZwlrOutputHeadV1, Option<Arc<Mutex<HeadChanges>>>)],
        test: bool,
    ) {
        if serial != self.nimbus.output_management.serial {
            configuration.cancelled();
            return;
        }
        let mut layout = Layout::new();
        for output in self.nimbus.heads().to_vec() {
            let name = output.name();
            let Some((_, changes)) = heads.iter().find(|(head, _)| head.data() == Some(&name))
            else {
                configuration.post_error(
                    zwlr_output_configuration_v1::Error::UnconfiguredHead,
                    format!("{name} isn't configured"),
                );
                return;
            };
            let mut state = self.nimbus.output_state(&output);
            let result = match changes {
                Some(changes) => changes.lock().unwrap().apply(&output, &mut state),
                None => {
                    state.enabled = false;
                    Ok(())
                }
            };
            if let Err(err) = result {
                tracing::info!("refusing a display configuration: {err}");
                configuration.failed();
                return;
            }
            layout.push((output, state));
        }
        match self.nimbus.configure_outputs(&mut self.backend, &layout, test) {
            Ok(()) => {
                if !test {
                    self.nimbus.save_outputs();
                }
                configuration.succeeded();
            }
            Err(err) => {
                tracing::info!("refusing a display configuration: {err}");
                configuration.failed();
            }
        }
    }
}
