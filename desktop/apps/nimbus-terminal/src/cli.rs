// SPDX-License-Identifier: MIT

//! Command-line parsing.

use std::ffi::OsString;
use std::path::PathBuf;

pub const USAGE: &str = "\
Usage: nimbus-terminal [OPTIONS] [-e COMMAND [ARGS...] | -- PROGRAM [ARGS...]]

Options:
  -e, --command COMMAND [ARGS...]  Run COMMAND instead of the shell; takes the rest of the line.
                                   A single COMMAND word is split like a shell command line
      --                           Run PROGRAM with ARGS exactly as given, without splitting
      --working-directory DIR      Start in DIR
      --title TITLE                Set the window title, ignoring titles set by programs
      --screenshot PATH            Render sample content to a PNG file and exit
      --screenshot-light           Use the light scheme for --screenshot
  -h, --help                       Show this help
  -V, --version                    Show the version";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// The program and its arguments.
    pub command: Option<(String, Vec<String>)>,
    pub working_directory: Option<PathBuf>,
    pub title: Option<String>,
    pub screenshot: Option<PathBuf>,
    pub screenshot_light: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    Help,
    Version,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    #[error("'{0}' needs a value")]
    MissingValue(String),
    #[error("unknown option '{0}'")]
    Unknown(String),
    #[error("arguments must be valid UTF-8")]
    NotUnicode,
}

/// Splits a single `-e` argument such as `"htop -d 10"` into words, honoring single and double quotes.
pub fn split_command_line(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (Some('"') | None, '\\') => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (_, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, CliError> {
    let mut args = args
        .into_iter()
        .map(|a| a.into_string().map_err(|_| CliError::NotUnicode))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter();
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };
        let value = |args: &mut std::vec::IntoIter<String>| {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| CliError::MissingValue(name.clone()))
        };
        match name.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "-e" | "-x" | "--command" | "--" => {
                let mut words: Vec<String> =
                    inline.clone().into_iter().chain(args.by_ref()).collect();
                if words.len() == 1 && name != "--" {
                    words = split_command_line(&words[0]);
                }
                let mut words = words.into_iter();
                match words.next() {
                    Some(program) => options.command = Some((program, words.collect())),
                    None if name == "--" => {}
                    None => return Err(CliError::MissingValue(name)),
                }
            }
            "--working-directory" | "-w" => {
                options.working_directory = Some(PathBuf::from(value(&mut args)?))
            }
            "--title" | "-T" => options.title = Some(value(&mut args)?),
            "--screenshot" => options.screenshot = Some(PathBuf::from(value(&mut args)?)),
            "--screenshot-light" => options.screenshot_light = true,
            _ => return Err(CliError::Unknown(arg)),
        }
    }
    Ok(Command::Run(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Options, CliError> {
        match parse(args.iter().map(OsString::from))? {
            Command::Run(options) => Ok(options),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn defaults_help_and_version() {
        assert_eq!(run(&[]), Ok(Options::default()));
        assert_eq!(parse(["--help"].map(OsString::from)), Ok(Command::Help));
        assert_eq!(parse(["-V"].map(OsString::from)), Ok(Command::Version));
    }

    #[test]
    fn commands_take_the_rest() {
        let options =
            run(&["--title", "Logs", "-e", "tail", "-f", "/var/log/syslog"]).expect("parses");
        assert_eq!(
            options.command,
            Some(("tail".into(), vec!["-f".into(), "/var/log/syslog".into()]))
        );
        assert_eq!(options.title.as_deref(), Some("Logs"));
        let options = run(&["--command", "htop -d 10"]).expect("parses");
        assert_eq!(options.command, Some(("htop".into(), vec!["-d".into(), "10".into()])));
        let options = run(&["--command=vim"]).expect("parses");
        assert_eq!(options.command, Some(("vim".into(), vec![])));
        let options = run(&["--", "ls", "-l"]).expect("parses");
        assert_eq!(options.command, Some(("ls".into(), vec!["-l".into()])));
        assert_eq!(run(&["--"]), Ok(Options::default()));
        let options = run(&["-e", "printf", "%s\\n", "arg with spaces"]).expect("parses");
        assert_eq!(
            options.command,
            Some(("printf".into(), vec!["%s\\n".into(), "arg with spaces".into()]))
        );
        let options = run(&["--", "ls", "--title", "-e"]).expect("parses");
        assert_eq!(
            options.command,
            Some(("ls".into(), vec!["--title".into(), "-e".into()])),
            "options after the program belong to it"
        );
        assert_eq!(run(&["-e"]), Err(CliError::MissingValue("-e".into())));
    }

    /// `nimbus_xdg::launch` runs `nimbus-terminal -- <argv>` for `Terminal=true` entries.
    #[test]
    fn double_dash_keeps_a_single_program_whole() {
        let options = run(&["--", "/opt/My Tools/monitor"]).expect("parses");
        assert_eq!(options.command, Some(("/opt/My Tools/monitor".into(), vec![])));
        let options = run(&["--", "it's"]).expect("parses");
        assert_eq!(options.command, Some(("it's".into(), vec![])));
    }

    #[test]
    fn values_inline_and_separate() {
        let options =
            run(&["--working-directory=/tmp", "--screenshot", "out.png", "--screenshot-light"])
                .expect("parses");
        assert_eq!(options.working_directory, Some(PathBuf::from("/tmp")));
        assert_eq!(options.screenshot, Some(PathBuf::from("out.png")));
        assert!(options.screenshot_light);
        assert_eq!(run(&["--title"]), Err(CliError::MissingValue("--title".into())));
        assert_eq!(run(&["--bogus"]), Err(CliError::Unknown("--bogus".into())));
    }

    #[test]
    fn command_line_splitting() {
        assert_eq!(split_command_line("a  b\tc"), ["a", "b", "c"]);
        assert_eq!(
            split_command_line(r#"sh -c "echo 'hi there'""#),
            ["sh", "-c", "echo 'hi there'"]
        );
        assert_eq!(split_command_line(r#"a\ b 'c\d' """#), ["a b", r"c\d", ""]);
        assert!(split_command_line("   ").is_empty());
    }
}
