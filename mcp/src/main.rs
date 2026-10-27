//! ache — the unified haptics MCP stdio server. One server, two
//! providers behind a seam: Lovense toys over the Standard API local
//! Game Mode route, and Buttplug devices over an embedded in-process
//! Buttplug server. Every output on either provider passes the pure
//! `decide()` gate in `decide.rs` before it can reach a device.

#![forbid(unsafe_code)]

mod config;
mod decide;
mod error;
mod provider;
mod provider_buttplug;
mod server;
mod state;
#[cfg(test)]
mod test_stub;
mod transport;

use std::process::ExitCode;

use config::{CONFIG_ENV, Config, TransportKind};
use provider::{DeviceProvider, FakeButtplug, InertProvider, ProviderId};
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
        "ache {}: lovense transport {}, buttplug {}, unified output core (decide()-gated)",
        server::SERVER_VERSION,
        config.transport.as_str(),
        if config.buttplug_enabled {
            "embedded"
        } else {
            "disabled"
        },
    );
    let transport = transport::from_config(&config);
    let buttplug = buttplug_provider(&config);
    let server = Server::new(config, transport).with_buttplug(buttplug);
    match server::serve(
        &server,
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout().lock(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ache: stdio failed: {error}");
            ExitCode::from(EXIT_IO_FAILED)
        }
    }
}

/// Wire the Buttplug provider for a config, mirroring how
/// `transport::from_config` wires the Lovense one: mock mode gets the
/// fixture provider (labelled fixture everywhere its data appears);
/// real mode gets the embedded in-process server; a disabled provider
/// is inert and says so. A failed embedded start is an unavailable
/// provider, never a dead server: the Lovense side keeps serving.
fn buttplug_provider(config: &Config) -> Box<dyn DeviceProvider> {
    if !config.buttplug_enabled {
        return Box::new(InertProvider::disabled(
            ProviderId::Buttplug,
            "buttplug provider is disabled by configuration ([buttplug] enabled = false)",
        ));
    }
    match config.transport {
        TransportKind::Mock => Box::new(FakeButtplug::new()),
        TransportKind::Real => Box::new(provider_buttplug::EmbeddedButtplug::start(
            config.buttplug_scan_ms,
        )),
    }
}
