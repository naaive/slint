// SPDX-License-Identifier: MIT

//! Session autostart: the configuration's `autostart` commands and XDG autostart entries.
//!
//! Follows the Desktop Application Autostart Specification and the parts of the
//! Desktop Entry Specification it relies on.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

/// The `[Desktop Entry]` group of a desktop file, with unlocalized keys only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopEntry {
    pub path: PathBuf,
    keys: HashMap<String, String>,
}

impl DesktopEntry {
    /// Returns `None` when `text` has no `[Desktop Entry]` group.
    pub fn parse(path: &Path, text: &str) -> Option<Self> {
        let mut keys = HashMap::new();
        let mut in_main_group = false;
        let mut seen_main_group = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(group) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                in_main_group = group == "Desktop Entry";
                seen_main_group |= in_main_group;
                continue;
            }
            if !in_main_group {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let key = key.trim();
            if key.contains('[') {
                continue;
            }
            // The first occurrence of a key wins, as in GLib's parser.
            keys.entry(key.to_string()).or_insert_with(|| value.trim().to_string());
        }
        seen_main_group.then(|| Self { path: path.to_path_buf(), keys })
    }

    fn raw(&self, key: &str) -> Option<&str> {
        self.keys.get(key).map(String::as_str)
    }

    pub fn string(&self, key: &str) -> Option<String> {
        self.raw(key).map(unescape)
    }

    pub fn boolean(&self, key: &str) -> bool {
        self.raw(key) == Some("true")
    }

    pub fn list(&self, key: &str) -> Vec<String> {
        self.raw(key).map(split_list).unwrap_or_default()
    }

    /// Returns the `Exec` command line as arguments, with field codes expanded for a launch without files.
    pub fn command_line(&self) -> Result<Vec<String>, SkipReason> {
        let exec =
            self.string("Exec").filter(|e| !e.trim().is_empty()).ok_or(SkipReason::NoExec)?;
        let args = split_exec(&exec).ok_or(SkipReason::MalformedExec)?;
        let mut argv = Vec::with_capacity(args.len());
        for arg in args {
            match arg.as_str() {
                "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
                "%i" => {
                    if let Some(icon) = self.string("Icon").filter(|i| !i.is_empty()) {
                        argv.push("--icon".to_string());
                        argv.push(icon);
                    }
                }
                _ => argv.push(self.expand_field_codes(&arg)),
            }
        }
        if argv.is_empty() { Err(SkipReason::MalformedExec) } else { Ok(argv) }
    }

    fn expand_field_codes(&self, arg: &str) -> String {
        let mut out = String::with_capacity(arg.len());
        let mut chars = arg.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('%') => out.push('%'),
                Some('c') => out.push_str(&self.string("Name").unwrap_or_default()),
                Some('k') => out.push_str(&self.path.to_string_lossy()),
                // Other codes expand to nothing inside a larger argument.
                _ => {}
            }
        }
        out
    }
}

/// Why an autostart entry doesn't start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    Hidden,
    Disabled,
    NotAnApplication,
    NotShownIn,
    TryExecMissing(String),
    Terminal,
    NoExec,
    MalformedExec,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hidden => f.write_str("it is hidden"),
            Self::Disabled => f.write_str("X-GNOME-Autostart-enabled is false"),
            Self::NotAnApplication => f.write_str("its type isn't Application"),
            Self::NotShownIn => f.write_str("OnlyShowIn or NotShowIn excludes this desktop"),
            Self::TryExecMissing(program) => {
                write!(f, "TryExec program '{program}' isn't installed")
            }
            Self::Terminal => write!(f, "it needs a terminal and {TERMINAL} isn't installed"),
            Self::NoExec => f.write_str("it has no Exec key"),
            Self::MalformedExec => f.write_str("its Exec key is malformed"),
        }
    }
}

/// Runs `Terminal=true` entries as `nimbus-terminal -e <command...>`.
const TERMINAL: &str = "nimbus-terminal";

/// One process to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// A name for log messages: the desktop file or the command line.
    pub label: String,
    pub argv: Vec<String>,
    pub working_dir: Option<PathBuf>,
}

