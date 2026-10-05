// SPDX-License-Identifier: MIT

//! File name validation and collision-free names.

/// Linux's `NAME_MAX`.
const MAX_NAME_BYTES: usize = 255;

/// Extensions that belong to the one before them, so "a.tar.gz" splits as "a" and ".tar.gz".
const DOUBLE_EXTENSIONS: &[&str] = &["gz", "bz2", "xz", "zst", "lz", "lzma", "z"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("The name can't be empty")]
    Empty,
    #[error("“.” and “..” are reserved names")]
    Reserved,
    #[error("Names can't contain “/”")]
    Slash,
    #[error("Names can't contain a null character")]
    Nul,
    #[error("The name is too long")]
    TooLong,
}

/// Checks a name typed for a new or renamed file.
pub fn validate(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        Err(NameError::Empty)
    } else if name == "." || name == ".." {
        Err(NameError::Reserved)
    } else if name.contains('/') {
        Err(NameError::Slash)
    } else if name.contains('\0') {
        Err(NameError::Nul)
    } else if name.len() > MAX_NAME_BYTES {
        Err(NameError::TooLong)
    } else {
        Ok(())
    }
}

/// Splits a file name into its stem and extension, including the dot.
///
/// Dot files without a further dot have no extension, and compressed tarballs keep both parts.
pub fn split_extension(name: &str) -> (&str, &str) {
    let search_from = usize::from(name.starts_with('.'));
    let Some(dot) = name[search_from..].rfind('.').map(|i| i + search_from) else {
        return (name, "");
    };
    if dot + 1 == name.len() {
        return (name, "");
    }
    let ext = &name[dot + 1..];
    if DOUBLE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
        let stem = &name[..dot];
        if let Some(inner) = stem.rfind('.').filter(|&i| i > search_from)
            && stem[inner + 1..].eq_ignore_ascii_case("tar")
        {
            return (&name[..inner], &name[inner..]);
        }
    }
    (&name[..dot], &name[dot..])
}

/// How a duplicate is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// "a (copy).txt", "a (copy 2).txt", for duplicates made in the same folder.
    Copy,
    /// "a (2).txt", "a (3).txt", for keeping both files on a conflict.
    Numbered,
}

/// The first name derived from `name` for which `exists` is false.
pub fn unique(name: &str, style: Style, exists: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = split_extension(name);
    let stem = strip_suffix(stem, style);
    (1u64..)
        .map(|n| match (style, n) {
            (Style::Copy, 1) => format!("{stem} (copy){ext}"),
            (Style::Copy, n) => format!("{stem} (copy {n}){ext}"),
            (Style::Numbered, n) => format!("{stem} ({}){ext}", n + 1),
        })
        .find(|candidate| !exists(candidate))
        .unwrap_or_else(|| name.to_string())
}

/// Drops a suffix an earlier duplicate added, so copying "a (copy).txt" gives "a (copy 2).txt".
fn strip_suffix(stem: &str, style: Style) -> &str {
    let Some(open) = stem.strip_suffix(')').and_then(|s| s.rfind(" (")) else {
        return stem;
    };
    let inner = &stem[open + 2..stem.len() - 1];
    let matches = match style {
        Style::Copy => {
            inner == "copy"
                || inner
                    .strip_prefix("copy ")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        }
        Style::Numbered => !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()),
    };
    if matches { &stem[..open] } else { stem }
}

/// The byte length of the part of a name to preselect when renaming.
pub fn stem_len(name: &str) -> usize {
    split_extension(name).0.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation() {
        assert_eq!(validate("ok.txt"), Ok(()));
        assert_eq!(validate(".hidden"), Ok(()));
        assert_eq!(validate(""), Err(NameError::Empty));
        assert_eq!(validate(".."), Err(NameError::Reserved));
        assert_eq!(validate("a/b"), Err(NameError::Slash));
        assert_eq!(validate("a\0b"), Err(NameError::Nul));
        assert_eq!(validate(&"x".repeat(256)), Err(NameError::TooLong));
        assert_eq!(validate(&"x".repeat(255)), Ok(()));
        assert_eq!(NameError::Slash.to_string(), "Names can't contain “/”");
    }

    #[test]
    fn extensions() {
        assert_eq!(split_extension("photo.jpg"), ("photo", ".jpg"));
        assert_eq!(split_extension("archive.tar.gz"), ("archive", ".tar.gz"));
        assert_eq!(split_extension("archive.TAR.XZ"), ("archive", ".TAR.XZ"));
        assert_eq!(split_extension("notes.gz"), ("notes", ".gz"));
        assert_eq!(split_extension(".bashrc"), (".bashrc", ""));
        assert_eq!(split_extension(".config.toml"), (".config", ".toml"));
        assert_eq!(split_extension("Makefile"), ("Makefile", ""));
        assert_eq!(split_extension("trailing."), ("trailing.", ""));
        assert_eq!(split_extension(".tar.gz"), (".tar", ".gz"));
        assert_eq!(split_extension("ünï.cödé"), ("ünï", ".cödé"));
        assert_eq!(stem_len("ünï.txt"), "ünï".len());
    }

    #[test]
    fn unique_names() {
        let taken = ["a.txt", "a (copy).txt", "a (2).txt", "dir"];
        let exists = |n: &str| taken.contains(&n);
        assert_eq!(unique("a.txt", Style::Copy, exists), "a (copy 2).txt");
        assert_eq!(unique("a (copy).txt", Style::Copy, exists), "a (copy 2).txt");
        assert_eq!(unique("a.txt", Style::Numbered, exists), "a (3).txt");
        assert_eq!(unique("a (2).txt", Style::Numbered, exists), "a (3).txt");
        assert_eq!(unique("dir", Style::Numbered, exists), "dir (2)");
        assert_eq!(unique("x.tar.gz", Style::Copy, |_| false), "x (copy).tar.gz");
        assert_eq!(unique("f (draft).txt", Style::Numbered, |_| false), "f (draft) (2).txt");
    }
}
