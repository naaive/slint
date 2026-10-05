// SPDX-License-Identifier: MIT

//! `file://` URIs as GLib writes them, so thumbnail hashes match other desktops.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str};

/// The bytes `g_filename_to_uri` leaves unescaped in a path.
const PATH: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'!')
    .remove(b'$')
    .remove(b'&')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')')
    .remove(b'*')
    .remove(b'+')
    .remove(b',')
    .remove(b'-')
    .remove(b'.')
    .remove(b'/')
    .remove(b':')
    .remove(b'=')
    .remove(b'@')
    .remove(b'_')
    .remove(b'~');

/// Percent-encodes a path with the same escaping as a `file://` URI, without the scheme.
pub fn escape_path(path: &Path) -> String {
    percent_encoding::percent_encode(path.as_os_str().as_bytes(), PATH).to_string()
}

/// The `file://` URI of an absolute path.
pub fn path_to_uri(path: &Path) -> String {
    format!("file://{}", escape_path(path))
}

/// Decodes a percent-encoded path, which may contain bytes that aren't UTF-8.
pub fn unescape_path(text: &str) -> PathBuf {
    PathBuf::from(OsString::from_vec(percent_decode_str(text).collect()))
}

/// The local path of a `file://` URI; `None` for other schemes or remote hosts.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost").filter(|p| p.starts_with('/'))?
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    Some(unescape_path(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_like_glib() {
        assert_eq!(path_to_uri(Path::new("/home/a b/c.png")), "file:///home/a%20b/c.png");
        assert_eq!(path_to_uri(Path::new("/x/!$&'()*+,-.:=@_~")), "file:///x/!$&'()*+,-.:=@_~");
        assert_eq!(path_to_uri(Path::new("/x/#;?[]%")), "file:///x/%23%3B%3F%5B%5D%25");
        assert_eq!(path_to_uri(Path::new("/tmp/ü")), "file:///tmp/%C3%BC");
    }

    #[test]
    fn round_trips_non_utf8() {
        let path = PathBuf::from(OsString::from_vec(b"/tmp/\xff name".to_vec()));
        let uri = path_to_uri(&path);
        assert_eq!(uri, "file:///tmp/%FF%20name");
        assert_eq!(uri_to_path(&uri), Some(path));
    }

    #[test]
    fn parses_uris() {
        assert_eq!(uri_to_path("file:///a/b%20c"), Some(PathBuf::from("/a/b c")));
        assert_eq!(uri_to_path("file://localhost/a"), Some(PathBuf::from("/a")));
        assert_eq!(uri_to_path("file:///a?x=1"), Some(PathBuf::from("/a")));
        assert_eq!(uri_to_path("file://server/a"), None);
        assert_eq!(uri_to_path("sftp://host/a"), None);
        assert_eq!(uri_to_path("/a"), None);
    }
}
