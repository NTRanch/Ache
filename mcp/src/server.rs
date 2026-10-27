//! Hand-rolled MCP stdio server: newline-delimited JSON-RPC 2.0, protocol
//! era 2024-11-05. Ten tools. The original eight (`lovense_*`) keep their
//! names and shapes: two read-only (status, toys) and six for the output
//! core (arm, disarm, function, pattern, preset, stop). Two unified
//! read-only tools (`ache_devices`, `ache_device`) enumerate devices
//! across both providers — the Lovense Standard API provider and the
//! embedded Buttplug provider — with capabilities as data; the output
//! tools route to either provider when given a unified `device` id.
//! Every output, on either provider, passes the one `decide()` gate in
//! `decide.rs` before anything can reach a device. Protocol misuse
//! (unknown method or tool, bad arguments) is a JSON-RPC error; an
//! operational failure (no target, unreachable app, a gate deny) is a
//! tool result with `isError: true` and the structured error as its text.

use std::io::{self, BufRead, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::config::{Config, TransportKind};
use crate::decide::{
    AllowedOutput, Decision, Feature, FunctionIntent, OutputIntent, PatternIntent, PresetIntent,
    TOY_ID_BYTES_MAX, decide, decide_plan, lovense_wire,
};
use crate::error::{AcheError, ErrorCode, truncate};
use crate::provider::{Device, DeviceProvider, InertProvider, LovenseProvider, ProviderId};
use crate::state::SafetyState;
use crate::transport::Transport;

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "ache";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Longest accepted input line, in bytes. Longer lines are discarded
/// unread and answered with an invalid-request error.
pub const LINE_BYTES_MAX: usize = 1024 * 1024;
/// Most input lines buffered between the reader thread and the serve
/// loop. Bounded, so a fast sender back-pressures instead of filling
/// memory.
const LINE_QUEUE_CAPACITY: usize = 64;
/// Most unexpected argument names echoed back in a refusal.
const ARGUMENT_NAMES_REPORTED_MAX: usize = 8;
const ARGUMENT_NAME_BYTES_MAX: usize = 64;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Every tool the server serves, in `tools/list` order. The original
/// eight keep their positions; the unified tools append.
const TOOL_NAMES: [&str; 10] = [
    "lovense_status",
    "lovense_toys",
    "lovense_arm",
    "lovense_disarm",
    "lovense_function",
    "lovense_pattern",
    "lovense_preset",
    "lovense_stop",
    "ache_devices",
    "ache_device",
];

pub struct Server {
    config: Config,
    /// The Lovense provider. Its transport is `None` exactly when the
    /// config names the real transport without a host; then nothing can
    /// reach the Lovense wire.
    lovense: LovenseProvider,
    /// The Buttplug provider: embedded in production, a fixture in mock
    /// mode, or inert (disabled / not wired by a bare constructor).
    buttplug: Box<dyn DeviceProvider>,
    /// The safety state machine: arm latch, output activity, watchdog
    /// bookkeeping, session health. One state for the whole server —
    /// the master toggle and the watchdog govern both providers.
    state: Mutex<SafetyState>,
}

/// A tool dispatch failure: protocol misuse is answered with a JSON-RPC
/// error, an operational failure with an `isError` tool result.
enum CallError {
    Protocol(i64, String),
    Operational(AcheError),
}

impl From<AcheError> for CallError {
    fn from(error: AcheError) -> CallError {
        CallError::Operational(error)
    }
}

impl From<(i64, String)> for CallError {
    fn from(error: (i64, String)) -> CallError {
        CallError::Protocol(error.0, error.1)
    }
}

impl Server {
    /// A server with the Lovense provider wired; the Buttplug
    /// provider is present in the surface but inert until
    /// [`Server::with_buttplug`] replaces it (production wiring in
    /// `main` always does).
    pub fn new(config: Config, transport: Option<Box<dyn Transport>>) -> Server {
        let lovense = LovenseProvider::new(&config, transport);
        Server {
            config,
            lovense,
            buttplug: Box::new(InertProvider::unavailable(
                ProviderId::Buttplug,
                "no buttplug provider is wired in this server",
            )),
            state: Mutex::new(SafetyState::new()),
        }
    }

    /// Wire the Buttplug provider behind the seam.
    #[must_use]
    pub fn with_buttplug(mut self, buttplug: Box<dyn DeviceProvider>) -> Server {
        self.buttplug = buttplug;
        self
    }

    fn lock_state(&self) -> MutexGuard<'_, SafetyState> {
        // A poisoned lock only means a thread panicked mid-update; the
        // state value is still coherent, so keep serving.
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Whether output could reach a target at all: either provider
    /// can accept output. The Lovense half is the original rule (a
    /// transport exists and the config gate permits it); the Buttplug
    /// half is its probe.
    fn target_configured(&self) -> bool {
        self.lovense.target_configured() || self.buttplug.probe().available
    }

    fn snapshot(&self) -> crate::decide::GateSnapshot {
        self.lock_state()
            .snapshot(self.target_configured(), self.config.intensity_ceiling)
    }

    /// Handle one input line. Returns the response, or `None` for a
    /// notification (a message without `id`), which is never answered.
    pub fn handle_line(&self, line: &[u8]) -> Option<Value> {
        let message: Value = match serde_json::from_slice(line) {
            Ok(message) => message,
            Err(cause) => return Some(rpc_error(Value::Null, PARSE_ERROR, cause.to_string())),
        };
        let Some(object) = message.as_object() else {
            return Some(rpc_error(
                Value::Null,
                INVALID_REQUEST,
                "message must be an object",
            ));
        };
        let id = match object.get("id") {
            None => return None,
            Some(id @ (Value::String(_) | Value::Number(_))) => id.clone(),
            Some(_) => {
                return Some(rpc_error(
                    Value::Null,
                    INVALID_REQUEST,
                    "id must be string or number",
                ));
            }
        };
        if object.get("jsonrpc") != Some(&json!("2.0")) {
            return Some(rpc_error(id, INVALID_REQUEST, "jsonrpc must be \"2.0\""));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Some(rpc_error(id, INVALID_REQUEST, "method must be a string"));
        };
        let outcome = match method {
            "initialize" => Ok(initialize_result()),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_list() })),
            "tools/call" => self.call_tool(object.get("params")),
            _ => Err((
                METHOD_NOT_FOUND,
                format!("unknown method `{}`", clip(method)),
            )),
        };
        Some(match outcome {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => rpc_error(id, code, message),
        })
    }

    fn call_tool(&self, params: Option<&Value>) -> Result<Value, (i64, String)> {
        let Some(params) = params.and_then(Value::as_object) else {
            return Err((INVALID_PARAMS, "params must be an object".into()));
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Err((INVALID_PARAMS, "params.name must be a string".into()));
        };
        if !TOOL_NAMES.contains(&name) {
            return Err((INVALID_PARAMS, format!("unknown tool `{}`", clip(name))));
        }
        // A tool call is a sign of life: fire the watchdog first if the
        // window already elapsed in silence, then count this call as
        // the latest heartbeat.
        self.trip_watchdog();
        self.lock_state().note_activity();
        let outcome = self.dispatch(name, params.get("arguments"));
        match outcome {
            Ok(value) => Ok(tool_result(&value, false)),
            Err(CallError::Operational(error)) => Ok(tool_result(&error.to_json(), true)),
            Err(CallError::Protocol(code, message)) => Err((code, message)),
        }
    }

    fn dispatch(&self, name: &str, arguments: Option<&Value>) -> Result<Value, CallError> {
        match name {
            "lovense_status" => {
                check_no_arguments(arguments)?;
                Ok(self.status())
            }
            "lovense_toys" => {
                check_no_arguments(arguments)?;
                Ok(self.toys()?)
            }
            "lovense_arm" => {
                check_no_arguments(arguments)?;
                Ok(self.arm()?)
            }
            "lovense_disarm" => {
                check_no_arguments(arguments)?;
                Ok(self.disarm()?)
            }
            "lovense_stop" => {
                check_no_arguments(arguments)?;
                Ok(self.stop()?)
            }
            "ache_devices" => {
                check_no_arguments(arguments)?;
                Ok(self.devices()?)
            }
            "ache_device" => {
                let args = arguments_object(arguments)?;
                check_fields_allowed(&args, &["device"])?;
                let device = required_string(&args, "device")?;
                Ok(self.device(&device)?)
            }
            "lovense_function" => {
                let args = arguments_object(arguments)?;
                check_fields_allowed(&args, &["feature", "strength", "time_sec", "toy", "device"])?;
                let device = optional_string(&args, "device")?;
                let intent = FunctionIntent {
                    feature: required_string(&args, "feature")?,
                    strength: required_int(&args, "strength")?,
                    time_sec: required_int(&args, "time_sec")?,
                    toy: optional_string(&args, "toy")?,
                };
                Ok(self.run_output(OutputIntent::Function(intent), device.as_deref())?)
            }
            "lovense_pattern" => {
                let args = arguments_object(arguments)?;
                check_fields_allowed(
                    &args,
                    &[
                        "features",
                        "interval_ms",
                        "strengths",
                        "time_sec",
                        "toy",
                        "device",
                    ],
                )?;
                let device = optional_string(&args, "device")?;
                let intent = PatternIntent {
                    features: optional_string(&args, "features")?.unwrap_or_default(),
                    interval_ms: required_int(&args, "interval_ms")?,
                    strengths: required_int_list(&args, "strengths")?,
                    time_sec: required_int(&args, "time_sec")?,
                    toy: optional_string(&args, "toy")?,
                };
                Ok(self.run_output(OutputIntent::Pattern(intent), device.as_deref())?)
            }
            "lovense_preset" => {
                let args = arguments_object(arguments)?;
                check_fields_allowed(&args, &["name", "time_sec", "toy", "device"])?;
                let device = optional_string(&args, "device")?;
                let intent = PresetIntent {
                    name: required_string(&args, "name")?,
                    time_sec: required_int(&args, "time_sec")?,
                    toy: optional_string(&args, "toy")?,
                };
                Ok(self.run_output(OutputIntent::Preset(intent), device.as_deref())?)
            }
            _ => unreachable!("tool names are validated before dispatch"),
        }
    }

    fn status(&self) -> Value {
        let probe = self.lovense.transport_probe();
        let (state, note) = match (self.config.transport, &probe) {
            (TransportKind::Mock, _) => (
                "mock",
                "mock transport: fixture toys only; no device or app is contacted",
            ),
            (TransportKind::Real, None) => (
                "unconfigured",
                "real transport with no host configured; nothing can reach the wire",
            ),
            (TransportKind::Real, Some(_)) => (
                "ready",
                "target configured but not verified; lovense_toys contacts it",
            ),
        };
        let target = probe.and_then(|probe| probe.target);
        let safety = self.lock_state();
        json!({
            "state": state,
            "transport": self.config.transport.as_str(),
            "target": target,
            "armed": safety.armed(),
            "output_active": safety.output_active(),
            "session_health": safety.session_health(),
            "watchdog_trips": safety.watchdog_trips(),
            "last_event": safety.last_event(),
            "intensity_ceiling": self.config.intensity_ceiling,
            "watchdog_secs": self.config.watchdog_secs,
            "output_path": "gated: every output on every provider passes the one \
                decide() gate; stop, disarm, the watchdog, a session drop, and \
                shutdown each zero output on both providers and latch the master off",
            "note": note,
            "providers": {
                "lovense": self.lovense.probe().to_json(),
                "buttplug": self.buttplug.probe().to_json(),
            },
            "server": { "name": SERVER_NAME, "version": SERVER_VERSION },
        })
    }

    fn toys(&self) -> Result<Value, AcheError> {
        // The provider applies the config gate first, so a real
        // transport without a host never reaches any transport,
        // whatever was injected.
        let toys = self.lovense.toys()?;
        let source = match self.config.transport {
            TransportKind::Mock => "mock_fixture",
            TransportKind::Real => "live_discovery",
        };
        Ok(json!({
            "transport": self.config.transport.as_str(),
            "source": source,
            "count": toys.len(),
            "toys": toys,
        }))
    }

    /// Unified discovery: every device either provider can see, in
    /// the unified model, with a per-provider account of what
    /// happened. A provider that cannot enumerate contributes its
    /// typed error to its entry — never silence — and the call itself
    /// fails only when NO provider could enumerate at all.
    fn devices(&self) -> Result<Value, AcheError> {
        let mut all: Vec<Device> = Vec::new();
        let mut providers = Map::new();
        let mut enumerated = false;
        let mut first_error: Option<AcheError> = None;
        for provider in [
            &self.lovense as &dyn DeviceProvider,
            self.buttplug.as_ref() as &dyn DeviceProvider,
        ] {
            let probe = provider.probe();
            let mut entry = Map::new();
            entry.insert("state".into(), json!(probe.state));
            entry.insert("available".into(), json!(probe.available));
            match provider.discover() {
                Ok(devices) => {
                    enumerated = true;
                    entry.insert("count".into(), json!(devices.len()));
                    all.extend(devices);
                }
                Err(error) => {
                    entry.insert("error".into(), error.to_json()["error"].clone());
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
            providers.insert(probe.provider.as_str().into(), Value::Object(entry));
        }
        if !enumerated {
            return Err(first_error.expect("no provider enumerated and none errored"));
        }
        Ok(json!({
            "count": all.len(),
            "devices": all.iter().map(Device::to_json).collect::<Vec<_>>(),
            "providers": Value::Object(providers),
        }))
    }

    /// One device by unified id, with its capabilities as data.
    fn device(&self, id: &str) -> Result<Value, AcheError> {
        Ok(self.resolve_device(id)?.to_json())
    }

    /// Resolve a unified device id to the device, asking only the
    /// provider the id names. A provider that cannot enumerate
    /// surfaces its own typed error — absence is never claimed on the
    /// word of a provider that could not look.
    fn resolve_device(&self, id: &str) -> Result<Device, AcheError> {
        if id.len() > TOY_ID_BYTES_MAX || id.chars().any(char::is_control) {
            return Err(AcheError::new(
                ErrorCode::RequestInvalid,
                format!(
                    "device id must be at most {TOY_ID_BYTES_MAX} bytes with no control characters"
                ),
            ));
        }
        let provider: &dyn DeviceProvider = if id.starts_with("lovense:") {
            &self.lovense
        } else if id.starts_with("buttplug:") {
            self.buttplug.as_ref()
        } else {
            return Err(AcheError::new(
                ErrorCode::RequestInvalid,
                format!("device id {id:?} must start with `lovense:` or `buttplug:`"),
            ));
        };
        let devices = provider.discover()?;
        devices
            .into_iter()
            .find(|device| device.id == id)
            .ok_or_else(|| {
                AcheError::new(
                    ErrorCode::DeviceNotFound,
                    format!("no device {id:?} is present"),
                )
            })
    }

    /// Arm the master toggle. Explicit, always: nothing else in the
    /// server arms, and arming refuses without a target.
    fn arm(&self) -> Result<Value, AcheError> {
        if !self.target_configured() {
            return Err(AcheError::new(
                ErrorCode::TargetUnconfigured,
                "no target is configured; arming is refused",
            ));
        }
        let output_active = {
            let mut state = self.lock_state();
            state.arm();
            state.output_active()
        };
        Ok(json!({
            "armed": true,
            "output_active": output_active,
            "intensity_ceiling": self.config.intensity_ceiling,
            "watchdog_secs": self.config.watchdog_secs,
        }))
    }

    /// Master off. If output is active it is zeroed on the wire first,
    /// synchronously, before this call answers.
    fn disarm(&self) -> Result<Value, AcheError> {
        let stop_sent = self.zero_output("disarm: output zeroed")?;
        Ok(json!({
            "armed": false,
            "output_active": false,
            "stop_sent": stop_sent,
        }))
    }

    /// The panic zero. The gate allows it in every state; it zeroes
    /// output on the wire synchronously and latches the master off, so
    /// recovery is always a fresh explicit arm.
    fn stop(&self) -> Result<Value, AcheError> {
        let stop_sent = self.zero_output("stop: output zeroed")?;
        Ok(json!({
            "armed": false,
            "output_active": false,
            "stop_sent": stop_sent,
            "effective": { "action": "Stop", "strength": 0, "time_sec": 0, "toy": "" },
        }))
    }

    /// Zero output and latch the master off. The wire zero is sent
    /// whenever a target exists — even when our state believes nothing
    /// is active, because a panic stop must not trust a possibly stale
    /// belief over a harmless redundant zero — and the state flips in
    /// the same call, so the zero is synchronous either way. A failed
    /// send is a session drop (state still zeroes; the error surfaces).
    /// Returns whether a stop body was actually sent.
    fn zero_output(&self, event: &str) -> Result<bool, AcheError> {
        let sent = self.send_stop_strict()?;
        self.lock_state().zero_and_disarm(event);
        Ok(sent)
    }

    /// Zero every provider. Both providers are ALWAYS attempted, even
    /// when the first fails: a panic stop that leaves the second
    /// provider running because the first one's send failed is not a
    /// stop. `Ok(false)` means no provider had a target to zero: the
    /// local zero still stands. A zero failure on either provider
    /// marks the session dropped; the first failure is the error.
    fn send_stop_strict(&self) -> Result<bool, AcheError> {
        let lovense = self.lovense.zero();
        let buttplug = self.buttplug.zero();
        match (lovense, buttplug) {
            (Ok(lovense_sent), Ok(buttplug_sent)) => Ok(lovense_sent || buttplug_sent),
            (Err(error), _) | (_, Err(error)) => {
                self.lock_state().on_session_drop();
                Err(error)
            }
        }
    }

    /// Best-effort zero on both providers, for the paths that must
    /// never fail: a watchdog trip, shutdown, recovery after a drop.
    /// Both are attempted even when one fails (never short-circuited).
    /// State has already been zeroed by the caller; a failure here
    /// means the session is gone and is recorded as a drop.
    fn send_stop_best_effort(&self) {
        let lovense_failed = self.lovense.zero().is_err();
        let buttplug_failed = self.buttplug.zero().is_err();
        if lovense_failed || buttplug_failed {
            self.lock_state().on_session_drop();
        }
    }

    /// Run one gated output intent (function, pattern, preset). Without
    /// a `device` argument this is the original Lovense path, unchanged:
    /// `decide()` against the Lovense caps, the gate's body onto the
    /// transport. With a unified `device` id, the intent is planned by
    /// the same gate against that device's own capabilities
    /// (`decide_plan`) and executed by the device's provider. A deny is
    /// final and sends nothing, on either path. A send failure is a
    /// session drop: output state zeroes, the master latches off, and a
    /// best-effort zero on both providers follows the failed command.
    fn run_output(&self, intent: OutputIntent, device: Option<&str>) -> Result<Value, AcheError> {
        let snapshot = self.snapshot();
        let Some(device_id) = device else {
            let (body, effective, clamped) = match decide(&snapshot, &intent) {
                Decision::Allow {
                    body,
                    effective,
                    clamped,
                } => (body, effective, clamped),
                Decision::Deny(error) => return Err(error),
            };
            return self.finish_output(
                self.lovense.send_gate_body(&body),
                json!({
                    "sent": true,
                    "gate": { "decision": "allow", "clamped": clamped },
                    "effective": effective,
                    "command": body,
                }),
            );
        };
        // Unified routing: resolve the device (its provider enumerates;
        // a provider that cannot look surfaces its own typed error),
        // plan against its capabilities, execute through its provider.
        let target = self.resolve_device(device_id)?;
        let intent = retarget(intent, &target);
        let plan = decide_plan(&snapshot, &intent, &target.caps())?;
        let clamped = plan.clamped();
        match target.provider {
            ProviderId::Lovense => {
                let (body, effective) = lovense_wire(&plan);
                self.finish_output(
                    self.lovense.send_gate_body(&body),
                    json!({
                        "sent": true,
                        "provider": "lovense",
                        "device": target.id,
                        "gate": { "decision": "allow", "clamped": clamped },
                        "effective": effective,
                        "command": body,
                    }),
                )
            }
            ProviderId::Buttplug => {
                let effective = plan_effective(&plan);
                self.finish_output(
                    self.buttplug.execute(&plan),
                    json!({
                        "sent": true,
                        "provider": "buttplug",
                        "device": target.id,
                        "gate": { "decision": "allow", "clamped": clamped },
                        "effective": effective,
                    }),
                )
            }
        }
    }

    /// The shared tail of every output path: on success the output is
    /// active; on failure the session drops (state zeroes, the master
    /// latches off, a best-effort zero follows).
    fn finish_output(
        &self,
        sent: Result<(), AcheError>,
        result: Value,
    ) -> Result<Value, AcheError> {
        match sent {
            Ok(()) => {
                self.lock_state().mark_output_started();
                Ok(result)
            }
            Err(error) => {
                self.lock_state().on_session_drop();
                self.send_stop_best_effort();
                Err(error)
            }
        }
    }

    /// Fire the watchdog if its window elapsed with output active:
    /// zero the state, latch off, then send the wire zero. Returns
    /// true exactly when it tripped.
    pub fn trip_watchdog(&self) -> bool {
        let tripped = self
            .lock_state()
            .trip_watchdog_if_expired(self.config.watchdog_secs);
        if tripped {
            eprintln!("ache: watchdog expired with output active; output zeroed, re-arm required");
            self.send_stop_best_effort();
        }
        tripped
    }

    /// Time until the watchdog fires, when output is active.
    pub fn watchdog_remaining(&self) -> Option<Duration> {
        self.lock_state()
            .watchdog_remaining(self.config.watchdog_secs)
    }

    pub fn output_active(&self) -> bool {
        self.lock_state().output_active()
    }

    /// The explicit shutdown path: input ended while output is active.
    /// Zero it and say so on stderr — no skipped cleanup.
    pub fn zero_on_shutdown(&self) -> bool {
        if !self.output_active() {
            return false;
        }
        self.lock_state().zero_and_disarm("shutdown: output zeroed");
        self.send_stop_best_effort();
        true
    }
}

/// Aim an intent at a resolved unified device: the gate's target
/// selector becomes the device's own id space — the raw toy id for a
/// Lovense device (what its wire expects), the unified id for a
/// Buttplug device (what its provider resolves). A caller-supplied
/// `toy` is replaced: when `device` is given, it is the target.
fn retarget(intent: OutputIntent, device: &Device) -> OutputIntent {
    let selector = match device.provider {
        ProviderId::Lovense => device
            .id
            .strip_prefix("lovense:")
            .unwrap_or(&device.id)
            .to_string(),
        ProviderId::Buttplug => device.id.clone(),
    };
    match intent {
        OutputIntent::Function(mut intent) => {
            intent.toy = Some(selector);
            OutputIntent::Function(intent)
        }
        OutputIntent::Pattern(mut intent) => {
            intent.toy = Some(selector);
            OutputIntent::Pattern(intent)
        }
        OutputIntent::Preset(mut intent) => {
            intent.toy = Some(selector);
            OutputIntent::Preset(intent)
        }
        OutputIntent::Stop => OutputIntent::Stop,
    }
}

/// The effective-values report for a plan executed by a non-Lovense
/// provider (the Lovense path reports through `lovense_wire`, whose
/// report the original tools have always carried).
fn plan_effective(plan: &AllowedOutput) -> Value {
    match plan {
        AllowedOutput::Function {
            feature,
            strength,
            time_sec,
            toy,
            ..
        } => json!({
            "feature": feature,
            "strength": strength,
            "time_sec": time_sec,
            "device": toy,
        }),
        AllowedOutput::Pattern {
            features,
            interval_ms,
            strengths,
            time_sec,
            toy,
            ..
        } => json!({
            "features": features,
            "interval_ms": interval_ms,
            "strengths": strengths,
            "time_sec": time_sec,
            "device": toy,
        }),
        AllowedOutput::Preset {
            name,
            time_sec,
            toy,
        } => json!({
            "name": name,
            "time_sec": time_sec,
            "device": toy,
        }),
        AllowedOutput::Stop => json!({
            "action": "Stop",
            "strength": 0,
            "time_sec": 0,
            "device": "",
        }),
    }
}

/// One line of input, as read by the reader thread.
enum LineEvent {
    Line(Vec<u8>),
    Oversized,
    /// End of input, or a read error (logged by the reader thread):
    /// either way the session is over and cleanup runs.
    End,
}

/// Serve until end of input. Each line in is at most one response out.
///
/// Lines are read on a thread so the loop can fire the heartbeat
/// watchdog while the client is silent: with output active, the loop
/// waits only until the watchdog deadline, trips it, and waits on.
/// When input ends, the explicit shutdown path zeroes any active
/// output before returning.
pub fn serve(
    server: &Server,
    input: impl BufRead + Send + 'static,
    mut output: impl Write,
) -> io::Result<()> {
    let (sender, receiver) = mpsc::sync_channel::<LineEvent>(LINE_QUEUE_CAPACITY);
    let _reader = std::thread::spawn(move || {
        let mut input = input;
        loop {
            let mut line = Vec::new();
            let limit = u64::try_from(LINE_BYTES_MAX + 1).expect("line limit fits u64");
            let read = match (&mut input).take(limit).read_until(b'\n', &mut line) {
                Ok(read) => read,
                Err(error) => {
                    eprintln!("ache: stdin read failed: {error}");
                    let _ = sender.send(LineEvent::End);
                    return;
                }
            };
            if read == 0 {
                let _ = sender.send(LineEvent::End);
                return;
            }
            let event = if line.len() > LINE_BYTES_MAX && line.last() != Some(&b'\n') {
                if input.skip_until(b'\n').is_err() {
                    let _ = sender.send(LineEvent::End);
                    return;
                }
                LineEvent::Oversized
            } else {
                LineEvent::Line(line)
            };
            if sender.send(event).is_err() {
                return;
            }
        }
    });
    let mut result = Ok(());
    loop {
        // Check the watchdog on every pass, not only on timeouts, so a
        // stream of non-tool messages cannot keep output alive past
        // its window: only tool calls count as heartbeats.
        server.trip_watchdog();
        let event = match server.watchdog_remaining() {
            Some(remaining) => match receiver.recv_timeout(remaining) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => LineEvent::End,
            },
            None => match receiver.recv() {
                Ok(event) => event,
                Err(_) => LineEvent::End,
            },
        };
        let response = match event {
            LineEvent::End => break,
            LineEvent::Oversized => Some(rpc_error(
                Value::Null,
                INVALID_REQUEST,
                format!("line exceeds {LINE_BYTES_MAX} bytes"),
            )),
            LineEvent::Line(line) => {
                if line.iter().all(u8::is_ascii_whitespace) {
                    None
                } else {
                    server.handle_line(&line)
                }
            }
        };
        if let Some(response) = response {
            let written = (|| -> io::Result<()> {
                serde_json::to_writer(&mut output, &response).map_err(io::Error::other)?;
                output.write_all(b"\n")?;
                output.flush()
            })();
            if let Err(error) = written {
                result = Err(error);
                break;
            }
        }
    }
    if server.zero_on_shutdown() {
        eprintln!("ache: input closed with output active; stop sent, output zeroed");
    }
    result
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        "instructions": "ache — unified haptics: Lovense local Game Mode and an \
            embedded Buttplug server behind one surface. Tools: lovense_status, \
            lovense_toys, lovense_arm, lovense_disarm, lovense_function, \
            lovense_pattern, lovense_preset, lovense_stop, ache_devices, \
            ache_device. Output is off by default: arm explicitly, every output \
            on either provider passes the one safety gate that clamps intensity \
            to the configured ceiling, and stop always works.",
    })
}

