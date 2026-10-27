//! The embedded Buttplug provider: a Buttplug server running
//! in-process (the `buttplug` crates' in-process client connector), so
//! the unified server needs no running Intiface instance. BLE and USB
//! serial hardware managers are registered; the websocket-server
//! manager is deliberately NOT — enabling it would open a listening
//! network socket as a side effect of starting a haptics server — and
//! neither are the Lovense dongle/connect managers, because the
//! Lovense provider already covers Lovense hardware.
//!
//! Shape: the provider owns one thread running a tokio runtime and
//! the in-process client. The MCP thread sends commands over a channel
//! and waits with a bounded timeout, so a wedged provider can never
//! wedge the server: a timeout or a dead thread is a typed error.
//!
//! Duration discipline: Buttplug output commands have no built-in
//! duration, while the gate's contract says indefinite output does not
//! exist. The provider therefore schedules an auto-zero for every
//! function output at the plan's `time_sec`, patterns run as stepping
//! tasks that end by zeroing what they drove, and a zero (stop,
//! disarm, watchdog) aborts every pending task before stopping all
//! devices. A new command to a device supersedes its pending tasks.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use buttplug_client::ButtplugClient;
use buttplug_client::device::ButtplugClientDevice;
use buttplug_client::device::{ClientDeviceCommandValue, ClientDeviceOutputCommand};
use buttplug_client_in_process::ButtplugInProcessClientConnectorBuilder;
use buttplug_core::message::OutputType;
use buttplug_server::ButtplugServerBuilder;
use buttplug_server::device::ServerDeviceManagerBuilder;
use buttplug_server_device_config::DeviceConfigurationManagerBuilder;
use buttplug_server_hwmgr_btleplug::BtlePlugCommunicationManagerBuilder;
use buttplug_server_hwmgr_serial::SerialPortCommunicationManagerBuilder;

use crate::decide::{AllowedOutput, pattern_letter_action};
use crate::error::{AcheError, ErrorCode, Result, err, truncate};
use crate::provider::{Capability, Device, DeviceProvider, ProviderId, ProviderProbe};

/// How long startup may take before the provider reports unavailable.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// Reply bound for one execute (commands are acknowledged, not run to
/// completion, so this is generous).
const EXECUTE_TIMEOUT: Duration = Duration::from_secs(10);
/// Reply bound for a zero. Stop must be fast; the bound matches the
/// server's stop-latency expectations with headroom.
const ZERO_TIMEOUT: Duration = Duration::from_secs(5);
/// Slack added to the configured scan window for a discovery reply.
const DISCOVER_SLACK: Duration = Duration::from_secs(10);

enum Command {
    Discover(Sender<Result<Vec<Device>>>),
    Execute(AllowedOutput, Sender<Result<()>>),
    Zero(Sender<Result<bool>>),
}

/// The embedded provider's handle. Construction never fails outwardly:
/// a stack that cannot start is a provider whose probe says
/// `unavailable` and whose operations return the typed error — the
/// Lovense provider keeps serving.
pub struct EmbeddedButtplug {
    sender: Option<Sender<Command>>,
    start_error: Option<String>,
    scan_ms: u32,
}

impl EmbeddedButtplug {
    pub fn start(scan_ms: u32) -> EmbeddedButtplug {
        let (command_sender, command_receiver) = channel::<Command>();
        let (ready_sender, ready_receiver) = channel::<std::result::Result<(), String>>();
        let spawned = std::thread::Builder::new()
            .name("ache-buttplug".to_string())
            .spawn(move || provider_thread(command_receiver, ready_sender, scan_ms));
        match spawned {
            Ok(_handle) => match ready_receiver.recv_timeout(START_TIMEOUT) {
                Ok(Ok(())) => EmbeddedButtplug {
                    sender: Some(command_sender),
                    start_error: None,
                    scan_ms,
                },
                Ok(Err(error)) => EmbeddedButtplug {
                    sender: None,
                    start_error: Some(error),
                    scan_ms,
                },
                Err(_) => EmbeddedButtplug {
                    sender: None,
                    start_error: Some(
                        "embedded buttplug provider did not start (timeout or thread exit)"
                            .to_string(),
                    ),
                    scan_ms,
                },
            },
            Err(error) => EmbeddedButtplug {
                sender: None,
                start_error: Some(format!("could not spawn buttplug thread: {error}")),
                scan_ms,
            },
        }
    }

