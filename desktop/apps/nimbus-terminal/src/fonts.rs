// SPDX-License-Identifier: MIT

//! Discovery of the monospace font and its fallbacks among the installed fonts.
//! Discovery reads the file system, so run it off the UI thread.

use std::collections::BTreeSet;
use std::sync::Arc;

use fontdb::{Database, Family, ID, Query, Style, Weight};
use swash::{CacheKey, FontRef};

/// Monospace families in order of preference when the user hasn't picked one.
pub const PREFERRED_FAMILIES: &[&str] = &[
    "JetBrains Mono",
    "Fira Code",
    "Cascadia Code",
    "Source Code Pro",
    "Hack",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Ubuntu Mono",
];

/// Families searched for characters the main font lacks: wide-coverage monospace fonts, CJK, emoji, symbols.
const FALLBACK_FAMILIES: &[&[&str]] = &[
    &["DejaVu Sans Mono", "Noto Sans Mono", "Liberation Mono"],
    &[
        "Noto Sans Mono CJK SC",
        "Noto Sans CJK SC",
        "Source Han Sans SC",
        "WenQuanYi Zen Hei Mono",
        "WenQuanYi Zen Hei",
        "Droid Sans Fallback",
        "IPAGothic",
    ],
    &["Noto Color Emoji", "Twemoji", "JoyPixels", "Apple Color Emoji"],
    &["Symbols Nerd Font Mono", "Symbols Nerd Font"],
    &["DejaVu Sans", "Noto Sans Symbols", "Noto Sans Symbols 2", "FreeMono", "FreeSerif"],
];

#[derive(Debug, thiserror::Error)]
pub enum FontError {
    #[error("no usable fonts are installed")]
    NoFonts,
}

/// A loaded font face, shared between threads.
#[derive(Clone)]
pub struct Face {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    offset: u32,
    key: CacheKey,
    pub family: String,
    pub bold: bool,
    pub italic: bool,
}

impl std::fmt::Debug for Face {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Face")
            .field("family", &self.family)
            .field("bold", &self.bold)
            .field("italic", &self.italic)
            .finish_non_exhaustive()
    }
}

impl Face {
    /// Wraps font data that's already in memory.
    pub fn from_data(
        data: Arc<dyn AsRef<[u8]> + Send + Sync>,
        index: u32,
        family: &str,
    ) -> Option<Self> {
        let font = FontRef::from_index((*data).as_ref(), index as usize)?;
        let attributes = font.attributes();
        let (offset, key) = (font.offset, font.key);
        Some(Self {
            data,
            offset,
            key,
            family: family.to_string(),
            bold: attributes.weight().0 >= 600,
            italic: attributes.style() != swash::Style::Normal,
        })
    }

    fn load(db: &mut Database, id: ID) -> Option<Self> {
        let family = db.face(id).map(family_name)?;
        // SAFETY: the file is mapped read-only. Package managers replace font files by renaming
        // rather than writing them in place, so the mapping stays valid while we hold it.
        let (data, index) = unsafe { db.make_shared_face_data(id) }?;
        Self::from_data(data, index, &family)
    }

    pub fn font(&self) -> FontRef<'_> {
        FontRef { data: (*self.data).as_ref(), offset: self.offset, key: self.key }
    }

    pub fn has_char(&self, c: char) -> bool {
        self.font().charmap().map(c) != 0
    }
}

fn family_name(info: &fontdb::FaceInfo) -> String {
    info.families.first().map(|(name, _)| name.clone()).unwrap_or_default()
}

/// The faces used to draw terminal text.
#[derive(Clone, Debug)]
pub struct FontSet {
    pub regular: Face,
    pub bold: Option<Face>,
    pub italic: Option<Face>,
    pub bold_italic: Option<Face>,
    /// Searched in order for characters `regular` lacks.
    pub fallbacks: Vec<Face>,
    /// The installed monospace families, sorted, for the preferences.
    pub monospace_families: Vec<String>,
}

impl FontSet {
    pub fn family(&self) -> &str {
        &self.regular.family
    }

