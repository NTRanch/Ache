//! lovense-mcp — MCP stdio server over the Lovense Standard API local
//! Game Mode route. Stage S2: the read core (status and toy discovery)
//! plus the output core, every output behind the pure `decide()` gate.

#![forbid(unsafe_code)]

mod config;
mod decide;
mod error;
mod server;
mod state;
#[cfg(test)]
mod test_stub;
mod transport;

use std::process::ExitCode;

use config::{CONFIG_ENV, Config};
use server::Server;

/// Exit status for a refused configuration.
const EXIT_CONFIG_INVALID: u8 = 2;
/// Exit status when stdio itself fails.
const EXIT_IO_FAILED: u8 = 1;

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
    eprintln!(
        "lovense-mcp {}: transport {}, S2 output core (decide()-gated)",
        server::SERVER_VERSION,
        config.transport.as_str()
    );
    let transport = transport::from_config(&config);
    let server = Server::new(config, transport);
    match server::serve(
        &server,
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout().lock(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lovense-mcp: stdio failed: {error}");
            ExitCode::from(EXIT_IO_FAILED)
        }
    }
}
