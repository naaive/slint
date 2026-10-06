// SPDX-License-Identifier: MIT

//! Choosing default applications in `$XDG_CONFIG_HOME`:
//! `mimeapps.list` for MIME types, and `xdg-terminals.list` from the xdg-terminal-exec proposal for the terminal.
//! Both have desktop-specific variants, such as `nimbus-mimeapps.list`, which take precedence,
//! so a choice also updates the variants that exist there.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::entry::ParseOptions;
use crate::index::AppIndex;
use crate::mime::MimeLookup;

const DEFAULTS_GROUP: &str = "[Default Applications]";
const TERMINAL_CATEGORY: &str = "TerminalEmulator";

impl MimeLookup {
    /// Makes `id`, a desktop file id without `.desktop`, the default application for each of `mimes`.
    pub fn set_default(&self, mimes: &[&str], id: &str) -> io::Result<()> {
        let home = self.config_home()?;
        edit(&home.join("mimeapps.list"), |text| set_defaults(text, mimes, Some(id)))?;
        for path in self.desktop_lists(home, "mimeapps.list") {
            edit(&path, |text| set_defaults(text, mimes, None))?;
        }
        Ok(())
    }

    /// The installed terminal emulators (`Categories=TerminalEmulator`) as desktop file ids,
    /// the ones listed in `xdg-terminals.list` files first, in their order, and the others by name.
    pub fn terminals(&self) -> Vec<String> {
        let options = ParseOptions { include_hidden_from_menus: true, ..self.options.clone() };
        let installed: Vec<String> = AppIndex::scan_dirs_with(&self.data_dirs, &options)
            .entries
            .into_iter()
            .filter(|entry| entry.categories.iter().any(|c| c == TERMINAL_CATEGORY))
            .map(|entry| entry.id)
            .collect();
        let mut disabled = HashSet::new();
        let mut listed = Vec::new();
        for path in self.terminal_lists() {
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            for line in text.lines().map(str::trim) {
                match line.strip_prefix('-') {
                    Some(id) => disabled.insert(terminal_id(id)),
                    None if line.is_empty() || line.starts_with('#') => false,
                    None => {
                        listed.push(terminal_id(line.trim_start_matches('+')));
                        false
                    }
                };
            }
        }
        let mut seen = HashSet::new();
        listed
            .into_iter()
            .filter(|id| installed.contains(id))
            .chain(installed.iter().cloned())
            .filter(|id| !disabled.contains(id) && seen.insert(id.clone()))
            .collect()
    }

    /// Makes `id`, a desktop file id without `.desktop`, the preferred terminal emulator.
    pub fn set_default_terminal(&self, id: &str) -> io::Result<()> {
        let home = self.config_home()?;
        edit(&home.join("xdg-terminals.list"), |text| prefer_terminal(text, id))?;
        for path in self.desktop_lists(home, "xdg-terminals.list") {
            edit(&path, |text| prefer_terminal(text, id))?;
        }
        Ok(())
    }

    fn config_home(&self) -> io::Result<&Path> {
        self.config_dirs
            .first()
            .map(PathBuf::as_path)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no configuration directory"))
    }

    /// The existing desktop-specific variants of `name` in `dir`.
    fn desktop_lists(&self, dir: &Path, name: &str) -> Vec<PathBuf> {
        self.options
            .desktops
            .iter()
            .map(|desktop| dir.join(format!("{}-{name}", desktop.to_lowercase())))
            .filter(|path| path.exists())
            .collect()
    }

    /// The `xdg-terminals.list` files in precedence order: desktop-specific first, in each config directory.
    fn terminal_lists(&self) -> Vec<PathBuf> {
        let names: Vec<String> = self
            .options
            .desktops
            .iter()
            .map(|d| format!("{}-xdg-terminals.list", d.to_lowercase()))
            .chain(["xdg-terminals.list".to_owned()])
            .collect();
        self.config_dirs
            .iter()
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .collect()
    }
}

/// An entry of `xdg-terminals.list`, such as `foot.desktop` or `foot.desktop:new-window`, as a desktop file id.
fn terminal_id(entry: &str) -> String {
    let entry = entry.trim();
    let entry = entry.split_once(':').map_or(entry, |(id, _)| id);
    entry.strip_suffix(".desktop").unwrap_or(entry).to_owned()
}

/// Rewrites the file at `path` through `change`, atomically; a missing file reads as empty.
fn edit(path: &Path, change: impl FnOnce(&str) -> String) -> io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let changed = change(&text);
    if changed == text {
        return Ok(());
    }
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let temporary = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&temporary, changed)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