fn tool_list() -> Value {
    let no_arguments = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    let toy_argument = json!({
        "type": "string",
        "description": "Toy id from lovense_toys; omit to target every toy."
    });
    let device_argument = json!({
        "type": "string",
        "description": "Unified device id from ache_devices (e.g. \"buttplug:0\" or \
            \"lovense:<toy id>\"). When given, it selects the target device and its \
            provider, the gate plans against that device's reported capabilities, \
            and `toy` is ignored."
    });
    let time_argument = json!({
        "type": "integer",
        "minimum": 1,
        "maximum": 3600,
        "description": "Duration in seconds. Bounded: indefinite output does not exist here."
    });
    json!([
        {
            "name": "lovense_status",
            "description": "Report the server state (unconfigured, mock, or ready), transport, \
                target, arm state, output activity, session health, intensity ceiling, \
                watchdog window, and version. Makes no network call; `ready` means a target \
                is configured, not that it answered.",
            "inputSchema": no_arguments.clone(),
            "annotations": {
                "title": "Lovense status",
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            },
        },
        {
            "name": "lovense_toys",
            "description": "Discover connected toys. Mock transport: the fixture toys. Real \
                transport: one GetToys request to the configured app; an unreachable app is \
                an error, never an empty list. Real with no host: target_unconfigured, and \
                no request is made. Discovery is safe while disarmed and arms nothing.",
            "inputSchema": no_arguments.clone(),
            "annotations": {
                "title": "Lovense toys",
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true,
            },
        },
        {
            "name": "lovense_arm",
            "description": "Arm the master toggle — the explicit act that makes output \
                possible. Refuses without a configured target. Arming starts no output by \
                itself, and nothing else arms: not connecting, not discovering, not a \
                previous session.",
            "inputSchema": no_arguments.clone(),
            "annotations": {
                "title": "Lovense arm",
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            },
        },
        {
            "name": "lovense_disarm",
            "description": "Master off. If output is active it is zeroed on the wire \
                synchronously before this call answers. Afterwards every output is denied \
                until lovense_arm is called again.",
            "inputSchema": no_arguments.clone(),
            "annotations": {
                "title": "Lovense disarm",
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true,
            },
        },
        {
            "name": "lovense_function",
            "description": "Run one function at one strength for a bounded duration. \
                Requires the armed state. The strength is clamped in code to the configured \
                ceiling and the feature's own range; the result reports the effective \
                values actually sent.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "feature": {
                        "type": "string",
                        "enum": Feature::action_names(),
                        "description": "The function to run."
                    },
                    "strength": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Requested strength; clamped to the ceiling and the \
                            feature range before it reaches the wire."
                    },
                    "time_sec": time_argument.clone(),
                    "toy": toy_argument.clone(),
                    "device": device_argument.clone(),
                },
                "required": ["feature", "strength", "time_sec"],
                "additionalProperties": false,
            },
            "annotations": {
                "title": "Lovense function",
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": true,
            },
        },
        {
            "name": "lovense_pattern",
            "description": "Run a pattern: a strength sequence stepped at a fixed interval. \
                Requires the armed state. The rule is built by the server from validated \
                parts — feature letters from v r p t f s d o, an interval strictly above \
                100 ms, at most 50 strengths, each clamped to the ceiling in code.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "features": {
                        "type": "string",
                        "description": "Feature letters (v r p t f s d o). Omit or empty \
                            for all functions."
                    },
                    "interval_ms": {
                        "type": "integer",
                        "minimum": 101,
                        "maximum": 3600000,
                        "description": "Step interval in milliseconds, strictly above 100."
                    },
                    "strengths": {
                        "type": "array",
                        "items": { "type": "integer", "minimum": 0 },
                        "minItems": 1,
                        "maxItems": 50,
                        "description": "One strength per step; each is clamped to the ceiling."
                    },
                    "time_sec": time_argument.clone(),
                    "toy": toy_argument.clone(),
                    "device": device_argument.clone(),
                },
                "required": ["interval_ms", "strengths", "time_sec"],
                "additionalProperties": false,
            },
            "annotations": {
                "title": "Lovense pattern",
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": true,
            },
        },
        {
            "name": "lovense_preset",
            "description": "Run a built-in Remote app preset (pulse, wave, fireworks, \
                earthquake) for a bounded duration. Requires the armed state. Names outside \
                the built-in list are refused; the server does not guess at presets.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "enum": crate::decide::PRESET_NAMES,
                        "description": "The built-in preset to run."
                    },
                    "time_sec": time_argument,
                    "toy": toy_argument,
                    "device": device_argument,
                },
                "required": ["name", "time_sec"],
                "additionalProperties": false,
            },
            "annotations": {
                "title": "Lovense preset",
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": true,
            },
        },
        {
            "name": "lovense_stop",
            "description": "Panic zero: stop every function on every device on both \
                providers, synchronously, and latch the master off so recovery is a \
                fresh explicit arm. Always available — disarmed, unconfigured, \
                mid-pattern — and never gated.",
            "inputSchema": no_arguments.clone(),
            "annotations": {
                "title": "Lovense stop",
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true,
            },
        },
        {
            "name": "ache_devices",
            "description": "Discover every device across both providers (Lovense \
                Standard API and embedded Buttplug), in one unified model: id, \
                provider, name, battery, and capabilities as data — each capability \
                carries its maximum on the unified 0..=20 strength scale, or null \
                when the capability is reported but not mapped. A provider that \
                cannot enumerate reports its typed error in its entry; the call \
                fails only when no provider could enumerate. Discovery is safe \
                while disarmed and arms nothing.",
            "inputSchema": no_arguments.clone(),
            "annotations": {
                "title": "All devices (unified)",
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true,
            },
        },
        {
            "name": "ache_device",
            "description": "One device by its unified id (from ache_devices), with \
                its full capability list as data. A device no provider can see is a \
                typed device_not_found; a provider that cannot enumerate surfaces \
                its own typed error rather than a claimed absence.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": {
                        "type": "string",
                        "description": "Unified device id, e.g. \"buttplug:0\" or \"lovense:<toy id>\"."
                    }
                },
                "required": ["device"],
                "additionalProperties": false,
            },
            "annotations": {
                "title": "One device (unified)",
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true,
            },
        },
    ])
}

