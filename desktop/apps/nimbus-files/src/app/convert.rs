// SPDX-License-Identifier: MIT

//! Conversions from the model to the UI's structs.

use std::path::Path;

use slint::{Image, SharedString};

use crate::core::entry::FileEntry;
use crate::core::format;
use crate::core::menu::MenuEntry;
use crate::core::mime::{self, Category};
use crate::core::names;
use crate::core::pathbar::{Segment, SegmentKind};
use crate::core::places::{self, Place, PlaceIcon, Section, Target};
use crate::{Crumb, FileItem, IconKind, MenuRow, PlaceItem};

/// Folders with their own icon, such as the XDG user folders.
#[derive(Clone, Debug, Default)]
pub struct SpecialFolders {
    pub folders: Vec<(std::path::PathBuf, IconKind)>,
}

impl SpecialFolders {
    pub fn from_places(places: &[Place]) -> Self {
        let folders = places
            .iter()
            .filter(|p| p.section == Section::Places)
            .filter_map(|p| match &p.target {
                Target::Dir(path) => Some((path.clone(), place_icon(p.icon))),
                Target::Trash | Target::Volume(_) => None,
            })
            .collect();
        Self { folders }
    }

    fn icon_for(&self, path: &Path) -> Option<IconKind> {
        self.folders.iter().find(|(p, _)| p == path).map(|(_, icon)| *icon)
    }
}

pub fn place_icon(icon: PlaceIcon) -> IconKind {
    match icon {
        PlaceIcon::Home => IconKind::FolderHome,
        PlaceIcon::Desktop => IconKind::FolderDesktop,
        PlaceIcon::Documents => IconKind::FolderDocuments,
        PlaceIcon::Downloads => IconKind::FolderDownloads,
        PlaceIcon::Music => IconKind::FolderMusic,
        PlaceIcon::Pictures => IconKind::FolderPictures,
        PlaceIcon::Videos => IconKind::FolderVideos,
        PlaceIcon::Trash => IconKind::Trash,
        PlaceIcon::Computer => IconKind::Computer,
        PlaceIcon::Drive => IconKind::Drive,
        PlaceIcon::Bookmark => IconKind::Bookmark,
    }
}

pub fn category_icon(category: Category) -> IconKind {
    match category {
        Category::Folder => IconKind::Folder,
        Category::Text => IconKind::Text,
        Category::Document => IconKind::Document,
        Category::Image => IconKind::Image,
        Category::Audio => IconKind::Audio,
        Category::Video => IconKind::Video,
        Category::Archive => IconKind::Archive,
        Category::Executable => IconKind::Executable,
        Category::Script => IconKind::Script,
        Category::Generic => IconKind::File,
    }
}

pub fn entry_icon(entry: &FileEntry, special: &SpecialFolders) -> IconKind {
    if entry.mime == mime::SYMLINK_BROKEN {
        return IconKind::BrokenLink;
    }
    if entry.is_dir()
        && let Some(icon) = special.icon_for(&entry.path)
    {
        return icon;
    }
    category_icon(entry.category)
}

/// The uppercase extension shown on large icons, at most four characters.
pub fn badge(entry: &FileEntry) -> SharedString {
    if entry.is_dir() {
        return SharedString::new();
    }
    let (_, ext) = names::split_extension(&entry.name);
    let ext = ext.rsplit('.').next().unwrap_or_default();
    if ext.is_empty() || ext.chars().count() > 4 {
        return SharedString::new();
    }
    ext.to_uppercase().into()
}

/// What the size column shows: bytes for files, the item count for folders.
pub fn size_label(entry: &FileEntry) -> String {
    if entry.is_dir() {
        entry.child_count.map(format::item_count).unwrap_or_default()
    } else if entry.kind == crate::core::entry::EntryKind::File {
        format::size(entry.size)
    } else {
        String::new()
    }
}

/// Options for converting an entry.
pub struct ItemContext<'a> {
    pub special: &'a SpecialFolders,
    pub now: chrono::NaiveDateTime,
    /// Text for the location column, if the view has one.
    pub location: Option<String>,
    pub selected: bool,
    pub cut: bool,
    pub thumbnail: Option<Image>,
    pub thumbnail_failed: bool,
}

pub fn file_item(entry: &FileEntry, ctx: ItemContext<'_>) -> FileItem {
    let thumbnailable =
        entry.kind == crate::core::entry::EntryKind::File && mime::is_thumbnailable(&entry.mime);
    let has_thumbnail = ctx.thumbnail.is_some();
    FileItem {
        name: entry.name.as_str().into(),
        path: entry.path.to_string_lossy().as_ref().into(),
        kind: entry_icon(entry, ctx.special),
        badge: badge(entry),
        type_label: mime::describe(&entry.mime).into(),
        size_label: size_label(entry).into(),
        modified_label: entry
            .modified
            .map(|t| format::short_date(format::local_time(t), ctx.now))
            .unwrap_or_default()
            .into(),
        location_label: ctx.location.unwrap_or_default().into(),
        is_dir: entry.is_dir(),
        is_link: entry.is_symlink,
        hidden: entry.hidden,
        selected: ctx.selected,
        cut: ctx.cut,
        thumbnail: ctx.thumbnail.unwrap_or_default(),
        has_thumbnail,
        wants_thumbnail: thumbnailable && !has_thumbnail && !ctx.thumbnail_failed,
    }
}

