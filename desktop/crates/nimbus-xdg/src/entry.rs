// SPDX-License-Identifier: MIT

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::exec::{ExecError, ExecLine, ExpandContext};
use crate::keyfile::{Group, KeyFile, locale_candidates};

/// The desktop name entries are shown for, in addition to those in `XDG_CURRENT_DESKTOP`.
pub const DESKTOP_NAME: &str = "Nimbus";

/// One application from a `.desktop` file of `Type=Application`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesktopEntry {
    /// The desktop file id, such as `org.gnome.Nautilus`, without the `.desktop` suffix.
    pub id: String,
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    /// The raw `Icon=` value: an icon name or an absolute path.
    pub icon: Option<String>,
    /// The raw `Exec=` value including field codes.
    pub exec: String,
    pub terminal: bool,
    pub categories: Vec<String>,
    pub keywords: Vec<String>,
    pub startup_wm_class: Option<String>,
    pub path: PathBuf,
    /// The `Path=` key: the directory to start the application in.
    pub working_dir: Option<PathBuf>,
    /// The `MimeType=` key.
    pub mime_types: Vec<String>,
    /// The `DBusActivatable=` key.
    pub dbus_activatable: bool,
    /// The `StartupNotify=` key.
    pub startup_notify: bool,
    /// The `[Desktop Action …]` groups listed in `Actions=`, such as "New Window".
    pub actions: Vec<DesktopAction>,
}

/// An additional way to start an application, from a `[Desktop Action <id>]` group.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesktopAction {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    /// The raw `Exec=` value including field codes.
    pub exec: String,
}

/// Why [`DesktopEntry::parse_with`] didn't produce an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    #[error("not a desktop entry of Type=Application")]
    NotApplication,
    #[error("required keys are missing")]
    Invalid,
    #[error("Hidden=true")]
    Hidden,
    #[error("NoDisplay=true")]
    NoDisplay,
    #[error("excluded by OnlyShowIn or NotShowIn")]
    NotShownIn,
    #[error("the TryExec program isn't installed")]
    TryExecFailed,
}

/// The environment desktop entries are evaluated in.
#[derive(Clone, Debug, Default)]
pub struct ParseOptions {
    /// The message locale, such as `de_DE.UTF-8`.
    pub locale: Option<String>,
    /// The current desktop names, highest priority first, compared against `OnlyShowIn` and `NotShowIn`.
    pub desktops: Vec<String>,
    /// The directories `TryExec` searches for relative program names.
    pub search_path: Vec<PathBuf>,
    /// Keeps entries that `NoDisplay`, `OnlyShowIn`, or `NotShowIn` hide from menus,
    /// such as MIME handlers. `Hidden=true` still rejects an entry.
    pub include_hidden_from_menus: bool,
}

impl ParseOptions {
    /// Uses `locale`, the desktops in `XDG_CURRENT_DESKTOP` followed by [`DESKTOP_NAME`], and `PATH`.
    pub fn from_env(locale: Option<&str>) -> Self {
        Self {
            locale: locale.map(str::to_owned),
            desktops: current_desktops(),
            search_path: std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
            include_hidden_from_menus: false,
        }
    }
}

/// The names in `XDG_CURRENT_DESKTOP`, followed by [`DESKTOP_NAME`] unless already present.
pub fn current_desktops() -> Vec<String> {
    let mut desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .collect();
    if !desktops.iter().any(|d| d.eq_ignore_ascii_case(DESKTOP_NAME)) {
        desktops.push(DESKTOP_NAME.to_owned());
    }
    desktops
}

fn shown_in(group: &Group, desktops: &[String]) -> bool {
    let only = group.strings("OnlyShowIn");
    let not = group.strings("NotShowIn");
    let contains =
        |list: &[String], desktop: &str| list.iter().any(|d| d.eq_ignore_ascii_case(desktop));
    for desktop in desktops {
        if contains(&only, desktop) {
            return true;
        }
        if contains(&not, desktop) {
            return false;
        }
    }
    only.is_empty()
}

fn is_executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Finds `program` like `execvp` does: as given when it contains a slash, else in `search_path`.
pub(crate) fn find_program(program: &str, search_path: &[PathBuf]) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        let path = PathBuf::from(program);
        return is_executable(&path).then_some(path);
    }
    search_path.iter().map(|dir| dir.join(program)).find(|candidate| is_executable(candidate))
}

