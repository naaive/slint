// SPDX-License-Identifier: MIT

//! The session lock, which belongs to the compositor rather than to the client that draws the lock screen.
//!
//! A locked session stays locked when its ext-session-lock client dies,
//! and then accepts a new client, such as a restarted shell.
//! Only the holding client's `unlock_and_destroy` ends it.

use crate::state::State;
use smithay::backend::renderer::buffer_dimensions;
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_surface_v1::{
    self, ExtSessionLockSurfaceV1,
};
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::{
    self, ExtSessionLockV1,
};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, SERIAL_COUNTER, Serial, Size};
use smithay::wayland::compositor::{
    self, BufferAssignment, SurfaceAttributes, add_pre_commit_hook, with_states,
};
use smithay::wayland::viewporter::ViewportCachedState;
use std::collections::HashMap;
use std::sync::Mutex;

const ROLE: &str = "ext_session_lock_surface_v1";

#[derive(Default)]
pub enum SessionLock {
    #[default]
    Unlocked,
    Locked(Option<LockClient>),
}

/// The ext-session-lock client holding the lock, with its lock surfaces by output name.
pub struct LockClient {
    lock: ExtSessionLockV1,
    confirmed: bool,
    surfaces: HashMap<String, LockSurface>,
}

impl LockClient {
    pub fn surface(&self, output: &str) -> Option<&LockSurface> {
        self.surfaces.get(output)
    }

    pub fn surfaces(&self) -> impl Iterator<Item = &LockSurface> {
        self.surfaces.values()
    }

    /// The output that `surface`, a lock surface, covers.
    pub fn output_of(&self, surface: &WlSurface) -> Option<&str> {
        self.surfaces.iter().find(|(_, s)| s.wl_surface() == surface).map(|(name, _)| name.as_str())
    }

    /// Gives `surface` the lock surface role on `output`, as `resource`,
    /// unless the protocol forbids it.
    pub fn add_surface(
        &mut self,
        output: String,
        surface: WlSurface,
        resource: ExtSessionLockSurfaceV1,
    ) -> Result<(), (ext_session_lock_v1::Error, &'static str)> {
        use ext_session_lock_v1::Error;
        if compositor::give_role(&surface, ROLE).is_err() {
            return Err((Error::Role, "the surface already has another role"));
        }
        if self.surfaces.contains_key(&output) {
            return Err((Error::DuplicateOutput, "the output already has a lock surface"));
        }
        if has_buffer(&surface) {
            return Err((Error::AlreadyConstructed, "the surface already has a buffer"));
        }
        let first_use = with_states(&surface, |states| {
            let first_use =
                states.data_map.insert_if_missing_threadsafe(|| Mutex::new(resource.clone()));
            let current = states.data_map.get::<Mutex<ExtSessionLockSurfaceV1>>().unwrap();
            *current.lock().unwrap() = resource.clone();
            first_use
        });
        if first_use {
            add_pre_commit_hook::<State, _>(&surface, |_, _, surface| check_commit(surface));
        }
        self.surfaces.insert(output, LockSurface { surface, resource });
        Ok(())
    }
}

impl SessionLock {
    pub fn is_locked(&self) -> bool {
        matches!(self, Self::Locked(_))
    }

    /// The live client holding the lock.
    pub fn client(&self) -> Option<&LockClient> {
        match self {
            Self::Locked(Some(client)) if client.lock.is_alive() => Some(client),
            _ => None,
        }
    }

    /// The client holding the lock, if it's `lock`.
    pub fn holder(&mut self, lock: &ExtSessionLockV1) -> Option<&mut LockClient> {
        match self {
            Self::Locked(Some(client)) if &client.lock == lock => Some(client),
            _ => None,
        }
    }

    /// Whether `lock` holds the session and was sent `locked`.
    pub fn is_confirmed_holder(&self, lock: &ExtSessionLockV1) -> bool {
        self.client().is_some_and(|c| &c.lock == lock && c.confirmed)
    }

    /// Locks without a client; returns whether the session was unlocked.
    pub fn lock(&mut self) -> bool {
        let unlocked = !self.is_locked();
        if unlocked {
            *self = Self::Locked(None);
        }
        unlocked
    }

    /// Hands the session to `lock` unless a live client holds it, and sends a refused lock `finished`.
    pub fn accept(&mut self, lock: ExtSessionLockV1) -> bool {
        if self.client().is_some() {
            lock.finished();
            return false;
        }
        *self = Self::Locked(Some(LockClient { lock, confirmed: false, surfaces: HashMap::new() }));
        true
    }

    pub fn unlock(&mut self) {
        *self = Self::Unlocked;
    }

    pub fn remove_output(&mut self, output: &str) {
        if let Self::Locked(Some(client)) = self {
            client.surfaces.remove(output);
        }
    }

    /// Forgets the holder's lock surface `resource`; returns whether it was one.
    pub fn remove_surface(&mut self, resource: &ExtSessionLockSurfaceV1) -> bool {
        match self {
            Self::Locked(Some(client)) => {
                let before = client.surfaces.len();
                client.surfaces.retain(|_, s| &s.resource != resource);
                client.surfaces.len() != before
            }
            _ => false,
        }
    }