/// Tools that take no arguments accept absent, null, and `{}`; any
/// field is refused, naming (a bounded list of) the fields.
fn check_no_arguments(arguments: Option<&Value>) -> Result<(), (i64, String)> {
    match arguments {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Object(fields)) if fields.is_empty() => Ok(()),
        Some(Value::Object(fields)) => Err((INVALID_PARAMS, unexpected_fields(fields))),
        Some(_) => Err((INVALID_PARAMS, "arguments must be an object".into())),
    }
}

fn unexpected_fields(fields: &Map<String, Value>) -> String {
    let names: Vec<String> = fields
        .keys()
        .take(ARGUMENT_NAMES_REPORTED_MAX)
        .map(|name| format!("`{}`", clip(name)))
        .collect();
    format!(
        "unexpected argument fields: {}; this tool takes none",
        names.join(", ")
    )
}

/// The argument object of an output tool: absent or null means an
/// empty object (whose missing required fields are then refused by
/// name); anything else non-object is refused.
fn arguments_object(arguments: Option<&Value>) -> Result<Map<String, Value>, (i64, String)> {
    match arguments {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(fields)) => Ok(fields.clone()),
        Some(_) => Err((INVALID_PARAMS, "arguments must be an object".into())),
    }
}

/// Refuse argument fields outside a tool's accepted set.
fn check_fields_allowed(
    fields: &Map<String, Value>,
    allowed: &[&str],
) -> Result<(), (i64, String)> {
    let unexpected: Vec<String> = fields
        .keys()
        .filter(|name| !allowed.contains(&name.as_str()))
        .take(ARGUMENT_NAMES_REPORTED_MAX)
        .map(|name| format!("`{}`", clip(name)))
        .collect();
    if unexpected.is_empty() {
        return Ok(());
    }
    Err((
        INVALID_PARAMS,
        format!(
            "unexpected argument fields: {}; this tool accepts: {}",
            unexpected.join(", "),
            allowed.join(", ")
        ),
    ))
}

