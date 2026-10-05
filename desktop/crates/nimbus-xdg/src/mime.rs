// SPDX-License-Identifier: MIT

//! Default applications per the MIME Applications Associations Specification 1.0.1.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::entry::{DesktopEntry, ParseOptions};
use crate::index::{data_dirs, desktop_file_map, desktop_files};
use crate::keyfile::{KeyFile, locale_from_env, split_list};

/// The directories and desktops that MIME associations are looked up for.
#[derive(Clone, Debug, Default)]
pub struct MimeLookup {
    /// `$XDG_CONFIG_HOME` followed by `$XDG_CONFIG_DIRS`.
    pub config_dirs: Vec<PathBuf>,
    /// The XDG data directories, highest precedence first.
    pub data_dirs: Vec<PathBuf>,
    /// The environment desktop entries are evaluated in.
    /// Its desktops also select desktop-specific files such as `nimbus-mimeapps.list`.
    pub options: ParseOptions,
}

/// The ids in one `mimeapps.list` file for one MIME type.
#[derive(Default)]
struct ListFile {
    defaults: Vec<String>,
    added: Vec<String>,
    removed: Vec<String>,
}

fn strip_suffix(id: &str) -> String {
    id.strip_suffix(".desktop").unwrap_or(id).to_owned()
}

fn ids_for(keyfile: &KeyFile, group: &str, mime: &str) -> Vec<String> {
    keyfile
        .group(group)
        .and_then(|g| {
            g.keys()
                .find(|(key, _)| key.eq_ignore_ascii_case(mime))
                .map(|(_, value)| split_list(value))
        })
        .unwrap_or_default()
        .iter()
        .map(|id| strip_suffix(id))
        .collect()
}

/// Resolves desktop file ids to entries, once per id.
struct Resolver<'a> {
    files: HashMap<String, PathBuf>,
    options: &'a ParseOptions,
    entries: HashMap<String, Option<DesktopEntry>>,
}

impl Resolver<'_> {
    /// The entry for `id` if it's installed and usable as a handler.
    fn get(&mut self, id: &str) -> Option<&DesktopEntry> {
        if !self.entries.contains_key(id) {
            let entry = self.files.get(id).and_then(|path| {
                let bytes = std::fs::read(path).ok()?;
                let contents = String::from_utf8_lossy(&bytes);
                let options =
                    ParseOptions { include_hidden_from_menus: true, ..self.options.clone() };
                DesktopEntry::parse_with(id, path, &contents, &options).ok()
            });
            self.entries.insert(id.to_owned(), entry);
        }
        self.entries.get(id).and_then(Option::as_ref)
    }

    fn handles(&mut self, id: &str, mime: &str) -> bool {
        self.get(id).is_some_and(|e| e.mime_types.iter().any(|m| m.eq_ignore_ascii_case(mime)))
    }
}

impl MimeLookup {
    /// Uses `$XDG_CONFIG_HOME`, `$XDG_CONFIG_DIRS`, the XDG data directories, and the current desktops.
    pub fn from_env() -> Self {
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| dirs::home_dir().map(|home| home.join(".config")));
        let config_dirs: Vec<PathBuf> = std::env::var_os("XDG_CONFIG_DIRS")
            .map(|v| std::env::split_paths(&v).filter(|p| p.is_absolute()).collect())
            .filter(|dirs: &Vec<PathBuf>| !dirs.is_empty())
            .unwrap_or_else(|| vec![PathBuf::from("/etc/xdg")]);
        let options = ParseOptions::from_env(locale_from_env().as_deref());
        Self {
            config_dirs: config_home.into_iter().chain(config_dirs).collect(),
            data_dirs: data_dirs(),
            options,
        }
    }

    /// The `mimeapps.list` files in precedence order.
    fn list_paths(&self) -> Vec<PathBuf> {
        let names: Vec<String> = self
            .options
            .desktops
            .iter()
            .map(|d| format!("{}-mimeapps.list", d.to_lowercase()))
            .chain(["mimeapps.list".to_owned()])
            .collect();
        let config = self.config_dirs.iter().cloned();
        let data = self.data_dirs.iter().map(|d| d.join("applications"));
        config.chain(data).flat_map(|dir| names.iter().map(move |name| dir.join(name))).collect()
    }

    fn list_files(&self, mime: &str) -> Vec<ListFile> {
        self.list_paths()
            .into_iter()
            .filter_map(|path| std::fs::read(path).ok())
            .map(|bytes| {
                let keyfile = KeyFile::parse(&String::from_utf8_lossy(&bytes));
                ListFile {
                    defaults: ids_for(&keyfile, "Default Applications", mime),
                    added: ids_for(&keyfile, "Added Associations", mime),
                    removed: ids_for(&keyfile, "Removed Associations", mime),
                }
            })
            .collect()
    }

    fn resolver(&self) -> Resolver<'_> {
        Resolver {
            files: desktop_file_map(&self.data_dirs),
            options: &self.options,
            entries: HashMap::new(),
        }
    }

    /// All installed applications associated with `mime`, most preferred first, as desktop file ids
    /// without the `.desktop` suffix.
    pub fn handlers(&self, mime: &str) -> Vec<String> {
        self.handlers_with(mime, &self.list_files(mime), &mut self.resolver())
    }

    fn handlers_with(
        &self,
        mime: &str,
        lists: &[ListFile],
        resolver: &mut Resolver<'_>,
    ) -> Vec<String> {
        let mut removed = HashSet::new();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for list in lists {
            for id in &list.added {
                if !removed.contains(id) && seen.insert(id.clone()) && resolver.get(id).is_some() {
                    out.push(id.clone());
                }
            }
            removed.extend(list.removed.iter().cloned());
        }
        for dir in &self.data_dirs {
            let applications = dir.join("applications");
            let candidates = match std::fs::read(applications.join("mimeinfo.cache")) {
                Ok(bytes) => {
                    ids_for(&KeyFile::parse(&String::from_utf8_lossy(&bytes)), "MIME Cache", mime)
                }
                Err(_) => desktop_files(&applications).into_iter().map(|(id, _)| id).collect(),
            };
            for id in candidates {
                if !removed.contains(&id) && !seen.contains(&id) && resolver.handles(&id, mime) {
                    seen.insert(id.clone());
                    out.push(id);
                }
            }
        }
        out
    }

    /// The default application for `mime` as a desktop file id without the `.desktop` suffix.
    /// Falls back to the most preferred associated application.
    pub fn default_handler(&self, mime: &str) -> Option<String> {
        let mime = mime.trim();
        if mime.is_empty() {
            return None;
        }
        let lists = self.list_files(mime);
        let mut resolver = self.resolver();
        for list in &lists {
            if let Some(id) = list.defaults.iter().find(|id| resolver.get(id).is_some()) {
                return Some(id.clone());
            }
        }
        self.handlers_with(mime, &lists, &mut resolver).into_iter().next()
    }
}

