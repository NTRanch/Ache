//! ache — the unified haptics MCP stdio server. One server, two
//! providers behind a seam: Lovense toys over the Standard API local
//! Game Mode route, and Buttplug devices over an embedded in-process
//! Buttplug server. Every output on either provider passes the pure
//! `decide()` gate in `decide.rs` before it can reach a device.
//!
//! The protocol layer is rmcp, modern era only (2026-07-28), behind
//! the byte-level era guard in `guard.rs`, which owns the stdio
//! framing contract the pre-migration hand-rolled server defined.
//! Configuration still fails closed before anything serves.

#![forbid(unsafe_code)]

mod config;
mod decide;
mod error;
mod guard;
mod provider;
mod provider_buttplug;
mod server;
mod service;
mod state;
#[cfg(test)]
mod test_stub;
mod transport;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use config::{CONFIG_ENV, Config, TransportKind};
use provider::{DeviceProvider, FakeButtplug, InertProvider, ProviderId};
use rmcp::ServiceExt;
use server::Server;
use service::AcheService;

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
    let server = Arc::new(Server::new(config, transport).with_buttplug(buttplug));
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("ache: stdio failed: {error}");
            return ExitCode::from(EXIT_IO_FAILED);
        }
    };
    let outcome = runtime.block_on(run(Arc::clone(&server)));
    // End of input (or a dead transport): the explicit shutdown path
    // zeroes any active output before the process goes, exactly as
    // the pre-migration serve loop did on EOF.
    if server.zero_on_shutdown() {
        eprintln!("ache: input closed with output active; stop sent, output zeroed");
    }
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ache: stdio failed: {error}");
            ExitCode::from(EXIT_IO_FAILED)
        }
    }
}

/// Serve one stdio session: the era guard reads real stdin and pumps
/// SDK responses to real stdout; the rmcp service lives on the other
/// end of an in-process duplex and only ever sees the frames the
/// guard forwards. A closed transport (the guard shut the duplex at
/// EOF) is a clean end of session, not an error.
async fn run(server: Arc<Server>) -> Result<(), Box<dyn std::error::Error>> {
    let stdout: guard::SharedStdout = Arc::new(Mutex::new(std::io::stdout()));
    let (client_side, service_side) = tokio::io::duplex(64 * 1024);
    let (service_read, service_write) = tokio::io::split(service_side);
    let (pump_read, guard_write) = tokio::io::split(client_side);
    let (eof_tx, eof_rx) = tokio::sync::oneshot::channel::<()>();
    let guard_task = tokio::spawn(guard::run(stdout, guard_write, pump_read, eof_tx));
    let watchdog = service::spawn_watchdog(Arc::clone(&server));
    let service = AcheService::new(server);
    let running = match service.serve((service_read, service_write)).await {
        Ok(running) => running,
        // The transport closed before the discover bootstrap
        // completed (a client that connects and goes away): that is
        // end of input, the clean EOF path, not a failure.
        Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => {
            watchdog.abort();
            guard_task.abort();
            return Ok(());
        }
        Err(error) => {
            watchdog.abort();
            guard_task.abort();
            return Err(Box::new(error));
        }
    };
    // The session ends when the guard reports end of input: the
    // guard shut the duplex, the service's transport closed, and its
    // task settles. A closed transport is a clean end of session;
    // only a failed service task is an error.
    let _ = eof_rx.await;
    let outcome = match running.waiting().await {
        Ok(_) => Ok(()),
        Err(error) => Err(Box::new(error) as Box<dyn std::error::Error>),
    };
    watchdog.abort();
    guard_task.abort();
    outcome
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