/// `text` with the `[Default Applications]` keys for `mimes` removed, then set to `id` when given.
/// Other groups, keys, and comments stay as they are.
fn set_defaults(text: &str, mimes: &[&str], id: Option<&str>) -> String {
    let is_mime = |line: &str| {
        line.split_once('=')
            .is_some_and(|(key, _)| mimes.iter().any(|m| m.eq_ignore_ascii_case(key.trim())))
    };
    let mut lines: Vec<String> = Vec::new();
    let mut in_group = false;
    // Where the new keys go: after the last key of the group.
    let mut insert_at = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_group = trimmed == DEFAULTS_GROUP;
            if in_group {
                insert_at = Some(lines.len() + 1);
            }
        } else if in_group && is_mime(trimmed) {
            continue;
        }
        lines.push(line.to_owned());
        if in_group && !trimmed.is_empty() {
            insert_at = Some(lines.len());
        }
    }
    if let Some(id) = id {
        let keys = mimes.iter().map(|mime| format!("{mime}={id}.desktop;"));
        match insert_at {
            Some(at) => {
                lines.splice(at..at, keys);
            }
            None => {
                if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push(DEFAULTS_GROUP.to_owned());
                lines.extend(keys);
            }
        }
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// `text` with `id` as its first entry, and without its other entries.
fn prefer_terminal(text: &str, id: &str) -> String {
    let others = text.lines().filter(|line| {
        let entry = line.trim().trim_start_matches(['+', '-']);
        entry.starts_with('#') || entry.is_empty() || terminal_id(entry) != id
    });
    let mut out = std::iter::once(format!("{id}.desktop"))
        .chain(others.map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn app(dir: &Path, id: &str, extra: &str) {
        let path = dir.join("applications").join(format!("{id}.desktop"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!("[Desktop Entry]\nType=Application\nName={id}\nExec={id}\n{extra}"),
        )
        .unwrap();
    }

    fn lookup(config: &Path, data: &Path) -> MimeLookup {
        MimeLookup {
            config_dirs: vec![config.to_path_buf()],
            data_dirs: vec![data.to_path_buf()],
            options: ParseOptions { desktops: vec!["Nimbus".into()], ..Default::default() },
        }
    }

    #[test]
    fn defaults_keep_the_rest_of_the_file() {
        let text = "# mine\n[Added Associations]\ntext/plain=a.desktop;\n\n[Default Applications]\ntext/plain=a.desktop;\nimage/png=b.desktop;\n\n[Removed Associations]\nimage/png=c.desktop;\n";
        assert_eq!(
            set_defaults(text, &["text/plain", "text/markdown"], Some("notes")),
            "# mine\n[Added Associations]\ntext/plain=a.desktop;\n\n[Default Applications]\nimage/png=b.desktop;\ntext/plain=notes.desktop;\ntext/markdown=notes.desktop;\n\n[Removed Associations]\nimage/png=c.desktop;\n"
        );
        assert_eq!(
            set_defaults("", &["text/plain"], Some("notes")),
            "[Default Applications]\ntext/plain=notes.desktop;\n"
        );
        assert_eq!(
            set_defaults("[Added Associations]\nx/y=a.desktop;", &["TEXT/PLAIN"], Some("notes")),
            "[Added Associations]\nx/y=a.desktop;\n\n[Default Applications]\nTEXT/PLAIN=notes.desktop;\n"
        );
        assert_eq!(
            set_defaults("[Default Applications]\ntext/plain=a.desktop;\n", &["text/plain"], None),
            "[Default Applications]\n"
        );
    }

    #[test]
    fn set_default_wins_over_desktop_specific_lists() {
        let config = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        for id in ["viewer", "gallery"] {
            app(data.path(), id, "MimeType=image/png;image/jpeg;\n");
        }
        let lookup = lookup(config.path(), data.path());
        fs::write(
            config.path().join("nimbus-mimeapps.list"),
            "[Default Applications]\nimage/png=viewer.desktop;\nimage/gif=viewer.desktop;\n",
        )
        .unwrap();
        assert_eq!(lookup.default_handler("image/png").as_deref(), Some("viewer"));
        lookup.set_default(&["image/png", "image/jpeg"], "gallery").unwrap();
        assert_eq!(lookup.default_handler("image/png").as_deref(), Some("gallery"));
        assert_eq!(lookup.default_handler("image/jpeg").as_deref(), Some("gallery"));
        assert_eq!(
            fs::read_to_string(config.path().join("nimbus-mimeapps.list")).unwrap(),
            "[Default Applications]\nimage/gif=viewer.desktop;\n"
        );
        assert!(!config.path().join("kde-mimeapps.list").exists());
    }

    #[test]
    fn terminals_follow_the_lists() {
        let config = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        for id in ["foot", "kitty", "org.nimbus.Terminal"] {
            app(data.path(), id, "Categories=System;TerminalEmulator;\n");
        }
        app(data.path(), "editor", "Categories=Utility;\n");
        let lookup = lookup(config.path(), data.path());
        assert_eq!(lookup.terminals(), ["foot", "kitty", "org.nimbus.Terminal"]);

        fs::write(
            config.path().join("xdg-terminals.list"),
            "# preferred\nkitty.desktop:new-window\n-foot.desktop\ngone.desktop\n",
        )
        .unwrap();
        assert_eq!(lookup.terminals(), ["kitty", "org.nimbus.Terminal"]);

        lookup.set_default_terminal("org.nimbus.Terminal").unwrap();
        assert_eq!(lookup.terminals(), ["org.nimbus.Terminal", "kitty"]);
        lookup.set_default_terminal("foot").unwrap();
        assert_eq!(lookup.terminals(), ["foot", "org.nimbus.Terminal", "kitty"]);
        assert_eq!(
            fs::read_to_string(config.path().join("xdg-terminals.list")).unwrap(),
            "foot.desktop\norg.nimbus.Terminal.desktop\n# preferred\nkitty.desktop:new-window\ngone.desktop\n"
        );
    }
}