    /// The face for a style, when the family has one; synthesize the style from `regular` otherwise.
    pub fn styled(&self, bold: bool, italic: bool) -> Option<&Face> {
        match (bold, italic) {
            (false, false) => Some(&self.regular),
            (true, false) => self.bold.as_ref(),
            (false, true) => self.italic.as_ref(),
            (true, true) => self.bold_italic.as_ref().or(self.bold.as_ref()),
        }
    }

    /// Finds the installed fonts, preferring `family` when it's installed.
    pub fn discover(family: &str) -> Result<Self, FontError> {
        let mut db = Database::new();
        db.load_system_fonts();
        Self::from_database(&mut db, family)
    }

    /// Like [`FontSet::discover`], over a prepared database.
    pub fn from_database(db: &mut Database, family: &str) -> Result<Self, FontError> {
        let exact = |db: &Database, name: &str, weight: Weight, style: Style| {
            let id = db.query(&Query {
                families: &[Family::Name(name)],
                weight,
                style,
                ..Query::default()
            })?;
            let info = db.face(id)?;
            info.families.iter().any(|(f, _)| f.eq_ignore_ascii_case(name)).then_some(id)
        };
        let family = family.trim();
        let regular_id = (!family.is_empty())
            .then(|| exact(db, family, Weight::NORMAL, Style::Normal))
            .flatten()
            .or_else(|| {
                PREFERRED_FAMILIES
                    .iter()
                    .find_map(|name| exact(db, name, Weight::NORMAL, Style::Normal))
            })
            .or_else(|| db.faces().find(|f| f.monospaced && f.style == Style::Normal).map(|f| f.id))
            .or_else(|| db.faces().next().map(|f| f.id))
            .ok_or(FontError::NoFonts)?;
        let regular_family = db.face(regular_id).map(family_name).unwrap_or_default();
        let regular = Face::load(db, regular_id).ok_or(FontError::NoFonts)?;

        let mut styled = |weight, style, want_bold: bool, want_italic: bool| {
            let id = exact(db, &regular_family, weight, style)?;
            let face = Face::load(db, id)?;
            (face.bold == want_bold && face.italic == want_italic).then_some(face)
        };
        let bold = styled(Weight::BOLD, Style::Normal, true, false);
        let italic = styled(Weight::NORMAL, Style::Italic, false, true);
        let bold_italic = styled(Weight::BOLD, Style::Italic, true, true);

        let fallbacks = FALLBACK_FAMILIES
            .iter()
            .filter_map(|group| {
                group
                    .iter()
                    .filter(|name| !name.eq_ignore_ascii_case(&regular_family))
                    .find_map(|name| exact(db, name, Weight::NORMAL, Style::Normal))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|id| Face::load(db, id))
            .collect();

        let monospace_families = db
            .faces()
            .filter(|f| f.monospaced)
            .map(family_name)
            .filter(|name| !name.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        Ok(Self { regular, bold, italic, bold_italic, fallbacks, monospace_families })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> Option<FontSet> {
        FontSet::discover("").ok()
    }

    #[test]
    fn discovers_a_monospace_family() {
        let Some(set) = system() else { return };
        assert!(set.regular.has_char('A'));
        assert!(!set.family().is_empty());
        assert!(!set.regular.bold);
        if let Some(bold) = &set.bold {
            assert!(bold.bold && !bold.italic);
        }
        assert!(set.styled(false, false).is_some());
        assert!(set.monospace_families.windows(2).all(|w| w[0] < w[1]));
        assert!(set.fallbacks.iter().all(|f| f.family != set.family()));
    }

    #[test]
    fn unknown_family_falls_back() {
        let Some(default) = system() else { return };
        let set = FontSet::discover("No Such Font Family 1234").expect("fonts exist");
        assert_eq!(set.family(), default.family());
    }

    #[test]
    fn empty_database_has_no_fonts() {
        let mut db = Database::new();
        assert!(matches!(FontSet::from_database(&mut db, ""), Err(FontError::NoFonts)));
    }

    #[test]
    fn invalid_data_is_rejected() {
        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(vec![0u8; 64]);
        assert!(Face::from_data(data, 0, "Broken").is_none());
    }
}
