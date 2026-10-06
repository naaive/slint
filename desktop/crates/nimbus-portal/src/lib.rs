// SPDX-License-Identifier: MIT

//! An `xdg-desktop-portal` backend for `org.freedesktop.impl.portal.Settings`.
//!
//! It publishes the `org.freedesktop.appearance` namespace from the Nimbus configuration,
//! so apps that ask the portal for the color scheme, accent color, or contrast follow Nimbus.
//! It watches the configuration file and emits `SettingChanged` for each key whose value changes.

use std::collections::HashMap;
use std::path::Path;

use nimbus_config::{Appearance, ColorScheme, Config};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Value;
use zbus::{Connection, interface};

/// The well-known name that `data/nimbus.portal` and the D-Bus service file refer to.
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.nimbus";
/// The object path at which `xdg-desktop-portal` calls every backend.
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
pub const NAMESPACE: &str = "org.freedesktop.appearance";

/// Values of the `color-scheme` key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ColorSchemePreference {
    NoPreference = 0,
    PreferDark = 1,
    PreferLight = 2,
}

/// The `org.freedesktop.appearance` settings derived from the configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppearanceSettings {
    pub color_scheme: ColorSchemePreference,
    /// sRGB, each channel in `0.0..=1.0`.
    pub accent_color: (f64, f64, f64),
}

impl AppearanceSettings {
    /// `ColorScheme::System` has no preference, since Nimbus resolves it by asking this portal.
    /// An accent that isn't `#rrggbb` or `#rgb` becomes the default accent, as in `nimbus-theme`.
    pub fn from_config(appearance: &Appearance) -> Self {
        let color_scheme = match appearance.color_scheme {
            ColorScheme::Dark => ColorSchemePreference::PreferDark,
            ColorScheme::Light => ColorSchemePreference::PreferLight,
            ColorScheme::System => ColorSchemePreference::NoPreference,
        };
        let (red, green, blue) = nimbus_theme::parse_hex_color(&appearance.accent)
            .unwrap_or(nimbus_theme::DEFAULT_ACCENT);
        let channel = |value: u8| f64::from(value) / 255.0;
        Self { color_scheme, accent_color: (channel(red), channel(green), channel(blue)) }
    }

    /// Every key with its value, in a fixed order.
    pub fn entries(&self) -> [(&'static str, Value<'static>); 3] {
        [
            ("color-scheme", Value::from(self.color_scheme as u32)),
            ("accent-color", Value::from(self.accent_color)),
            ("contrast", Value::from(0u32)),
        ]
    }

    pub fn get(&self, key: &str) -> Option<Value<'static>> {
        self.entries().into_iter().find_map(|(k, value)| (k == key).then_some(value))
    }

    /// The keys whose values differ from `previous`, with their new values.
    pub fn changes_since(&self, previous: &Self) -> Vec<(&'static str, Value<'static>)> {
        self.entries()
            .into_iter()
            .zip(previous.entries())
            .filter(|(new, old)| new != old)
            .map(|(new, _)| new)
            .collect()
    }
}

/// Whether a `ReadAll` filter selects `namespace`.
///
/// The portal specification allows only a trailing `*` as a glob, and an empty filter selects everything.
fn namespace_matches(filter: &str, namespace: &str) -> bool {
    filter.is_empty()
        || filter == namespace
        || filter.strip_suffix('*').is_some_and(|prefix| namespace.starts_with(prefix))
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.portal.Error")]
pub enum PortalError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NotFound(String),
}

struct SettingsPortal {
    appearance: AppearanceSettings,
}

#[interface(name = "org.freedesktop.impl.portal.Settings")]
impl SettingsPortal {
    async fn read_all(
        &self,
        namespaces: Vec<String>,
    ) -> HashMap<String, HashMap<String, Value<'static>>> {
        let selected = namespaces.is_empty()
            || namespaces.iter().any(|filter| namespace_matches(filter, NAMESPACE));
        if !selected {
            return HashMap::new();
        }
        let settings =
            self.appearance.entries().into_iter().map(|(key, value)| (key.to_owned(), value));
        HashMap::from([(NAMESPACE.to_owned(), settings.collect())])
    }

    async fn read(&self, namespace: &str, key: &str) -> Result<Value<'static>, PortalError> {
        (namespace == NAMESPACE)
            .then(|| self.appearance.get(key))
            .flatten()
            .ok_or_else(|| PortalError::NotFound(format!("no setting {namespace} {key}")))
    }

    #[zbus(signal)]
    async fn setting_changed(
        emitter: &SignalEmitter<'_>,
        namespace: &str,
        key: &str,
        value: Value<'_>,
    ) -> zbus::Result<()>;

    #[zbus(property(emits_changed_signal = "const"), name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

/// The running portal; dropping it stops watching the configuration.
pub struct Portal {
    _watcher: nimbus_config::ConfigWatcher,
    updates: JoinHandle<()>,
}

impl Drop for Portal {
    fn drop(&mut self) {
        self.updates.abort();
    }
}