/// Decides whether `entry` starts in a session whose `XDG_CURRENT_DESKTOP` names are `desktops`.
pub fn evaluate(
    entry: &DesktopEntry,
    desktops: &[&str],
    program_exists: impl Fn(&str) -> bool,
) -> Result<Launch, SkipReason> {
    if entry.boolean("Hidden") {
        return Err(SkipReason::Hidden);
    }
    if entry.raw("X-GNOME-Autostart-enabled") == Some("false") {
        return Err(SkipReason::Disabled);
    }
    if entry.raw("Type").is_some_and(|t| t != "Application") {
        return Err(SkipReason::NotAnApplication);
    }
    let shown_in = |key: &str| entry.list(key).iter().any(|d| desktops.contains(&d.as_str()));
    let only = entry.list("OnlyShowIn");
    if (!only.is_empty() && !shown_in("OnlyShowIn")) || shown_in("NotShowIn") {
        return Err(SkipReason::NotShownIn);
    }
    if let Some(try_exec) = entry.string("TryExec").filter(|t| !t.is_empty())
        && !program_exists(&try_exec)
    {
        return Err(SkipReason::TryExecMissing(try_exec));
    }
    let mut argv = entry.command_line()?;
    if entry.boolean("Terminal") {
        if !program_exists(TERMINAL) {
            return Err(SkipReason::Terminal);
        }
        argv.splice(0..0, [TERMINAL.to_owned(), "-e".to_owned()]);
    }
    let working_dir = entry.string("Path").filter(|p| !p.is_empty()).map(PathBuf::from);
    Ok(Launch { label: entry.path.display().to_string(), argv, working_dir })
}

/// Returns the autostart directories, most important first.
pub fn autostart_dirs(lookup: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let absolute = |v: String| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
    let config_home = lookup("XDG_CONFIG_HOME")
        .and_then(absolute)
        .or_else(|| lookup("HOME").and_then(absolute).map(|home| home.join(".config")));
    let config_dirs: Vec<PathBuf> = lookup("XDG_CONFIG_DIRS")
        .map(|dirs| dirs.split(':').filter_map(|d| absolute(d.to_string())).collect())
        .filter(|dirs: &Vec<PathBuf>| !dirs.is_empty())
        .unwrap_or_else(|| vec![PathBuf::from("/etc/xdg")]);
    config_home.into_iter().chain(config_dirs).map(|dir| dir.join("autostart")).collect()
}

/// Reads the `.desktop` files in `dirs`; a file name found in an earlier directory hides later ones.
pub fn discover(dirs: &[PathBuf]) -> Vec<DesktopEntry> {
    let mut chosen: BTreeMap<std::ffi::OsString, PathBuf> = BTreeMap::new();
    for dir in dirs {
        let Ok(read_dir) = std::fs::read_dir(dir) else { continue };
        for item in read_dir.flatten() {
            let path = item.path();
            if path.extension().is_some_and(|e| e == "desktop") {
                chosen.entry(item.file_name()).or_insert(path);
            }
        }
    }
    chosen
        .into_values()
        .filter_map(|path| match std::fs::read_to_string(&path) {
            Ok(text) => {
                let entry = DesktopEntry::parse(&path, &text);
                if entry.is_none() {
                    tracing::warn!("{} has no [Desktop Entry] group", path.display());
                }
                entry
            }
            Err(error) => {
                tracing::warn!("cannot read autostart entry {}: {error}", path.display());
                None
            }
        })
        .collect()
}

/// Runs each of the configuration's `autostart` command lines through `sh -c`.
pub fn config_launches(commands: &[String]) -> Vec<Launch> {
    commands
        .iter()
        .filter(|command| !command.trim().is_empty())
        .map(|command| Launch {
            label: command.clone(),
            argv: vec!["/bin/sh".into(), "-c".into(), command.clone()],
            working_dir: None,
        })
        .collect()
}

/// Lists the XDG entries in `dirs` that apply.
pub fn plan(
    dirs: &[PathBuf],
    desktops: &[&str],
    program_exists: impl Fn(&str) -> bool,
) -> Vec<Launch> {
    discover(dirs)
        .into_iter()
        .filter_map(|entry| match evaluate(&entry, desktops, &program_exists) {
            Ok(launch) => Some(launch),
            Err(reason) => {
                tracing::debug!("not autostarting {}: {reason}", entry.path.display());
                None
            }
        })
        .collect()
}

