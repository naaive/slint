// SPDX-License-Identifier: MIT

//! Displays: the connected heads, their configuration, and the layout stored in the configuration file.
//!
//! A head is a connected display's `Output`, and it's enabled while it's mapped in the window manager's space.
//! Every change of a head's configuration goes through [`Nimbus::configure_outputs`]:
//! the stored layout at startup and on hotplug, edits of the configuration file, and wlr-output-management.

pub mod edid;
pub mod layout;
mod management;

pub use management::OutputManagementState;

use crate::state::{Nimbus, State};
use crate::wm::layout as geometry;
use management::HeadSnapshot;
use nimbus_config::{Config, OutputConfig};
use smithay::desktop::layer_map_for_output;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::{Logical, Point, Transform};

/// What a backend knows about a newly connected display.
pub struct HeadDescription {
    pub name: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    /// In millimeters; zero when unknown.
    pub physical_size: (i32, i32),
    pub modes: Vec<Mode>,
    pub preferred: Mode,
    /// The transform that shows the picture upright, which a configuration entry leaves out.
    pub native_transform: Transform,
}

impl HeadDescription {
    /// A head that's disabled until [`Nimbus::configure_outputs`] enables it.
    pub fn into_output(self) -> Output {
        let output = Output::new(
            self.name,
            PhysicalProperties {
                size: self.physical_size.into(),
                subpixel: Subpixel::Unknown,
                make: self.make,
                model: self.model,
            },
        );
        for mode in self.modes {
            output.add_mode(mode);
        }
        output.set_preferred(self.preferred);
        output.change_current_state(Some(self.preferred), Some(self.native_transform), None, None);
        let info = HeadInfo { serial: self.serial, native_transform: self.native_transform };
        output.user_data().insert_if_missing_threadsafe(|| info);
        output
    }
}

/// What a head's `Output` doesn't keep.
#[derive(Clone, Debug, Default)]
struct HeadInfo {
    serial: String,
    native_transform: Transform,
}

fn head_info(output: &Output) -> HeadInfo {
    output.user_data().get::<HeadInfo>().cloned().unwrap_or_default()
}

/// The configuration of one head; a disabled head keeps the rest for when it's enabled again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputState {
    pub enabled: bool,
    pub mode: Mode,
    pub position: Point<i32, Logical>,
    pub transform: Transform,
    pub scale: f64,
}

/// A configuration for every connected head.
pub type Layout = Vec<(Output, OutputState)>;

#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error("{0}")]
    Invalid(String),
    #[error("this backend can't {0}")]
    Unsupported(&'static str),
    #[error("{0}")]
    Failed(String),
}

/// A backend's part in applying a layout.
pub trait OutputBackend {
    /// Sets up the devices for `layout`, which names every head, or only checks that it could with `test`.
    /// On failure, the devices are as they were.
    fn apply_outputs(
        &mut self,
        layout: &[(Output, OutputState)],
        test: bool,
    ) -> Result<(), OutputError>;
}

impl Nimbus {
    /// Every connected head, enabled or not.
    pub fn heads(&self) -> &[Output] {
        &self.heads
    }

    /// Adds a head made by [`HeadDescription::into_output`]; [`Nimbus::reconfigure_outputs`] then enables it.
    pub fn connect_head(&mut self, output: Output) {
        tracing::info!(name = %output.name(), description = %output.description(), "display connected");
        self.heads.push(output);
    }

    /// Removes a head; [`Nimbus::reconfigure_outputs`] then arranges the rest.
    pub fn disconnect_head(&mut self, output: &Output) {
        tracing::info!(name = %output.name(), "display disconnected");
        self.heads.retain(|o| o != output);
        if self.is_enabled(output) {
            self.unmap_output(output);
            self.outputs_changed(&[]);
        }
    }

    fn is_enabled(&self, output: &Output) -> bool {
        self.outputs().any(|o| o == output)
    }