    /// Forgets `lock` if it held the session, which stays locked; returns whether it did.
    pub fn release(&mut self, lock: &ExtSessionLockV1) -> bool {
        match self {
            Self::Locked(client @ Some(_)) if client.as_ref().is_some_and(|c| &c.lock == lock) => {
                *client = None;
                true
            }
            _ => false,
        }
    }

    /// Sends `locked` to a client still waiting for it.
    pub fn confirm(&mut self) {
        if let Self::Locked(Some(client)) = self
            && !client.confirmed
            && client.lock.is_alive()
        {
            client.lock.locked();
            client.confirmed = true;
        }
    }
}

/// A lock surface of the lock holder.
///
/// Its `wl_surface` keeps the latest `ExtSessionLockSurfaceV1` in its data map for [`check_commit`];
/// the protocol lets a later lock reuse a `wl_surface` that never had a buffer.
#[derive(Clone)]
pub struct LockSurface {
    surface: WlSurface,
    resource: ExtSessionLockSurfaceV1,
}

impl LockSurface {
    pub fn wl_surface(&self) -> &WlSurface {
        &self.surface
    }

    /// Sends a configure with `size`, unless the last one had it.
    pub fn configure(&self, size: Size<u32, Logical>) {
        let Some(mut configures) = configures(&self.resource) else {
            return;
        };
        if configures.pending.last().map(|&(_, size)| size).or(configures.acked) == Some(size) {
            return;
        }
        let serial = SERIAL_COUNTER.next_serial();
        configures.pending.push((serial, size));
        self.resource.configure(serial.into(), size.w, size.h);
    }
}

/// The user data of an `ExtSessionLockSurfaceV1`: its configures,
/// or `None` for an inert one, made by a lock that doesn't hold the session.
#[derive(Default)]
pub struct LockSurfaceData(Option<Mutex<Configures>>);

impl LockSurfaceData {
    pub fn active() -> Self {
        Self(Some(Mutex::default()))
    }

    /// Handles `ack_configure`; returns whether `serial` names a configure still waiting for it.
    pub fn ack(&self, serial: Serial) -> bool {
        let Some(mut configures) = self.0.as_ref().map(|c| c.lock().unwrap()) else {
            return true;
        };
        let Some(index) = configures.pending.iter().position(|&(s, _)| s == serial) else {
            return false;
        };
        configures.acked = Some(configures.pending[index].1);
        configures.pending.drain(..=index);
        true
    }
}

#[derive(Default)]
struct Configures {
    /// Sent and not yet acknowledged, oldest first.
    pending: Vec<(Serial, Size<u32, Logical>)>,
    acked: Option<Size<u32, Logical>>,
}

fn configures(resource: &ExtSessionLockSurfaceV1) -> Option<std::sync::MutexGuard<'_, Configures>> {
    resource.data::<LockSurfaceData>()?.0.as_ref().map(|c| c.lock().unwrap())
}

fn has_buffer(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        let mut attributes = states.cached_state.get::<SurfaceAttributes>();
        let new_buffer =
            |buffer: &Option<_>| matches!(buffer, Some(BufferAssignment::NewBuffer(_)));
        new_buffer(&attributes.pending().buffer) || new_buffer(&attributes.current().buffer)
    })
}

/// Rejects a commit of a lock surface before its first `ack_configure`,
/// with a null buffer, or with a buffer of another size than the acknowledged one.
fn check_commit(surface: &WlSurface) {
    use ext_session_lock_surface_v1::Error;
    with_states(surface, |states| {
        let resource = states.data_map.get::<Mutex<ExtSessionLockSurfaceV1>>().unwrap();
        let resource = resource.lock().unwrap();
        let Some(configures) = configures(&resource).filter(|_| resource.is_alive()) else {
            return;
        };
        let Some(acked) = configures.acked else {
            resource.post_error(
                Error::CommitBeforeFirstAck,
                "committed before the first ack_configure",
            );
            return;
        };
        let mut guard = states.cached_state.get::<SurfaceAttributes>();
        let attributes = guard.pending();
        let buffer = match &attributes.buffer {
            Some(BufferAssignment::NewBuffer(buffer)) => buffer,
            Some(BufferAssignment::Removed) => {
                resource.post_error(Error::NullBuffer, "committed a null buffer");
                return;
            }
            None => return,
        };
        let viewport = states.cached_state.get::<ViewportCachedState>().pending().dst;
        let size = viewport.or_else(|| {
            let transform = attributes.buffer_transform.into();
            Some(buffer_dimensions(buffer)?.to_logical(attributes.buffer_scale, transform))
        });
        let size =
            size.map(|s| Size::from((s.w.try_into().unwrap_or(0), s.h.try_into().unwrap_or(0))));
        if size.is_some_and(|size| size != acked) {
            resource.post_error(
                Error::DimensionsMismatch,
                format!("the buffer's size isn't the acknowledged {}x{}", acked.w, acked.h),
            );
        }
    });
}
