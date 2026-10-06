// SPDX-License-Identifier: MIT

//! Locking through `ext-session-lock-v1`: a lock screen on a lock surface per output, and PAM to unlock.
//!
//! The compositor keeps the session locked when the shell dies, and accepts a new lock from a restarted shell.

use crate::auth::{AuthWorker, PamAuthenticator};
use crate::state::State;
use crate::surface::SlintSurface;
use anyhow::anyhow;
use nimbus_services::ServiceCommand;
use nimbus_shell::LockView;
use smithay_client_toolkit::reexports::calloop::LoopHandle;
use smithay_client_toolkit::reexports::calloop::channel::{self, Event as ChannelEvent};
use smithay_client_toolkit::session_lock::{SessionLock, SessionLockSurface};
use wayland_client::protocol::wl_output::WlOutput;

/// A lock screen on the lock surface of one output.
pub struct LockSurface {
    pub output: WlOutput,
    _view: LockView,
    pub surface: SlintSurface,
    role: SessionLockSurface,
}

impl LockSurface {
    pub fn is(&self, role: &SessionLockSurface) -> bool {
        self.role.wl_surface() == role.wl_surface()
    }
}

#[derive(Default)]
pub struct Lock {
    session: Option<SessionLock>,
    pub surfaces: Vec<LockSurface>,
    /// An unlock arrived before the compositor confirmed the lock.
    unlock_when_locked: bool,
    /// logind waits for [`ServiceCommand::LockPresented`] once the compositor confirms the lock.
    pub report_presented: bool,
}

impl Lock {
    /// Whether the compositor confirmed that it shows nothing but lock surfaces.
    fn confirmed(&self) -> bool {
        self.session.as_ref().is_some_and(SessionLock::is_locked)
    }
}

/// Starts the PAM worker; its answers reach [`State::unlock_result`] on the event loop.
pub fn spawn_auth(handle: &LoopHandle<'static, State>) -> anyhow::Result<Option<AuthWorker>> {
    let (sender, receiver) = channel::channel::<bool>();
    handle
        .insert_source(receiver, |event, _, state| {
            if let ChannelEvent::Msg(ok) = event {
                state.unlock_result(ok);
            }
        })
        .map_err(|e| anyhow!("cannot receive authentication results: {e}"))?;
    let service = crate::auth::service_name();
    let worker = AuthWorker::spawn(
        Box::new(PamAuthenticator::new(service)),
        crate::auth::current_user(),
        move |ok| {
            let _ = sender.send(ok);
        },
    );
    match worker {
        Ok(worker) => {
            tracing::info!(service, "lock screen authentication uses PAM");
            Ok(Some(worker))
        }
        Err(err) => {
            tracing::error!("cannot start lock screen authentication: {err}");
            Ok(None)
        }
    }
}

impl State {
    /// Locks the session, unless the shell holds the lock already; lock screens follow once the compositor confirms.
    pub fn lock(&mut self) {
        if self.lock.session.is_some() {
            return;
        }
        let session = match self.session_lock_state.lock(&self.qh) {
            Ok(session) => session,
            Err(err) => {
                tracing::error!("cannot lock the session: {err}");
                return;
            }
        };
        tracing::info!("locking");
        self.lock.session = Some(session);
        self.model.set_locked(true);
    }

    /// Shows a lock screen when the session is locked without one,
    /// as after `Request::Lock` or when the lock screen client, such as an earlier shell, died.
    pub fn lock_state(&mut self, locked: bool, held: bool) {
        if locked && !held {
            self.lock();
        }
    }

    /// Adds a lock screen on `output` while the shell holds the lock.
    pub fn add_lock_surface(&mut self, output: &WlOutput) {
        // The compositor rejects lock surfaces of a lock it refused, so they wait for its confirmation.
        let Some(session) = self.lock.session.as_ref().filter(|s| s.is_locked()) else {
            return;
        };
        let wl_surface = self.compositor.create_surface(&self.qh);
        let created = self.windows.create(&wl_surface, || LockView::new(&self.model)).and_then(
            |(view, renderer)| {
                view.show()?;
                Ok((view, renderer))
            },
        );
        let (view, renderer) = match created {
            Ok(created) => created,
            Err(err) => {
                tracing::error!("cannot show the lock screen: {err}");
                wl_surface.destroy();
                return;
            }
        };
        let role = session.create_lock_surface(wl_surface.clone(), output, &self.qh);
        let scale = self.output_scale(output);
        let surface = SlintSurface::new(wl_surface, renderer, &self.scaling, scale, &self.qh);
        self.lock.surfaces.push(LockSurface { output: output.clone(), _view: view, surface, role });
    }

    pub fn remove_lock_surface(&mut self, output: &WlOutput) {
        self.lock.surfaces.retain(|s| &s.output != output);
    }

    /// Ends the lock, unless the compositor hasn't confirmed it yet; then it ends once confirmed.
    pub fn unlock(&mut self) {
        let Some(session) = &self.lock.session else {
            return;
        };
        if !session.is_locked() {
            self.lock.unlock_when_locked = true;
            return;
        }
        tracing::info!("unlocked");
        session.unlock();
        self.lock = Lock::default();
        self.model.set_locked(false);
    }

    pub fn unlock_result(&mut self, ok: bool) {
        if ok {
            self.unlock();
        } else {
            self.model.unlock_failed();
        }
    }

    /// The compositor confirmed the lock.
    pub fn lock_confirmed(&mut self) {
        tracing::info!("locked");
        if std::mem::take(&mut self.lock.unlock_when_locked) {
            self.unlock();
            return;
        }
        let outputs: Vec<WlOutput> = self.outputs.iter().map(|o| o.output().clone()).collect();
        for output in outputs {
            self.add_lock_surface(&output);
        }
        self.report_lock_presented();
    }

    /// The compositor refused the lock or ended it.
    pub fn lock_finished(&mut self) {
        if self.lock.confirmed() {
            tracing::warn!("the compositor ended the lock");
        } else {
            tracing::warn!("the compositor refused to lock; another client holds the lock");
            if self.lock.report_presented {
                // That client's lock screen covers the outputs.
                self.services.send(ServiceCommand::LockPresented);
            }
        }
        self.lock = Lock::default();
        self.model.set_locked(false);
    }

    /// Tells logind the screen is locked, if it asked and the compositor confirmed the lock.
    pub fn report_lock_presented(&mut self) {
        if self.lock.report_presented && self.lock.confirmed() {
            self.lock.report_presented = false;
            self.services.send(ServiceCommand::LockPresented);
        }
    }
}