/// Returns the desktop file id, without the `.desktop` suffix, of the default application for `mime`,
/// following `mimeapps.list` files and `mimeinfo.cache` in the XDG directories.
pub fn default_handler_for_mime(mime: &str) -> Option<String> {
    MimeLookup::from_env().default_handler(mime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, contents).expect("write");
    }

    fn app(dir: &Path, id: &str, mime: &str, extra: &str) {
        write(
            &dir.join("applications").join(format!("{id}.desktop")),
            &format!(
                "[Desktop Entry]\nType=Application\nName={id}\nExec={id} %f\nMimeType={mime}\n{extra}"
            ),
        );
    }

    struct Fixture {
        config: tempfile::TempDir,
        home: tempfile::TempDir,
        system: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let fixture = Self {
                config: tempfile::tempdir().expect("tempdir"),
                home: tempfile::tempdir().expect("tempdir"),
                system: tempfile::tempdir().expect("tempdir"),
            };
            app(fixture.system.path(), "viewer", "image/png;text/plain;", "");
            app(fixture.system.path(), "editor", "text/plain;", "NoDisplay=true\n");
            app(fixture.system.path(), "gone", "text/plain;", "Hidden=true\n");
            app(fixture.home.path(), "notes", "text/plain;", "");
            fixture
        }

        fn lookup(&self) -> MimeLookup {
            MimeLookup {
                config_dirs: vec![self.config.path().to_path_buf()],
                data_dirs: vec![self.home.path().to_path_buf(), self.system.path().to_path_buf()],
                options: ParseOptions { desktops: vec!["Nimbus".into()], ..Default::default() },
            }
        }
    }

    #[test]
    fn associations_without_lists_follow_data_dir_order() {
        let fixture = Fixture::new();
        let lookup = fixture.lookup();
        assert_eq!(lookup.handlers("text/plain"), vec!["notes", "editor", "viewer"]);
        assert_eq!(lookup.default_handler("TEXT/PLAIN"), Some("notes".into()));
        assert_eq!(lookup.default_handler("image/png"), Some("viewer".into()));
        assert_eq!(lookup.default_handler("video/mp4"), None);
        assert_eq!(lookup.default_handler(""), None);
    }

    #[test]
    fn defaults_skip_uninstalled_and_respect_precedence() {
        let fixture = Fixture::new();
        write(
            &fixture.system.path().join("applications/mimeapps.list"),
            "[Default Applications]\ntext/plain=viewer.desktop\n",
        );
        let lookup = fixture.lookup();
        assert_eq!(lookup.default_handler("text/plain"), Some("viewer".into()));
        write(
            &fixture.config.path().join("mimeapps.list"),
            "[Default Applications]\ntext/plain=missing.desktop;gone.desktop;editor.desktop;\n",
        );
        assert_eq!(lookup.default_handler("text/plain"), Some("editor".into()));
        write(
            &fixture.config.path().join("nimbus-mimeapps.list"),
            "[Default Applications]\ntext/plain=notes.desktop\n",
        );
        assert_eq!(lookup.default_handler("text/plain"), Some("notes".into()));
    }

    #[test]
    fn added_and_removed_associations() {
        let fixture = Fixture::new();
        write(
            &fixture.config.path().join("mimeapps.list"),
            "[Added Associations]\nimage/png=editor.desktop;\n\n[Removed Associations]\ntext/plain=notes.desktop;\n",
        );
        write(
            &fixture.system.path().join("applications/mimeapps.list"),
            "[Added Associations]\ntext/plain=notes.desktop;viewer.desktop;\n",
        );
        let lookup = fixture.lookup();
        assert_eq!(lookup.handlers("image/png"), vec!["editor", "viewer"]);
        assert_eq!(lookup.handlers("text/plain"), vec!["viewer", "editor"]);
        assert_eq!(lookup.default_handler("text/plain"), Some("viewer".into()));
    }

    #[test]
    fn mimeinfo_cache_is_used_and_verified() {
        let fixture = Fixture::new();
        write(
            &fixture.system.path().join("applications/mimeinfo.cache"),
            "[MIME Cache]\ntext/plain=viewer.desktop;stale.desktop;\n",
        );
        let lookup = fixture.lookup();
        // `editor` isn't in the system cache, so only the cached and verified `viewer` remains there.
        assert_eq!(lookup.handlers("text/plain"), vec!["notes", "viewer"]);
    }
}
