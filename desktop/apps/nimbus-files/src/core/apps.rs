// SPDX-License-Identifier: MIT

//! Opening files with the default or a chosen application.
//!
//! The default handler is resolved by `xdg-open` or `gio open`. For "Open With", installed desktop
//! entries are scanned for the file's MIME type, with the default from `xdg-mime query default` first.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// An application that can open files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    /// The desktop file id, such as `org.gnome.TextEditor.desktop`.
    pub id: String,
    pub name: String,
    pub exec: String,
    pub terminal: bool,
    pub mime_types: Vec<String>,
}

/// Parses the `[Desktop Entry]` group of a desktop file; hidden and non-application entries yield `None`.
pub fn parse_desktop_entry(id: &str, text: &str) -> Option<AppInfo> {
    let mut in_entry = false;
    let mut name = None;
    let mut exec = None;
    let mut terminal = false;
    let mut mime_types = Vec::new();
    let mut is_app = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        match key.trim() {
            "Type" => is_app = value == "Application",
            "Name" => name = Some(value.to_string()),
            "Exec" => exec = Some(value.to_string()),
            "Terminal" => terminal = value == "true",
            "MimeType" => {
                mime_types =
                    value.split(';').filter(|m| !m.is_empty()).map(str::to_string).collect();
            }
            "NoDisplay" | "Hidden" if value == "true" => return None,
            _ => {}
        }
    }
    if !is_app {
        return None;
    }
    Some(AppInfo { id: id.to_string(), name: name?, exec: exec?, terminal, mime_types })
}

/// Splits an `Exec` value into arguments per the Desktop Entry spec's quoting rules.
pub fn tokenize_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    args.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        args.push(current);
    }
    args
}

/// Builds the command line for opening `files`, expanding the field codes `%f %F %u %U`.
///
/// Programs that take one file (`%f`, `%u`) get only the first; the caller launches one per file.
/// Without a field code, the files are appended, as most launchers do.
pub fn expand_exec(app: &AppInfo, files: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    let mut used = false;
    let uris = || files.iter().map(|f| super::uri::path_to_uri(f));
    let paths = || files.iter().map(|f| f.to_string_lossy().into_owned());
    for arg in tokenize_exec(&app.exec) {
        match arg.as_str() {
            "%f" => {
                out.extend(paths().take(1));
                used = true;
            }
            "%F" => {
                out.extend(paths());
                used = true;
            }
            "%u" => {
                out.extend(uris().take(1));
                used = true;
            }
            "%U" => {
                out.extend(uris());
                used = true;
            }
            "%i" | "%k" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            "%c" => out.push(app.name.clone()),
            _ => {
                let mut expanded = String::new();
                let mut chars = arg.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '%' {
                        match chars.next() {
                            Some('%') => expanded.push('%'),
                            Some('c') => expanded.push_str(&app.name),
                            _ => {}
                        }
                    } else {
                        expanded.push(c);
                    }
                }
                out.push(expanded);
            }
        }
    }
    if !used {
        out.extend(paths());
    }
    out
}

/// Whether the application takes several files in one invocation.
pub fn accepts_many(app: &AppInfo) -> bool {
    tokenize_exec(&app.exec).iter().any(|a| a == "%F" || a == "%U")
}

/// `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`, in priority order.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::data_dir() {
        dirs.push(home);
    }
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let system =
        if system.trim().is_empty() { "/usr/local/share:/usr/share".to_string() } else { system };
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs
}

