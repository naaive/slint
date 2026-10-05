// SPDX-License-Identifier: MIT

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::index::{data_home, system_data_dirs};
use crate::keyfile::KeyFile;

const FALLBACK_THEME: &str = "hicolor";

/// Extensions in theme directories, by priority. XPM is left out because Slint can't render it.
const THEME_EXTENSIONS: [&str; 2] = ["png", "svg"];
/// Extensions accepted in unthemed fallback directories such as `/usr/share/pixmaps`.
const FALLBACK_EXTENSIONS: [&str; 4] = ["png", "svg", "jpg", "jpeg"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirType {
    Fixed,
    Scalable,
    Threshold,
}

#[derive(Clone, Debug)]
struct ThemeDir {
    path: String,
    size: i64,
    scale: i64,
    kind: DirType,
    min_size: i64,
    max_size: i64,
    threshold: i64,
}

impl ThemeDir {
    fn matches_size(&self, size: i64, scale: i64) -> bool {
        if self.scale != scale {
            return false;
        }
        match self.kind {
            DirType::Fixed => self.size == size,
            DirType::Scalable => self.min_size <= size && size <= self.max_size,
            DirType::Threshold => {
                self.size - self.threshold <= size && size <= self.size + self.threshold
            }
        }
    }

    /// `DirectorySizeDistance` from the Icon Theme Specification, including its use of
    /// `MinSize` and `MaxSize` for threshold directories.
    fn size_distance(&self, size: i64, scale: i64) -> i64 {
        let target = size * scale;
        match self.kind {
            DirType::Fixed => (self.size * self.scale - target).abs(),
            DirType::Scalable | DirType::Threshold => {
                let (low, high) = match self.kind {
                    DirType::Scalable => (self.min_size, self.max_size),
                    _ => (self.size - self.threshold, self.size + self.threshold),
                };
                if target < low * self.scale {
                    self.min_size * self.scale - target
                } else if target > high * self.scale {
                    target - self.max_size * self.scale
                } else {
                    0
                }
            }
        }
    }
}

/// Where one icon file of a theme is.
#[derive(Clone, Copy, Debug)]
struct IconFile {
    dir: usize,
    base: usize,
    extension: usize,
}

/// A loaded theme with an index of every icon file in its directories.
#[derive(Debug)]
struct Theme {
    name: String,
    dirs: Vec<ThemeDir>,
    parents: Vec<String>,
    icons: HashMap<String, Vec<IconFile>>,
}

impl Theme {
    fn load(name: &str, bases: &[PathBuf]) -> Option<Self> {
        let index_path =
            bases.iter().map(|b| b.join(name).join("index.theme")).find(|p| p.is_file())?;
        let contents = std::fs::read(&index_path).ok()?;
        let keyfile = KeyFile::parse(&String::from_utf8_lossy(&contents));
        let main = keyfile.group("Icon Theme")?;
        let list = |key: &str| -> Vec<String> {
            main.raw(key)
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let mut seen = HashSet::new();
        let dirs: Vec<ThemeDir> = list("Directories")
            .into_iter()
            .chain(list("ScaledDirectories"))
            .filter(|d| seen.insert(d.clone()))
            .filter_map(|path| {
                let group = keyfile.group(&path)?;
                let size = group.integer("Size").filter(|s| *s > 0)?;
                let kind = match group.raw("Type").map(str::trim) {
                    Some("Fixed") => DirType::Fixed,
                    Some("Scalable") => DirType::Scalable,
                    _ => DirType::Threshold,
                };
                Some(ThemeDir {
                    size,
                    scale: group.integer("Scale").filter(|s| *s > 0).unwrap_or(1),
                    kind,
                    min_size: group.integer("MinSize").unwrap_or(size),
                    max_size: group.integer("MaxSize").unwrap_or(size),
                    threshold: group.integer("Threshold").unwrap_or(2),
                    path,
                })
            })
            .collect();
        let parents = list("Inherits");

        let mut icons: HashMap<String, Vec<IconFile>> = HashMap::new();
        for (dir_index, dir) in dirs.iter().enumerate() {
            for (base_index, base) in bases.iter().enumerate() {
                let Ok(read_dir) = std::fs::read_dir(base.join(name).join(&dir.path)) else {
                    continue;
                };
                for file in read_dir.filter_map(Result::ok) {
                    let file_name = file.file_name();
                    let Some((stem, ext)) = file_name.to_str().and_then(|n| n.rsplit_once('.'))
                    else {
                        continue;
                    };
                    let Some(extension) = THEME_EXTENSIONS.iter().position(|e| *e == ext) else {
                        continue;
                    };
                    icons.entry(stem.to_owned()).or_default().push(IconFile {
                        dir: dir_index,
                        base: base_index,
                        extension,
                    });
                }
            }
        }
        for files in icons.values_mut() {
            files.sort_by_key(|f| (f.dir, f.base, f.extension));
        }
        Some(Self { name: name.to_owned(), dirs, parents, icons })
    }

    /// `LookupIcon` from the specification: an exact size match in directory order, else the closest size.
    fn lookup(&self, icon: &str, size: i64, scale: i64, bases: &[PathBuf]) -> Option<PathBuf> {
        let files = self.icons.get(icon)?;
        let path = |f: &IconFile| {
            let dir = &self.dirs[f.dir];
            bases[f.base]
                .join(&self.name)
                .join(&dir.path)
                .join(format!("{icon}.{}", THEME_EXTENSIONS[f.extension]))
        };
        if let Some(exact) = files.iter().find(|f| self.dirs[f.dir].matches_size(size, scale)) {
            return Some(path(exact));
        }
        // `min_by_key` keeps the first of equal elements, preserving directory order on ties.
        files.iter().min_by_key(|f| self.dirs[f.dir].size_distance(size, scale)).map(path)
    }
}

type CacheKey = (String, u32, u32);

struct Inner {
    theme: String,
    /// Directories holding themes, highest precedence first.
    bases: Vec<PathBuf>,
    /// Directories searched for unthemed icons after all themes.
    fallback_dirs: Vec<PathBuf>,
    themes: Mutex<HashMap<String, Option<Arc<Theme>>>>,
    cache: Mutex<HashMap<CacheKey, Option<PathBuf>>>,
}

/// Icon lookup following the theme inheritance chain, falling back to `hicolor` and `pixmaps`.
///
/// Clones share the loaded themes and the lookup cache.
#[derive(Clone)]
pub struct IconResolver {
    inner: Arc<Inner>,
}

impl fmt::Debug for IconResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IconResolver")
            .field("theme", &self.inner.theme)
            .field("bases", &self.inner.bases)
            .field("fallback_dirs", &self.inner.fallback_dirs)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded maps stay consistent even if a holder panicked, so a poisoned lock is still usable.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl IconResolver {
    /// Searches `$HOME/.icons`, `icons/` below the XDG data directories, and `/usr/share/pixmaps`.
    pub fn new(theme: &str) -> Self {
        let mut bases: Vec<PathBuf> =
            dirs::home_dir().map(|home| home.join(".icons")).into_iter().collect();
        bases
            .extend(data_home().into_iter().chain(system_data_dirs()).map(|dir| dir.join("icons")));
        let mut seen = HashSet::new();
        bases.retain(|b| seen.insert(b.clone()));
        Self::from_parts(theme, bases, vec![PathBuf::from("/usr/share/pixmaps")])
    }

    /// Like [`IconResolver::new`], searching `icons/` below `data_dirs` instead of the XDG ones.
    /// Unthemed icons are looked up in `pixmaps/` below `data_dirs`.
    pub fn with_data_dirs(theme: &str, data_dirs: &[PathBuf]) -> Self {
        Self::from_parts(
            theme,
            data_dirs.iter().map(|d| d.join("icons")).collect(),
            data_dirs.iter().map(|d| d.join("pixmaps")).collect(),
        )
    }

    fn from_parts(theme: &str, bases: Vec<PathBuf>, pixmaps: Vec<PathBuf>) -> Self {
        let theme = if theme.trim().is_empty() { FALLBACK_THEME } else { theme.trim() };
        let mut fallback_dirs = bases.clone();
        fallback_dirs.extend(pixmaps);
        Self {
            inner: Arc::new(Inner {
                theme: theme.to_owned(),
                bases,
                fallback_dirs,
                themes: Mutex::new(HashMap::new()),
                cache: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn theme_name(&self) -> &str {
        &self.inner.theme
    }

    /// Forgets loaded themes and cached lookups, for example after icons were installed.
    pub fn clear_cache(&self) {
        lock(&self.inner.themes).clear();
        lock(&self.inner.cache).clear();
    }

    /// Returns the best file for `icon` (a name or absolute path) at `size` logical pixels and `scale`.
    /// Prefers PNG and SVG; results are cached.
    pub fn lookup(&self, icon: &str, size: u32, scale: u32) -> Option<PathBuf> {
        let icon = icon.trim();
        if icon.is_empty() {
            return None;
        }
        if icon.starts_with('/') {
            let path = Path::new(icon);
            return path.is_file().then(|| path.to_path_buf());
        }
        if icon.contains('/') {
            return None;
        }
        let (size, scale) = (size.max(1), scale.max(1));
        let key = (icon.to_owned(), size, scale);
        if let Some(cached) = lock(&self.inner.cache).get(&key) {
            return cached.clone();
        }
        let result = self.find(icon, size, scale).or_else(|| {
            // Tolerates `Icon=name.png`, which the specification forbids but some applications ship.
            let stem = icon
                .rsplit_once('.')
                .filter(|(_, ext)| FALLBACK_EXTENSIONS.contains(ext) || *ext == "xpm")?
                .0;
            self.find(stem, size, scale)
        });
        lock(&self.inner.cache).insert(key, result.clone());
        result
    }

    fn theme(&self, name: &str) -> Option<Arc<Theme>> {
        if let Some(theme) = lock(&self.inner.themes).get(name) {
            return theme.clone();
        }
        // Loading reads many directories, so it runs without holding the lock;
        // a concurrent load of the same theme only costs duplicate work.
        let loaded = Theme::load(name, &self.inner.bases).map(Arc::new);
        lock(&self.inner.themes).entry(name.to_owned()).or_insert(loaded).clone()
    }

    /// `FindIcon` from the specification: the theme, its parents depth first, `hicolor`, then unthemed directories.
    fn find(&self, icon: &str, size: u32, scale: u32) -> Option<PathBuf> {
        let mut visited = HashSet::new();
        let (size, scale) = (i64::from(size), i64::from(scale));
        self.find_in_theme(&self.inner.theme, icon, size, scale, &mut visited)
            .or_else(|| self.find_in_theme(FALLBACK_THEME, icon, size, scale, &mut visited))
            .or_else(|| self.find_unthemed(icon))
    }

    fn find_in_theme(
        &self,
        name: &str,
        icon: &str,
        size: i64,
        scale: i64,
        visited: &mut HashSet<String>,
    ) -> Option<PathBuf> {
        if !visited.insert(name.to_owned()) {
            return None;
        }
        let theme = self.theme(name)?;
        theme.lookup(icon, size, scale, &self.inner.bases).or_else(|| {
            theme
                .parents
                .iter()
                .find_map(|parent| self.find_in_theme(parent, icon, size, scale, visited))
        })
    }

    fn find_unthemed(&self, icon: &str) -> Option<PathBuf> {
        self.inner.fallback_dirs.iter().find_map(|dir| {
            FALLBACK_EXTENSIONS
                .iter()
                .map(|ext| dir.join(format!("{icon}.{ext}")))
                .find(|p| p.is_file())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, b"").expect("write");
    }

    fn theme(base: &Path, name: &str, index: &str) {
        let dir = base.join("icons").join(name);
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(dir.join("index.theme"), index).expect("write");
    }

    fn icon(base: &Path, theme: &str, dir: &str, file: &str) -> PathBuf {
        let path = base.join("icons").join(theme).join(dir).join(file);
        touch(&path);
        path
    }

    const HICOLOR: &str = "[Icon Theme]\nName=Hicolor\nDirectories=16x16/apps,48x48/apps,scalable/apps\n\n\
        [16x16/apps]\nSize=16\nType=Threshold\n\n[48x48/apps]\nSize=48\nType=Fixed\n\n\
        [scalable/apps]\nSize=128\nMinSize=8\nMaxSize=512\nType=Scalable\n";

    #[test]
    fn exact_size_and_closest_distance() {
        let data = tempfile::tempdir().expect("tempdir");
        theme(data.path(), "hicolor", HICOLOR);
        let small = icon(data.path(), "hicolor", "16x16/apps", "app.png");
        let large = icon(data.path(), "hicolor", "48x48/apps", "app.png");
        let scalable = icon(data.path(), "hicolor", "scalable/apps", "vector.svg");
        let resolver = IconResolver::with_data_dirs("hicolor", &[data.path().to_path_buf()]);
        assert_eq!(resolver.lookup("app", 16, 1), Some(small.clone()));
        // Threshold 2 around 16 still matches exactly.
        assert_eq!(resolver.lookup("app", 18, 1), Some(small.clone()));
        assert_eq!(resolver.lookup("app", 48, 1), Some(large.clone()));
        assert_eq!(resolver.lookup("app", 40, 1), Some(large.clone()));
        assert_eq!(resolver.lookup("app", 24, 1), Some(small));
        assert_eq!(resolver.lookup("app", 96, 1), Some(large));
        assert_eq!(resolver.lookup("vector", 256, 1), Some(scalable));
        assert_eq!(resolver.lookup("missing", 16, 1), None);
    }

    #[test]
    fn png_preferred_over_svg_and_xpm_skipped() {
        let data = tempfile::tempdir().expect("tempdir");
        theme(data.path(), "hicolor", HICOLOR);
        icon(data.path(), "hicolor", "48x48/apps", "both.svg");
        let png = icon(data.path(), "hicolor", "48x48/apps", "both.png");
        icon(data.path(), "hicolor", "48x48/apps", "old.xpm");
        let resolver = IconResolver::with_data_dirs("hicolor", &[data.path().to_path_buf()]);
        assert_eq!(resolver.lookup("both", 48, 1), Some(png));
        assert_eq!(resolver.lookup("old", 48, 1), None);
    }

    #[test]
    fn scale_selects_scaled_directories() {
        let data = tempfile::tempdir().expect("tempdir");
        theme(
            data.path(),
            "hicolor",
            "[Icon Theme]\nDirectories=32x32/apps\nScaledDirectories=32x32@2/apps\n\n\
             [32x32/apps]\nSize=32\nType=Fixed\n\n[32x32@2/apps]\nSize=32\nScale=2\nType=Fixed\n",
        );
        let one = icon(data.path(), "hicolor", "32x32/apps", "app.png");
        let two = icon(data.path(), "hicolor", "32x32@2/apps", "app.png");
        let resolver = IconResolver::with_data_dirs("hicolor", &[data.path().to_path_buf()]);
        assert_eq!(resolver.lookup("app", 32, 1), Some(one.clone()));
        assert_eq!(resolver.lookup("app", 32, 2), Some(two.clone()));
        // 64 physical pixels: 32@2 is an exact distance-0 fit, closer than 32@1.
        assert_eq!(resolver.lookup("app", 64, 1), Some(two));
        assert_eq!(resolver.lookup("app", 0, 0), Some(one));
    }

    #[test]
    fn inheritance_with_cycles_then_hicolor_then_pixmaps() {
        let data = tempfile::tempdir().expect("tempdir");
        let dirs = "Directories=48/apps\n\n[48/apps]\nSize=48\n";
        theme(data.path(), "Child", &format!("[Icon Theme]\nInherits=Parent,Missing\n{dirs}"));
        theme(data.path(), "Parent", &format!("[Icon Theme]\nInherits=Child\n{dirs}"));
        theme(data.path(), "hicolor", HICOLOR);
        let own = icon(data.path(), "Child", "48/apps", "own.png");
        let inherited = icon(data.path(), "Parent", "48/apps", "inherited.png");
        let shadowed = icon(data.path(), "Child", "48/apps", "both.png");
        icon(data.path(), "Parent", "48/apps", "both.png");
        let fallback = icon(data.path(), "hicolor", "48x48/apps", "fallback.png");
        let pixmap = data.path().join("pixmaps/legacy.png");
        touch(&pixmap);
        let resolver = IconResolver::with_data_dirs("Child", &[data.path().to_path_buf()]);
        assert_eq!(resolver.lookup("own", 48, 1), Some(own));
        assert_eq!(resolver.lookup("inherited", 48, 1), Some(inherited));
        assert_eq!(resolver.lookup("both", 48, 1), Some(shadowed));
        assert_eq!(resolver.lookup("fallback", 48, 1), Some(fallback));
        assert_eq!(resolver.lookup("legacy", 48, 1), Some(pixmap.clone()));
        assert_eq!(resolver.lookup("legacy.png", 48, 1), Some(pixmap));
        assert_eq!(resolver.lookup("nowhere", 48, 1), None);
    }

    #[test]
    fn theme_files_merge_across_base_directories() {
        let user = tempfile::tempdir().expect("tempdir");
        let system = tempfile::tempdir().expect("tempdir");
        theme(system.path(), "hicolor", HICOLOR);
        let user_icon = icon(user.path(), "hicolor", "48x48/apps", "user-app.png");
        let resolver = IconResolver::with_data_dirs(
            "hicolor",
            &[user.path().to_path_buf(), system.path().to_path_buf()],
        );
        assert_eq!(resolver.lookup("user-app", 48, 1), Some(user_icon));
    }

    #[test]
    fn absolute_paths_and_cache() {
        let data = tempfile::tempdir().expect("tempdir");
        let file = data.path().join("abs.png");
        touch(&file);
        let resolver = IconResolver::with_data_dirs("", &[data.path().to_path_buf()]);
        assert_eq!(resolver.theme_name(), "hicolor");
        assert_eq!(resolver.lookup(&file.to_string_lossy(), 48, 1), Some(file.clone()));
        assert_eq!(resolver.lookup("/does/not/exist.png", 48, 1), None);
        assert_eq!(resolver.lookup("relative/path", 48, 1), None);
        assert_eq!(resolver.lookup("", 48, 1), None);

        theme(data.path(), "hicolor", HICOLOR);
        assert_eq!(resolver.lookup("late", 48, 1), None);
        let late = icon(data.path(), "hicolor", "48x48/apps", "late.png");
        let clone = resolver.clone();
        assert_eq!(clone.lookup("late", 48, 1), None, "the miss is cached");
        resolver.clear_cache();
        assert_eq!(clone.lookup("late", 48, 1), Some(late));
    }

    #[test]
    fn resolver_is_shareable() {
        fn assert_traits<T: Clone + Send + Sync + fmt::Debug>() {}
        assert_traits::<IconResolver>();
        let resolver = IconResolver::with_data_dirs("hicolor", &[]);
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let resolver = resolver.clone();
                std::thread::spawn(move || resolver.lookup("x", 16, 1))
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().ok().flatten(), None);
        }
        assert!(format!("{resolver:?}").contains("hicolor"));
    }
}
