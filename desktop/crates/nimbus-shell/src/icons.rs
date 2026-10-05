// SPDX-License-Identifier: MIT

//! Application icons, loaded once per name, and the tinted initial shown when an application has none.

use std::collections::HashMap;

use nimbus_xdg::IconResolver;

use crate::AppVisual;

/// The size icons are looked up at, in logical pixels; the largest the shell draws them.
pub const ICON_SIZE: u32 = 64;

/// Tints for applications without an icon, picked by a hash of the name.
const TINTS: [(u8, u8, u8); 9] = [
    (0x35, 0x84, 0xe4),
    (0x21, 0x90, 0xa4),
    (0x3a, 0x94, 0x4a),
    (0xc8, 0x88, 0x00),
    (0xed, 0x5b, 0x00),
    (0xe6, 0x2d, 0x42),
    (0xd5, 0x61, 0x99),
    (0x91, 0x41, 0xac),
    (0x6f, 0x83, 0x96),
];

#[derive(Default)]
pub struct IconCache {
    resolver: Option<IconResolver>,
    scale: u32,
    images: HashMap<String, Option<slint::Image>>,
}

impl IconCache {
    pub fn set_resolver(&mut self, resolver: IconResolver) {
        self.resolver = Some(resolver);
        self.images.clear();
    }

    /// Sets the output scale; icons are reloaded at the new size.
    pub fn set_scale(&mut self, scale: f64) {
        let scale = if scale.is_finite() { scale.ceil().clamp(1.0, 8.0) as u32 } else { 1 };
        if scale != self.scale {
            self.scale = scale;
            self.images.clear();
        }
    }

    /// The output scale icons are loaded at, rounded up to a whole number.
    pub fn scale(&self) -> u32 {
        self.scale.max(1)
    }

    /// Returns the image for an icon name or absolute path, or `None` if it can't be found or decoded.
    pub fn image(&mut self, icon: &str) -> Option<slint::Image> {
        let icon = icon.trim();
        if icon.is_empty() {
            return None;
        }
        if let Some(cached) = self.images.get(icon) {
            return cached.clone();
        }
        let path = self.resolver.as_ref()?.lookup(icon, ICON_SIZE, self.scale.max(1));
        let image = path.and_then(|path| match slint::Image::load_from_path(&path) {
            Ok(image) => Some(image),
            Err(err) => {
                tracing::debug!(path = %path.display(), ?err, "can't load icon");
                None
            }
        });
        self.images.insert(icon.to_owned(), image.clone());
        image
    }

    /// The visual for an application named `name` with an optional icon.
    pub fn visual(&mut self, name: &str, icon: Option<&str>) -> AppVisual {
        let image = icon.and_then(|icon| self.image(icon));
        visual(name, image)
    }
}

/// An application's visual: its icon, or its initial on a tint derived from its name.
pub fn visual(name: &str, icon: Option<slint::Image>) -> AppVisual {
    let initial: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".into());
    let hash = name
        .bytes()
        .fold(0x811c_9dc5u32, |hash, byte| (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193));
    let (r, g, b) = TINTS[hash as usize % TINTS.len()];
    AppVisual {
        has_icon: icon.is_some(),
        icon: icon.unwrap_or_default(),
        initial: initial.into(),
        tint: slint::Color::from_rgb_u8(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_and_tints_are_stable() {
        let a = visual("files", None);
        assert_eq!(a.initial, "F");
        assert!(!a.has_icon);
        assert_eq!(visual("files", None).tint, a.tint);
        assert_eq!(visual("  émail", None).initial, "É");
        assert_eq!(visual("", None).initial, "?");
        assert_eq!(visual("!!!", None).initial, "?");
    }

    #[test]
    fn missing_icons_are_cached_as_missing() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let mut cache = IconCache::default();
        assert!(cache.image("anything").is_none(), "no resolver yet");
        cache.set_resolver(IconResolver::with_data_dirs("hicolor", &[dir.path().to_path_buf()]));
        cache.set_scale(1.5);
        assert!(cache.image("does-not-exist").is_none());
        assert!(cache.images.contains_key("does-not-exist"));
        assert!(cache.image("  ").is_none());
        cache.set_scale(f64::NAN);
        assert!(cache.images.is_empty(), "a scale change drops the cache");
    }
}
