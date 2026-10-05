// SPDX-License-Identifier: MIT

//! The `Exec=` key: quoting and field codes.

use std::path::{Path, PathBuf};

/// Why an `Exec=` value can't be turned into a command line.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    #[error("the Exec line has an unterminated quote")]
    UnterminatedQuote,
    #[error("the Exec line is empty")]
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Segment {
    text: String,
    quoted: bool,
}

type Arg = Vec<Segment>;

fn push_text(arg: &mut Arg, c: char, quoted: bool) {
    match arg.last_mut() {
        Some(last) if last.quoted == quoted => last.text.push(c),
        _ => arg.push(Segment { text: c.to_string(), quoted }),
    }
}

/// Splits an already string-unescaped `Exec=` value into arguments.
///
/// Double quotes follow the specification. Single quotes and backslashes outside quotes
/// aren't in the specification but are accepted like GLib does, since many desktop files use them.
fn split(exec: &str) -> Result<Vec<Arg>, ExecError> {
    let mut args = Vec::new();
    let mut current: Option<Arg> = None;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if let Some(arg) = current.take() {
                    args.push(arg);
                }
            }
            '"' => {
                let arg = current.get_or_insert_with(Vec::new);
                arg.push(Segment { text: String::new(), quoted: true });
                loop {
                    match chars.next() {
                        None => return Err(ExecError::UnterminatedQuote),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e @ ('"' | '`' | '$' | '\\')) => push_text(arg, e, true),
                            Some(other) => {
                                push_text(arg, '\\', true);
                                push_text(arg, other, true);
                            }
                            None => return Err(ExecError::UnterminatedQuote),
                        },
                        Some(other) => push_text(arg, other, true),
                    }
                }
            }
            '\'' => {
                let arg = current.get_or_insert_with(Vec::new);
                arg.push(Segment { text: String::new(), quoted: true });
                loop {
                    match chars.next() {
                        None => return Err(ExecError::UnterminatedQuote),
                        Some('\'') => break,
                        Some(other) => push_text(arg, other, true),
                    }
                }
            }
            '\\' => {
                let arg = current.get_or_insert_with(Vec::new);
                push_text(arg, chars.next().unwrap_or('\\'), false);
            }
            other => push_text(current.get_or_insert_with(Vec::new), other, false),
        }
    }
    if let Some(arg) = current.take() {
        args.push(arg);
    }
    Ok(args)
}

/// What field codes expand to, besides the files.
pub(crate) struct ExpandContext<'a> {
    pub name: &'a str,
    pub icon: Option<&'a str>,
    pub desktop_file: Option<&'a Path>,
}

struct FileArg {
    path: Option<String>,
    url: String,
}

impl FileArg {
    fn new(file: &Path) -> Self {
        let text = file.to_string_lossy();
        if let Some(rest) = text.strip_prefix("file://") {
            // Drop an optional host, as in file://localhost/path.
            let path = rest.find('/').map_or("", |i| &rest[i..]);
            return Self { path: Some(percent_decode(path)), url: text.into_owned() };
        }
        if has_uri_scheme(&text) {
            return Self { path: None, url: text.into_owned() };
        }
        let absolute = if file.is_absolute() {
            file.to_path_buf()
        } else {
            std::env::current_dir().map(|cwd| cwd.join(file)).unwrap_or_else(|_| file.to_path_buf())
        };
        let path = absolute.to_string_lossy().into_owned();
        let url = format!("file://{}", percent_encode_path(absolute.as_os_str().as_encoded_bytes()));
        Self { path: Some(path), url }
    }
}

