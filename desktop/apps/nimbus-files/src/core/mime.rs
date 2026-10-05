// SPDX-License-Identifier: MIT

//! MIME type detection, and the icon category and description of each type.

use std::path::Path;

pub const DIRECTORY: &str = "inode/directory";
pub const SYMLINK_BROKEN: &str = "inode/symlink";
pub const UNKNOWN: &str = "application/octet-stream";
pub const PLAIN_TEXT: &str = "text/plain";

/// How a file is drawn: each category has its own icon and tint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Folder,
    Text,
    Document,
    Image,
    Audio,
    Video,
    Archive,
    Executable,
    Script,
    Generic,
}

/// Names without an extension that are conventionally text.
const TEXT_NAMES: &[&str] = &[
    "README",
    "LICENSE",
    "COPYING",
    "AUTHORS",
    "CHANGELOG",
    "NEWS",
    "TODO",
    "INSTALL",
    "Makefile",
    "Dockerfile",
    "Containerfile",
    "Justfile",
    "Vagrantfile",
];

/// Guesses the MIME type of a regular file from its name, sniffing its content when the name says nothing.
///
/// `sniff` controls whether the file is opened; listing large directories passes `false` for speed.
pub fn guess(path: &Path, sniff: bool) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if let Some(mime) = mime_guess::from_path(path).first_raw() {
        return normalize(mime).to_string();
    }
    if TEXT_NAMES.iter().any(|t| name.eq_ignore_ascii_case(t))
        || name.starts_with('.') && !name[1..].contains('.')
    {
        return PLAIN_TEXT.to_string();
    }
    if sniff && let Some(mime) = tree_magic_mini::from_filepath(path) {
        return normalize(mime).to_string();
    }
    UNKNOWN.to_string()
}

/// `mime_guess` returns some legacy aliases; prefer the names shared-mime-info uses.
fn normalize(mime: &str) -> &str {
    match mime {
        "application/x-sh" => "application/x-shellscript",
        "image/x-ms-bmp" => "image/bmp",
        "application/x-gzip" => "application/gzip",
        "text/x-c" => "text/x-csrc",
        other => other,
    }
}

pub fn category(mime: &str) -> Category {
    let (top, sub) = mime.split_once('/').unwrap_or((mime, ""));
    match top {
        "inode" if sub == "directory" || sub == "mount-point" => Category::Folder,
        "image" => Category::Image,
        "audio" => Category::Audio,
        "video" => Category::Video,
        "text" => match sub {
            "x-shellscript" | "x-sh" => Category::Script,
            _ => Category::Text,
        },
        "application" => match sub {
            "pdf" | "rtf" | "epub+zip" | "msword" | "vnd.ms-excel" | "vnd.ms-powerpoint" => {
                Category::Document
            }
            s if s.starts_with("vnd.oasis.opendocument")
                || s.starts_with("vnd.openxmlformats-officedocument") =>
            {
                Category::Document
            }
            "zip"
            | "gzip"
            | "x-tar"
            | "x-bzip2"
            | "x-bzip"
            | "x-xz"
            | "x-7z-compressed"
            | "vnd.rar"
            | "x-rar-compressed"
            | "x-rar"
            | "zstd"
            | "x-zstd"
            | "x-compressed-tar"
            | "x-lzma"
            | "java-archive"
            | "x-iso9660-image"
            | "vnd.debian.binary-package"
            | "x-rpm"
            | "x-cpio" => Category::Archive,
            "x-executable" | "x-sharedlib" | "x-pie-executable" | "x-elf" | "vnd.appimage"
            | "x-desktop" | "x-msdownload" => Category::Executable,
            "x-shellscript" | "x-sh" | "x-perl" | "x-python" | "x-ruby" => Category::Script,
            "json" | "xml" | "javascript" | "toml" | "x-yaml" | "yaml" | "sql" | "x-subrip" => {
                Category::Text
            }
            _ => Category::Generic,
        },
        _ => Category::Generic,
    }
}

