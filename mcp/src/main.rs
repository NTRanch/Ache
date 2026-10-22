//! lovense-mcp — MCP stdio server over the Lovense Standard API local
//! Game Mode route. Stage S1: the read core (status and toy discovery).
//! There is no output path in this stage.

#![forbid(unsafe_code)]

mod config;
mod error;
#[cfg(test)]
mod test_stub;
mod transport;

use std::process::ExitCode;

use config::{CONFIG_ENV, Config};

/// Exit status for a refused configuration.
const EXIT_CONFIG_INVALID: u8 = 2;

fn main() -> ExitCode {
    // Fail closed: refuse to serve under a config that does not validate.
    let config = match std::env::var_os(CONFIG_ENV) {
        None => Config::default(),
        Some(path) => match Config::load(std::path::Path::new(&path)) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("{}", error.to_json());
                return ExitCode::from(EXIT_CONFIG_INVALID);
            }
        },
    };
    eprintln!("lovense-mcp: transport {}", config.transport.as_str());
    ExitCode::SUCCESS
}
