// SPDX-License-Identifier: MIT

//! Command-line arguments.

use std::path::PathBuf;

pub const USAGE: &str = "Usage: nimbus-files [PATH]\n\nBrowse files and folders, starting at PATH or the home folder.\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cli {
    Run {
        path: Option<PathBuf>,
    },
    /// Renders the main window with sample data into a PNG, for documentation.
    Screenshot {
        output: PathBuf,
        light: bool,
        list: bool,
        scene: crate::screenshot::Scene,
    },
    Help,
    Version,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    #[error("unknown option {0}")]
    UnknownOption(String),
    #[error("--screenshot needs an output path")]
    MissingScreenshotPath,
    #[error("only one path can be given")]
    TooManyPaths,
    #[error("{0}")]
    BadScene(String),
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Cli, CliError> {
    let mut path = None;
    let mut screenshot = None;
    let mut light = false;
    let mut list = false;
    let mut scene = crate::screenshot::Scene::Main;
    let mut args = args.into_iter();
    let mut only_paths = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            _ if only_paths => {}
            "-h" | "--help" => return Ok(Cli::Help),
            "-V" | "--version" => return Ok(Cli::Version),
            "--screenshot" => {
                screenshot =
                    Some(args.next().map(PathBuf::from).ok_or(CliError::MissingScreenshotPath)?);
                continue;
            }
            "--light" => {
                light = true;
                continue;
            }
            "--list" => {
                list = true;
                continue;
            }
            "--scene" => {
                let name = args.next().unwrap_or_default();
                scene = name.parse().map_err(CliError::BadScene)?;
                continue;
            }
            "--" => {
                only_paths = true;
                continue;
            }
            option if option.starts_with('-') && option.len() > 1 => {
                return Err(CliError::UnknownOption(arg));
            }
            _ => {}
        }
        if path.is_some() {
            return Err(CliError::TooManyPaths);
        }
        path = Some(location_arg(&arg));
    }
    Ok(match screenshot {
        Some(output) => Cli::Screenshot { output, light, list, scene },
        None => Cli::Run { path },
    })
}

/// Accepts plain paths and `file://` URIs, as launchers pass either.
fn location_arg(arg: &str) -> PathBuf {
    let path = crate::core::uri::uri_to_path(arg).unwrap_or_else(|| PathBuf::from(arg));
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map(|cwd| cwd.join(&path)).unwrap_or(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Cli, CliError> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses() {
        assert_eq!(parse_args(&[]), Ok(Cli::Run { path: None }));
        assert_eq!(parse_args(&["/tmp"]), Ok(Cli::Run { path: Some("/tmp".into()) }));
        assert_eq!(parse_args(&["file:///a%20b"]), Ok(Cli::Run { path: Some("/a b".into()) }));
        assert_eq!(parse_args(&["--help"]), Ok(Cli::Help));
        assert_eq!(parse_args(&["-V"]), Ok(Cli::Version));
        assert_eq!(
            parse_args(&["--screenshot", "out.png", "--light"]),
            Ok(Cli::Screenshot {
                output: "out.png".into(),
                light: true,
                list: false,
                scene: crate::screenshot::Scene::Main
            })
        );
        assert_eq!(parse_args(&["--screenshot"]), Err(CliError::MissingScreenshotPath));
        assert!(matches!(
            parse_args(&["--screenshot", "a.png", "--scene", "menu"]),
            Ok(Cli::Screenshot { scene: crate::screenshot::Scene::Menu, .. })
        ));
        assert_eq!(
            parse_args(&["--scene", "nope"]),
            Err(CliError::BadScene("unknown scene nope".into()))
        );
        assert_eq!(parse_args(&["--bogus"]), Err(CliError::UnknownOption("--bogus".into())));
        assert_eq!(parse_args(&["/a", "/b"]), Err(CliError::TooManyPaths));
        assert_eq!(
            parse_args(&["--", "-dash"]).map(|c| matches!(c, Cli::Run { path: Some(_) })),
            Ok(true)
        );
        let relative = parse_args(&["docs"]).expect("parsed");
        assert!(
            matches!(relative, Cli::Run { path: Some(p) } if p.is_absolute() && p.ends_with("docs"))
        );
    }
}