fn required_string(fields: &Map<String, Value>, name: &str) -> Result<String, (i64, String)> {
    match fields.get(name) {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err((INVALID_PARAMS, format!("`{name}` must be a string"))),
        None => Err((INVALID_PARAMS, format!("`{name}` is required"))),
    }
}

fn optional_string(
    fields: &Map<String, Value>,
    name: &str,
) -> Result<Option<String>, (i64, String)> {
    match fields.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err((INVALID_PARAMS, format!("`{name}` must be a string"))),
    }
}

fn required_int(fields: &Map<String, Value>, name: &str) -> Result<i64, (i64, String)> {
    match fields.get(name) {
        Some(value) => value
            .as_i64()
            .ok_or_else(|| (INVALID_PARAMS, format!("`{name}` must be an integer"))),
        None => Err((INVALID_PARAMS, format!("`{name}` is required"))),
    }
}

fn required_int_list(fields: &Map<String, Value>, name: &str) -> Result<Vec<i64>, (i64, String)> {
    match fields.get(name) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_i64()
                    .ok_or_else(|| (INVALID_PARAMS, format!("`{name}` entries must be integers")))
            })
            .collect(),
        Some(_) => Err((INVALID_PARAMS, format!("`{name}` must be an array"))),
        None => Err((INVALID_PARAMS, format!("`{name}` is required"))),
    }
}

