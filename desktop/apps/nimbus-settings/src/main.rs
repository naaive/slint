// SPDX-License-Identifier: MIT

use std::process::ExitCode;

use nimbus_settings::cli::{self, Command};
use nimbus_settings::view::{App, AppOptions};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let options = match cli::parse(std::env::args_os().skip(1)) {
        Ok(Command::Run(options)) => options,
        Ok(Command::Help) => {
            println!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            println!("nimbus-settings {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("nimbus-settings: {error}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nimbus-settings: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(options: cli::Options) -> anyhow::Result<()> {
    if let Some(path) = &options.screenshot {
        return nimbus_settings::screenshot::run(path, options.page, options.screenshot_light);
    }
    let config_path = match options.config {
        Some(path) => path,
        None => nimbus_config::default_path()?,
    };
    let app = App::new(AppOptions::system(config_path, options.page))?;
    if let Err(error) = slint::set_xdg_app_id("org.nimbus.Settings") {
        tracing::warn!("cannot set the application id: {error}");
    }
    app.run()?;
    Ok(())
}
