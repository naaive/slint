// SPDX-License-Identifier: MIT

//! Command-line parsing.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::core::prefs::Page;

pub const USAGE: &str = "\
Usage: nimbus-monitor [OPTIONS]

Options:
  --page <processes|resources|file-systems>  Open on this page
  -h, --help                                 Show this help
  -V, --version                              Show the version";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub page: Option<Page>,
    /// Hidden: render a window with sample data into this PNG file and exit.
    pub screenshot: Option<PathBuf>,
    /// Hidden: use the light scheme for `screenshot`.
    pub light: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Run(Options),
    Help,
    Version,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    #[error("unknown option '{0}'")]
    Unknown(String),
    #[error("'{0}' needs a value")]
    MissingValue(&'static str),
    #[error("unknown page '{0}'; use processes, resources, or file-systems")]
    Page(String),
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, CliError> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--page" => {
                let value = args.next().ok_or(CliError::MissingValue("--page"))?;
                let value = value.to_string_lossy();
                options.page = Some(
                    Page::from_name(&value).ok_or_else(|| CliError::Page(value.into_owned()))?,
                );
            }
            "--screenshot" => {
                options.screenshot =
                    Some(args.next().ok_or(CliError::MissingValue("--screenshot"))?.into());
            }
            "--light" => options.light = true,
            other => return Err(CliError::Unknown(other.to_owned())),
        }
    }
    Ok(Command::Run(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_options() {
        assert_eq!(parse(args(&[])), Ok(Command::Run(Options::default())));
        assert_eq!(parse(args(&["-V"])), Ok(Command::Version));
        assert_eq!(parse(args(&["--help", "--bogus"])), Ok(Command::Help));
        assert_eq!(
            parse(args(&["--page", "resources", "--screenshot", "a.png", "--light"])),
            Ok(Command::Run(Options {
                page: Some(Page::Resources),
                screenshot: Some("a.png".into()),
                light: true
            }))
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(parse(args(&["--page"])), Err(CliError::MissingValue("--page")));
        assert_eq!(parse(args(&["--page", "x"])), Err(CliError::Page("x".into())));
        assert_eq!(parse(args(&["-x"])), Err(CliError::Unknown("-x".into())));
    }
}
