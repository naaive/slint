// SPDX-License-Identifier: MIT

use super::State;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::input::Seat;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::foreign_toplevel_list::{
    ForeignToplevelListHandler, ForeignToplevelListState,
};
use smithay::wayland::fractional_scale::{FractionalScaleHandler, with_fractional_scale};
use smithay::wayland::idle_inhibit::IdleInhibitHandler;
use smithay::wayland::idle_notify::{IdleNotifierHandler, IdleNotifierState};
use smithay::wayland::output::OutputHandler;
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_surface_v1::{
    self, ExtSessionLockSurfaceV1,
};
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::{
    self, ExtSessionLockV1,
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, delegate_dispatch, delegate_global_dispatch,
};
use smithay::wayland::session_lock::{
    ExtLockSurfaceUserData, LockSurface, SessionLockHandler, SessionLockManagerGlobalData,
    SessionLockManagerState, SessionLockState, SessionLocker,
};
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::{
    delegate_dmabuf, delegate_foreign_toplevel_list, delegate_fractional_scale,
    delegate_idle_inhibit, delegate_idle_notify, delegate_output, delegate_presentation,
    delegate_single_pixel_buffer, delegate_viewporter,
    delegate_xdg_activation,
};
use std::time::Duration;

/// How long an activation token stays valid.
const ACTIVATION_TOKEN_LIFETIME: Duration = Duration::from_secs(10);

impl OutputHandler for State {}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.nimbus.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        if self.backend.import_dmabuf(&dmabuf) {
            if let Err(err) = notifier.successful::<State>() {
                tracing::debug!("dmabuf client vanished during import: {err}");
            }
        } else {
            notifier.failed();
        }
    }
}

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.nimbus.xdg_activation_state
    }

    /// Accepts tokens requested in response to a recent input event while the client had keyboard focus.
    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        let Some((serial, seat)) = data.serial else {
            return false;
        };
        let Some(keyboard) = self.nimbus.keyboard.as_ref() else {
            return false;
        };
        Seat::<State>::from_resource(&seat).as_ref() == Some(&self.nimbus.seat)
            && keyboard.last_enter().is_some_and(|last_enter| serial.is_no_older_than(&last_enter))
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if token_data.timestamp.elapsed() < ACTIVATION_TOKEN_LIFETIME
            && let Some(id) = self.nimbus.wm.find_surface(&surface)
        {
            self.nimbus.wm.activate(id);
            self.nimbus.arrange();
        }
        self.nimbus.xdg_activation_state.remove_token(&token);
    }
}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale =
            self.nimbus.output_of(&surface).map_or(1.0, |o| o.current_scale().fractional_scale());
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
        });
    }
}

impl IdleNotifierHandler for State {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.nimbus.idle_notifier_state
    }
}

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        self.nimbus.idle_inhibitors.insert(surface);
        self.nimbus.refresh_idle_inhibit();
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.nimbus.idle_inhibitors.remove(&surface);
        self.nimbus.refresh_idle_inhibit();
    }
}

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.nimbus.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        if !self.nimbus.lock.accept(confirmation) {
            tracing::warn!("refusing a second session lock");
            return;
        }
        self.lock_changed();
    }

    /// Only the confirmed lock holder gets here; see the `ExtSessionLockV1` dispatch below.
    fn unlock(&mut self) {
        self.unlock_session();
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        let Some(output) = Output::from_resource(&output) else {
            return;
        };
        self.nimbus.lock.add_surface(output.name(), surface);
        self.nimbus.sync_lock_surfaces();
        self.nimbus.queue_redraw(&output);
    }
}

impl ForeignToplevelListHandler for State {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.nimbus.foreign_toplevel_state
    }
}

delegate_output!(State);
delegate_dmabuf!(State);
delegate_xdg_activation!(State);
delegate_presentation!(State);
delegate_viewporter!(State);
delegate_fractional_scale!(State);
delegate_single_pixel_buffer!(State);
delegate_idle_notify!(State);
delegate_idle_inhibit!(State);
delegate_global_dispatch!(State: [ExtSessionLockManagerV1: SessionLockManagerGlobalData] => SessionLockManagerState);
delegate_dispatch!(State: [ExtSessionLockManagerV1: ()] => SessionLockManagerState);
delegate_dispatch!(State: [ExtSessionLockSurfaceV1: ExtLockSurfaceUserData] => SessionLockManagerState);

/// Smithay lets any lock unlock the session and claim outputs,
/// so requests from locks other than the holder are handled here before they reach it.
impl Dispatch<ExtSessionLockV1, SessionLockState> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        lock: &ExtSessionLockV1,
        request: ext_session_lock_v1::Request,
        data: &SessionLockState,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let owner = state.nimbus.lock.is_held_by(lock);
        match request {
            ext_session_lock_v1::Request::UnlockAndDestroy
                if !owner || state.nimbus.lock.awaits_confirmation() =>
            {
                tracing::warn!(
                    "refusing an unlock from a client that doesn't hold the session lock"
                );
                lock.post_error(
                    ext_session_lock_v1::Error::InvalidUnlock,
                    "this lock doesn't hold the session",
                );
            }
            // The protocol lets clients create lock surfaces before `locked` or `finished`,
            // and a refused lock never displays them.
            ext_session_lock_v1::Request::GetLockSurface { id, .. } if !owner => {
                data_init.init(id, InertLockSurface);
            }
            request => {
                <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::request(
                    state, client, lock, request, data, dh, data_init,
                );
            }
        }
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        lock: &ExtSessionLockV1,
        data: &SessionLockState,
    ) {
        <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::destroyed(
            state, client, lock, data,
        );
        if state.nimbus.lock.release(lock) {
            tracing::warn!("the session lock client went away; the session stays locked");
            state.lock_changed();
        }
    }
}

/// A lock surface of a lock the compositor refused.
pub struct InertLockSurface;

impl Dispatch<ExtSessionLockSurfaceV1, InertLockSurface> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _surface: &ExtSessionLockSurfaceV1,
        _request: ext_session_lock_surface_v1::Request,
        _data: &InertLockSurface,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}
delegate_foreign_toplevel_list!(State);
