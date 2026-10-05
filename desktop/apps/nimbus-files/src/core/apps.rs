// SPDX-License-Identifier: MIT

//! Opening files with the default or a chosen application.
//!
//! The default handler is resolved by `xdg-open` or `gio open`.
//! "Open With" lists the handlers that `nimbus-xdg` finds for the file's MIME type, the default first.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use nimbus_xdg::{AppIndex, DesktopEntry, MimeLookup, ParseOptions};

/// Applications that open files of type `mime`, the default first, and the default's id.
///
/// Includes handlers hidden from menus with `NoDisplay=true`.
pub fn apps_for_mime(lookup: &MimeLookup, mime: &str) -> (Vec<DesktopEntry>, Option<String>) {
    let default = lookup.default_handler(mime);
    let options = ParseOptions { include_hidden_from_menus: true, ..lookup.options.clone() };
    let index = AppIndex::scan_dirs_with(&lookup.data_dirs, &options);
    let mut seen = HashSet::new();
    let apps = default
        .iter()
        .cloned()
        .chain(lookup.handlers(mime))
        .filter(|id| seen.insert(id.clone()))
        .filter_map(|id| index.get(&id).cloned())
        .collect();
    (apps, default)
}

/// Starts a program without waiting for it; a thread reaps it so it never becomes a zombie.
pub fn spawn_detached(program: &str, args: &[String], cwd: Option<&Path>) -> io::Result<()> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command.spawn()?;
    std::thread::Builder::new().name("nimbus-files-reaper".into()).spawn(move || {
        let _ = child.wait();
    })?;
    Ok(())
}

/// Opens a file with its default application through `xdg-open`, falling back to `gio open`.
pub fn open_default(path: &Path) -> io::Result<()> {
    let arg = vec![path.to_string_lossy().into_owned()];
    match spawn_detached("xdg-open", &arg, path.parent()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let gio = vec!["open".to_string(), arg[0].clone()];
            spawn_detached("gio", &gio, path.parent())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_default_handler_even_when_hidden_from_menus() {
        let dir = tempfile::tempdir().expect("temp dir");
        let apps = dir.path().join("data/applications");
        std::fs::create_dir_all(&apps).expect("mkdir");
        std::fs::write(
            apps.join("editor.desktop"),
            "[Desktop Entry]\nType=Application\nName=Editor\nExec=editor %U\nMimeType=text/plain;\n",
        )
        .expect("write");
        std::fs::write(
            apps.join("handler.desktop"),
            "[Desktop Entry]\nType=Application\nName=Handler\nExec=handler %f\n\
             MimeType=text/plain;\nNoDisplay=true\n",
        )
        .expect("write");
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).expect("mkdir");
        std::fs::write(
            config.join("mimeapps.list"),
            "[Default Applications]\ntext/plain=handler.desktop\n",
        )
        .expect("write");
        let lookup = MimeLookup {
            config_dirs: vec![config],
            data_dirs: vec![dir.path().join("data")],
            options: ParseOptions::default(),
        };
        let (found, default) = apps_for_mime(&lookup, "text/plain");
        let ids: Vec<&str> = found.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["handler", "editor"]);
        assert_eq!(default.as_deref(), Some("handler"));
        assert!(apps_for_mime(&lookup, "image/png").0.is_empty());
    }

    #[test]
    fn missing_programs_fail_cleanly() {
        let err = spawn_detached("/nonexistent/program", &[], None).expect_err("missing");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
