// SPDX-License-Identifier: MIT

use super::{ClientState, State};
use crate::wm::grabs;
use smithay::backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state};
use smithay::desktop::{PopupKind, WindowSurfaceType, layer_map_for_output};
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
    with_states,
};
use smithay::wayland::shell::wlr_layer::LayerSurfaceData;
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{delegate_compositor, delegate_shm};

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.nimbus.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        match client.get_data::<ClientState>() {
            Some(state) => &state.compositor_state,
            // Every client is inserted with `ClientState`; this only guards against misuse.
            None => {
                static FALLBACK: std::sync::OnceLock<CompositorClientState> =
                    std::sync::OnceLock::new();
                FALLBACK.get_or_init(CompositorClientState::default)
            }
        }
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.backend.early_import(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(id) = self.nimbus.wm.find_surface(&root)
                && let Some(w) = self.nimbus.wm.get(id)
            {
                w.window.on_commit();
            }
        }
        self.nimbus.popups.commit(surface);

        self.handle_toplevel_commit(surface);
        self.handle_layer_commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.nimbus.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
            && let Err(err) = popup.send_configure()
        {
            tracing::debug!("cannot configure popup: {err}");
        }
        if let Some(PopupKind::InputMethod(popup)) = self.nimbus.popups.find_popup(surface) {
            self.nimbus.place_input_method_popup(&popup);
        }
        self.nimbus.queue_redraw_for_surface(surface);
    }
}

impl State {
    fn handle_toplevel_commit(&mut self, surface: &WlSurface) {
        let Some(id) = self.nimbus.wm.find_surface(surface) else {
            return;
        };
        let Some(w) = self.nimbus.wm.get(id) else {
            return;
        };
        let Some(toplevel) = w.toplevel().cloned() else {
            return;
        };
        if !w.initial_commit {
            let output = self.nimbus.active_output().map(|o| o.name());
            self.nimbus.wm.initial_commit(id, output);
            self.nimbus.wm.refresh_metadata(id);
            self.nimbus.arrange();
            if !toplevel.is_initial_configure_sent() {
                toplevel.send_configure();
            }
            return;
        }

        let has_buffer =
            with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
        let mapped = w.mapped;
        if has_buffer && !mapped {
            let areas = self.nimbus.output_areas();
            self.nimbus.wm.refresh_metadata(id);
            self.nimbus.wm.map(id, &areas);
            let handle = self.nimbus.wm.get(id).map(|w| (w.title.clone(), w.app_id.clone())).map(
                |(title, app_id)| {
                    self.nimbus.foreign_toplevel_state.new_toplevel::<State>(title, app_id)
                },
            );
            if let Some(w) = self.nimbus.wm.get_mut(id) {
                w.foreign = handle;
            }
            self.nimbus.arrange();
            return;
        }
        if !has_buffer && mapped {
            self.nimbus.wm.unmap(id);
            if let Some(handle) = self.nimbus.wm.get_mut(id).and_then(|w| w.foreign.take()) {
                self.nimbus.foreign_toplevel_state.remove_toplevel(&handle);
            }
            toplevel.reset_initial_configure_sent();
            self.nimbus.arrange();
            return;
        }

        if let Some(resize) = self.nimbus.wm.get(id).and_then(|w| w.resize) {
            let size = self.nimbus.wm.get(id).map(|w| w.window.geometry().size).unwrap_or_default();
            let location = grabs::resized_location(resize.edges, resize.initial, size);
            self.nimbus.wm.move_floating(id, location);
            if resize.finishing
                && !crate::wm::configure_pending(&toplevel)
                && let Some(w) = self.nimbus.wm.get_mut(id)
            {
                w.resize = None;
            }
        }
        self.nimbus.wm.track_floating_size(id);
        self.nimbus.wm.refresh_metadata(id);
    }

    fn handle_layer_commit(&mut self, surface: &WlSurface) {
        let Some(output) = self
            .nimbus
            .outputs()
            .find(|o| {
                layer_map_for_output(o)
                    .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned()
        else {
            return;
        };
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .and_then(|data| data.lock().ok())
                .is_some_and(|data| data.initial_configure_sent)
        });
        {
            let mut map = layer_map_for_output(&output);
            map.arrange();
            if !initial_configure_sent
                && let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            {
                layer.layer_surface().send_configure();
            }
        }
        self.nimbus.arrange();
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.nimbus.shm_state
    }
}

delegate_compositor!(State);
delegate_shm!(State);
