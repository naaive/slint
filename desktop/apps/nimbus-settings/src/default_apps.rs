// SPDX-License-Identifier: MIT

//! The Default Applications page's logic: the kinds of applications, the installed choices for each,
//! and choosing one through `nimbus-xdg`.

use std::collections::HashSet;
use std::io;
use std::sync::Mutex;

use nimbus_xdg::{AppIndex, DesktopEntry, MimeLookup, ParseOptions};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Web,
    Mail,
    Files,
    Terminal,
    Text,
    Images,
    Video,
    Music,
}

impl Category {
    pub const ALL: [Category; 8] = [
        Category::Web,
        Category::Mail,
        Category::Files,
        Category::Terminal,
        Category::Text,
        Category::Images,
        Category::Video,
        Category::Music,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Category::Web => "Web browser",
            Category::Mail => "Mail",
            Category::Files => "Files",
            Category::Terminal => "Terminal",
            Category::Text => "Text editor",
            Category::Images => "Images",
            Category::Video => "Video",
            Category::Music => "Music",
        }
    }

    /// The MIME types a choice applies to, starting with the one whose default is the current choice.
    /// The terminal has none; `xdg-terminals.list` names it.
    pub fn mime_types(self) -> &'static [&'static str] {
        match self {
            Category::Web => &[
                "x-scheme-handler/http",
                "x-scheme-handler/https",
                "text/html",
                "application/xhtml+xml",
            ],
            Category::Mail => &["x-scheme-handler/mailto"],
            Category::Files => &["inode/directory"],
            Category::Terminal => &[],
            Category::Text => &["text/plain"],
            Category::Images => &[
                "image/png",
                "image/jpeg",
                "image/gif",
                "image/webp",
                "image/bmp",
                "image/tiff",
                "image/svg+xml",
            ],
            Category::Video => &[
                "video/mp4",
                "video/webm",
                "video/x-matroska",
                "video/mpeg",
                "video/quicktime",
                "video/x-msvideo",
                "video/ogg",
            ],
            Category::Music => &[
                "audio/mpeg",
                "audio/flac",
                "audio/ogg",
                "audio/x-vorbis+ogg",
                "audio/mp4",
                "audio/x-wav",
                "audio/webm",
            ],
        }
    }
}

/// An installed application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// The desktop file id without `.desktop`.
    pub id: String,
    pub name: String,
}

/// The applications of one category, and which of them is the default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choices {
    pub category: Category,
    pub apps: Vec<App>,
    /// The default's position in `apps`; `None` only when `apps` is empty.
    pub current: Option<usize>,
}

/// Where default applications are read from and written to.
pub trait AppDefaults: Send + Sync + 'static {
    /// The choices of every category, in [`Category::ALL`] order; may block.
    fn load(&self) -> Vec<Choices>;
    /// Makes `id` the default of `category`; may block.
    fn set(&self, category: Category, id: &str) -> io::Result<()>;
}

/// The defaults in the XDG directories: `mimeapps.list` and `xdg-terminals.list`.
pub struct XdgDefaults {
    pub lookup: MimeLookup,
}

impl XdgDefaults {
    fn index(&self) -> AppIndex {
        let options =
            ParseOptions { include_hidden_from_menus: true, ..self.lookup.options.clone() };
        AppIndex::scan_dirs_with(&self.lookup.data_dirs, &options)
    }

    fn choices(&self, index: &AppIndex, category: Category) -> Choices {
        let (ids, default) = match category.mime_types().first() {
            Some(mime) => {
                let default = self.lookup.default_handler(mime);
                (default.iter().cloned().chain(self.lookup.handlers(mime)).collect(), default)
            }
            None => {
                let terminals = self.lookup.terminals();
                let default = terminals.first().cloned();
                (terminals, default)
            }
        };
        let mut seen = HashSet::new();
        let mut apps: Vec<App> = ids
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .filter_map(|id| index.get(&id))
            .map(|entry| App { id: entry.id.clone(), name: entry.name.clone() })
            .collect();
        apps.sort_by_key(|app| app.name.to_lowercase());
        let current = default.and_then(|id| apps.iter().position(|app| app.id == id));
        Choices { category, current: current.or((!apps.is_empty()).then_some(0)), apps }
    }
}

/// Whether `entry` declares `mime`, itself or through a `type/*` wildcard.
fn handles(entry: &DesktopEntry, mime: &str) -> bool {
    let major = mime.split_once('/').map(|(major, _)| major);
    entry.mime_types.iter().any(|declared| {
        declared.eq_ignore_ascii_case(mime)
            || declared.strip_suffix("/*").is_some_and(|m| Some(m) == major)
    })
}

impl AppDefaults for XdgDefaults {
    fn load(&self) -> Vec<Choices> {
        let index = self.index();
        Category::ALL.into_iter().map(|category| self.choices(&index, category)).collect()
    }

    fn set(&self, category: Category, id: &str) -> io::Result<()> {
        let Some((primary, others)) = category.mime_types().split_first() else {
            return self.lookup.set_default_terminal(id);
        };
        // The application takes the other types it declares, so a browser doesn't take XHTML it can't show.
        let index = self.index();
        let entry = index.get(id);
        let mimes: Vec<&str> = std::iter::once(*primary)
            .chain(others.iter().copied().filter(|mime| entry.is_some_and(|e| handles(e, mime))))
            .collect();
        self.lookup.set_default(&mimes, id)
    }
}