impl DesktopEntry {
    /// Parses one desktop file, picking localized keys for `locale` (such as `de_DE`).
    /// Returns `None` for entries that are hidden, `NoDisplay`, excluded by `OnlyShowIn`/`NotShowIn`
    /// for the `Nimbus` desktop, or fail `TryExec`.
    pub fn parse(id: &str, path: &Path, contents: &str, locale: Option<&str>) -> Option<Self> {
        Self::parse_with(id, path, contents, &ParseOptions::from_env(locale)).ok()
    }

    /// Like [`DesktopEntry::parse`] with an explicit environment, reporting why an entry is rejected.
    pub fn parse_with(
        id: &str,
        path: &Path,
        contents: &str,
        options: &ParseOptions,
    ) -> Result<Self, Rejection> {
        let keyfile = KeyFile::parse(contents);
        let group = keyfile.group("Desktop Entry").ok_or(Rejection::Invalid)?;
        if group.raw("Type").map(str::trim) != Some("Application") {
            return Err(Rejection::NotApplication);
        }
        let locales = locale_candidates(options.locale.as_deref());
        let name = group
            .locale_string("Name", &locales)
            .filter(|n| !n.trim().is_empty())
            .ok_or(Rejection::Invalid)?;
        let exec = group.string("Exec").filter(|e| !e.trim().is_empty());
        let dbus_activatable = group.boolean("DBusActivatable").unwrap_or(false);
        if exec.is_none() && !dbus_activatable {
            return Err(Rejection::Invalid);
        }
        if group.boolean("Hidden") == Some(true) {
            return Err(Rejection::Hidden);
        }
        if !options.include_hidden_from_menus {
            if group.boolean("NoDisplay") == Some(true) {
                return Err(Rejection::NoDisplay);
            }
            if !shown_in(group, &options.desktops) {
                return Err(Rejection::NotShownIn);
            }
        }
        if let Some(try_exec) = group.string("TryExec").filter(|t| !t.is_empty())
            && find_program(&try_exec, &options.search_path).is_none()
        {
            return Err(Rejection::TryExecFailed);
        }

        let non_empty = |value: Option<String>| value.filter(|v| !v.is_empty());
        let actions = group
            .strings("Actions")
            .into_iter()
            .filter_map(|action_id| {
                let action = keyfile.group(&format!("Desktop Action {action_id}"))?;
                Some(DesktopAction {
                    name: action.locale_string("Name", &locales).filter(|n| !n.is_empty())?,
                    icon: non_empty(action.locale_string("Icon", &locales)),
                    exec: action.string("Exec").unwrap_or_default(),
                    id: action_id,
                })
            })
            .collect();

        Ok(Self {
            id: id.to_owned(),
            name,
            generic_name: non_empty(group.locale_string("GenericName", &locales)),
            comment: non_empty(group.locale_string("Comment", &locales)),
            icon: non_empty(group.locale_string("Icon", &locales)),
            exec: exec.unwrap_or_default(),
            terminal: group.boolean("Terminal").unwrap_or(false),
            categories: group.strings("Categories"),
            keywords: group.locale_strings("Keywords", &locales),
            startup_wm_class: non_empty(group.string("StartupWMClass")),
            path: path.to_path_buf(),
            working_dir: non_empty(group.string("Path")).map(PathBuf::from),
            mime_types: group.strings("MimeType"),
            dbus_activatable,
            startup_notify: group.boolean("StartupNotify").unwrap_or(false),
            actions,
        })
    }

