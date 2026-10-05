// SPDX-License-Identifier: MIT

use super::{SessionLock, State};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::input::Seat;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Size};
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
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_surface_v1::ExtSessionLockSurfaceV1;
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
            self.nimbus.active_output().map_or(1.0, |o| o.current_scale().fractional_scale());
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
        // A new locker may replace one that died, but never a live one.
        if self.nimbus.is_session_locked()
            && self.nimbus.lock_owner.as_ref().is_some_and(|l| l.is_alive())
        {
            // Dropping the locker sends `finished`.
            tracing::warn!("refusing a second session lock");
            return;
        }
        self.nimbus.lock_surfaces.clear();
        self.nimbus.lock_owner = Some(confirmation.ext_session_lock().clone());
        self.nimbus.session_lock = SessionLock::Pending(confirmation);
        self.break_grabs_for_lock();
        self.nimbus.queue_redraw_all();
    }

    /// Only the confirmed lock holder gets here; see the `ExtSessionLockV1` dispatch below.
    fn unlock(&mut self) {
        self.nimbus.session_lock = SessionLock::Unlocked;
        self.nimbus.lock_owner = None;
        self.nimbus.lock_surfaces.clear();
        self.nimbus.queue_redraw_all();
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        let Some(output) = Output::from_resource(&output) else {
            return;
        };
        let owner = self.nimbus.lock_owner.as_ref().and_then(|l| l.client());
        if owner.is_none() || surface.wl_surface().client() != owner {
            tracing::warn!(
                "ignoring a lock surface from a client that doesn't hold the session lock"
            );
            return;
        }
        if let Some(geo) = self.nimbus.output_geometry(&output) {
            let size: Size<u32, Logical> =
                (u32::try_from(geo.size.w).unwrap_or(0), u32::try_from(geo.size.h).unwrap_or(0))
                    .into();
            surface.with_pending_state(|state| state.size = Some(size));
            surface.send_configure();
        }
        self.nimbus.lock_surfaces.insert(output.name(), surface);
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

/// Smithay lets any lock unlock the session and claim outputs, so requests from locks other than
/// the confirmed holder are refused here before they reach it.
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
        let owner = state.nimbus.lock_owner.as_ref() == Some(lock);
        match &request {
            ext_session_lock_v1::Request::UnlockAndDestroy
                if !(owner && matches!(state.nimbus.session_lock, SessionLock::Locked)) =>
            {
                tracing::warn!(
                    "refusing an unlock from a client that doesn't hold the session lock"
                );
                lock.post_error(
                    ext_session_lock_v1::Error::InvalidUnlock,
                    "this lock doesn't hold the session",
                );
                return;
            }
            ext_session_lock_v1::Request::GetLockSurface { .. } if !owner => {
                lock.post_error(
                    ext_session_lock_v1::Error::DuplicateOutput,
                    "another client holds the session lock",
                );
                return;
            }
            _ => {}
        }
        <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::request(
            state, client, lock, request, data, dh, data_init,
        );
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
    }
}
delegate_foreign_toplevel_list!(State);
