// SPDX-License-Identifier: MIT

//! Command-line parsing.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::page::{Page, UnknownPage};

pub const USAGE: &str = "\
Usage: nimbus-settings [--page <page>] [--config <path>]

Options:
  --page <page>     Open a page: network, bluetooth, sound, appearance, panel,
                    workspaces, input, shortcuts, power, displays, notifications,
                    default-apps, date-time, or about
  --config <path>   Edit this file instead of $XDG_CONFIG_HOME/nimbus/config.toml
  -h, --help        Print this help
  -V, --version     Print the version";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub page: Page,
    pub config: Option<PathBuf>,
    /// Renders the window with sample data into this PNG file and exits.
    pub screenshot: Option<PathBuf>,
    /// The color scheme for `screenshot`; dark when unset.
    pub screenshot_light: bool,
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Run(Options),
    Help,
    Version,
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum CliError {
    #[error("{0} needs a value")]
    MissingValue(&'static str),
    #[error(transparent)]
    Page(#[from] UnknownPage),
    #[error("unknown argument '{0}'")]
    Unknown(String),
    #[error("--screenshot-scheme must be 'light' or 'dark', not '{0}'")]
    Scheme(String),
}

/// Parses the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, CliError> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg.to_string_lossy().into_owned();
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = |name: &'static str| -> Result<OsString, CliError> {
            match &inline {
                Some(v) => Ok(v.into()),
                None => args.next().ok_or(CliError::MissingValue(name)),
            }
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--page" => options.page = value("--page")?.to_string_lossy().parse()?,
            "--config" => options.config = Some(value("--config")?.into()),
            "--screenshot" => options.screenshot = Some(value("--screenshot")?.into()),
            "--screenshot-scheme" => {
                let scheme = value("--screenshot-scheme")?.to_string_lossy().into_owned();
                options.screenshot_light = match scheme.as_str() {
                    "light" => true,
                    "dark" => false,
                    _ => return Err(CliError::Scheme(scheme)),
                };
            }
            _ => return Err(CliError::Unknown(arg)),
        }
    }
    Ok(Command::Run(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Command, CliError> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn pages_and_paths() {
        assert_eq!(run(&[]), Ok(Command::Run(Options::default())));
        let Ok(Command::Run(options)) = run(&["--page", "shortcuts", "--config=/tmp/c.toml"])
        else {
            panic!("expected options");
        };
        assert_eq!(options.page, Page::Shortcuts);
        assert_eq!(options.config, Some(PathBuf::from("/tmp/c.toml")));
        let Ok(Command::Run(options)) =
            run(&["--page=about", "--screenshot", "out.png", "--screenshot-scheme", "light"])
        else {
            panic!("expected options");
        };
        assert_eq!(options.page, Page::About);
        assert_eq!(options.screenshot, Some(PathBuf::from("out.png")));
        assert!(options.screenshot_light);
        // The shell's quick settings open these.
        for (id, page) in [
            ("network", Page::Network),
            ("bluetooth", Page::Bluetooth),
            ("sound", Page::Sound),
            ("date-time", Page::DateTime),
        ] {
            let Ok(Command::Run(options)) = run(&["--page", id]) else {
                panic!("expected options")
            };
            assert_eq!(options.page, page);
        }
    }

    #[test]
    fn errors_and_info() {
        assert_eq!(run(&["--help", "--bogus"]), Ok(Command::Help));
        assert_eq!(run(&["-V"]), Ok(Command::Version));
        assert_eq!(run(&["--page"]), Err(CliError::MissingValue("--page")));
        assert!(matches!(run(&["--page", "audio"]), Err(CliError::Page(_))));
        assert_eq!(run(&["--bogus"]), Err(CliError::Unknown("--bogus".into())));
        assert_eq!(run(&["--screenshot-scheme=sepia"]), Err(CliError::Scheme("sepia".into())));
    }
}