/// Applications declaring support for `mime`, the default first and then by name; each id appears once.
pub fn apps_for_mime(mime: &str, data_dirs: &[PathBuf], default_id: Option<&str>) -> Vec<AppInfo> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for dir in data_dirs {
        let root = dir.join("applications");
        for entry in walkdir::WalkDir::new(&root).max_depth(3).into_iter().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(relative) = path.strip_prefix(&root) else { continue };
            // Desktop file ids replace folder separators with dashes.
            let id = relative.to_string_lossy().replace('/', "-");
            if !seen.insert(id.clone()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            if let Some(app) = parse_desktop_entry(&id, &text)
                && app.mime_types.iter().any(|m| mime_matches(m, mime))
            {
                apps.push(app);
            }
        }
    }
    apps.sort_by(|a, b| {
        let a_default = Some(a.id.as_str()) == default_id;
        let b_default = Some(b.id.as_str()) == default_id;
        b_default.cmp(&a_default).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    apps
}

fn mime_matches(declared: &str, mime: &str) -> bool {
    declared == mime
        || declared.strip_suffix("/*").is_some_and(|top| mime.split('/').next() == Some(top))
        || (declared == "text/plain" && mime.starts_with("text/"))
}

/// The default application id for a MIME type, from `xdg-mime query default`.
pub fn default_app_id(mime: &str) -> Option<String> {
    let output = Command::new("xdg-mime")
        .args(["query", "default", mime])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !id.is_empty()).then_some(id)
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

/// Opens files with a chosen application, once per file when it takes only one.
pub fn launch(app: &AppInfo, files: &[PathBuf]) -> io::Result<()> {
    let groups: Vec<Vec<PathBuf>> = if accepts_many(app) || files.len() <= 1 {
        vec![files.to_vec()]
    } else {
        files.iter().map(|f| vec![f.clone()]).collect()
    };
    for group in groups {
        let mut argv = expand_exec(app, &group);
        if argv.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty Exec line"));
        }
        if app.terminal {
            let terminal = std::env::var("TERMINAL").unwrap_or_else(|_| "nimbus-terminal".into());
            argv.splice(0..0, [terminal, "-e".to_string()]);
        }
        let program = argv.remove(0);
        spawn_detached(&program, &argv, group.first().and_then(|f| f.parent()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDITOR: &str = "[Desktop Entry]\nType=Application\nName=Text Editor\nExec=editor --new %U\n\
                          MimeType=text/plain;text/markdown;\n[Desktop Action new]\nName=Other\n";

    #[test]
    fn parses_entries() {
        let app = parse_desktop_entry("editor.desktop", EDITOR).expect("parsed");
        assert_eq!(app.name, "Text Editor");
        assert_eq!(app.mime_types, ["text/plain", "text/markdown"]);
        assert!(!app.terminal);
        assert!(accepts_many(&app));
        assert_eq!(parse_desktop_entry("x", "[Desktop Entry]\nType=Link\nName=a\nExec=b\n"), None);
        assert_eq!(
            parse_desktop_entry(
                "x",
                "[Desktop Entry]\nType=Application\nName=a\nExec=b\nNoDisplay=true\n"
            ),
            None
        );
        assert_eq!(parse_desktop_entry("x", "[Desktop Entry]\nType=Application\nName=a\n"), None);
    }

    #[test]
    fn exec_quoting_and_field_codes() {
        assert_eq!(
            tokenize_exec(r#"app "two words" "a \"q\"" plain  "" "#),
            ["app", "two words", "a \"q\"", "plain", ""]
        );
        let app = AppInfo {
            id: "v".into(),
            name: "Viewer".into(),
            exec: "viewer --title=%c %f %i 100%%".into(),
            terminal: false,
            mime_types: vec![],
        };
        let files = [PathBuf::from("/a b.png"), PathBuf::from("/c.png")];
        assert_eq!(expand_exec(&app, &files), ["viewer", "--title=Viewer", "/a b.png", "100%"]);
        assert!(!accepts_many(&app));
        let uris = AppInfo { exec: "web %U".into(), ..app.clone() };
        assert_eq!(expand_exec(&uris, &files), ["web", "file:///a%20b.png", "file:///c.png"]);
        let bare = AppInfo { exec: "bare".into(), ..app };
        assert_eq!(expand_exec(&bare, &files[..1]), ["bare", "/a b.png"]);
    }

    #[test]
    fn finds_apps_for_a_type() {
        let dir = tempfile::tempdir().expect("temp dir");
        let apps = dir.path().join("applications");
        std::fs::create_dir_all(apps.join("vendor")).expect("mkdir");
        std::fs::write(apps.join("editor.desktop"), EDITOR).expect("write");
        std::fs::write(
            apps.join("vendor/zed.desktop"),
            "[Desktop Entry]\nType=Application\nName=Another\nExec=zed %F\nMimeType=text/*;\n",
        )
        .expect("write");
        std::fs::write(
            apps.join("img.desktop"),
            "[Desktop Entry]\nType=Application\nName=Img\nExec=i\nMimeType=image/png;\n",
        )
        .expect("write");
        let found =
            apps_for_mime("text/x-rust", &[dir.path().to_path_buf()], Some("vendor-zed.desktop"));
        let ids: Vec<&str> = found.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["vendor-zed.desktop", "editor.desktop"]);
        let found = apps_for_mime("image/png", &[dir.path().to_path_buf()], None);
        assert_eq!(found.len(), 1);
        assert!(!data_dirs().is_empty());
    }

    #[test]
    fn missing_programs_fail_cleanly() {
        let err = spawn_detached("/nonexistent/program", &[], None).expect_err("missing");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