pub fn place_items(places: &[Place]) -> Vec<PlaceItem> {
    let mut last_section = None;
    places
        .iter()
        .map(|place| {
            let heading = if last_section != Some(place.section) && last_section.is_some() {
                match place.section {
                    Section::Places => "Places",
                    Section::Devices => "Devices",
                    Section::Bookmarks => "Bookmarks",
                }
            } else {
                ""
            };
            last_section = Some(place.section);
            PlaceItem {
                label: place.label.as_str().into(),
                kind: place_icon(place.icon),
                heading: heading.into(),
                detail: match (&place.target, &place.volume) {
                    (Target::Dir(path), _) => path.to_string_lossy().as_ref().into(),
                    (Target::Trash, _) => "Trash".into(),
                    (Target::Volume(_), Some(volume)) => {
                        volume.device.to_string_lossy().as_ref().into()
                    }
                    (Target::Volume(id), None) => id.as_str().into(),
                },
                can_eject: place.volume.as_ref().is_some_and(places::can_eject),
            }
        })
        .collect()
}

pub fn crumbs(segments: &[Segment]) -> Vec<Crumb> {
    segments
        .iter()
        .map(|s| Crumb {
            label: s.label.as_str().into(),
            kind: match s.kind {
                SegmentKind::Root => IconKind::Computer,
                SegmentKind::Home => IconKind::FolderHome,
                SegmentKind::Dir => IconKind::Folder,
            },
        })
        .collect()
}

pub fn menu_rows(entries: &[MenuEntry]) -> Vec<MenuRow> {
    entries
        .iter()
        .map(|entry| match entry {
            MenuEntry::Item { label, shortcut, enabled, destructive, .. } => MenuRow {
                label: (*label).into(),
                shortcut: (*shortcut).into(),
                enabled: *enabled,
                destructive: *destructive,
                separator: false,
                checkable: false,
                checked: false,
            },
            MenuEntry::Check { label, shortcut, checked, .. } => MenuRow {
                label: (*label).into(),
                shortcut: (*shortcut).into(),
                enabled: true,
                destructive: false,
                separator: false,
                checkable: true,
                checked: *checked,
            },
            MenuEntry::Separator => MenuRow { separator: true, ..MenuRow::default() },
        })
        .collect()
}

/// The next enabled, non-separator row from `current` in the direction of `delta`, wrapping around.
pub fn menu_step(rows: &[MenuRow], current: i32, delta: i32) -> i32 {
    let count = rows.len() as i32;
    if count == 0 {
        return -1;
    }
    let step = if delta < 0 { -1 } else { 1 };
    let mut index = if current < 0 && step < 0 { count } else { current };
    for _ in 0..count {
        index = (index + step).rem_euclid(count);
        let row = &rows[index as usize];
        if !row.separator && row.enabled {
            return index;
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testutil::entry;

    #[test]
    fn badges_and_sizes() {
        assert_eq!(badge(&entry("a.png", false, 1, 0)), "PNG");
        assert_eq!(badge(&entry("a.tar.gz", false, 1, 0)), "GZ");
        assert_eq!(badge(&entry("a.backup", false, 1, 0)), "");
        assert_eq!(badge(&entry("Makefile", false, 1, 0)), "");
        assert_eq!(badge(&entry("docs.d", true, 1, 0)), "");
        assert_eq!(size_label(&entry("a", false, 1500, 0)), "1.5 kB");
        assert_eq!(size_label(&entry("d", true, 3, 0)), "3 items");
    }

    #[test]
    fn items_reflect_state() {
        let special = SpecialFolders { folders: vec![("/x/Music".into(), IconKind::FolderMusic)] };
        let now = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .expect("date");
        let ctx = |selected| ItemContext {
            special: &special,
            now,
            location: Some("Documents".into()),
            selected,
            cut: false,
            thumbnail: None,
            thumbnail_failed: false,
        };
        let music = file_item(&entry("Music", true, 2, 0), ctx(true));
        assert_eq!(music.kind, IconKind::FolderMusic);
        assert!(music.selected && music.is_dir);
        assert_eq!(music.location_label, "Documents");
        let photo = file_item(&entry("p.jpg", false, 2, 0), ctx(false));
        assert_eq!(photo.kind, IconKind::Image);
        assert!(photo.wants_thumbnail);
        assert_eq!(photo.type_label, "JPEG image");
    }

    #[test]
    fn menu_navigation() {
        let rows = menu_rows(&crate::core::menu::item_menu(crate::core::menu::MenuContext {
            selected: 1,
            writable: false,
            ..Default::default()
        }));
        assert!(rows[2].separator);
        let first = menu_step(&rows, -1, 1);
        assert_eq!(first, 0);
        assert_eq!(menu_step(&rows, 1, 1), 4, "skips the separator and the disabled Cut");
        let last = menu_step(&rows, -1, -1);
        assert_eq!(rows[last as usize].label, "Properties");
        assert_eq!(menu_step(&[], 0, 1), -1);
    }
}