/// Applies the string escapes `\s`, `\n`, `\t`, `\r`, and `\\`.
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits a `;`-separated list, where `\;` is a literal semicolon.
fn split_list(raw: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(';') => current.push(';'),
                Some(other) => {
                    current.push('\\');
                    current.push(other);
                }
                None => current.push('\\'),
            },
            ';' => items.push(unescape(&std::mem::take(&mut current))),
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        items.push(unescape(&current));
    }
    items.retain(|item| !item.is_empty());
    items
}

/// Splits an unescaped `Exec` value into arguments; returns `None` for an unterminated quote.
fn split_exec(exec: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut current: Option<String> = None;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if let Some(arg) = current.take() {
                    args.push(arg);
                }
            }
            '"' => {
                let arg = current.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            escaped @ ('"' | '`' | '$' | '\\') => arg.push(escaped),
                            other => {
                                arg.push('\\');
                                arg.push(other);
                            }
                        },
                        other => arg.push(other),
                    }
                }
            }
            _ => current.get_or_insert_with(String::new).push(c),
        }
    }
    args.extend(current);
    Some(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> DesktopEntry {
        DesktopEntry::parse(Path::new("/xdg/autostart/test.desktop"), text).unwrap()
    }

    fn launch(text: &str) -> Result<Launch, SkipReason> {
        evaluate(&entry(text), &["Nimbus"], |_| true)
    }

    #[test]
    fn parses_main_group_only() {
        let e = entry(
            "# comment\n[Desktop Entry]\nName=Agent\nName[de]=Agent DE\nExec=agent\nExec=ignored\n\n[Desktop Action x]\nExec=other\n",
        );
        assert_eq!(e.string("Name").as_deref(), Some("Agent"));
        assert_eq!(e.string("Exec").as_deref(), Some("agent"));
        assert!(DesktopEntry::parse(Path::new("x"), "[Other]\nExec=x\n").is_none());
    }

    #[test]
    fn exec_quoting_and_field_codes() {
        let e = entry(
            "[Desktop Entry]\nName=My App\nIcon=my-icon\nExec=\"/opt/my app/run\" --name=%c %i --file %U 100%% \"say \\\\\"hi\\\\\"\"\n",
        );
        assert_eq!(
            e.command_line().unwrap(),
            vec![
                "/opt/my app/run",
                "--name=My App",
                "--icon",
                "my-icon",
                "--file",
                "100%",
                "say \"hi\""
            ]
        );
        assert_eq!(
            entry("[Desktop Entry]\nExec=run %k\n").command_line().unwrap(),
            vec!["run", "/xdg/autostart/test.desktop"]
        );
        assert_eq!(
            entry("[Desktop Entry]\nExec=\"unterminated\n").command_line(),
            Err(SkipReason::MalformedExec)
        );
        assert_eq!(
            entry("[Desktop Entry]\nExec=%U\n").command_line(),
            Err(SkipReason::MalformedExec)
        );
        assert_eq!(entry("[Desktop Entry]\nName=x\n").command_line(), Err(SkipReason::NoExec));
    }

    #[test]
    fn lists_and_escapes() {
        let e = entry("[Desktop Entry]\nOnlyShowIn=GNOME;Nimbus;\nX=a\\;b;c\\sd\n");
        assert_eq!(e.list("OnlyShowIn"), vec!["GNOME", "Nimbus"]);
        assert_eq!(e.list("X"), vec!["a;b", "c d"]);
    }

    #[test]
    fn filtering_rules() {
        let base = "[Desktop Entry]\nType=Application\nExec=agent --start\n";
        assert_eq!(launch(base).unwrap().argv, vec!["agent", "--start"]);
        assert_eq!(launch(&format!("{base}Hidden=true\n")), Err(SkipReason::Hidden));
        assert_eq!(
            launch(&format!("{base}X-GNOME-Autostart-enabled=false\n")),
            Err(SkipReason::Disabled)
        );
        assert!(launch(&format!("{base}X-GNOME-Autostart-enabled=true\n")).is_ok());
        assert_eq!(launch(&format!("{base}OnlyShowIn=KDE;GNOME;\n")), Err(SkipReason::NotShownIn));
        assert!(launch(&format!("{base}OnlyShowIn=KDE;Nimbus;\n")).is_ok());
        assert_eq!(launch(&format!("{base}NotShowIn=Nimbus;\n")), Err(SkipReason::NotShownIn));
        assert!(launch(&format!("{base}NotShowIn=KDE;\n")).is_ok());
        assert_eq!(
            launch("[Desktop Entry]\nType=Link\nURL=x\n"),
            Err(SkipReason::NotAnApplication)
        );
        let in_terminal = entry(&format!("{base}Terminal=true\n"));
        assert_eq!(
            evaluate(&in_terminal, &["Nimbus"], |_| true).unwrap().argv,
            vec!["nimbus-terminal", "-e", "agent", "--start"]
        );
        assert_eq!(
            evaluate(&in_terminal, &["Nimbus"], |p| p != "nimbus-terminal"),
            Err(SkipReason::Terminal)
        );
        let with_path = launch(&format!("{base}Path=/tmp\n")).unwrap();
        assert_eq!(with_path.working_dir, Some(PathBuf::from("/tmp")));

        let try_exec = entry(&format!("{base}TryExec=missing-tool\n"));
        assert_eq!(
            evaluate(&try_exec, &["Nimbus"], |p| p != "missing-tool"),
            Err(SkipReason::TryExecMissing("missing-tool".into()))
        );
        assert!(evaluate(&try_exec, &["Nimbus"], |_| true).is_ok());
    }

    #[test]
    fn config_commands_run_through_the_shell() {
        let launches = config_launches(&["nm-applet --indicator".into(), "  ".into()]);
        assert_eq!(
            launches,
            vec![Launch {
                label: "nm-applet --indicator".into(),
                argv: vec!["/bin/sh".into(), "-c".into(), "nm-applet --indicator".into()],
                working_dir: None,
            }]
        );
    }

    #[test]
    fn directories_follow_xdg_defaults() {
        let lookup = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
        };
        assert_eq!(
            autostart_dirs(lookup(&[("HOME", "/home/u")])),
            vec![PathBuf::from("/home/u/.config/autostart"), PathBuf::from("/etc/xdg/autostart")]
        );
        assert_eq!(
            autostart_dirs(lookup(&[
                ("HOME", "/home/u"),
                ("XDG_CONFIG_HOME", "/cfg"),
                ("XDG_CONFIG_DIRS", "/a:relative:/b")
            ])),
            vec![
                PathBuf::from("/cfg/autostart"),
                PathBuf::from("/a/autostart"),
                PathBuf::from("/b/autostart")
            ]
        );
    }

    #[test]
    fn user_entries_override_system_entries() {
        let user = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let write =
            |dir: &Path, name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        write(system.path(), "masked.desktop", "[Desktop Entry]\nType=Application\nExec=masked\n");
        write(
            user.path(),
            "masked.desktop",
            "[Desktop Entry]\nType=Application\nExec=masked\nHidden=true\n",
        );
        write(system.path(), "shown.desktop", "[Desktop Entry]\nType=Application\nExec=shown\n");
        write(
            system.path(),
            "gnome.desktop",
            "[Desktop Entry]\nType=Application\nExec=g\nOnlyShowIn=GNOME;\n",
        );
        write(system.path(), "notes.txt", "[Desktop Entry]\nExec=never\n");

        let dirs =
            [user.path().to_path_buf(), PathBuf::from("/nonexistent"), system.path().to_path_buf()];
        let plan = plan(&dirs, &["Nimbus"], |_| true);
        let argvs: Vec<_> = plan.iter().map(|l| l.argv.clone()).collect();
        assert_eq!(argvs, vec![vec!["shown".to_string()]]);
    }

    #[test]
    fn shipped_application_entries_are_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/applications");
        let mut count = 0;
        for item in std::fs::read_dir(&dir).unwrap() {
            let path = item.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            let e = DesktopEntry::parse(&path, &text).unwrap();
            let id = path.file_stem().unwrap().to_str().unwrap();
            assert!(id.starts_with("org.nimbus."), "{id}");
            assert!(e.string("Name").is_some() && e.string("Icon").is_some(), "{id}");
            let launch = evaluate(&e, &["Nimbus"], |_| true).unwrap();
            assert!(launch.argv[0].starts_with("nimbus-"), "{id}");
            count += 1;
        }
        assert_eq!(count, 4);

        let session = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/nimbus.desktop");
        let text = std::fs::read_to_string(&session).unwrap();
        let e = DesktopEntry::parse(&session, &text).unwrap();
        assert_eq!(e.command_line().unwrap(), vec!["nimbus-session"]);
        assert_eq!(e.list("DesktopNames"), vec!["Nimbus"]);
    }
}
