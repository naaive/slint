// SPDX-License-Identifier: MIT

use super::State;
use crate::lock::LockSurfaceData;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::input::Seat;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::{
    ext_session_lock_manager_v1::{self, ExtSessionLockManagerV1},
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
};
use smithay::wayland::compositor::with_states;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::foreign_toplevel_list::{
    ForeignToplevelListHandler, ForeignToplevelListState,
};
use smithay::wayland::fractional_scale::{FractionalScaleHandler, with_fractional_scale};
use smithay::wayland::idle_inhibit::IdleInhibitHandler;
use smithay::wayland::idle_notify::{IdleNotifierHandler, IdleNotifierState};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::{
    delegate_dmabuf, delegate_foreign_toplevel_list, delegate_fractional_scale,
    delegate_idle_inhibit, delegate_idle_notify, delegate_output, delegate_presentation,
    delegate_single_pixel_buffer, delegate_viewporter, delegate_xdg_activation,
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
delegate_foreign_toplevel_list!(State);

impl GlobalDispatch<ExtSessionLockManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<ExtSessionLockManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(manager, ());
    }
}

impl Dispatch<ExtSessionLockManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ExtSessionLockManagerV1,
        request: ext_session_lock_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_session_lock_manager_v1::Request::Lock { id } = request {
            if state.nimbus.lock.accept(data_init.init(id, ())) {
                state.lock_changed();
            } else {
                tracing::warn!("refusing a second session lock");
            }
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        lock: &ExtSessionLockV1,
        request: ext_session_lock_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_session_lock_v1::Request::GetLockSurface { id, surface, output } => {
                let output = Output::from_resource(&output);
                let (Some(holder), Some(output)) = (state.nimbus.lock.holder(lock), output) else {
                    data_init.init(id, LockSurfaceData::default());
                    return;
                };
                let resource = data_init.init(id, LockSurfaceData::active());
                if let Err((error, message)) = holder.add_surface(output.name(), surface, resource)
                {
                    lock.post_error(error, message);
                    return;
                }
                state.nimbus.sync_lock_surfaces();
                state.nimbus.queue_redraw(&output);
            }
            ext_session_lock_v1::Request::UnlockAndDestroy => {
                if state.nimbus.lock.is_confirmed_holder(lock) {
                    state.unlock_session();
                } else {
                    tracing::warn!("refusing an unlock from a lock that doesn't hold the session");
                    lock.post_error(
                        ext_session_lock_v1::Error::InvalidUnlock,
                        "this lock doesn't hold the session",
                    );
                }
            }
            ext_session_lock_v1::Request::Destroy
                if state.nimbus.lock.is_confirmed_holder(lock) =>
            {
                lock.post_error(
                    ext_session_lock_v1::Error::InvalidDestroy,
                    "a lock that was sent 'locked' must use unlock_and_destroy",
                );
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, lock: &ExtSessionLockV1, _data: &()) {
        if state.nimbus.lock.release(lock) {
            tracing::warn!("the session lock client went away; the session stays locked");
            state.lock_changed();
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, LockSurfaceData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        surface: &ExtSessionLockSurfaceV1,
        request: ext_session_lock_surface_v1::Request,
        data: &LockSurfaceData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_session_lock_surface_v1::Request::AckConfigure { serial } = request
            && !data.ack(serial.into())
        {
            surface.post_error(
                ext_session_lock_surface_v1::Error::InvalidSerial,
                format!("no configure waits for serial {serial}"),
            );
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        surface: &ExtSessionLockSurfaceV1,
        _data: &LockSurfaceData,
    ) {
        if state.nimbus.lock.remove_surface(surface) {
            state.nimbus.queue_redraw_all();
        }
    }
}
