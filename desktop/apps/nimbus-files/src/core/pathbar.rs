// SPDX-License-Identifier: MIT

//! The segments of the breadcrumb path bar.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    /// The file system root, labeled "Computer".
    Root,
    Home,
    Dir,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub label: String,
    pub path: PathBuf,
    pub kind: SegmentKind,
}

/// Splits a folder path into clickable segments, starting at the home folder when it's inside it.
pub fn segments(path: &Path, home: &Path) -> Vec<Segment> {
    let (mut segments, rest, mut current) = match path.strip_prefix(home) {
        Ok(rest) if home.components().count() > 1 => (
            vec![Segment {
                label: "Home".into(),
                path: home.to_path_buf(),
                kind: SegmentKind::Home,
            }],
            rest,
            home.to_path_buf(),
        ),
        _ => (
            vec![Segment {
                label: "Computer".into(),
                path: PathBuf::from("/"),
                kind: SegmentKind::Root,
            }],
            path.strip_prefix("/").unwrap_or(path),
            PathBuf::from("/"),
        ),
    };
    for component in rest.components() {
        if let Component::Normal(name) = component {
            current.push(name);
            segments.push(Segment {
                label: name.to_string_lossy().into_owned(),
                path: current.clone(),
                kind: SegmentKind::Dir,
            });
        }
    }
    segments
}

/// Resolves text typed into the location field against the current folder.
///
/// Supports `~`, `~/…`, `file://` URIs, and relative paths; `.` and `..` are normalized lexically.
pub fn resolve_typed(text: &str, current: &Path, home: &Path) -> Option<PathBuf> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let raw = if let Some(path) = super::uri::uri_to_path(text) {
        path
    } else if text == "~" {
        home.to_path_buf()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home.join(rest)
    } else {
        current.join(text)
    };
    let mut normalized = PathBuf::from("/");
    for component in raw.components() {
        match component {
            Component::Normal(name) => normalized.push(name),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(path: &str) -> Vec<(String, SegmentKind)> {
        segments(Path::new(path), Path::new("/home/ada"))
            .into_iter()
            .map(|s| (s.label, s.kind))
            .collect()
    }

    #[test]
    fn segments_under_home_and_root() {
        assert_eq!(labels("/home/ada"), [("Home".into(), SegmentKind::Home)]);
        assert_eq!(
            labels("/home/ada/Documents/Work"),
            [
                ("Home".into(), SegmentKind::Home),
                ("Documents".into(), SegmentKind::Dir),
                ("Work".into(), SegmentKind::Dir)
            ]
        );
        assert_eq!(
            labels("/usr/share"),
            [
                ("Computer".into(), SegmentKind::Root),
                ("usr".into(), SegmentKind::Dir),
                ("share".into(), SegmentKind::Dir)
            ]
        );
        assert_eq!(labels("/home/adam").len(), 3);
        let last = segments(Path::new("/home/ada/a/b"), Path::new("/home/ada"));
        assert_eq!(last[2].path, PathBuf::from("/home/ada/a/b"));
        // A home of "/" mustn't swallow every path.
        assert_eq!(segments(Path::new("/etc"), Path::new("/"))[0].kind, SegmentKind::Root);
    }

    #[test]
    fn typed_paths() {
        let cur = Path::new("/home/ada/Documents");
        let home = Path::new("/home/ada");
        assert_eq!(resolve_typed("~", cur, home), Some(PathBuf::from("/home/ada")));
        assert_eq!(resolve_typed(" ~/Music ", cur, home), Some(PathBuf::from("/home/ada/Music")));
        assert_eq!(
            resolve_typed("../Pictures", cur, home),
            Some(PathBuf::from("/home/ada/Pictures"))
        );
        assert_eq!(resolve_typed("/etc/./x/..", cur, home), Some(PathBuf::from("/etc")));
        assert_eq!(resolve_typed("/../..", cur, home), Some(PathBuf::from("/")));
        assert_eq!(resolve_typed("file:///tmp/a%20b", cur, home), Some(PathBuf::from("/tmp/a b")));
        assert_eq!(resolve_typed("  ", cur, home), None);
    }
}