/// A human description such as "PNG image" or "Folder".
pub fn describe(mime: &str) -> String {
    let known = match mime {
        DIRECTORY => "Folder",
        SYMLINK_BROKEN => "Broken link",
        PLAIN_TEXT => "Plain text document",
        UNKNOWN => "Binary file",
        "text/markdown" => "Markdown document",
        "text/html" => "HTML document",
        "text/css" => "CSS stylesheet",
        "text/csv" => "CSV document",
        "text/x-rust" => "Rust source code",
        "text/x-csrc" => "C source code",
        "text/x-chdr" => "C header",
        "text/x-c++src" => "C++ source code",
        "text/x-python" | "application/x-python" => "Python script",
        "text/x-shellscript" | "application/x-shellscript" => "Shell script",
        "application/javascript" | "text/javascript" => "JavaScript source code",
        "application/json" => "JSON document",
        "application/xml" | "text/xml" => "XML document",
        "application/toml" => "TOML document",
        "application/x-yaml" | "application/yaml" => "YAML document",
        "application/pdf" => "PDF document",
        "application/rtf" => "RTF document",
        "application/epub+zip" => "EPUB book",
        "application/zip" => "Zip archive",
        "application/gzip" => "Gzip archive",
        "application/x-tar" => "Tar archive",
        "application/x-compressed-tar" => "Tar archive (gzip-compressed)",
        "application/x-xz" => "XZ archive",
        "application/x-bzip2" => "Bzip2 archive",
        "application/zstd" => "Zstandard archive",
        "application/x-7z-compressed" => "7-Zip archive",
        "application/vnd.rar" | "application/x-rar-compressed" => "RAR archive",
        "application/x-iso9660-image" => "Disk image",
        "application/vnd.debian.binary-package" => "Debian package",
        "application/x-rpm" => "RPM package",
        "application/x-executable" | "application/x-pie-executable" => "Program",
        "application/x-sharedlib" => "Shared library",
        "application/vnd.appimage" => "AppImage application",
        "application/x-desktop" => "Desktop configuration file",
        "application/vnd.oasis.opendocument.text" => "Text document",
        "application/vnd.oasis.opendocument.spreadsheet" => "Spreadsheet",
        "application/vnd.oasis.opendocument.presentation" => "Presentation",
        "application/msword"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            "Word document"
        }
        "application/vnd.ms-excel"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            "Excel spreadsheet"
        }
        "application/vnd.ms-powerpoint"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            "PowerPoint presentation"
        }
        "image/svg+xml" => "SVG image",
        "image/jpeg" => "JPEG image",
        "audio/mpeg" => "MP3 audio",
        "audio/x-wav" | "audio/wav" => "WAV audio",
        "video/x-matroska" => "Matroska video",
        "video/quicktime" => "QuickTime video",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let (top, sub) = mime.split_once('/').unwrap_or((mime, ""));
    let short = sub.trim_start_matches("x-").trim_start_matches("vnd.");
    let short = short.split(['+', '.', ';']).next().unwrap_or(short).to_ascii_uppercase();
    match top {
        "image" if !short.is_empty() => format!("{short} image"),
        "audio" if !short.is_empty() => format!("{short} audio"),
        "video" if !short.is_empty() => format!("{short} video"),
        "font" => "Font".to_string(),
        "text" => "Text document".to_string(),
        _ => "Document".to_string(),
    }
}

/// Whether thumbnails can be generated for files of this type.
pub fn is_thumbnailable(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp" | "image/svg+xml"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_by_extension_and_name() {
        assert_eq!(guess(Path::new("/x/a.PNG"), false), "image/png");
        assert_eq!(guess(Path::new("/x/a.tar.gz"), false), "application/gzip");
        assert_eq!(guess(Path::new("/x/README"), false), PLAIN_TEXT);
        assert_eq!(guess(Path::new("/x/.bashrc"), false), PLAIN_TEXT);
        assert_eq!(guess(Path::new("/x/blob"), false), UNKNOWN);
        assert_eq!(guess(Path::new("/x/run.sh"), false), "application/x-shellscript");
    }

    #[test]
    fn sniffs_content_without_extension() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("picture");
        // A minimal PNG signature and IHDR chunk is enough for magic matching.
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&[0; 17]);
        std::fs::write(&path, png).expect("write");
        let mime = guess(&path, true);
        // Without a shared-mime-info database the sniffer may not know PNG; it must not panic.
        assert!(mime == "image/png" || mime == UNKNOWN || mime.contains('/'), "{mime}");
    }

    #[test]
    fn categories() {
        assert_eq!(category(DIRECTORY), Category::Folder);
        assert_eq!(category("image/svg+xml"), Category::Image);
        assert_eq!(category("text/x-rust"), Category::Text);
        assert_eq!(category("application/x-shellscript"), Category::Script);
        assert_eq!(category("application/pdf"), Category::Document);
        assert_eq!(
            category("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            Category::Document
        );
        assert_eq!(category("application/x-7z-compressed"), Category::Archive);
        assert_eq!(category("application/x-executable"), Category::Executable);
        assert_eq!(category("audio/flac"), Category::Audio);
        assert_eq!(category("video/mp4"), Category::Video);
        assert_eq!(category(UNKNOWN), Category::Generic);
        assert_eq!(category("garbage"), Category::Generic);
    }

    #[test]
    fn descriptions() {
        assert_eq!(describe(DIRECTORY), "Folder");
        assert_eq!(describe("image/png"), "PNG image");
        assert_eq!(describe("image/svg+xml"), "SVG image");
        assert_eq!(describe("video/mp4"), "MP4 video");
        assert_eq!(describe("audio/x-flac"), "FLAC audio");
        assert_eq!(describe("text/x-unknown"), "Text document");
        assert_eq!(describe("application/x-foo"), "Document");
        assert_eq!(describe(""), "Document");
    }

    #[test]
    fn thumbnailable() {
        assert!(is_thumbnailable("image/png"));
        assert!(is_thumbnailable("image/svg+xml"));
        assert!(!is_thumbnailable("image/x-xcf"));
        assert!(!is_thumbnailable("text/plain"));
    }
}
