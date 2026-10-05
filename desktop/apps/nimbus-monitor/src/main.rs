// SPDX-License-Identifier: MIT

use std::process::ExitCode;

use nimbus_monitor::cli::{self, Command};

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
            println!("nimbus-monitor {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            eprintln!("nimbus-monitor: {err}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let result = match &options.screenshot {
        Some(path) => {
            nimbus_monitor::screenshot::run(path, options.page.unwrap_or_default(), options.light)
        }
        None => nimbus_monitor::app::run(options.page),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("nimbus-monitor: {err:#}");
            ExitCode::FAILURE
        }
    }
}
