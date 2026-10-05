// SPDX-License-Identifier: MIT

use super::State;
use smithay::delegate_layer_shell;
use smithay::desktop::{LayerSurface, PopupKind, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};
use smithay::wayland::shell::xdg::PopupSurface;

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.nimbus.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output =
            output.as_ref().and_then(Output::from_resource).or_else(|| self.nimbus.active_output());
        let Some(output) = output else {
            surface.send_close();
            return;
        };
        let mut map = layer_map_for_output(&output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!("cannot map layer surface: {err}");
        }
    }

    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        self.nimbus.unconstrain_popup(&popup);
        if let Err(err) = self.nimbus.popups.track_popup(PopupKind::from(popup)) {
            tracing::debug!("layer popup vanished before it was tracked: {err}");
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for output in self.nimbus.outputs().cloned().collect::<Vec<_>>() {
            let mut map = layer_map_for_output(&output);
            let layer = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        if self.nimbus.layer_focus.as_ref() == Some(surface.wl_surface()) {
            self.nimbus.layer_focus = None;
        }
        self.nimbus.arrange();
    }
}

delegate_layer_shell!(State);