/// Fixed applications for screenshots and tests.
pub struct SampleDefaults {
    choices: Mutex<Vec<Choices>>,
}

impl Default for SampleDefaults {
    fn default() -> Self {
        let apps = |apps: &[(&str, &str)]| -> Vec<App> {
            apps.iter().map(|(id, name)| App { id: (*id).into(), name: (*name).into() }).collect()
        };
        let choices = Category::ALL
            .into_iter()
            .map(|category| {
                let apps = apps(match category {
                    Category::Web => {
                        &[("org.chromium.Chromium", "Chromium"), ("firefox", "Firefox")]
                    }
                    Category::Mail => {
                        &[("org.gnome.Geary", "Geary"), ("thunderbird", "Thunderbird")]
                    }
                    Category::Files => &[("org.nimbus.Files", "Files")],
                    Category::Terminal => &[("foot", "Foot"), ("org.nimbus.Terminal", "Terminal")],
                    Category::Text => {
                        &[("org.gnome.TextEditor", "Text Editor"), ("nvim", "Neovim")]
                    }
                    Category::Images => &[("org.gnome.Loupe", "Image Viewer"), ("gimp", "GIMP")],
                    Category::Video => &[("mpv", "mpv"), ("org.gnome.Totem", "Videos")],
                    Category::Music => &[],
                });
                let current = match category {
                    Category::Web | Category::Terminal => Some(1),
                    _ => (!apps.is_empty()).then_some(0),
                };
                Choices { category, apps, current }
            })
            .collect();
        Self { choices: Mutex::new(choices) }
    }
}

impl AppDefaults for SampleDefaults {
    fn load(&self) -> Vec<Choices> {
        self.choices.lock().map(|choices| choices.clone()).unwrap_or_default()
    }

    fn set(&self, category: Category, id: &str) -> io::Result<()> {
        let mut choices = self.choices.lock().map_err(|_| io::Error::other("poisoned"))?;
        let choices = choices.iter_mut().find(|c| c.category == category);
        let Some(choices) = choices else { return Err(io::ErrorKind::NotFound.into()) };
        let index = choices.apps.iter().position(|app| app.id == id);
        choices.current = Some(index.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn app(dir: &Path, id: &str, name: &str, keys: &str) {
        let path = dir.join("applications").join(format!("{id}.desktop"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!("[Desktop Entry]\nType=Application\nName={name}\nExec={id} %U\n{keys}"),
        )
        .unwrap();
    }

    #[test]
    fn loads_and_sets_defaults_in_xdg_directories() {
        let config = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let browser_types = "MimeType=x-scheme-handler/http;x-scheme-handler/https;text/html;\n";
        app(data.path(), "firefox", "Firefox", browser_types);
        app(data.path(), "chromium", "Chromium", &format!("{browser_types}NoDisplay=true\n"));
        app(data.path(), "foot", "Foot", "Categories=System;TerminalEmulator;\n");
        app(data.path(), "kitty", "kitty", "Categories=System;TerminalEmulator;\n");
        app(data.path(), "viewer", "Image Viewer", "MimeType=image/*;\n");
        let defaults = XdgDefaults {
            lookup: MimeLookup {
                config_dirs: vec![config.path().to_path_buf()],
                data_dirs: vec![data.path().to_path_buf()],
                options: ParseOptions { desktops: vec!["Nimbus".into()], ..Default::default() },
            },
        };
        let names = |choices: &Choices| -> Vec<String> {
            choices.apps.iter().map(|app| app.name.clone()).collect()
        };
        let loaded = defaults.load();
        assert_eq!(loaded.len(), Category::ALL.len());
        let web = &loaded[0];
        assert_eq!(names(web), ["Chromium", "Firefox"], "hidden handlers count");
        assert!(web.current.is_some());
        let mail = &loaded[1];
        assert_eq!((mail.apps.len(), mail.current), (0, None));
        assert_eq!(names(&loaded[3]), ["Foot", "kitty"]);
        assert_eq!(names(&loaded[5]), ["Image Viewer"], "wildcard types count");

        defaults.set(Category::Web, "firefox").unwrap();
        let list = fs::read_to_string(config.path().join("mimeapps.list")).unwrap();
        assert_eq!(
            list,
            "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\nx-scheme-handler/https=firefox.desktop;\ntext/html=firefox.desktop;\n",
            "only the types Firefox declares"
        );
        let web = &defaults.load()[0];
        assert_eq!(web.apps[web.current.unwrap()].id, "firefox");

        defaults.set(Category::Terminal, "kitty").unwrap();
        let terminal = &defaults.load()[3];
        assert_eq!(terminal.apps[terminal.current.unwrap()].id, "kitty");
        assert_eq!(
            fs::read_to_string(config.path().join("xdg-terminals.list")).unwrap(),
            "kitty.desktop\n"
        );
    }

    #[test]
    fn samples_cover_every_category() {
        let sample = SampleDefaults::default();
        let loaded = sample.load();
        assert_eq!(loaded.iter().map(|c| c.category).collect::<Vec<_>>(), Category::ALL);
        sample.set(Category::Mail, "thunderbird").unwrap();
        assert_eq!(sample.load()[1].current, Some(1));
        assert!(sample.set(Category::Mail, "nothing").is_err());
    }
}
