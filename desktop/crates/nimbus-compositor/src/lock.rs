// SPDX-License-Identifier: MIT

//! The session lock, which belongs to the compositor rather than to the client that draws the lock screen.
//!
//! A locked session stays locked when its ext-session-lock client dies,
//! and then accepts a new client, such as a restarted shell.
//! Only the holding client's `unlock_and_destroy` ends it.

use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::session_lock::{LockSurface, SessionLocker};
use std::collections::HashMap;

#[derive(Default)]
pub enum SessionLock {
    #[default]
    Unlocked,
    Locked(Option<LockClient>),
}

/// The ext-session-lock client holding the lock.
pub struct LockClient {
    lock: ExtSessionLockV1,
    /// Sends `locked` once every output rendered a locked frame; `None` after that.
    pending: Option<SessionLocker>,
    surfaces: HashMap<String, LockSurface>,
}

impl LockClient {
    pub fn surface(&self, output: &str) -> Option<&LockSurface> {
        self.surfaces.get(output)
    }

    pub fn surfaces(&self) -> impl Iterator<Item = &LockSurface> {
        self.surfaces.values()
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

    pub fn is_held_by(&self, lock: &ExtSessionLockV1) -> bool {
        self.client().is_some_and(|c| &c.lock == lock)
    }

    /// Locks without a client; returns whether the session was unlocked.
    pub fn lock(&mut self) -> bool {
        let unlocked = !self.is_locked();
        if unlocked {
            *self = Self::Locked(None);
        }
        unlocked
    }

    /// Hands the session to a new client unless a live one holds it.
    /// A refused locker is dropped, which sends it `finished`.
    pub fn accept(&mut self, locker: SessionLocker) -> bool {
        if self.client().is_some() {
            return false;
        }
        let lock = locker.ext_session_lock().clone();
        *self = Self::Locked(Some(LockClient {
            lock,
            pending: Some(locker),
            surfaces: HashMap::new(),
        }));
        true
    }

    pub fn unlock(&mut self) {
        *self = Self::Unlocked;
    }

    /// Records a lock surface; returns `false` unless its client holds the session.
    pub fn add_surface(&mut self, output: String, surface: LockSurface) -> bool {
        match self {
            Self::Locked(Some(client))
                if client.lock.is_alive()
                    && surface.wl_surface().client() == client.lock.client() =>
            {
                client.surfaces.insert(output, surface);
                true
            }
            _ => false,
        }
    }

    pub fn remove_output(&mut self, output: &str) {
        if let Self::Locked(Some(client)) = self {
            client.surfaces.remove(output);
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
            && let Some(locker) = client.pending.take()
        {
            locker.lock();
        }
    }

    pub fn awaits_confirmation(&self) -> bool {
        self.client().is_some_and(|c| c.pending.is_some())
    }
}
