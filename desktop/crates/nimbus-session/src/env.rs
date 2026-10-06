// SPDX-License-Identifier: MIT

//! The environment the session exports to the compositor and everything it starts.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Variables that identify the session; they always replace inherited values.
const SESSION_IDENTITY: &[(&str, &str)] = &[
    ("XDG_CURRENT_DESKTOP", "Nimbus"),
    ("XDG_SESSION_DESKTOP", "nimbus"),
    ("XDG_SESSION_TYPE", "wayland"),
];

/// Toolkit backend hints; a value the user already exported wins.
const TOOLKIT_HINTS: &[(&str, &str)] = &[
    ("MOZ_ENABLE_WAYLAND", "1"),
    ("QT_QPA_PLATFORM", "wayland;xcb"),
    ("SDL_VIDEODRIVER", "wayland,x11"),
    ("ELECTRON_OZONE_PLATFORM_HINT", "auto"),
    ("_JAVA_AWT_WM_NONREPARENTING", "1"),
    ("GDK_BACKEND", "wayland,x11"),
    ("CLUTTER_BACKEND", "wayland"),
];

/// Variables pushed into the D-Bus and systemd activation environments once the compositor is ready.
pub const ACTIVATION_VARIABLES: &[&str] = &[
    "WAYLAND_DISPLAY",
    "NIMBUS_SOCKET",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "XDG_SESSION_DESKTOP",
];

/// Set in the re-executed process so a failing `dbus-run-session` can't loop.
pub const REEXEC_GUARD: &str = "NIMBUS_SESSION_UNDER_DBUS_RUN_SESSION";

/// Overrides applied on top of the inherited environment of every process the session starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionEnv {
    vars: BTreeMap<String, String>,
}

impl SessionEnv {
    /// Builds the session's overrides; `lookup` reads the inherited environment.
    pub fn new(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mut vars = BTreeMap::new();
        for (name, value) in SESSION_IDENTITY {
            vars.insert(name.to_string(), value.to_string());
        }
        for (name, value) in TOOLKIT_HINTS {
            if lookup(name).is_none_or(|v| v.is_empty()) {
                vars.insert(name.to_string(), value.to_string());
            }
        }
        Self { vars }
    }

    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        self.vars.insert(name.to_string(), value.into());
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    pub fn apply(&self, command: &mut Command) {
        command.envs(&self.vars);
    }
}

/// How the session gets a D-Bus session bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DbusPlan {
    /// `DBUS_SESSION_BUS_ADDRESS` is already set.
    Inherited,
    /// A per-user bus (from systemd or dbus-broker) listens at `$XDG_RUNTIME_DIR/bus`.
    UserBus(String),
    /// Start a private bus by re-executing under `dbus-run-session`.
    Reexec,
    /// No bus can be found or started; features that need one stay hidden.
    Unavailable,
}

pub fn dbus_plan(
    lookup: impl Fn(&str) -> Option<String>,
    socket_exists: impl Fn(&Path) -> bool,
    dbus_run_session_available: bool,
) -> DbusPlan {
    if lookup("DBUS_SESSION_BUS_ADDRESS").is_some_and(|v| !v.is_empty()) {
        return DbusPlan::Inherited;
    }
    if let Some(runtime) = lookup("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        let bus = Path::new(&runtime).join("bus");
        if socket_exists(&bus) {
            return DbusPlan::UserBus(format!("unix:path={}", bus.display()));
        }
    }
    if dbus_run_session_available && lookup(REEXEC_GUARD).is_none() {
        DbusPlan::Reexec
    } else {
        DbusPlan::Unavailable
    }
}