    fn call<T>(
        &self,
        build: impl FnOnce(Sender<Result<T>>) -> Command,
        timeout: Duration,
        what: &str,
    ) -> Result<T> {
        let Some(sender) = &self.sender else {
            return err(
                ErrorCode::ProviderUnavailable,
                format!(
                    "embedded buttplug provider is unavailable: {}",
                    self.start_error.as_deref().unwrap_or("not started")
                ),
            );
        };
        let (reply_sender, reply_receiver) = channel();
        if sender.send(build(reply_sender)).is_err() {
            return err(
                ErrorCode::ProviderUnavailable,
                "embedded buttplug provider thread is gone",
            );
        }
        match reply_receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => err(
                ErrorCode::TargetUnreachable,
                format!("embedded buttplug provider did not answer a {what} within {timeout:?}"),
            ),
        }
    }
}

impl DeviceProvider for EmbeddedButtplug {
    fn probe(&self) -> ProviderProbe {
        match (&self.sender, &self.start_error) {
            (Some(_), _) => ProviderProbe {
                provider: ProviderId::Buttplug,
                state: "ready",
                available: true,
                note: "embedded buttplug server in-process (BLE + serial managers); \
                    devices appear after a discovery scan"
                    .to_string(),
            },
            (None, error) => ProviderProbe {
                provider: ProviderId::Buttplug,
                state: "unavailable",
                available: false,
                note: format!(
                    "embedded buttplug failed to start: {}",
                    error.as_deref().unwrap_or("unknown")
                ),
            },
        }
    }

    fn discover(&self) -> Result<Vec<Device>> {
        let timeout = Duration::from_millis(u64::from(self.scan_ms)) + DISCOVER_SLACK;
        self.call(Command::Discover, timeout, "discovery scan")
    }

    fn execute(&self, plan: &AllowedOutput) -> Result<()> {
        self.call(
            |reply| Command::Execute(plan.clone(), reply),
            EXECUTE_TIMEOUT,
            "execute",
        )
    }

    fn zero(&self) -> Result<bool> {
        self.call(Command::Zero, ZERO_TIMEOUT, "zero")
    }
}

// ------------------------------------------------------- provider side

fn provider_thread(
    commands: Receiver<Command>,
    ready: Sender<std::result::Result<(), String>>,
    scan_ms: u32,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(format!("tokio runtime: {error}")));
            return;
        }
    };
    let client = match runtime.block_on(connect()) {
        Ok(client) => client,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let mut tasks = TaskSet::default();
    while let Ok(command) = commands.recv() {
        match command {
            Command::Discover(reply) => {
                let result = runtime.block_on(discover(&client, scan_ms));
                let _ = reply.send(result);
            }
            Command::Execute(plan, reply) => {
                let result = runtime.block_on(execute(&client, &mut tasks, &plan));
                let _ = reply.send(result);
            }
            Command::Zero(reply) => {
                tasks.abort_all();
                let result = runtime.block_on(zero(&client));
                let _ = reply.send(result);
            }
        }
    }
    // The server is gone: stop everything, best effort, then exit.
    tasks.abort_all();
    let _ = runtime.block_on(client.stop_all_devices());
}

/// Build the embedded server (BLE + serial managers only) and connect
/// an in-process client to it.
async fn connect() -> std::result::Result<ButtplugClient, String> {
    let config_manager = DeviceConfigurationManagerBuilder::default()
        .finish()
        .map_err(|error| format!("device configuration: {error}"))?;
    let mut manager_builder = ServerDeviceManagerBuilder::new(config_manager);
    manager_builder.comm_manager(BtlePlugCommunicationManagerBuilder::default());
    manager_builder.comm_manager(SerialPortCommunicationManagerBuilder::default());
    let device_manager = manager_builder
        .finish()
        .map_err(|error| format!("device manager: {error}"))?;
    let server = ButtplugServerBuilder::new(device_manager)
        .finish()
        .map_err(|error| format!("buttplug server: {error}"))?;
    let connector = ButtplugInProcessClientConnectorBuilder::default()
        .server(server)
        .finish();
    let client = ButtplugClient::new("ache");
    client
        .connect(connector)
        .await
        .map_err(|error| format!("buttplug client connect: {error}"))?;
    Ok(client)
}

async fn discover(client: &ButtplugClient, scan_ms: u32) -> Result<Vec<Device>> {
    client
        .start_scanning()
        .await
        .map_err(|error| transport_error("start scanning", &error))?;
    tokio::time::sleep(Duration::from_millis(u64::from(scan_ms))).await;
    let _ = client.stop_scanning().await;
    let mut devices = Vec::new();
    for device in client.devices().values() {
        devices.push(map_device(device).await);
    }
    devices.sort_by_key(|device| device.id.clone());
    Ok(devices)
}

async fn zero(client: &ButtplugClient) -> Result<bool> {
    client
        .stop_all_devices()
        .await
        .map_err(|error| transport_error("stop all devices", &error))?;
    Ok(true)
}