/// Serves the settings from `config_path` on `connection` and claims [`BUS_NAME`].
///
/// Must run inside a Tokio runtime.
/// A missing or invalid configuration file serves the defaults.
pub async fn serve(connection: &Connection, config_path: &Path) -> anyhow::Result<Portal> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    // Watching before loading means a change in between is reported rather than lost.
    let watcher = nimbus_config::watch(config_path, move |config| {
        let _ = sender.send(config);
    })?;
    let config = Config::load_from_or_default(config_path);
    let portal = SettingsPortal { appearance: AppearanceSettings::from_config(&config.appearance) };
    connection.object_server().at(OBJECT_PATH, portal).await?;
    connection.request_name(BUS_NAME).await?;

    let connection = connection.clone();
    let updates = tokio::spawn(async move {
        while let Some(config) = receiver.recv().await {
            if let Err(error) = apply(&connection, &config).await {
                tracing::warn!("publishing the appearance settings failed: {error}");
            }
        }
    });
    Ok(Portal { _watcher: watcher, updates })
}

async fn apply(connection: &Connection, config: &Config) -> zbus::Result<()> {
    let portal = connection.object_server().interface::<_, SettingsPortal>(OBJECT_PATH).await?;
    let appearance = AppearanceSettings::from_config(&config.appearance);
    let previous = std::mem::replace(&mut portal.get_mut().await.appearance, appearance);
    for (key, value) in appearance.changes_since(&previous) {
        SettingsPortal::setting_changed(portal.signal_emitter(), NAMESPACE, key, value).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn appearance(color_scheme: ColorScheme, accent: &str) -> Appearance {
        Appearance { color_scheme, accent: accent.into(), ..Appearance::default() }
    }

    #[test]
    fn color_scheme_maps_to_preference() {
        let scheme =
            |scheme| AppearanceSettings::from_config(&appearance(scheme, "#000")).color_scheme;
        assert_eq!(scheme(ColorScheme::Dark), ColorSchemePreference::PreferDark);
        assert_eq!(scheme(ColorScheme::Light), ColorSchemePreference::PreferLight);
        assert_eq!(scheme(ColorScheme::System), ColorSchemePreference::NoPreference);
    }

    #[test]
    fn accent_maps_to_unit_srgb() {
        let accent = |text| {
            AppearanceSettings::from_config(&appearance(ColorScheme::Dark, text)).accent_color
        };
        assert_eq!(accent("#ff0033"), (1.0, 0.0, 0.2));
        assert_eq!(accent("f03"), (1.0, 0.0, 0.2));
        assert_eq!(accent(" #FFFFFF "), (1.0, 1.0, 1.0));
    }

    #[test]
    fn invalid_accent_falls_back_to_default() {
        let default = AppearanceSettings::from_config(&Appearance::default()).accent_color;
        assert_eq!(default, (0x35 as f64 / 255.0, 0x84 as f64 / 255.0, 0xe4 as f64 / 255.0));
        for text in ["", "blue", "#12345", "+12345", "#ggg"] {
            let settings = AppearanceSettings::from_config(&appearance(ColorScheme::Dark, text));
            assert_eq!(settings.accent_color, default, "{text:?}");
        }
    }

    #[test]
    fn contrast_has_no_preference() {
        let settings = AppearanceSettings::from_config(&Appearance::default());
        assert_eq!(settings.get("contrast"), Some(Value::from(0u32)));
    }

    #[test]
    fn get_returns_portal_types() {
        let settings = AppearanceSettings::from_config(&appearance(ColorScheme::Light, "#ff0033"));
        assert_eq!(settings.get("color-scheme"), Some(Value::from(2u32)));
        assert_eq!(settings.get("accent-color"), Some(Value::from((1.0, 0.0, 0.2))));
        assert_eq!(settings.get("accent-color").unwrap().value_signature(), "(ddd)");
        assert_eq!(settings.get("font-name"), None);
    }

    #[test]
    fn changes_list_only_changed_keys() {
        let old = AppearanceSettings::from_config(&appearance(ColorScheme::Dark, "#3584e4"));
        assert!(old.changes_since(&old).is_empty());

        let new = AppearanceSettings::from_config(&appearance(ColorScheme::Light, "#3584e4"));
        assert_eq!(new.changes_since(&old), vec![("color-scheme", Value::from(2u32))]);

        let new = AppearanceSettings::from_config(&appearance(ColorScheme::Light, "#ffffff"));
        let keys: Vec<_> = new.changes_since(&old).into_iter().map(|(key, _)| key).collect();
        assert_eq!(keys, ["color-scheme", "accent-color"]);
    }

    #[test]
    fn namespace_filters() {
        assert!(namespace_matches("", NAMESPACE));
        assert!(namespace_matches(NAMESPACE, NAMESPACE));
        assert!(namespace_matches("org.freedesktop.*", NAMESPACE));
        assert!(namespace_matches("*", NAMESPACE));
        assert!(!namespace_matches("org.gnome.*", NAMESPACE));
        assert!(!namespace_matches("org.freedesktop", NAMESPACE));
        assert!(!namespace_matches("org.freedesktop.appearance.extra", NAMESPACE));
    }
}