/// Returns the first executable file called `name` in the `PATH`-style list `path`.
pub fn find_in_path(name: &str, path: Option<OsString>) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 {
        return is_executable(candidate).then(|| candidate.to_path_buf());
    }
    std::env::split_paths(&path?).map(|dir| dir.join(name)).find(|p| is_executable(p))
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Returns the executable `name` next to `current_exe`, or the bare name for a `PATH` lookup.
pub fn sibling_program(name: &str, current_exe: Option<&Path>) -> PathBuf {
    current_exe
        .and_then(Path::parent)
        .map(|dir| dir.join(name))
        .filter(|p| is_executable(p))
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn identity_is_forced_and_hints_respect_user_values() {
        let session = SessionEnv::new(env(&[
            ("XDG_CURRENT_DESKTOP", "GNOME"),
            ("QT_QPA_PLATFORM", "xcb"),
            ("GDK_BACKEND", ""),
        ]));
        assert_eq!(session.get("XDG_CURRENT_DESKTOP"), Some("Nimbus"));
        assert_eq!(session.get("XDG_SESSION_DESKTOP"), Some("nimbus"));
        assert_eq!(session.get("XDG_SESSION_TYPE"), Some("wayland"));
        assert_eq!(session.get("QT_QPA_PLATFORM"), None);
        assert_eq!(session.get("GDK_BACKEND"), Some("wayland,x11"));
        assert_eq!(session.get("SDL_VIDEODRIVER"), Some("wayland,x11"));
        assert_eq!(session.get("MOZ_ENABLE_WAYLAND"), Some("1"));
        assert_eq!(session.get("ELECTRON_OZONE_PLATFORM_HINT"), Some("auto"));
        assert_eq!(session.get("_JAVA_AWT_WM_NONREPARENTING"), Some("1"));
        assert_eq!(session.get("CLUTTER_BACKEND"), Some("wayland"));
    }

    #[test]
    fn overrides_reach_commands() {
        let mut session = SessionEnv::new(env(&[]));
        session.set("WAYLAND_DISPLAY", "wayland-7");
        let mut command = Command::new("true");
        session.apply(&mut command);
        let envs: HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            envs[std::ffi::OsStr::new("WAYLAND_DISPLAY")],
            Some(std::ffi::OsStr::new("wayland-7"))
        );
        assert_eq!(
            envs[std::ffi::OsStr::new("XDG_SESSION_TYPE")],
            Some(std::ffi::OsStr::new("wayland"))
        );
    }

    #[test]
    fn dbus_plan_prefers_existing_buses() {
        let never = |_: &Path| false;
        assert_eq!(
            dbus_plan(env(&[("DBUS_SESSION_BUS_ADDRESS", "unix:path=/x")]), never, true),
            DbusPlan::Inherited
        );
        assert_eq!(
            dbus_plan(
                env(&[("XDG_RUNTIME_DIR", "/run/user/1000")]),
                |p| p == Path::new("/run/user/1000/bus"),
                true
            ),
            DbusPlan::UserBus("unix:path=/run/user/1000/bus".into())
        );
        assert_eq!(
            dbus_plan(env(&[("XDG_RUNTIME_DIR", "/run/user/1000")]), never, true),
            DbusPlan::Reexec
        );
        assert_eq!(dbus_plan(env(&[]), never, false), DbusPlan::Unavailable);
        assert_eq!(dbus_plan(env(&[(REEXEC_GUARD, "1")]), never, true), DbusPlan::Unavailable);
    }

    #[test]
    fn path_lookup_finds_executables_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("tool");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::write(dir.path().join("data"), "").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = Some(OsString::from(format!("/nonexistent:{}", dir.path().display())));
        assert_eq!(find_in_path("tool", path.clone()), Some(exe.clone()));
        assert_eq!(find_in_path("data", path.clone()), None);
        assert_eq!(find_in_path(exe.to_str().unwrap(), None), Some(exe.clone()));

        let sibling = dir.path().join("nimbus-compositor");
        let session = dir.path().join("nimbus-session");
        assert_eq!(
            sibling_program("nimbus-compositor", Some(&session)),
            PathBuf::from("nimbus-compositor")
        );
        std::fs::copy(&exe, &sibling).unwrap();
        assert_eq!(sibling_program("nimbus-compositor", Some(&session)), sibling);
    }
}