fn tool_result(value: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|cause| {
        json!({ "error": { "code": "output_write_failed", "message": cause.to_string() } })
            .to_string()
    });
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    let message = truncate(message.into());
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn clip(text: &str) -> &str {
    let mut cut = text.len().min(ARGUMENT_NAME_BYTES_MAX);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_stub::{AppStub, closed_port};
    use crate::transport::{MockTransport, ReadRequest, from_config};

    fn server_for(config: Config) -> Server {
        let transport = from_config(&config);
        Server::new(config, transport)
    }

    fn real(host: &str, port: u16) -> Config {
        Config {
            transport: TransportKind::Real,
            host: host.into(),
            port,
            ..Config::default()
        }
    }

    fn request(server: &Server, line: &str) -> Value {
        server
            .handle_line(line.as_bytes())
            .expect("a request is answered")
    }

    fn call(server: &Server, name: &str, arguments: Value) -> Value {
        let line = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}});
        request(server, &line.to_string())
    }

    /// The tool payload and its isError flag.
    fn payload(response: &Value) -> (Value, bool) {
        let result = &response["result"];
        let text = result["content"][0]["text"].as_str().expect("text content");
        (
            serde_json::from_str(text).unwrap(),
            result["isError"].as_bool().unwrap(),
        )
    }

    fn status_of(server: &Server) -> Value {
        payload(&call(server, "lovense_status", json!({}))).0
    }

    fn arm(server: &Server) {
        let (_, is_error) = payload(&call(server, "lovense_arm", json!({})));
        assert!(!is_error, "arm must succeed here");
    }

    fn function_args(strength: i64) -> Value {
        json!({"feature": "Vibrate", "strength": strength, "time_sec": 10})
    }

    // --- validation ---------------------------------------------------

    #[test]
    fn initialize_reports_legacy_era_and_server_info() {
        let response = request(
            &server_for(Config::default()),
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        );
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(
            response["result"]["serverInfo"],
            json!({"name": "ache", "version": "0.3.0"})
        );
        assert_eq!(response["result"]["capabilities"], json!({"tools": {}}));
    }

    #[test]
    fn tools_list_is_exactly_the_ten_tools_with_full_schemas() {
        let response = request(
            &server_for(Config::default()),
            r#"{"jsonrpc":"2.0","id":"a","method":"tools/list"}"#,
        );
        let tools = response["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, TOOL_NAMES);
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object", "{}", tool["name"]);
            assert!(tool["inputSchema"]["properties"].is_object());
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert_eq!(tool["annotations"]["destructiveHint"], false);
            let read_only = tool["annotations"]["readOnlyHint"].as_bool().unwrap();
            let expected = matches!(
                tool["name"].as_str().unwrap(),
                "lovense_status" | "lovense_toys" | "ache_devices" | "ache_device"
            );
            assert_eq!(read_only, expected, "{}", tool["name"]);
        }
        // Output tools declare their required arguments in the schema.
        let function = &tools[4];
        assert_eq!(function["name"], "lovense_function");
        assert_eq!(
            function["inputSchema"]["required"],
            json!(["feature", "strength", "time_sec"])
        );
        assert_eq!(
            function["inputSchema"]["properties"]["feature"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            10
        );
    }

    #[test]
    fn mock_status_is_labeled_mock_never_connected() {
        let (status, is_error) = payload(&call(
            &server_for(Config::default()),
            "lovense_status",
            json!({}),
        ));
        assert!(!is_error);
        assert_eq!(status["state"], "mock");
        assert_eq!(status["transport"], "mock");
        assert_eq!(status["target"], Value::Null);
        assert_eq!(status["armed"], false);
        assert_eq!(status["output_active"], false);
        assert_eq!(status["session_health"], "ok");
        assert_eq!(status["intensity_ceiling"], 20);
        assert_eq!(status["watchdog_secs"], 5);
        assert_eq!(status["server"]["version"], "0.3.0");
        assert!(!status.to_string().contains("connected"));
    }

    #[test]
    fn real_status_reports_unconfigured_or_ready_with_target() {
        let (status, _) = payload(&call(
            &server_for(real("", 20010)),
            "lovense_status",
            Value::Null,
        ));
        assert_eq!(status["state"], "unconfigured");
        assert_eq!(status["target"], Value::Null);
        let config = Config {
            use_https: true,
            intensity_ceiling: 7,
            watchdog_secs: 9,
            ..real("10.1.2.3", 30010)
        };
        let (status, _) = payload(&call(&server_for(config), "lovense_status", json!({})));
        assert_eq!(status["state"], "ready");
        assert_eq!(
            status["target"],
            json!({"scheme": "https", "host": "10.1.2.3", "port": 30010})
        );
        assert_eq!(
            (
                status["intensity_ceiling"].clone(),
                status["watchdog_secs"].clone()
            ),
            (json!(7), json!(9))
        );
    }

    #[test]
    fn mock_toys_returns_the_fixture() {
        let (toys, is_error) = payload(&call(
            &server_for(Config::default()),
            "lovense_toys",
            json!({}),
        ));
        assert!(!is_error);
        assert_eq!(toys["source"], "mock_fixture");
        assert_eq!(toys["count"], 2);
        assert_eq!(toys["toys"][0]["type"], "lush");
        assert_eq!(toys["toys"][1]["battery"], 42);
    }

    #[test]
    fn real_toys_runs_live_discovery_against_the_stub() {
        let mut stub = AppStub::start(
            r#"{"code":200,"data":{"toys":"{\"z\":{\"name\":\"nora\"}}"}}"#,
            1,
            200,
        );
        let (toys, is_error) = payload(&call(
            &server_for(real("127.0.0.1", stub.port)),
            "lovense_toys",
            json!({}),
        ));
        assert!(!is_error);
        assert_eq!(toys["source"], "live_discovery");
        assert_eq!(toys["toys"][0]["name"], "nora");
        assert_eq!(stub.received()["body"], r#"{"command":"GetToys"}"#);
    }

    #[test]
    fn ping_answers_and_notifications_get_no_response() {
        let server = server_for(Config::default());
        assert_eq!(
            request(&server, r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#)["result"],
            json!({})
        );
        assert!(
            server
                .handle_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_none()
        );
        assert!(
            server
                .handle_line(br#"{"jsonrpc":"2.0","method":"no/such"}"#)
                .is_none()
        );
    }

    #[test]
    fn full_mock_session_arm_function_pattern_preset_stop() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));

        let (armed, is_error) = payload(&call(&server, "lovense_arm", json!({})));
        assert!(!is_error);
        assert_eq!(armed["armed"], true);
        assert_eq!(status_of(&server)["armed"], true);

        let (ran, is_error) = payload(&call(&server, "lovense_function", function_args(12)));
        assert!(!is_error);
        assert_eq!(ran["sent"], true);
        assert_eq!(ran["effective"]["strength"], 12);
        assert_eq!(status_of(&server)["output_active"], true);

        let (ran, is_error) = payload(&call(
            &server,
            "lovense_pattern",
            json!({"features": "vr", "interval_ms": 250, "strengths": [5, 20], "time_sec": 4}),
        ));
        assert!(!is_error);
        assert_eq!(ran["effective"]["rule"], "V:1;F:vr;S:250#");

        let (ran, is_error) = payload(&call(
            &server,
            "lovense_preset",
            json!({"name": "pulse", "time_sec": 9}),
        ));
        assert!(!is_error);
        assert_eq!(ran["effective"]["name"], "pulse");

        let (stopped, is_error) = payload(&call(&server, "lovense_stop", json!({})));
        assert!(!is_error);
        assert_eq!(stopped["stop_sent"], true);
        let status = status_of(&server);
        assert_eq!(status["output_active"], false);
        assert_eq!(status["armed"], false, "stop latches the master off");

        let sent = sent.lock().unwrap();
        let kinds: Vec<&str> = sent
            .iter()
            .map(|body| body["command"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["Function", "Pattern", "Preset", "Function"]);
        assert_eq!(sent[0]["action"], "Vibrate:12");
        assert_eq!(sent[1]["strength"], "5;20");
        assert_eq!(sent[3]["action"], "Stop");
        assert_eq!(sent[3]["timeSec"], 0);
    }

    #[test]
    fn function_over_ceiling_is_clamped_on_the_wire_and_reported() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        arm(&server);
        let (ran, is_error) = payload(&call(&server, "lovense_function", function_args(25)));
        assert!(!is_error);
        assert_eq!(ran["gate"]["clamped"], true);
        assert_eq!(ran["effective"]["strength"], 20);
        assert_eq!(sent.lock().unwrap()[0]["action"], "Vibrate:20");
    }

    #[test]
    fn function_can_target_one_toy() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        arm(&server);
        let args = json!({"feature": "Rotate", "strength": 3, "time_sec": 2, "toy": "mock-toy-2"});
        let (ran, is_error) = payload(&call(&server, "lovense_function", args));
        assert!(!is_error);
        assert_eq!(ran["effective"]["toy"], "mock-toy-2");
        assert_eq!(sent.lock().unwrap()[0]["toy"], "mock-toy-2");
    }

    #[test]
    fn pattern_strengths_clamp_per_value_on_the_wire() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        arm(&server);
        let args =
            json!({"features": "v", "interval_ms": 500, "strengths": [3, 99], "time_sec": 2});
        let (ran, is_error) = payload(&call(&server, "lovense_pattern", args));
        assert!(!is_error);
        assert_eq!(ran["effective"]["strengths"], json!([3, 20]));
        assert_eq!(sent.lock().unwrap()[0]["strength"], "3;20");
    }

    #[test]
    fn disarm_zeroes_synchronously_and_blocks_further_output() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        arm(&server);
        payload(&call(&server, "lovense_function", function_args(9)));
        let (disarmed, is_error) = payload(&call(&server, "lovense_disarm", json!({})));
        assert!(!is_error);
        assert_eq!(disarmed["stop_sent"], true);
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 2);
            assert_eq!(
                sent[1]["action"], "Stop",
                "the zero is on the wire before disarm answers"
            );
        }
        let (error, is_error) = payload(&call(&server, "lovense_function", function_args(9)));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "not_armed");
        assert_eq!(sent.lock().unwrap().len(), 2, "a denied call sends nothing");
    }

    #[test]
    fn stop_is_available_and_idempotent_in_any_state() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        // Never armed, nothing active: stop still answers, still sends
        // the zero (a panic stop does not trust a stale belief), and
        // can be repeated.
        for _ in 0..2 {
            let (stopped, is_error) = payload(&call(&server, "lovense_stop", json!({})));
            assert!(!is_error);
            assert_eq!(stopped["stop_sent"], true);
        }
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent.iter().all(|body| body["action"] == "Stop"));
    }

    // --- adversarial --------------------------------------------------

    #[test]
    fn real_with_empty_host_makes_no_network_call() {
        // Inject a recording transport under a real/no-host config: the
        // config gate must refuse before any transport is touched.
        let mock = MockTransport::new();
        let log = mock.request_log();
        let server = Server::new(real("", 20010), Some(Box::new(mock)));
        let (error, is_error) = payload(&call(&server, "lovense_toys", json!({})));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "target_unconfigured");
        assert!(log.lock().unwrap().is_empty());
        // And production wiring builds no transport at all for it.
        assert!(from_config(&real("", 20010)).is_none());
        let (error, _) = payload(&call(
            &server_for(real("", 20010)),
            "lovense_toys",
            json!({}),
        ));
        assert_eq!(error["error"]["code"], "target_unconfigured");
    }

    #[test]
    fn unreachable_target_is_a_structured_tool_error() {
        let (error, is_error) = payload(&call(
            &server_for(real("127.0.0.1", closed_port())),
            "lovense_toys",
            json!({}),
        ));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "target_unreachable");
        assert_eq!(error["error"]["class"], "transport");
        assert!(error.get("toys").is_none());
    }

    #[test]
    fn stub_malformed_json_is_a_structured_tool_error() {
        let mut stub = AppStub::start("<html>nope", 1, 200);
        let (error, is_error) = payload(&call(
            &server_for(real("127.0.0.1", stub.port)),
            "lovense_toys",
            json!({}),
        ));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "response_invalid");
        stub.received();
    }

    #[test]
    fn unknown_tool_is_refused_and_touches_nothing() {
        let mock = MockTransport::new();
        let log = mock.request_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        for name in [
            "lovense_heartbeat",
            "lovense_patternv2",
            "lovense_vibrate",
            "LOVENSE_TOYS",
            "",
        ] {
            let response = call(&server, name, json!({}));
            assert_eq!(response["error"]["code"], INVALID_PARAMS, "tool {name}");
            assert!(
                response["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("unknown tool")
            );
        }
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn extra_argument_fields_are_refused_before_any_transport_call() {
        let mock = MockTransport::new();
        let log = mock.request_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        for tool in ["lovense_status", "lovense_toys"] {
            let response = call(&server, tool, json!({"strength": 20, "host": "evil"}));
            assert_eq!(response["error"]["code"], INVALID_PARAMS);
            let message = response["error"]["message"].as_str().unwrap();
            assert!(
                message.contains("`strength`") && message.contains("`host`"),
                "{message}"
            );
            for bad in [json!([]), json!("x"), json!(1)] {
                assert_eq!(call(&server, tool, bad)["error"]["code"], INVALID_PARAMS);
            }
        }
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn malformed_json_rpc_is_answered_with_protocol_errors() {
        let server = server_for(Config::default());
        let cases: &[(&str, i64)] = &[
            ("{not json", PARSE_ERROR),
            ("\u{0}\u{1}", PARSE_ERROR),
            ("[]", INVALID_REQUEST),
            (
                r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
                INVALID_REQUEST,
            ),
            ("42", INVALID_REQUEST),
            (
                r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
                INVALID_REQUEST,
            ),
            (r#"{"id":1,"method":"ping"}"#, INVALID_REQUEST),
            (
                r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
                INVALID_REQUEST,
            ),
            (
                r#"{"jsonrpc":"2.0","id":{},"method":"ping"}"#,
                INVALID_REQUEST,
            ),
            (r#"{"jsonrpc":"2.0","id":1,"method":7}"#, INVALID_REQUEST),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/destroy"}"#,
                METHOD_NOT_FOUND,
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#,
                INVALID_PARAMS,
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":[]}"#,
                INVALID_PARAMS,
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":3}}"#,
                INVALID_PARAMS,
            ),
        ];
        for (line, code) in cases {
            assert_eq!(
                request(&server, line)["error"]["code"],
                *code,
                "line {line}"
            );
        }
    }

    #[test]
    fn serve_bounds_lines_and_survives_garbage() {
        let server = server_for(Config::default());
        let mut input = Vec::new();
        input.extend_from_slice(&vec![b'x'; LINE_BYTES_MAX + 10]);
        input.extend_from_slice(b"\n\n   \n{oops\n");
        input.extend_from_slice(br#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#);
        let mut output = Vec::new();
        serve(&server, io::Cursor::new(input), &mut output).unwrap();
        let lines: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(
            lines.len(),
            3,
            "oversized, parse error, ping; blank lines unanswered"
        );
        assert_eq!(lines[0]["error"]["code"], INVALID_REQUEST);
        assert_eq!(lines[1]["error"]["code"], PARSE_ERROR);
        assert_eq!(lines[2]["id"], 5);
    }

    #[test]
    fn transport_is_probed_for_status_and_not_discovered() {
        let mock = MockTransport::new();
        let log = mock.request_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        call(&server, "lovense_status", json!({}));
        assert_eq!(*log.lock().unwrap(), vec![ReadRequest::Probe]);
    }

    #[test]
    fn output_while_disarmed_is_denied_and_sends_nothing() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        let attempts = [
            ("lovense_function", function_args(5)),
            (
                "lovense_pattern",
                json!({"interval_ms": 500, "strengths": [5], "time_sec": 3}),
            ),
            ("lovense_preset", json!({"name": "pulse", "time_sec": 3})),
        ];
        for (tool, args) in attempts {
            let (error, is_error) = payload(&call(&server, tool, args));
            assert!(is_error, "{tool}");
            assert_eq!(error["error"]["code"], "not_armed", "{tool}");
            assert_eq!(error["error"]["class"], "safety", "{tool}");
        }
        assert!(
            sent.lock().unwrap().is_empty(),
            "a deny is final and silent on the wire"
        );
    }

    #[test]
    fn arming_is_explicit_and_discovery_does_not_arm() {
        let server = server_for(Config::default());
        payload(&call(&server, "lovense_toys", json!({})));
        payload(&call(&server, "lovense_status", json!({})));
        assert_eq!(status_of(&server)["armed"], false);
        let (error, is_error) = payload(&call(&server, "lovense_function", function_args(5)));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "not_armed");
    }

    #[test]
    fn arm_refuses_without_a_target() {
        for server in [
            server_for(real("", 20010)),
            Server::new(real("", 20010), Some(Box::new(MockTransport::new()))),
        ] {
            let (error, is_error) = payload(&call(&server, "lovense_arm", json!({})));
            assert!(is_error);
            assert_eq!(error["error"]["code"], "target_unconfigured");
            assert_eq!(status_of(&server)["armed"], false);
        }
    }

    #[test]
    fn malformed_output_arguments_are_refused_before_the_wire() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        arm(&server);
        // Domain-invalid values: gate denies, structured code, no send.
        let denied: &[(&str, Value)] = &[
            (
                "lovense_function",
                json!({"feature": "Vibrate", "strength": -1, "time_sec": 5}),
            ),
            (
                "lovense_function",
                json!({"feature": "Vibrate", "strength": 5, "time_sec": 0}),
            ),
            (
                "lovense_function",
                json!({"feature": "Explode", "strength": 5, "time_sec": 5}),
            ),
            (
                "lovense_pattern",
                json!({"interval_ms": 500, "strengths": vec![1_i64; 51], "time_sec": 5}),
            ),
            (
                "lovense_pattern",
                json!({"interval_ms": 100, "strengths": [1], "time_sec": 5}),
            ),
            (
                "lovense_pattern",
                json!({"features": "x", "interval_ms": 500, "strengths": [1], "time_sec": 5}),
            ),
            (
                "lovense_pattern",
                json!({"interval_ms": 500, "strengths": [], "time_sec": 5}),
            ),
            (
                "lovense_preset",
                json!({"name": "custom-thing", "time_sec": 5}),
            ),
        ];
        for (tool, args) in denied {
            let (error, is_error) = payload(&call(&server, tool, args.clone()));
            assert!(is_error, "{tool} {args}");
            assert_eq!(error["error"]["code"], "request_invalid", "{tool} {args}");
        }
        // Protocol-invalid shapes: JSON-RPC errors, no send.
        let refused: &[(&str, Value)] = &[
            (
                "lovense_function",
                json!({"feature": "Vibrate", "strength": "20", "time_sec": 5}),
            ),
            (
                "lovense_function",
                json!({"feature": "Vibrate", "strength": 5.5, "time_sec": 5}),
            ),
            (
                "lovense_function",
                json!({"feature": "Vibrate", "time_sec": 5}),
            ),
            (
                "lovense_function",
                json!({"feature": "Vibrate", "strength": 5, "time_sec": 5, "override": true}),
            ),
            (
                "lovense_pattern",
                json!({"interval_ms": 500, "strengths": [1, "x"], "time_sec": 5}),
            ),
        ];
        for (tool, args) in refused {
            let response = call(&server, tool, args.clone());
            assert_eq!(response["error"]["code"], INVALID_PARAMS, "{tool} {args}");
        }
        assert!(
            sent.lock().unwrap().is_empty(),
            "nothing refused ever reaches the wire"
        );
    }

    #[test]
    fn watchdog_expiry_zeroes_output_and_denies_until_rearmed() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        arm(&server);
        payload(&call(&server, "lovense_function", function_args(8)));
        // Silence past the 5 s window, simulated on the state clock.
        server.lock_state().backdate_activity(6);
        let status = status_of(&server);
        assert_eq!(status["watchdog_trips"], 1);
        assert_eq!(status["output_active"], false);
        assert_eq!(status["armed"], false, "a trip latches the master off");
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 2);
            assert_eq!(sent[1]["action"], "Stop", "the trip sends the wire zero");
        }
        // Output after expiry is denied until a fresh explicit arm.
        let (error, is_error) = payload(&call(&server, "lovense_function", function_args(8)));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "not_armed");
        assert_eq!(sent.lock().unwrap().len(), 2);
        arm(&server);
        let (_, is_error) = payload(&call(&server, "lovense_function", function_args(8)));
        assert!(!is_error, "a fresh arm restores output");
    }

    /// A reader that yields the first chunk, then sleeps, then the
    /// rest: silence on the wire, in serve-loop time.
    struct SlowLines {
        first: Vec<u8>,
        rest: Vec<u8>,
        position: usize,
        slept: bool,
        delay: Duration,
    }

    impl io::Read for SlowLines {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.position < self.first.len() {
                let n = (self.first.len() - self.position).min(buffer.len());
                buffer[..n].copy_from_slice(&self.first[self.position..self.position + n]);
                self.position += n;
                return Ok(n);
            }
            if !self.slept {
                std::thread::sleep(self.delay);
                self.slept = true;
            }
            let offset = self.position - self.first.len();
            if offset < self.rest.len() {
                let n = (self.rest.len() - offset).min(buffer.len());
                buffer[..n].copy_from_slice(&self.rest[offset..offset + n]);
                self.position += n;
                return Ok(n);
            }
            Ok(0)
        }
    }

    #[test]
    fn watchdog_fires_during_silence_without_a_next_request() {
        let config = Config {
            watchdog_secs: 1,
            ..Config::default()
        };
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(config, Some(Box::new(mock)));
        let request_line = |id: i64, name: &str, arguments: Value| -> String {
            json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": name, "arguments": arguments}})
            .to_string()
        };
        let first = format!(
            "{}\n{}\n",
            request_line(1, "lovense_arm", json!({})),
            request_line(2, "lovense_function", function_args(6)),
        );
        let rest = format!("{}\n", request_line(3, "lovense_status", json!({})));
        let slow = SlowLines {
            first: first.into_bytes(),
            rest: rest.into_bytes(),
            position: 0,
            slept: false,
            delay: Duration::from_millis(1_600),
        };
        let mut output = Vec::new();
        serve(&server, io::BufReader::new(slow), &mut output).unwrap();
        // The wire zero went out during the silence, before the status
        // request was even read.
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 2);
            assert_eq!(sent[0]["action"], "Vibrate:6");
            assert_eq!(sent[1]["action"], "Stop");
        }
        let responses: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(responses.len(), 3);
        let (status, _) = payload(&responses[2]);
        assert_eq!(status["watchdog_trips"], 1);
        assert_eq!(status["output_active"], false);
    }

    #[test]
    fn session_drop_mid_pattern_zeroes_and_never_resumes() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let fail = mock.fail_flag();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        arm(&server);
        fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let args =
            json!({"features": "v", "interval_ms": 400, "strengths": [10, 10], "time_sec": 30});
        let (error, is_error) = payload(&call(&server, "lovense_pattern", args));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "target_unreachable");
        let status = status_of(&server);
        assert_eq!(status["output_active"], false, "a drop zeroes output");
        assert_eq!(status["armed"], false, "a drop latches the master off");
        assert_eq!(status["session_health"], "degraded");
        {
            let sent = sent.lock().unwrap();
            assert_eq!(
                sent.len(),
                2,
                "the pattern attempt and the cleanup zero attempt"
            );
            assert_eq!(sent[0]["command"], "Pattern");
            assert_eq!(sent[1]["action"], "Stop");
        }
        // The transport comes back: nothing resumes. Output is denied
        // until a fresh explicit arm, which then works.
        fail.store(false, std::sync::atomic::Ordering::SeqCst);
        let (error, is_error) = payload(&call(&server, "lovense_function", function_args(5)));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "not_armed");
        arm(&server);
        let (_, is_error) = payload(&call(&server, "lovense_function", function_args(5)));
        assert!(!is_error);
    }

    #[test]
    fn stop_during_an_active_pattern_is_the_next_wire_command() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        arm(&server);
        let args =
            json!({"features": "v", "interval_ms": 400, "strengths": [10, 10], "time_sec": 30});
        payload(&call(&server, "lovense_pattern", args));
        let started = std::time::Instant::now();
        let (stopped, is_error) = payload(&call(&server, "lovense_stop", json!({})));
        let elapsed = started.elapsed();
        assert!(!is_error);
        assert_eq!(stopped["stop_sent"], true);
        assert!(
            elapsed < Duration::from_secs(1),
            "stop latency bound (1 s, CI-safe) exceeded: {elapsed:?}"
        );
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(
            sent[1]["action"], "Stop",
            "no output command follows the stop"
        );
        assert_eq!(status_of(&server)["output_active"], false);
    }

    #[test]
    fn arm_disarm_interleavings_stay_serialized_and_safe() {
        let server = server_for(Config::default());
        arm(&server);
        arm(&server); // re-arm while armed: idempotent, no output starts
        assert_eq!(status_of(&server)["output_active"], false);
        let (_, is_error) = payload(&call(&server, "lovense_function", function_args(4)));
        assert!(!is_error);
        payload(&call(&server, "lovense_disarm", json!({})));
        payload(&call(&server, "lovense_disarm", json!({}))); // idempotent
        let (error, is_error) = payload(&call(&server, "lovense_function", function_args(4)));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "not_armed");
        // Disarm during what would be a pattern: arm, then disarm
        // before any output; a pattern is then denied, never queued.
        arm(&server);
        payload(&call(&server, "lovense_disarm", json!({})));
        let args = json!({"interval_ms": 400, "strengths": [10], "time_sec": 5});
        let (error, is_error) = payload(&call(&server, "lovense_pattern", args));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "not_armed");
        // And the full recovery path still works.
        arm(&server);
        let (_, is_error) = payload(&call(&server, "lovense_function", function_args(4)));
        assert!(!is_error);
    }

    #[test]
    fn shutdown_at_eof_zeroes_active_output() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        let input = format!(
            "{}\n{}\n",
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "lovense_arm", "arguments": {}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "lovense_function", "arguments": function_args(7)}}),
        );
        let mut output = Vec::new();
        serve(&server, io::Cursor::new(input.into_bytes()), &mut output).unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2, "the function and the shutdown zero");
        assert_eq!(sent[1]["action"], "Stop");
        assert!(!server.output_active());
    }

    #[test]
    fn s2_wire_bodies_are_constructed_only_in_the_gate() {
        // The architectural invariant behind the safety contract: the
        // gate module (decide.rs) is the only production code that
        // builds wire command bodies. Server, transport, and both
        // providers move gate plans; they never make a body. (The
        // unified era keeps the invariant and widens its sweep to the
        // provider modules.)
        let sources = [
            include_str!("server.rs"),
            include_str!("transport.rs"),
            include_str!("main.rs"),
            include_str!("config.rs"),
            include_str!("state.rs"),
            include_str!("provider.rs"),
            include_str!("provider_buttplug.rs"),
        ];
        for source in sources {
            let production = source.split("#[cfg(test)]").next().unwrap();
            assert!(
                !production.contains("apiVer"),
                "a wire body field appears outside decide.rs"
            );
        }
        let gate = include_str!("decide.rs");
        let gate_production = gate.split("#[cfg(test)]").next().unwrap();
        assert!(gate_production.contains("\"apiVer\": 1"));
        assert!(gate_production.contains("\"apiVer\": 2"));
    }

    // --- unified surface (ache) ---------------------------------------
    //
    // The merge battery: the Lovense mock and the Buttplug fixture
    // behind one server. New tests split validation / adversarial like
    // the rest of the suite.

    use std::sync::Arc;

    use crate::provider::FakeButtplug;

    /// A unified server plus the fixture provider's logs and flags:
    /// executed plans, zero count, pattern-cancelled, fail switch.
    type UnifiedServer = (
        Server,
        Arc<Mutex<Vec<AllowedOutput>>>,
        Arc<Mutex<u32>>,
        Arc<Mutex<bool>>,
        Arc<std::sync::atomic::AtomicBool>,
    );

    fn unified() -> UnifiedServer {
        let fake = FakeButtplug::new();
        let executed = fake.executed_log();
        let zeros = fake.zero_log();
        let cancelled = fake.pattern_cancelled_flag();
        let fail = fake.fail_flag();
        let server = Server::new(Config::default(), Some(Box::new(MockTransport::new())))
            .with_buttplug(Box::new(fake));
        (server, executed, zeros, cancelled, fail)
    }

    fn unified_with_mock() -> (Server, Arc<Mutex<Vec<Value>>>) {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)))
            .with_buttplug(Box::new(FakeButtplug::new()));
        (server, sent)
    }

    // --- validation (unified) -----------------------------------------

    #[test]
    fn unified_discovery_merges_both_providers() {
        let (server, ..) = unified();
        let (devices, is_error) = payload(&call(&server, "ache_devices", json!({})));
        assert!(!is_error);
        assert_eq!(devices["count"], 4);
        assert_eq!(devices["providers"]["lovense"]["count"], 2);
        assert_eq!(devices["providers"]["buttplug"]["count"], 2);
        let vibrator = &devices["devices"][2];
        assert_eq!(vibrator["id"], "buttplug:0");
        assert_eq!(vibrator["provider"], "buttplug");
        assert_eq!(vibrator["capability_source"], "fixture");
        assert_eq!(vibrator["capabilities"][0]["feature"], "Vibrate");
        assert_eq!(vibrator["capabilities"][0]["max"], 20);
        assert_eq!(vibrator["capabilities"][0]["steps"], 20);
    }

    #[test]
    fn unified_device_query_returns_capabilities_as_data() {
        let (server, ..) = unified();
        let (device, is_error) = payload(&call(
            &server,
            "ache_device",
            json!({"device": "buttplug:1"}),
        ));
        assert!(!is_error);
        assert_eq!(device["name"], "Fixture Duo");
        assert_eq!(device["battery"], 63);
        let features: Vec<&str> = device["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cap| cap["feature"].as_str().unwrap())
            .collect();
        assert_eq!(features, ["Vibrate", "Rotate"]);
        let (device, _) = payload(&call(
            &server,
            "ache_device",
            json!({"device": "lovense:mock-toy-1"}),
        ));
        assert_eq!(device["provider"], "lovense");
        assert_eq!(device["capability_source"], "reported");
    }

    #[test]
    fn gated_function_on_a_buttplug_device_executes_the_plan() {
        let (server, executed, ..) = unified();
        arm(&server);
        let args =
            json!({"feature": "Rotate", "strength": 7, "time_sec": 5, "device": "buttplug:1"});
        let (ran, is_error) = payload(&call(&server, "lovense_function", args));
        assert!(!is_error);
        assert_eq!(ran["provider"], "buttplug");
        assert_eq!(ran["device"], "buttplug:1");
        assert_eq!(ran["effective"]["strength"], 7);
        let executed = executed.lock().unwrap();
        assert_eq!(executed.len(), 1);
        match &executed[0] {
            AllowedOutput::Function {
                feature,
                strength,
                toy,
                ..
            } => {
                assert_eq!(feature, "Rotate");
                assert_eq!(*strength, 7);
                assert_eq!(toy, "buttplug:1");
            }
            other => panic!("expected a function plan, got {other:?}"),
        }
    }

    #[test]
    fn pattern_on_a_buttplug_device_executes_the_clamped_plan() {
        let (server, executed, ..) = unified();
        arm(&server);
        let args = json!({"features": "v", "interval_ms": 500, "strengths": [30, 5], "time_sec": 4, "device": "buttplug:0"});
        let (ran, is_error) = payload(&call(&server, "lovense_pattern", args));
        assert!(!is_error);
        assert_eq!(ran["gate"]["clamped"], true);
        assert_eq!(ran["effective"]["strengths"], json!([20, 5]));
        let executed = executed.lock().unwrap();
        match &executed[0] {
            AllowedOutput::Pattern { strengths, .. } => {
                assert_eq!(
                    strengths,
                    &vec![20, 5],
                    "the plan carries clamped values only"
                );
            }
            other => panic!("expected a pattern plan, got {other:?}"),
        }
    }

    #[test]
    fn status_reports_both_providers() {
        let (server, ..) = unified();
        let status = status_of(&server);
        assert_eq!(status["providers"]["lovense"]["state"], "mock");
        assert_eq!(status["providers"]["buttplug"]["state"], "mock");
        assert_eq!(status["providers"]["buttplug"]["available"], true);
    }

    #[test]
    fn unified_lovense_device_routes_onto_the_legacy_wire() {
        let (server, sent) = unified_with_mock();
        arm(&server);
        let args = json!({"feature": "Vibrate", "strength": 3, "time_sec": 2, "device": "lovense:mock-toy-2"});
        let (ran, is_error) = payload(&call(&server, "lovense_function", args));
        assert!(!is_error);
        assert_eq!(ran["provider"], "lovense");
        assert_eq!(
            ran["command"]["toy"], "mock-toy-2",
            "the wire sees the raw toy id"
        );
        assert_eq!(ran["command"]["action"], "Vibrate:3");
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    // --- adversarial (unified) ----------------------------------------

    #[test]
    fn cross_provider_refusal_parity() {
        // The merge's core safety obligation: an intent the contract
        // refuses on the Lovense path is refused, with the same code,
        // on the Buttplug path — because both go through the one gate.
        let (server, executed, ..) = unified();
        let battery: Vec<(Value, &str)> = vec![
            // (output-tool arguments, expected code while DISARMED)
            (function_args(5), "not_armed"),
        ];
        for (args, code) in &battery {
            let (legacy, _) = payload(&call(&server, "lovense_function", args.clone()));
            let mut unified_args = args.clone();
            unified_args["device"] = json!("buttplug:1");
            let (unified, _) = payload(&call(&server, "lovense_function", unified_args));
            assert_eq!(legacy["error"]["code"], *code);
            assert_eq!(unified["error"]["code"], *code, "parity for {args}");
        }
        arm(&server);
        // Armed refusals: same malformed intents, both paths.
        let denied: Vec<Value> = vec![
            json!({"feature": "Vibrate", "strength": 5, "time_sec": 0}),
            json!({"feature": "Vibrate", "strength": -1, "time_sec": 5}),
            json!({"feature": "Explode", "strength": 5, "time_sec": 5}),
            json!({"feature": "Vibrate", "strength": 5, "time_sec": 3601}),
        ];
        for args in denied {
            let (legacy, _) = payload(&call(&server, "lovense_function", args.clone()));
            let mut unified_args = args.clone();
            unified_args["device"] = json!("buttplug:1");
            let (unified, _) = payload(&call(&server, "lovense_function", unified_args));
            assert_eq!(
                legacy["error"]["code"], unified["error"]["code"],
                "refusal parity for {args}"
            );
            assert_eq!(legacy["error"]["code"], "request_invalid");
        }
        // Pattern refusals: interval floor and strength count, both paths.
        for args in [
            json!({"features": "v", "interval_ms": 100, "strengths": [5], "time_sec": 5}),
            json!({"features": "v", "interval_ms": 500, "strengths": vec![1_i64; 51], "time_sec": 5}),
        ] {
            let (legacy, _) = payload(&call(&server, "lovense_pattern", args.clone()));
            let mut unified_args = args.clone();
            unified_args["device"] = json!("buttplug:1");
            let (unified, _) = payload(&call(&server, "lovense_pattern", unified_args));
            assert_eq!(legacy["error"]["code"], "request_invalid");
            assert_eq!(unified["error"]["code"], "request_invalid");
        }
        // Clamping parity: 25 becomes the same effective 20 on both.
        let (legacy, _) = payload(&call(&server, "lovense_function", function_args(25)));
        let (unified, _) = payload(&call(
            &server,
            "lovense_function",
            json!({"feature": "Vibrate", "strength": 25, "time_sec": 5, "device": "buttplug:1"}),
        ));
        assert_eq!(legacy["effective"]["strength"], 20);
        assert_eq!(unified["effective"]["strength"], 20);
        // Only the two allowed calls ever executed anywhere.
        assert_eq!(executed.lock().unwrap().len(), 1);
    }

    #[test]
    fn preset_on_a_buttplug_device_is_refused_and_executes_nothing() {
        let (server, executed, ..) = unified();
        arm(&server);
        let args = json!({"name": "pulse", "time_sec": 5, "device": "buttplug:0"});
        let (error, is_error) = payload(&call(&server, "lovense_preset", args));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "capability_unsupported");
        assert_eq!(error["error"]["class"], "target");
        assert!(executed.lock().unwrap().is_empty());
    }

    #[test]
    fn no_provider_available_is_typed_never_a_fake_success() {
        // Lovense: real transport, no host, no transport at all.
        // Buttplug: inert (start failure / not wired).
        let server =
            Server::new(real("", 20010), None).with_buttplug(Box::new(InertProvider::unavailable(
                ProviderId::Buttplug,
                "embedded buttplug failed to start: no bluetooth",
            )));
        let (error, is_error) = payload(&call(&server, "ache_devices", json!({})));
        assert!(is_error, "no provider could enumerate: the call must fail");
        assert_eq!(error["error"]["code"], "target_unconfigured");
        let (error, is_error) = payload(&call(
            &server,
            "ache_device",
            json!({"device": "buttplug:0"}),
        ));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "provider_unavailable");
        let (error, is_error) = payload(&call(&server, "lovense_arm", json!({})));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "target_unconfigured");
        let (error, is_error) = payload(&call(&server, "lovense_function", function_args(5)));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "target_unconfigured");
    }

    #[test]
    fn unknown_and_malformed_device_ids_are_refused() {
        let (server, executed, ..) = unified();
        arm(&server);
        let (error, is_error) = payload(&call(
            &server,
            "ache_device",
            json!({"device": "buttplug:99"}),
        ));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "device_not_found");
        let (error, is_error) = payload(&call(&server, "ache_device", json!({"device": "wat:1"})));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "request_invalid");
        let args =
            json!({"feature": "Vibrate", "strength": 5, "time_sec": 5, "device": "buttplug:99"});
        let (error, is_error) = payload(&call(&server, "lovense_function", args));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "device_not_found");
        assert!(
            executed.lock().unwrap().is_empty(),
            "a refused route executes nothing"
        );
    }

    #[test]
    fn stop_zeroes_both_providers_and_cancels_the_pattern() {
        let (server, _e, zeros, cancelled, _f) = unified();
        arm(&server);
        let args = json!({"features": "v", "interval_ms": 400, "strengths": [10, 10], "time_sec": 30, "device": "buttplug:1"});
        payload(&call(&server, "lovense_pattern", args));
        let (stopped, is_error) = payload(&call(&server, "lovense_stop", json!({})));
        assert!(!is_error);
        assert_eq!(stopped["stop_sent"], true);
        assert!(
            *zeros.lock().unwrap() >= 1,
            "the buttplug provider was zeroed"
        );
        assert!(
            *cancelled.lock().unwrap(),
            "the running pattern was cancelled by the zero"
        );
        assert_eq!(status_of(&server)["armed"], false);
    }

    #[test]
    fn buttplug_execute_failure_is_a_session_drop() {
        let (server, _e, _z, _c, fail) = unified();
        arm(&server);
        fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let args =
            json!({"feature": "Vibrate", "strength": 5, "time_sec": 5, "device": "buttplug:0"});
        let (error, is_error) = payload(&call(&server, "lovense_function", args));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "target_unreachable");
        let status = status_of(&server);
        assert_eq!(status["output_active"], false, "a drop zeroes output");
        assert_eq!(status["armed"], false, "a drop latches the master off");
        assert_eq!(status["session_health"], "degraded");
        // Nothing resumes when the provider recovers: re-arm first.
        fail.store(false, std::sync::atomic::Ordering::SeqCst);
        let args =
            json!({"feature": "Vibrate", "strength": 5, "time_sec": 5, "device": "buttplug:0"});
        let (error, is_error) = payload(&call(&server, "lovense_function", args));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "not_armed");
    }

    #[test]
    fn watchdog_trip_zeroes_the_buttplug_provider_too() {
        let (server, _e, zeros, _c, _f) = unified();
        arm(&server);
        let args =
            json!({"feature": "Vibrate", "strength": 8, "time_sec": 30, "device": "buttplug:0"});
        payload(&call(&server, "lovense_function", args));
        server.lock_state().backdate_activity(6);
        let status = status_of(&server);
        assert_eq!(status["watchdog_trips"], 1);
        assert_eq!(status["armed"], false);
        assert!(
            *zeros.lock().unwrap() >= 1,
            "the trip zeroes every provider, not just the Lovense wire"
        );
    }

    #[test]
    fn stop_attempts_both_providers_even_when_one_zero_fails() {
        // The Lovense send is failing (injected drop). A stop that
        // short-circuited on that failure would leave a Buttplug
        // device running — the zero must be attempted everywhere.
        let mock = MockTransport::new();
        let fail = mock.fail_flag();
        let fake = FakeButtplug::new();
        let zeros = fake.zero_log();
        let server =
            Server::new(Config::default(), Some(Box::new(mock))).with_buttplug(Box::new(fake));
        fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let (error, is_error) = payload(&call(&server, "lovense_stop", json!({})));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "target_unreachable");
        assert!(
            *zeros.lock().unwrap() >= 1,
            "the buttplug provider was zeroed despite the lovense failure"
        );
        assert_eq!(status_of(&server)["armed"], false);
    }

    #[test]
    fn watchdog_zero_attempts_both_providers_when_one_fails() {
        // Same obligation on the best-effort path: the watchdog's
        // zero must reach the Buttplug provider even though the
        // Lovense zero is failing.
        let mock = MockTransport::new();
        let fail = mock.fail_flag();
        let fake = FakeButtplug::new();
        let zeros = fake.zero_log();
        let server =
            Server::new(Config::default(), Some(Box::new(mock))).with_buttplug(Box::new(fake));
        arm(&server);
        let args =
            json!({"feature": "Vibrate", "strength": 8, "time_sec": 30, "device": "buttplug:0"});
        payload(&call(&server, "lovense_function", args));
        fail.store(true, std::sync::atomic::Ordering::SeqCst);
        server.lock_state().backdate_activity(6);
        let status = status_of(&server);
        assert_eq!(status["watchdog_trips"], 1);
        assert!(
            *zeros.lock().unwrap() >= 1,
            "the trip attempted the buttplug zero despite the lovense failure"
        );
    }

    #[test]
    fn unified_route_applies_reported_lovense_caps_legacy_route_unchanged() {
        // The mock Lush reports Vibrate only. Through the unified
        // route the gate refuses Rotate against the toy's own reported
        // capabilities; through the legacy toy selector the historical
        // behavior stands (the app arbitrates, as it always has).
        let (server, sent) = unified_with_mock();
        arm(&server);
        let args = json!({"feature": "Rotate", "strength": 3, "time_sec": 2, "device": "lovense:mock-toy-1"});
        let (error, is_error) = payload(&call(&server, "lovense_function", args));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "capability_unsupported");
        let args = json!({"feature": "Rotate", "strength": 3, "time_sec": 2, "toy": "mock-toy-1"});
        let (_, is_error) = payload(&call(&server, "lovense_function", args));
        assert!(!is_error, "the legacy path keeps its historical reach");
        assert_eq!(sent.lock().unwrap().len(), 1);
    }
}