fn has_uri_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once("://") else { return false };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn percent_encode_path(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A parsed `Exec=` value, ready for expansion.
pub(crate) struct ExecLine {
    args: Vec<Arg>,
}

impl ExecLine {
    pub fn parse(exec: &str) -> Result<Self, ExecError> {
        let args = split(exec)?;
        if args.is_empty() {
            return Err(ExecError::Empty);
        }
        Ok(Self { args })
    }

    fn codes(&self) -> impl Iterator<Item = char> + '_ {
        self.args.iter().flatten().flat_map(|segment| {
            let mut codes = Vec::new();
            let mut chars = segment.text.chars();
            while let Some(c) = chars.next() {
                if c == '%'
                    && let Some(code) = chars.next()
                {
                    codes.push(code);
                }
            }
            codes
        })
    }

    /// Whether the application takes a single file per invocation (`%f`/`%u` but no `%F`/`%U`).
    fn single_file(&self) -> bool {
        let mut single = false;
        for code in self.codes() {
            match code {
                'F' | 'U' => return false,
                'f' | 'u' => single = true,
                _ => {}
            }
        }
        single
    }

    /// One command line per instance to start: several when `files` has more entries than a
    /// single-file `%f`/`%u` application accepts, as the specification requires.
    pub fn expand_all(&self, ctx: &ExpandContext<'_>, files: &[PathBuf]) -> Vec<Vec<String>> {
        let files: Vec<FileArg> = files.iter().map(|f| FileArg::new(f)).collect();
        if files.len() > 1 && self.single_file() {
            files.iter().map(|file| self.expand(ctx, std::slice::from_ref(file))).collect()
        } else {
            vec![self.expand(ctx, &files)]
        }
    }

    fn expand(&self, ctx: &ExpandContext<'_>, files: &[FileArg]) -> Vec<String> {
        let paths = || files.iter().filter_map(|f| f.path.clone());
        let urls = || files.iter().map(|f| f.url.clone());
        let mut out = Vec::new();
        for arg in &self.args {
            if let [Segment { text, quoted: false }] = arg.as_slice()
                && let Some(code) = text.strip_prefix('%')
            {
                match code {
                    "F" => {
                        out.extend(paths());
                        continue;
                    }
                    "U" => {
                        out.extend(urls());
                        continue;
                    }
                    "f" => {
                        out.extend(paths().next());
                        continue;
                    }
                    "u" => {
                        out.extend(urls().next());
                        continue;
                    }
                    "i" => {
                        if let Some(icon) = ctx.icon.filter(|i| !i.is_empty()) {
                            out.push("--icon".to_owned());
                            out.push(icon.to_owned());
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            let mut value = String::new();
            for segment in arg {
                let mut chars = segment.text.chars();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        value.push(c);
                        continue;
                    }
                    match chars.next() {
                        None => value.push('%'),
                        Some('%') => value.push('%'),
                        Some('f' | 'F') => value.extend(paths().next()),
                        Some('u' | 'U') => value.extend(urls().next()),
                        Some('i') => value.push_str(ctx.icon.unwrap_or_default()),
                        Some('c') => value.push_str(ctx.name),
                        Some('k') => {
                            if let Some(path) = ctx.desktop_file {
                                value.push_str(&path.to_string_lossy());
                            }
                        }
                        // Deprecated (%d %D %n %N %v %m) and unknown codes expand to nothing.
                        Some(_) => {}
                    }
                }
            }
            if !value.is_empty() || arg.iter().any(|s| s.quoted) {
                out.push(value);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ExpandContext<'static> {
        ExpandContext { name: "My App", icon: Some("my-icon"), desktop_file: Some(Path::new("/apps/my.desktop")) }
    }

    fn expand(exec: &str, files: &[&str]) -> Vec<String> {
        let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        let line = ExecLine::parse(exec).expect("valid exec");
        line.expand_all(&ctx(), &files).into_iter().next().unwrap_or_default()
    }

    #[test]
    fn quoting() {
        assert_eq!(expand(r#"app "a b" c"#, &[]), vec!["app", "a b", "c"]);
        assert_eq!(expand(r#"app "q\"x\`y\$z\\w""#, &[]), vec!["app", "q\"x`y$z\\w"]);
        assert_eq!(expand(r#"app "" pre"mid"post"#, &[]), vec!["app", "", "premidpost"]);
        assert_eq!(expand("app 'single quoted'  tab\targ", &[]), vec!["app", "single quoted", "tab", "arg"]);
        assert_eq!(expand(r"app a\ b", &[]), vec!["app", "a b"]);
        assert_eq!(ExecLine::parse(r#"app "open"#).err(), Some(ExecError::UnterminatedQuote));
        assert_eq!(ExecLine::parse("   ").err(), Some(ExecError::Empty));
    }

    #[test]
    fn field_codes() {
        assert_eq!(expand("app %F", &["/a b", "/c"]), vec!["app", "/a b", "/c"]);
        assert_eq!(expand("app %U", &["/a b"]), vec!["app", "file:///a%20b"]);
        assert_eq!(expand("app %U", &["https://x.org/p"]), vec!["app", "https://x.org/p"]);
        assert_eq!(expand("app %f", &["file:///tmp/x%20y"]), vec!["app", "/tmp/x y"]);
        assert_eq!(expand("app %F", &["https://x.org/p"]), vec!["app"]);
        assert_eq!(expand("app %f", &[]), vec!["app"]);
        assert_eq!(expand("app %i %c %k", &[]), vec!["app", "--icon", "my-icon", "My App", "/apps/my.desktop"]);
        assert_eq!(expand("app 100%% %d %D %n %N %v %m", &[]), vec!["app", "100%"]);
        assert_eq!(expand("app --file=%f", &["/x"]), vec!["app", "--file=/x"]);
        assert_eq!(expand(r#"app "%c""#, &[]), vec!["app", "My App"]);
        let no_icon = ExpandContext { icon: None, ..ctx() };
        let line = ExecLine::parse("app %i").expect("valid");
        assert_eq!(line.expand_all(&no_icon, &[]), vec![vec!["app".to_owned()]]);
    }

    #[test]
    fn relative_paths_become_absolute() {
        let out = expand("app %u", &["rel/file"]);
        assert!(out[1].starts_with("file:///"), "{out:?}");
        assert!(out[1].ends_with("/rel/file"), "{out:?}");
    }

    #[test]
    fn single_file_codes_start_one_instance_per_file() {
        let line = ExecLine::parse("app %f").expect("valid");
        let all = line.expand_all(&ctx(), &[PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(all, vec![vec!["app".to_owned(), "/a".to_owned()], vec!["app".to_owned(), "/b".to_owned()]]);
        let multi = ExecLine::parse("app %F").expect("valid");
        assert_eq!(multi.expand_all(&ctx(), &[PathBuf::from("/a"), PathBuf::from("/b")]).len(), 1);
    }

    #[test]
    fn files_without_field_codes_are_ignored() {
        assert_eq!(expand("app --new", &["/a"]), vec!["app", "--new"]);
    }

    #[test]
    fn percent_coding_round_trips() {
        assert_eq!(percent_encode_path("/ä #".as_bytes()), "/%C3%A4%20%23");
        assert_eq!(percent_decode("/%C3%A4%20%23%zz"), "/ä #%zz");
    }
}