/// Pending per-device tasks (pattern stepping, scheduled auto-zeros).
/// Any new command to a device, and any zero, aborts its tasks first:
/// output never outlives the intent that scheduled it.
#[derive(Default)]
struct TaskSet {
    by_device: HashMap<u32, Vec<tokio::task::AbortHandle>>,
}

impl TaskSet {
    fn add(&mut self, device: u32, handle: tokio::task::AbortHandle) {
        self.by_device.entry(device).or_default().push(handle);
    }

    fn abort_device(&mut self, device: u32) {
        if let Some(handles) = self.by_device.remove(&device) {
            for handle in handles {
                handle.abort();
            }
        }
    }

    fn abort_all(&mut self) {
        for (_, handles) in self.by_device.drain() {
            for handle in handles {
                handle.abort();
            }
        }
    }
}

/// (output type, native name, unified feature it maps to). Outputs
/// with no unified mapping are still reported as capabilities — as
/// data, with `max: None`, so the gate refuses them by name.
const OUTPUTS: &[(OutputType, &str, Option<&str>)] = &[
    (OutputType::Vibrate, "Vibrate", Some("Vibrate")),
    (OutputType::Rotate, "Rotate", Some("Rotate")),
    (OutputType::Oscillate, "Oscillate", Some("Oscillate")),
    // Buttplug's constriction output is the air-pump action (the
    // Lovense Max family): it maps to the unified Pump feature.
    (OutputType::Constrict, "Constrict", Some("Pump")),
    (OutputType::Position, "Position", None),
    (
        OutputType::HwPositionWithDuration,
        "PositionWithDuration",
        None,
    ),
    (OutputType::Temperature, "Temperature", None),
    (OutputType::Led, "Led", None),
    (OutputType::Spray, "Spray", None),
];

/// The unified strength scale's top, for percent conversion.
const UNIFIED_SCALE: f64 = 20.0;

async fn map_device(device: &ButtplugClientDevice) -> Device {
    let mut capabilities: Vec<Capability> = Vec::new();
    for feature in device.device_features().values() {
        let feature = feature.feature();
        for (output_type, native, unified) in OUTPUTS {
            if let Some(limits) = feature.get_output_limits(*output_type) {
                capabilities.push(Capability {
                    feature: unified.unwrap_or(native).to_string(),
                    max: unified.map(|_| 20),
                    native: (*native).to_string(),
                    steps: Some(limits.step_count()),
                });
            }
        }
    }
    if capabilities.iter().any(|cap| cap.max.is_some()) {
        capabilities.push(Capability {
            feature: "All".to_string(),
            max: Some(20),
            native: "All".to_string(),
            steps: None,
        });
    }
    let battery = device
        .battery()
        .await
        .ok()
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| *value <= 100);
    Device {
        id: format!("buttplug:{}", device.index()),
        provider: ProviderId::Buttplug,
        name: device
            .display_name()
            .clone()
            .unwrap_or_else(|| device.name().clone()),
        kind: Some(device.name().clone()),
        battery,
        capabilities,
        capability_source: "reported",
    }
}

fn find_device(client: &ButtplugClient, unified_id: &str) -> Result<ButtplugClientDevice> {
    let index: Option<u32> = unified_id
        .strip_prefix("buttplug:")
        .and_then(|rest| rest.parse().ok());
    match index.and_then(|index| client.devices().get(&index).cloned()) {
        Some(device) => Ok(device),
        None => err(
            ErrorCode::DeviceNotFound,
            format!("no buttplug device {unified_id:?} is present"),
        ),
    }
}

/// Drive one unified feature on one device at one unified strength.
/// The gate has already validated and clamped; this only translates.
async fn run_feature(device: &ButtplugClientDevice, feature: &str, strength: i64) -> Result<()> {
    let value = ClientDeviceCommandValue::Percent(f64::from(strength as i32) / UNIFIED_SCALE);
    let commands: Vec<ClientDeviceOutputCommand> = match feature {
        "Vibrate" => vec![ClientDeviceOutputCommand::Vibrate(value)],
        "Rotate" => vec![ClientDeviceOutputCommand::Rotate(value)],
        "Oscillate" => vec![ClientDeviceOutputCommand::Oscillate(value)],
        "Pump" => vec![ClientDeviceOutputCommand::Constrict(value)],
        "All" => [
            OutputType::Vibrate,
            OutputType::Rotate,
            OutputType::Oscillate,
            OutputType::Constrict,
        ]
        .into_iter()
        .filter(|output| device.output_available(*output))
        .filter_map(|output| ClientDeviceOutputCommand::from_command_value(output, &value).ok())
        .collect(),
        _ => {
            return err(
                ErrorCode::CapabilityUnsupported,
                format!("feature {feature} has no buttplug output mapping"),
            );
        }
    };
    for command in &commands {
        device
            .run_output(command)
            .await
            .map_err(|error| transport_error("run output", &error))?;
    }
    Ok(())
}