    pub fn output_state(&self, output: &Output) -> OutputState {
        OutputState {
            enabled: self.is_enabled(output),
            mode: output
                .current_mode()
                .or_else(|| output.preferred_mode())
                .unwrap_or(Mode { size: (0, 0).into(), refresh: 0 }),
            position: output.current_location(),
            transform: output.current_transform(),
            scale: output.current_scale().fractional_scale(),
        }
    }

    /// The layout the configuration file asks for.
    pub fn configured_layout(&self) -> Layout {
        layout::resolve(&self.heads, self.config.current())
    }

    /// Validates `layout`, has `backend` apply or `test` it, and then applies it to the heads.
    pub fn configure_outputs(
        &mut self,
        backend: &mut impl OutputBackend,
        layout: &[(Output, OutputState)],
        test: bool,
    ) -> Result<(), OutputError> {
        layout::validate(layout)?;
        backend.apply_outputs(layout, test)?;
        if !test {
            self.apply_output_states(layout);
        }
        Ok(())
    }

    /// Applies the configured layout, or the defaults when the backend refuses it.
    pub fn reconfigure_outputs(&mut self, backend: &mut impl OutputBackend) {
        let configured = self.configured_layout();
        if let Err(err) = self.configure_outputs(backend, &configured, false) {
            tracing::warn!("cannot apply the stored display layout: {err}; using the defaults");
            let config = Config { outputs: Vec::new(), ..self.config.current().clone() };
            let defaults = layout::resolve(&self.heads, &config);
            if let Err(err) = self.configure_outputs(backend, &defaults, false) {
                tracing::error!("cannot apply the default display layout: {err}");
            }
        }
        self.refresh_output_management();
    }

    /// Records every head's configuration in the configuration file.
    pub fn save_outputs(&mut self) {
        let default_scale = self.config.current().appearance.scale;
        let entries: Vec<OutputConfig> = self
            .heads
            .iter()
            .map(|output| layout::entry(output, &self.output_state(output), default_scale))
            .collect();
        self.config.update(|config| {
            for entry in &entries {
                config.set_output(entry.clone());
            }
        });
    }

    /// Tells wlr-output-management clients what changed.
    pub fn refresh_output_management(&mut self) {
        let heads = self.heads.iter().map(|o| HeadSnapshot::new(o, self.output_state(o))).collect();
        self.output_management.update(&self.display_handle, heads);
    }

    fn apply_output_states(&mut self, layout: &[(Output, OutputState)]) {
        let mut moved = Vec::new();
        for (output, new) in layout {
            let old = self.output_state(output);
            if !new.enabled {
                if old.enabled {
                    self.unmap_output(output);
                }
                continue;
            }
            output.change_current_state(
                (old.mode != new.mode).then_some(new.mode),
                (old.transform != new.transform).then_some(new.transform),
                (old.scale != new.scale).then_some(Scale::Fractional(new.scale)),
                (old.position != new.position).then_some(new.position),
            );
            if !old.enabled {
                let global = output.create_global::<State>(&self.display_handle);
                self.output_globals.insert(output.name(), global);
                tracing::info!(name = %output.name(), "display enabled");
            } else if old.position != new.position {
                moved.push((output.name(), new.position - old.position));
            }
            self.wm.space.map_output(output, new.position);
            if self.pointer_location == Point::from((0.0, 0.0))
                && let Some(geometry) = self.wm.space.output_geometry(output)
            {
                self.pointer_location = geometry::center(geometry).into();
            }
        }
        self.outputs_changed(&moved);
    }

    /// Takes `output` out of the layout, closing its layer surfaces.
    fn unmap_output(&mut self, output: &Output) {
        self.wm.space.unmap_output(output);
        if let Some(global) = self.output_globals.remove(&output.name()) {
            self.display_handle.remove_global::<State>(global);
        }
        {
            let mut map = layer_map_for_output(output);
            for layer in map.layers().cloned().collect::<Vec<_>>() {
                layer.layer_surface().send_close();
                map.unmap_layer(&layer);
            }
        }
        self.lock.remove_output(&output.name());
        self.pending_redraws.remove(&output.name());
        tracing::info!(name = %output.name(), "display disabled");
    }
}
