// SPDX-License-Identifier: MIT

use std::process::ExitCode;

use nimbus_files::cli::{self, Cli};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let result = match cli::parse(std::env::args().skip(1)) {
        Ok(Cli::Help) => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        Ok(Cli::Version) => {
            println!("nimbus-files {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Ok(Cli::Run { path }) => nimbus_files::app::run(path),
        Ok(Cli::Screenshot { output, light, list, scene }) => {
            nimbus_files::screenshot::render_to_file(
                &output,
                nimbus_files::screenshot::Options { light, list, scene },
            )
        }
        Err(error) => {
            eprintln!("nimbus-files: {error}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nimbus-files: {error:#}");
            ExitCode::FAILURE
        }
    }
}