async fn execute(client: &ButtplugClient, tasks: &mut TaskSet, plan: &AllowedOutput) -> Result<()> {
    match plan {
        AllowedOutput::Stop => {
            tasks.abort_all();
            zero(client).await?;
            Ok(())
        }
        AllowedOutput::Function {
            feature,
            strength,
            time_sec,
            toy,
            ..
        } => {
            let device = find_device(client, toy)?;
            tasks.abort_device(device.index());
            run_feature(&device, feature, *strength).await?;
            // Honour the plan's bounded duration: Buttplug commands
            // carry none, so the zero is scheduled here and aborted
            // by any superseding command or stop.
            let sleeper = device.clone();
            let duration = Duration::from_secs(u64::try_from(*time_sec).unwrap_or(0));
            let handle = tokio::spawn(async move {
                tokio::time::sleep(duration).await;
                let _ = sleeper.stop().await;
            });
            tasks.add(device.index(), handle.abort_handle());
            Ok(())
        }
        AllowedOutput::Pattern {
            features,
            interval_ms,
            strengths,
            time_sec,
            toy,
            ..
        } => {
            let device = find_device(client, toy)?;
            tasks.abort_device(device.index());
            // The features this pattern drives: the named letters, or
            // every mapped feature the device offers when unnamed.
            let mapped = map_device(&device).await;
            let driven: Vec<String> = if features.is_empty() {
                mapped
                    .capabilities
                    .iter()
                    .filter(|cap| cap.max.is_some() && cap.feature != "All")
                    .map(|cap| cap.feature.clone())
                    .collect()
            } else {
                features
                    .bytes()
                    .filter_map(pattern_letter_action)
                    .map(str::to_string)
                    .collect()
            };
            if driven.is_empty() {
                return err(
                    ErrorCode::CapabilityUnsupported,
                    "the device offers no function a pattern can drive",
                );
            }
            let strengths = strengths.clone();
            let interval = Duration::from_millis(u64::try_from(*interval_ms).unwrap_or(0));
            let deadline = Duration::from_secs(u64::try_from(*time_sec).unwrap_or(0));
            let stepping = device.clone();
            let handle = tokio::spawn(async move {
                let started = tokio::time::Instant::now();
                let mut step = 0usize;
                loop {
                    let strength = strengths[step % strengths.len()];
                    for feature in &driven {
                        let _ = run_feature(&stepping, feature, strength).await;
                    }
                    step += 1;
                    if started.elapsed() + interval >= deadline {
                        break;
                    }
                    tokio::time::sleep(interval).await;
                    if started.elapsed() >= deadline {
                        break;
                    }
                }
                // A pattern ends by zeroing what it drove.
                for feature in &driven {
                    let _ = run_feature(&stepping, feature, 0).await;
                }
            });
            tasks.add(device.index(), handle.abort_handle());
            Ok(())
        }
        AllowedOutput::Preset { .. } => err(
            ErrorCode::CapabilityUnsupported,
            "presets are built into the Lovense Remote app; buttplug has no presets",
        ),
    }
}

fn transport_error(context: &str, error: &impl std::fmt::Display) -> AcheError {
    AcheError::new(
        ErrorCode::TargetUnreachable,
        truncate(format!("buttplug: {context}: {error}")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- adversarial --------------------------------------------------

    #[test]
    fn embedded_provider_never_fakes_devices_without_hardware() {
        // The sandbox has no Bluetooth stack and no serial devices.
        // Whatever the embedded stack does here — refuse to start,
        // start and find nothing, or fail the scan — it must be
        // honest: a typed outcome, never fixture data, never a panic,
        // and always inside the call bounds.
        let provider = EmbeddedButtplug::start(100);
        let probe = provider.probe();
        assert_eq!(probe.provider, ProviderId::Buttplug);
        match provider.discover() {
            Ok(devices) => {
                assert!(
                    devices
                        .iter()
                        .all(|device| device.capability_source != "fixture"),
                    "real discovery must never serve fixture devices"
                );
            }
            Err(error) => {
                assert!(
                    matches!(
                        error.code,
                        ErrorCode::ProviderUnavailable | ErrorCode::TargetUnreachable
                    ),
                    "typed refusal expected, got {}",
                    error.code.as_str()
                );
            }
        }
        // Zero is always safe to call, whatever state the stack is in.
        let _ = provider.zero();
    }
}
