// SPDX-License-Identifier: MIT

//! End-to-end tests of the portal against a private `dbus-daemon`.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use nimbus_config::{Appearance, ColorScheme, Config};
use nimbus_portal::{BUS_NAME, NAMESPACE, OBJECT_PATH};
use nimbus_test_support::PrivateBus;
use tokio::time::timeout;
use zbus::zvariant::OwnedValue;
use zbus::{Connection, Proxy};

const WAIT: Duration = Duration::from_secs(10);
/// Longer than the configuration watcher takes to report a change.
const QUIET: Duration = Duration::from_secs(2);

fn save(path: &Path, color_scheme: ColorScheme, accent: &str) {
    let appearance = Appearance { color_scheme, accent: accent.into(), ..Appearance::default() };
    Config { appearance, ..Config::default() }.save_to(path).unwrap();
}

async fn settings_proxy(connection: &Connection) -> Proxy<'static> {
    Proxy::new(connection, BUS_NAME, OBJECT_PATH, "org.freedesktop.impl.portal.Settings")
        .await
        .unwrap()
}

async fn read(proxy: &Proxy<'_>, namespace: &str, key: &str) -> zbus::Result<OwnedValue> {
    proxy.call("Read", &(namespace, key)).await
}

async fn read_all(
    proxy: &Proxy<'_>,
    namespaces: &[&str],
) -> HashMap<String, HashMap<String, OwnedValue>> {
    proxy.call("ReadAll", &(namespaces,)).await.unwrap()
}

fn error_name(error: zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(name, _, _) => name.to_string(),
        other => panic!("expected a D-Bus error reply, got {other}"),
    }
}

#[tokio::test]
async fn read_and_read_all() {
    let Some(bus) = PrivateBus::start() else { return };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    save(&path, ColorScheme::Dark, "#ff0033");
    let server = bus.connect().await;
    let _portal = nimbus_portal::serve(&server, &path).await.unwrap();
    let proxy = settings_proxy(&bus.connect().await).await;

    let scheme = read(&proxy, NAMESPACE, "color-scheme").await.unwrap();
    assert_eq!(u32::try_from(scheme).unwrap(), 1);
    let accent = read(&proxy, NAMESPACE, "accent-color").await.unwrap();
    assert_eq!(<(f64, f64, f64)>::try_from(accent).unwrap(), (1.0, 0.0, 0.2));
    let contrast = read(&proxy, NAMESPACE, "contrast").await.unwrap();
    assert_eq!(u32::try_from(contrast).unwrap(), 0);
    let version: u32 = proxy.get_property("version").await.unwrap();
    assert_eq!(version, 1);

    let not_found = "org.freedesktop.portal.Error.NotFound";
    let unknown_key = read(&proxy, NAMESPACE, "font-name").await.unwrap_err();
    assert_eq!(error_name(unknown_key), not_found);
    let unknown_namespace = read(&proxy, "org.gnome.desktop.interface", "color-scheme").await;
    assert_eq!(error_name(unknown_namespace.unwrap_err()), not_found);

    for filter in [&[][..], &[""], &["org.freedesktop.*"], &[NAMESPACE]] {
        let all = read_all(&proxy, filter).await;
        assert_eq!(all.len(), 1, "{filter:?}");
        let mut keys: Vec<_> = all[NAMESPACE].keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["accent-color", "color-scheme", "contrast"], "{filter:?}");
    }
    assert!(read_all(&proxy, &["org.gnome.*"]).await.is_empty());
}

#[tokio::test]
async fn setting_changed_reports_only_changed_keys() {
    let Some(bus) = PrivateBus::start() else { return };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    save(&path, ColorScheme::Dark, "#ff0033");
    let server = bus.connect().await;
    let _portal = nimbus_portal::serve(&server, &path).await.unwrap();
    let proxy = settings_proxy(&bus.connect().await).await;
    let mut changes = proxy.receive_signal("SettingChanged").await.unwrap();

    save(&path, ColorScheme::Light, "#ff0033");
    let signal = timeout(WAIT, changes.next()).await.unwrap().unwrap();
    let (namespace, key, value): (String, String, OwnedValue) =
        signal.body().deserialize().unwrap();
    assert_eq!((namespace.as_str(), key.as_str()), (NAMESPACE, "color-scheme"));
    assert_eq!(u32::try_from(value).unwrap(), 2);
    assert!(timeout(QUIET, changes.next()).await.is_err(), "unexpected second signal");

    let scheme = read(&proxy, NAMESPACE, "color-scheme").await.unwrap();
    assert_eq!(u32::try_from(scheme).unwrap(), 2);
}