    fn expand_context(&self) -> ExpandContext<'_> {
        ExpandContext {
            name: &self.name,
            icon: self.icon.as_deref(),
            desktop_file: (!self.path.as_os_str().is_empty()).then_some(self.path.as_path()),
        }
    }

    /// Returns the command line for `Exec=` with field codes expanded for `files`, split into argv.
    pub fn command_line(&self, files: &[PathBuf]) -> Vec<String> {
        self.command_lines(files)
            .map(|lines| lines.into_iter().next().unwrap_or_default())
            .unwrap_or_else(|err| {
                tracing::warn!(id = %self.id, %err, "invalid Exec line");
                Vec::new()
            })
    }

    /// Returns one command line per instance to start for `files`.
    /// An application taking a single file (`%f`, `%u`) gets one instance per file.
    pub fn command_lines(&self, files: &[PathBuf]) -> Result<Vec<Vec<String>>, ExecError> {
        Ok(ExecLine::parse(&self.exec)?.expand_all(&self.expand_context(), files))
    }

    /// Returns the command lines for the action with `action_id`, like [`DesktopEntry::command_lines`].
    pub fn action_command_lines(
        &self,
        action_id: &str,
        files: &[PathBuf],
    ) -> Option<Result<Vec<Vec<String>>, ExecError>> {
        let action = self.actions.iter().find(|a| a.id == action_id)?;
        let context = ExpandContext {
            name: &action.name,
            icon: action.icon.as_deref().or(self.icon.as_deref()),
            ..self.expand_context()
        };
        Some(ExecLine::parse(&action.exec).map(|line| line.expand_all(&context, files)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn options() -> ParseOptions {
        ParseOptions { desktops: vec![DESKTOP_NAME.to_owned()], ..Default::default() }
    }

    fn parse(contents: &str, options: &ParseOptions) -> Result<DesktopEntry, Rejection> {
        DesktopEntry::parse_with("app", Path::new("/apps/app.desktop"), contents, options)
    }

    const BASIC: &str = "[Desktop Entry]\nType=Application\nName=Files\nName[de]=Dateien\nName[de_AT]=Dateien AT\n\
        Name[sr@latin]=Datoteke\nName[sr]=Датотеке\nGenericName=File Manager\nComment=Browse\\sfiles\\nnow\n\
        Exec=files %U\nIcon=org.files\nCategories=System;FileManager;\nKeywords=folder;dir\\;ectory;\n\
        Keywords[de]=Ordner;\nStartupWMClass=files-wm\nPath=/tmp\nMimeType=inode/directory;\n\
        Actions=new-window;missing;\n\n[Desktop Action new-window]\nName=New Window\nName[de]=Neues Fenster\nExec=files --new\n";

    #[test]
    fn parses_basic_entry() {
        let entry = parse(BASIC, &options()).expect("valid entry");
        assert_eq!(entry.name, "Files");
        assert_eq!(entry.generic_name.as_deref(), Some("File Manager"));
        assert_eq!(entry.comment.as_deref(), Some("Browse files\nnow"));
        assert_eq!(entry.exec, "files %U");
        assert_eq!(entry.categories, vec!["System", "FileManager"]);
        assert_eq!(entry.keywords, vec!["folder", "dir;ectory"]);
        assert_eq!(entry.startup_wm_class.as_deref(), Some("files-wm"));
        assert_eq!(entry.working_dir, Some(PathBuf::from("/tmp")));
        assert_eq!(entry.mime_types, vec!["inode/directory"]);
        assert_eq!(entry.actions.len(), 1);
        assert_eq!(entry.actions[0].name, "New Window");
        assert_eq!(
            entry.action_command_lines("new-window", &[]),
            Some(Ok(vec![vec!["files".to_owned(), "--new".to_owned()]]))
        );
        assert!(!entry.terminal);
    }

    #[test]
    fn localization() {
        let with = |locale: &str| ParseOptions { locale: Some(locale.to_owned()), ..options() };
        let name = |locale: &str| parse(BASIC, &with(locale)).map(|e| e.name).unwrap_or_default();
        assert_eq!(name("de_DE.UTF-8"), "Dateien");
        assert_eq!(name("de_AT"), "Dateien AT");
        assert_eq!(name("sr_RS@latin"), "Datoteke");
        assert_eq!(name("sr_RS"), "Датотеке");
        assert_eq!(name("fr_FR"), "Files");
        assert_eq!(name("C"), "Files");
        let entry = parse(BASIC, &with("de_CH")).expect("valid");
        assert_eq!(entry.keywords, vec!["Ordner"]);
        assert_eq!(entry.actions[0].name, "Neues Fenster");
    }

    #[test]
    fn rejections() {
        let base = "[Desktop Entry]\nType=Application\nName=A\nExec=a\n";
        assert!(parse(base, &options()).is_ok());
        assert_eq!(
            parse("[Desktop Entry]\nType=Link\nName=A\nURL=x\n", &options()),
            Err(Rejection::NotApplication)
        );
        assert_eq!(parse("[Other]\nType=Application\n", &options()), Err(Rejection::Invalid));
        assert_eq!(
            parse("[Desktop Entry]\nType=Application\nExec=a\n", &options()),
            Err(Rejection::Invalid)
        );
        assert_eq!(
            parse("[Desktop Entry]\nType=Application\nName=A\n", &options()),
            Err(Rejection::Invalid)
        );
        assert!(
            parse("[Desktop Entry]\nType=Application\nName=A\nDBusActivatable=true\n", &options())
                .is_ok()
        );
        assert_eq!(parse(&format!("{base}Hidden=true\n"), &options()), Err(Rejection::Hidden));
        assert_eq!(
            parse(&format!("{base}NoDisplay=true\n"), &options()),
            Err(Rejection::NoDisplay)
        );
        let all = ParseOptions { include_hidden_from_menus: true, ..options() };
        assert!(parse(&format!("{base}NoDisplay=true\nOnlyShowIn=KDE;\n"), &all).is_ok());
        assert_eq!(parse(&format!("{base}Hidden=true\n"), &all), Err(Rejection::Hidden));
    }

    #[test]
    fn only_and_not_show_in() {
        let base = "[Desktop Entry]\nType=Application\nName=A\nExec=a\n";
        let with = |desktops: &[&str]| ParseOptions {
            desktops: desktops.iter().map(|d| d.to_string()).collect(),
            ..options()
        };
        let shown = |extra: &str, desktops: &[&str]| {
            parse(&format!("{base}{extra}\n"), &with(desktops)).is_ok()
        };
        assert!(!shown("OnlyShowIn=GNOME;KDE;", &["Nimbus"]));
        assert!(shown("OnlyShowIn=GNOME;Nimbus;", &["Nimbus"]));
        assert!(shown("OnlyShowIn=GNOME;", &["GNOME", "Nimbus"]));
        assert!(!shown("NotShowIn=Nimbus;", &["Nimbus"]));
        assert!(shown("NotShowIn=KDE;", &["Nimbus"]));
        // The first desktop in the list decides.
        assert!(shown("OnlyShowIn=GNOME;\nNotShowIn=Nimbus;", &["GNOME", "Nimbus"]));
        assert!(!shown("OnlyShowIn=GNOME;\nNotShowIn=Nimbus;", &["Nimbus", "GNOME"]));
    }

    #[test]
    fn try_exec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = dir.path().join("prog");
        fs::write(&program, "#!/bin/sh\n").expect("write");
        let plain = dir.path().join("plain");
        fs::write(&plain, "").expect("write");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("chmod");
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).expect("chmod");
        let with_path = ParseOptions { search_path: vec![dir.path().to_path_buf()], ..options() };
        let base = "[Desktop Entry]\nType=Application\nName=A\nExec=a\n";
        let ok = |try_exec: &str, opts: &ParseOptions| {
            parse(&format!("{base}TryExec={try_exec}\n"), opts).is_ok()
        };
        assert!(ok("prog", &with_path));
        assert!(!ok("prog", &options()));
        assert!(!ok("plain", &with_path));
        assert!(!ok("missing", &with_path));
        assert!(ok(&program.to_string_lossy(), &options()));
        assert!(!ok(&plain.to_string_lossy(), &options()));
        assert_eq!(
            parse(&format!("{base}TryExec=missing\n"), &with_path),
            Err(Rejection::TryExecFailed)
        );
    }

    #[test]
    fn command_line_uses_entry_fields() {
        let entry = DesktopEntry {
            name: "Viewer".into(),
            icon: Some("viewer".into()),
            exec: "view %i --title=%c %k %f".into(),
            path: PathBuf::from("/apps/viewer.desktop"),
            ..Default::default()
        };
        assert_eq!(
            entry.command_line(&[PathBuf::from("/x.png")]),
            vec!["view", "--icon", "viewer", "--title=Viewer", "/apps/viewer.desktop", "/x.png"]
        );
        let broken = DesktopEntry { exec: "view \"open".into(), ..Default::default() };
        assert!(broken.command_line(&[]).is_empty());
    }
}
