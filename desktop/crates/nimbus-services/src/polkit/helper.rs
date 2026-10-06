// SPDX-License-Identifier: MIT

//! polkit's setuid `polkit-agent-helper-1`, which runs PAM for one user and reports to polkitd itself,
//! and the line protocol it speaks on its standard input and output.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::Secret;

/// Where distributions install the helper: Debian, Fedora, and Arch; openSUSE and newer polkit; NixOS's setuid wrapper.
const HELPER_PATHS: &[&str] = &[
    "/usr/lib/polkit-1/polkit-agent-helper-1",
    "/usr/libexec/polkit-agent-helper-1",
    "/usr/libexec/polkit-1/polkit-agent-helper-1",
    "/usr/local/lib/polkit-1/polkit-agent-helper-1",
    "/usr/local/libexec/polkit-agent-helper-1",
    "/run/wrappers/bin/polkit-agent-helper-1",
];

/// Finds the installed helper.
pub(crate) fn find() -> Option<PathBuf> {
    HELPER_PATHS.iter().map(PathBuf::from).find(|path| path.is_file())
}

/// A line the helper writes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Message {
    /// PAM asks for a response, shown as typed when `echo` is set, such as a user name, or hidden, such as a password.
    Prompt {
        text: String,
        echo: bool,
    },
    Info(String),
    Error(String),
    Success,
    Failure,
}

/// Parses one line without its line break; `None` for lines outside the protocol.
pub(crate) fn parse(line: &str) -> Option<Message> {
    if line == "SUCCESS" {
        return Some(Message::Success);
    }
    if line == "FAILURE" {
        return Some(Message::Failure);
    }
    let (kind, text) = line.split_once(' ').unwrap_or((line, ""));
    // polkit's own agent library unescapes the text with `g_strcompress`.
    let text = unescape(text);
    match kind {
        "PAM_PROMPT_ECHO_OFF" => Some(Message::Prompt { text, echo: false }),
        "PAM_PROMPT_ECHO_ON" => Some(Message::Prompt { text, echo: true }),
        "PAM_TEXT_INFO" => Some(Message::Info(text)),
        "PAM_ERROR_MSG" => Some(Message::Error(text)),
        _ => None,
    }
}

/// Resolves C escapes as GLib's `g_strcompress` does.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('v') => out.push('\u{b}'),
            Some(digit @ '0'..='7') => {
                let mut value = digit.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match chars.peek().and_then(|c| c.to_digit(8)) {
                        Some(next) => {
                            value = value * 8 + next;
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// A running helper, killed on drop.
pub(crate) struct Helper {
    _child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl Helper {
    /// Starts `program` to authenticate `user` for the request identified by `cookie`.
    pub(crate) async fn spawn(program: &Path, user: &str, cookie: &str) -> io::Result<Self> {
        let mut child = Command::new(program)
            .arg(user)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(io::Error::other("the helper has no pipes"));
        };
        // On standard input rather than the command line, where other users could read it.
        stdin.write_all(format!("{cookie}\n").as_bytes()).await?;
        Ok(Self { _child: child, stdin, lines: BufReader::new(stdout).lines() })
    }

    /// The next message; `None` once the helper closed its output.
    pub(crate) async fn next(&mut self) -> io::Result<Option<Message>> {
        while let Some(line) = self.lines.next_line().await? {
            match parse(&line) {
                Some(message) => return Ok(Some(message)),
                None => tracing::debug!("Ignoring a line from polkit-agent-helper-1: {line:?}"),
            }
        }
        Ok(None)
    }

    /// Answers the last prompt.
    pub(crate) async fn respond(&mut self, response: &Secret) -> io::Result<()> {
        let response = response.expose();
        if response.contains(['\n', '\0']) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the response spans lines"));
        }
        let mut line = Vec::with_capacity(response.len() + 1);
        line.extend_from_slice(response.as_bytes());
        line.push(b'\n');
        let result = self.stdin.write_all(&line).await;
        line.fill(0);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_protocol() {
        assert_eq!(
            parse("PAM_PROMPT_ECHO_OFF Password: "),
            Some(Message::Prompt { text: "Password: ".into(), echo: false })
        );
        assert_eq!(
            parse("PAM_PROMPT_ECHO_ON login:"),
            Some(Message::Prompt { text: "login:".into(), echo: true })
        );
        assert_eq!(
            parse("PAM_TEXT_INFO Touch the key"),
            Some(Message::Info("Touch the key".into()))
        );
        assert_eq!(parse("PAM_ERROR_MSG Expired"), Some(Message::Error("Expired".into())));
        assert_eq!(parse("PAM_ERROR_MSG"), Some(Message::Error(String::new())));
        assert_eq!(parse("SUCCESS"), Some(Message::Success));
        assert_eq!(parse("FAILURE"), Some(Message::Failure));
        assert_eq!(parse("SUCCESSFUL"), None);
        assert_eq!(parse("polkit-agent-helper-1: warning"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn unescapes_like_glib() {
        assert_eq!(unescape(r#"a\nb\tc\\d\"e"#), "a\nb\tc\\d\"e");
        assert_eq!(unescape(r"\101\60x\7"), "A0x\u{7}");
        assert_eq!(unescape(r"trailing\"), "trailing");
        assert_eq!(unescape("Contraseña:"), "Contraseña:");
    }
}
