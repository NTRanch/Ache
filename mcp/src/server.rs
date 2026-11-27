//! The tool surface and dispatch core. Sixteen tools. The original eight
//! (`lovense_*`) keep their names and shapes: two read-only (status,
//! toys) and six for the output core (arm, disarm, function, pattern,
//! preset, stop). Two unified read-only tools (`ache_devices`,
//! `ache_device`) enumerate devices across every provider — the
//! Lovense Standard API provider, the embedded Buttplug provider, and
//! the opt-in Server API provider (D-1451) — with capabilities as
//! data; the output tools route to any provider when given a unified
//! `device` id. `ache_pairing` serves the Server API provider's
//! pairing surface (state, and an explicit QR request); it can cause
//! no output. The four `ache_program_*` tools (1.4.0: the KoiSync and
//! XToys landings) load frozen specs — patterns, funscripts and packs,
//! depth curves, automations, sessions — and run, stop, and feed events
//! to one program at a time; the TCode provider joins the other three.
//! Every output, on any provider, passes the one
//! `decide()` gate in `decide.rs` before anything can reach a device.
//! Protocol misuse (bad arguments) is a JSON-RPC error; an operational
//! failure (no target, unreachable app, a gate deny) is a tool result
//! with `isError: true` and the structured error as its text.
//!
//! The wire itself is no longer hand-rolled here: the era guard
//! (`guard.rs`) owns the framing contract and the rmcp service
//! (`service.rs`) owns the protocol, modern era only (2026-07-28).
//! This module owns what neither may touch: the handlers, the tool
//! definitions (served raw, unchanged), and the safety state.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::automation::{Automation, Event, TICK_MS};
use crate::config::{Config, TransportKind};
use crate::decide::{
    AllowedOutput, Decision, Feature, FunctionIntent, OutputIntent, PatternIntent, PresetIntent,
    ScorePart, TOY_ID_BYTES_MAX, decide, decide_plan, decide_score, lovense_wire,
};
use crate::error::{AcheError, ErrorCode};
use crate::pattern::{Function, Pattern};
use crate::provider::{Device, DeviceProvider, InertProvider, LovenseProvider, ProviderId};
use crate::provider_server_api::{NoPairing, PairingProvider};
use crate::provider_tcode::TcodeProvider;
use crate::session::{
    AxisView, CompiledSession, Gated, Library, SESSION_FORMAT, SessionRun, artefact_line,
    gate_proposed, seed_library,
};
use crate::state::SafetyState;
use crate::transport::Transport;

/// Most specs of one kind the library holds.
const LIBRARY_ENTRIES_MAX: usize = 128;
/// Most events waiting for the next program tick.
const PROGRAM_EVENTS_QUEUED_MAX: usize = 64;

pub const SERVER_NAME: &str = "ache";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Most unexpected argument names echoed back in a refusal.
const ARGUMENT_NAMES_REPORTED_MAX: usize = 8;
const ARGUMENT_NAME_BYTES_MAX: usize = 64;

const INVALID_PARAMS: i64 = -32602;

/// Every tool the server serves, in `tools/list` order. The original
/// eight keep their positions; the unified tools append.
pub(crate) const TOOL_NAMES: [&str; 16] = [
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
    "ache_score_start",
    "ache_pairing",
    "ache_program_load",
    "ache_program_run",
    "ache_program_stop",
    "ache_program_event",
];

pub struct Server {
    config: Config,
    /// The Lovense provider. Its transport is `None` exactly when the
    /// config names the real transport without a host; then nothing can
    /// reach the Lovense wire.
    lovense: Arc<LovenseProvider>,
    /// The Buttplug provider: embedded in production, a fixture in mock
    /// mode, or inert (disabled / not wired by a bare constructor).
    buttplug: Arc<dyn DeviceProvider>,
    /// The Server API provider (D-1451): the cloud route, inert unless
    /// the operator enabled it in config and production wiring
    /// replaced it. Inert is its default state in every sense.
    server_api: Arc<dyn DeviceProvider>,
    /// The Server API pairing surface, wired alongside the provider:
    /// a `NoPairing` (refusing, state-reporting) instance whenever the
    /// provider is disabled or unwired.
    pairing: Arc<dyn PairingProvider>,
    /// The safety state machine: arm latch, output activity, watchdog
    /// bookkeeping, session health. One state for the whole server —
    /// the master toggle and the watchdog govern every provider.
    state: Arc<Mutex<SafetyState>>,
    output_lock: Arc<Mutex<()>>,
    score: Mutex<Option<Arc<ScoreRun>>>,
    /// The TCode provider (KoiSync landing): disabled unless wired.
    tcode: Arc<TcodeProvider>,
    /// Loaded frozen specs, seeded with the estate library.
    library: Mutex<Library>,
    /// The current (or last) program run.
    program: Mutex<Option<Arc<ProgramRun>>>,
}

struct ScoreRun {
    cancelled: Mutex<bool>,
    wake: Condvar,
    remaining: Mutex<usize>,
}

/// One running program: a compiled session, its pending events, and
/// its run artefact (D-713: numbers from the run, not from belief).
struct ProgramRun {
    cancelled: Mutex<bool>,
    wake: Condvar,
    events: Mutex<Vec<Event>>,
    session: Arc<CompiledSession>,
    report: Mutex<ProgramReport>,
}

#[derive(Default)]
struct ProgramReport {
    t_ms: u64,
    commands: u64,
    clamps: u64,
    lines: u64,
    hasher: Sha256,
    stream_hash: Option<String>,
    stop_reason: Option<String>,
    log: Vec<String>,
}

impl ProgramReport {
    fn record(&mut self, line: &str, clamps: u32) {
        if self.lines > 0 {
            self.hasher.update(b"\n");
        }
        self.hasher.update(line.as_bytes());
        self.lines += 1;
        self.commands += 1;
        self.clamps += u64::from(clamps);
    }

    fn finish(&mut self, reason: String, log: Vec<String>) {
        if self.stop_reason.is_none() {
            self.stop_reason = Some(reason);
        }
        let digest = std::mem::take(&mut self.hasher).finalize();
        self.stream_hash = Some(digest.iter().map(|b| format!("{b:02x}")).collect());
        self.log = log;
    }

    fn to_json(&self, session: &CompiledSession) -> Value {
        json!({
            "session": session.id,
            "session_hash": session.hash,
            "duration_ms": session.duration_ms,
            "t_ms": self.t_ms,
            "commands": self.commands,
            "clamps": self.clamps,
            "stop_reason": self.stop_reason,
            "stream_hash": self.stream_hash,
            "log": self.log,
        })
    }
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
    /// A server with the Lovense provider wired; the Buttplug and
    /// Server API providers are present in the surface but inert
    /// until [`Server::with_buttplug`] / [`Server::with_server_api`]
    /// replace them (production wiring in `main` always does).
    pub fn new(config: Config, transport: Option<Box<dyn Transport>>) -> Server {
        let lovense = LovenseProvider::new(&config, transport);
        Server {
            config,
            lovense: Arc::new(lovense),
            buttplug: Arc::new(InertProvider::unavailable(
                ProviderId::Buttplug,
                "no buttplug provider is wired in this server",
            )),
            server_api: Arc::new(InertProvider::disabled(
                ProviderId::ServerApi,
                "server api provider is disabled by configuration \
                 ([server_api] enabled = false); local routes remain the default",
            )),
            pairing: Arc::new(NoPairing::disabled(
                "server api provider is disabled by configuration \
                 ([server_api] enabled = false); local routes remain the default",
            )),
            state: Arc::new(Mutex::new(SafetyState::new())),
            output_lock: Arc::new(Mutex::new(())),
            score: Mutex::new(None),
            tcode: Arc::new(TcodeProvider::disabled()),
            library: Mutex::new(
                seed_library().expect("the seed library validates (session::tests gate it)"),
            ),
            program: Mutex::new(None),
        }
    }

    /// Wire the TCode provider behind the seam (KoiSync landing).
    #[must_use]
    pub fn with_tcode(mut self, tcode: TcodeProvider) -> Server {
        self.tcode = Arc::new(tcode);
        self
    }

    /// Wire the Buttplug provider behind the seam.
    #[must_use]
    pub fn with_buttplug(mut self, buttplug: Box<dyn DeviceProvider>) -> Server {
        self.buttplug = Arc::from(buttplug);
        self
    }

    /// Wire the Server API provider behind the seam (D-1451).
    #[must_use]
    pub fn with_server_api(mut self, server_api: Box<dyn DeviceProvider>) -> Server {
        self.server_api = Arc::from(server_api);
        self
    }

    /// Wire the Server API pairing surface alongside its provider.
    #[must_use]
    pub fn with_pairing(mut self, pairing: Box<dyn PairingProvider>) -> Server {
        self.pairing = Arc::from(pairing);
        self
    }

    fn lock_state(&self) -> MutexGuard<'_, SafetyState> {
        // A poisoned lock only means a thread panicked mid-update; the
        // state value is still coherent, so keep serving.
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Whether output could reach a target at all: any provider
    /// can accept output. The Lovense half is the original rule (a
    /// transport exists and the config gate permits it); the other
    /// providers answer with their probes.
    fn target_configured(&self) -> bool {
        self.lovense.target_configured()
            || self.buttplug.probe().available
            || self.server_api.probe().available
            || self.tcode.probe().available
    }

    fn snapshot(&self) -> crate::decide::GateSnapshot {
        self.lock_state()
            .snapshot(self.target_configured(), self.config.intensity_ceiling)
    }

    /// Dispatch one shape-validated tool call. The era guard has
    /// already answered the envelope questions (params shape, name,
    /// known tool) with the historical messages; what remains is the
    /// handler contract: a tool result value, or a protocol refusal
    /// `(code, message)` for field-level misuse.
    ///
    /// A tool call is a sign of life: fire the watchdog first if the
    /// window already elapsed in silence, then count this call as
    /// the latest heartbeat.
    pub(crate) fn dispatch_call(
        &self,
        name: &str,
        arguments: Option<Value>,
    ) -> Result<Value, (i64, String)> {
        self.trip_watchdog();
        self.lock_state().note_activity();
        let outcome = self.dispatch(name, arguments.as_ref());
        match outcome {
            Ok(value) => Ok(tool_result(&value, false)),
            Err(CallError::Operational(error)) => Ok(tool_result(&error.to_json(), true)),
            Err(CallError::Protocol(code, message)) => Err((code, message)),
        }
    }

    fn dispatch(&self, name: &str, arguments: Option<&Value>) -> Result<Value, CallError> {
        match name {
            "ache_program_load" => {
                let args = arguments_object(arguments)?;
                check_fields_allowed(&args, &["kind", "spec", "id", "function"])?;
                let kind = required_string(&args, "kind")?;
                let spec = args
                    .get("spec")
                    .filter(|v| v.is_object())
                    .cloned()
                    .ok_or((INVALID_PARAMS, "`spec` must be an object".to_string()))?;
                let id = optional_string(&args, "id")?;
                let function = optional_string(&args, "function")?;
                Ok(self.program_load(&kind, &spec, id.as_deref(), function.as_deref())?)
            }
            "ache_program_run" => {
                let args = arguments_object(arguments)?;
                check_fields_allowed(&args, &["session", "pattern", "pack", "device", "function"])?;
                let pick = |name: &str| optional_string(&args, name);
                Ok(self.program_run(
                    pick("session")?.as_deref(),
                    pick("pattern")?.as_deref(),
                    pick("pack")?.as_deref(),
                    pick("device")?.as_deref(),
                    pick("function")?.as_deref(),
                )?)
            }
            "ache_program_stop" => {
                check_no_arguments(arguments)?;
                Ok(self.program_stop()?)
            }
            "ache_program_event" => {
                let args = arguments_object(arguments)?;
                check_fields_allowed(&args, &["kind", "name", "value"])?;
                let kind = required_string(&args, "kind")?;
                let name = required_string(&args, "name")?;
                let value = match args.get("value") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(
                        v.as_f64()
                            .ok_or((INVALID_PARAMS, "`value` must be a number".to_string()))?,
                    ),
                };
                Ok(self.program_event(&kind, &name, value)?)
            }
            "ache_score_start" => {
                let args = arguments_object(arguments)?;
                check_fields_allowed(&args, &["parts"])?;
                let parts: Vec<ScorePart> =
                    serde_json::from_value(args.get("parts").cloned().unwrap_or(Value::Null))
                        .map_err(|e| (INVALID_PARAMS, format!("invalid score parts: {e}")))?;
                Ok(self.start_score(&parts)?)
            }
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
            "ache_pairing" => {
                let args = arguments_object(arguments)?;
                check_fields_allowed(&args, &["action"])?;
                let action = required_string(&args, "action")?;
                Ok(self.pairing(&action)?)
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
        let score_running = self.score.lock().unwrap().as_ref().is_some_and(|run| {
            !*run.cancelled.lock().unwrap() && *run.remaining.lock().unwrap() > 0
        });
        let program = self.program.lock().unwrap().as_ref().map(|run| {
            let report = run.report.lock().unwrap();
            let mut value = report.to_json(&run.session);
            value["running"] = json!(report.stop_reason.is_none());
            value
        });
        let safety = self.lock_state();
        json!({
            "program": program,
            "score_running": score_running,
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
                shutdown each zero output on every provider and latch the master off",
            "note": note,
            "providers": {
                "lovense": self.lovense.probe().to_json(),
                "buttplug": self.buttplug.probe().to_json(),
                "serverapi": self.server_api.probe().to_json(),
                "tcode": self.tcode.probe().to_json(),
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
            self.lovense.as_ref() as &dyn DeviceProvider,
            self.buttplug.as_ref() as &dyn DeviceProvider,
            self.server_api.as_ref() as &dyn DeviceProvider,
            self.tcode.as_ref() as &dyn DeviceProvider,
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

    /// The Server API pairing surface (D-1451). `status` presents
    /// pairing state — presence only, never a credential value.
    /// `request_qr` is the explicit operator act that asks Lovense
    /// for a QR code for the Owner to scan in the Remote app; it
    /// pairs nothing itself and can cause no device output. The
    /// provider's config enablement is the approval boundary: a
    /// disabled provider's pairing surface refuses both actions.
    fn pairing(&self, action: &str) -> Result<Value, AcheError> {
        match action {
            "status" => Ok(self.pairing.pairing_state().to_json()),
            "request_qr" => {
                let qr = self.pairing.request_qr()?;
                Ok(json!({
                    "qr": qr.to_json(),
                    "note": "scan this code in Lovense Remote to pair; pairing \
                        completes in the app, and the resulting user id is placed \
                        in the password store at the configured uid entry as the \
                        final step",
                }))
            }
            other => Err(AcheError::new(
                ErrorCode::RequestInvalid,
                format!("pairing action {other:?} must be `status` or `request_qr`"),
            )),
        }
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
            self.lovense.as_ref()
        } else if id.starts_with("buttplug:") {
            self.buttplug.as_ref()
        } else if id.starts_with("serverapi:") {
            self.server_api.as_ref()
        } else if id.starts_with("tcode:") {
            self.tcode.as_ref()
        } else {
            return Err(AcheError::new(
                ErrorCode::RequestInvalid,
                format!(
                    "device id {id:?} must start with `lovense:`, `buttplug:`, `serverapi:`, or `tcode:`"
                ),
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
        let _output = self.output_lock.lock().unwrap();
        let sent = self.send_stop_strict()?;
        self.lock_state().zero_and_disarm(event);
        Ok(sent)
    }

    /// Zero every provider. All providers are ALWAYS attempted, even
    /// when the first fails: a panic stop that leaves another
    /// provider running because the first one's send failed is not a
    /// stop. `Ok(false)` means no provider had a target to zero: the
    /// local zero still stands. A zero failure on any provider
    /// marks the session dropped; the first failure is the error.
    fn send_stop_strict(&self) -> Result<bool, AcheError> {
        self.cancel_score();
        self.cancel_program();
        let lovense = self.lovense.zero();
        let buttplug = self.buttplug.zero();
        let server_api = self.server_api.zero();
        let tcode = self.tcode.zero();
        match (lovense, buttplug, server_api, tcode) {
            (Ok(lovense_sent), Ok(buttplug_sent), Ok(server_api_sent), Ok(tcode_sent)) => {
                Ok(lovense_sent || buttplug_sent || server_api_sent || tcode_sent)
            }
            (Err(error), _, _, _)
            | (_, Err(error), _, _)
            | (_, _, Err(error), _)
            | (_, _, _, Err(error)) => {
                self.lock_state().on_session_drop();
                Err(error)
            }
        }
    }

    /// Best-effort zero on every provider, for the paths that must
    /// never fail: a watchdog trip, shutdown, recovery after a drop.
    /// All are attempted even when one fails (never short-circuited).
    /// State has already been zeroed by the caller; a failure here
    /// means the session is gone and is recorded as a drop.
    fn send_stop_best_effort(&self) {
        self.cancel_score();
        self.cancel_program();
        let lovense_failed = self.lovense.zero().is_err();
        let buttplug_failed = self.buttplug.zero().is_err();
        let server_api_failed = self.server_api.zero().is_err();
        let tcode_failed = self.tcode.zero().is_err();
        if lovense_failed || buttplug_failed || server_api_failed || tcode_failed {
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
    /// best-effort zero on every provider follows the failed command.
    fn run_output(&self, intent: OutputIntent, device: Option<&str>) -> Result<Value, AcheError> {
        let _output = self.output_lock.lock().unwrap();
        if self.score_running() {
            return Err(AcheError::new(
                ErrorCode::RequestInvalid,
                "stop the running score before other output",
            ));
        }
        if self.program_running() {
            return Err(AcheError::new(
                ErrorCode::RequestInvalid,
                "stop the running program before other output",
            ));
        }
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
            ProviderId::ServerApi => {
                let effective = plan_effective(&plan);
                self.finish_output(
                    self.server_api.execute(&plan),
                    json!({
                        "sent": true,
                        "provider": "serverapi",
                        "device": target.id,
                        "gate": { "decision": "allow", "clamped": clamped },
                        "effective": effective,
                    }),
                )
            }
            // The gate already refuses every function/pattern/preset on
            // a TCode device (its capabilities are axes); this arm is a
            // second refusal, never a session drop.
            ProviderId::Tcode => Err(AcheError::new(
                ErrorCode::CapabilityUnsupported,
                "tcode devices take axis frames from ache_program_run only",
            )),
        }
    }

    fn cancel_score(&self) {
        if let Some(run) = self.score.lock().unwrap().take() {
            *run.cancelled.lock().unwrap() = true;
            run.wake.notify_all();
        }
    }

    fn score_running(&self) -> bool {
        self.score.lock().unwrap().as_ref().is_some_and(|run| {
            !*run.cancelled.lock().unwrap() && *run.remaining.lock().unwrap() > 0
        })
    }

    fn start_score(&self, parts: &[ScorePart]) -> Result<Value, AcheError> {
        let _output = self.output_lock.lock().unwrap();
        if self.lock_state().output_active() || self.program_running() {
            return Err(AcheError::new(
                ErrorCode::RequestInvalid,
                "stop active output before starting a score",
            ));
        }
        let mut targets = Vec::new();
        let mut devices = Vec::new();
        for part in parts {
            if let Ok(device) = self.resolve_device(&part.device) {
                let selector = match device.provider {
                    ProviderId::Lovense => device.id.strip_prefix("lovense:").unwrap().to_string(),
                    ProviderId::ServerApi => {
                        device.id.strip_prefix("serverapi:").unwrap().to_string()
                    }
                    ProviderId::Buttplug | ProviderId::Tcode => device.id.clone(),
                };
                targets.push((device.id.clone(), selector, device.caps()));
                devices.push(device);
            }
        }
        let plans = decide_score(&self.snapshot(), parts, &targets)?;
        let verdict: Vec<Value> = plans
            .iter()
            .map(|part| {
                json!({
                    "device": part.device,
                    "segments": part.segments.iter().map(|p| json!({
                        "effective": plan_effective(p), "clamped": p.clamped()
                    })).collect::<Vec<_>>()
                })
            })
            .collect();
        let run = Arc::new(ScoreRun {
            cancelled: Mutex::new(false),
            wake: Condvar::new(),
            remaining: Mutex::new(plans.len()),
        });
        *self.score.lock().unwrap() = Some(run.clone());
        self.lock_state().mark_output_started();
        for part in plans {
            let provider: Arc<dyn DeviceProvider> = match devices
                .iter()
                .find(|d| d.id == part.device)
                .unwrap()
                .provider
            {
                ProviderId::Lovense => self.lovense.clone(),
                ProviderId::Buttplug => self.buttplug.clone(),
                ProviderId::ServerApi => self.server_api.clone(),
                ProviderId::Tcode => self.tcode.clone(),
            };
            let run = run.clone();
            let output_lock = self.output_lock.clone();
            let state = self.state.clone();
            let lovense = self.lovense.clone();
            let buttplug = self.buttplug.clone();
            let server_api = self.server_api.clone();
            // One independently waiting stream per device. Only sends are
            // serialized; no stream owns the lock during its duration.
            let spawned = std::thread::Builder::new()
                .name("ache-score".into())
                .spawn(move || {
                    for (index, plan) in part.segments.iter().enumerate() {
                        {
                            let _output = output_lock.lock().unwrap();
                            if *run.cancelled.lock().unwrap() {
                                return;
                            }
                            let previous_zero = if index > 0 {
                                let mut zero = part.segments[index - 1].clone();
                                if let AllowedOutput::Function { strength, .. } = &mut zero {
                                    *strength = 0;
                                }
                                provider.execute(&zero)
                            } else {
                                Ok(())
                            };
                            if previous_zero.and_then(|()| provider.execute(plan)).is_err() {
                                *run.cancelled.lock().unwrap() = true;
                                run.wake.notify_all();
                                state.lock().unwrap().on_session_drop();
                                let _ = lovense.zero();
                                let _ = buttplug.zero();
                                let _ = server_api.zero();
                                return;
                            }
                        }
                        let AllowedOutput::Function { time_sec, .. } = plan else {
                            unreachable!()
                        };
                        let cancelled = run.cancelled.lock().unwrap();
                        let (cancelled, _) = run
                            .wake
                            .wait_timeout_while(
                                cancelled,
                                Duration::from_secs(*time_sec as u64),
                                |cancelled| !*cancelled,
                            )
                            .unwrap();
                        if *cancelled {
                            return;
                        }
                    }
                    let _output = output_lock.lock().unwrap();
                    if *run.cancelled.lock().unwrap() {
                        return;
                    }
                    // Explicit per-device zero on completion, through an already
                    // approved action and selector; never a broadcast part stop.
                    let mut zero = part.segments.last().unwrap().clone();
                    if let AllowedOutput::Function { strength, .. } = &mut zero {
                        *strength = 0;
                    }
                    if provider.execute(&zero).is_err() {
                        *run.cancelled.lock().unwrap() = true;
                        run.wake.notify_all();
                        state.lock().unwrap().on_session_drop();
                        let _ = lovense.zero();
                        let _ = buttplug.zero();
                        let _ = server_api.zero();
                        return;
                    }
                    let mut remaining = run.remaining.lock().unwrap();
                    *remaining -= 1;
                    if *remaining == 0 {
                        state
                            .lock()
                            .unwrap()
                            .zero_and_disarm("score completed; re-arm required");
                    }
                });
            if let Err(error) = spawned {
                self.lock_state().on_session_drop();
                self.send_stop_best_effort();
                return Err(AcheError::new(
                    ErrorCode::RequestInvalid,
                    format!("score worker failed: {error}"),
                ));
            }
        }
        Ok(json!({"gate": {"decision": "allow"}, "parts": verdict, "started": true}))
    }

    // ------------------------------------------------ program engine

    fn cancel_program(&self) {
        if let Some(run) = self.program.lock().unwrap().as_ref() {
            *run.cancelled.lock().unwrap() = true;
            run.wake.notify_all();
        }
    }

    fn program_running(&self) -> bool {
        self.program.lock().unwrap().as_ref().is_some_and(|run| {
            !*run.cancelled.lock().unwrap() && run.report.lock().unwrap().stop_reason.is_none()
        })
    }

    /// Every device any provider reports now; a provider that cannot
    /// enumerate contributes nothing (the session then names a device
    /// that is absent, which refuses by name).
    fn all_devices(&self) -> Vec<Device> {
        [
            self.lovense.as_ref() as &dyn DeviceProvider,
            self.buttplug.as_ref() as &dyn DeviceProvider,
            self.server_api.as_ref() as &dyn DeviceProvider,
            self.tcode.as_ref() as &dyn DeviceProvider,
        ]
        .iter()
        .filter_map(|p| p.discover().ok())
        .flatten()
        .collect()
    }

    /// Load one frozen spec into the library. Loading validates and
    /// hashes; it can cause no output and is safe while disarmed.
    fn program_load(
        &self,
        kind: &str,
        spec: &Value,
        id: Option<&str>,
        function: Option<&str>,
    ) -> Result<Value, AcheError> {
        let refuse = |m: &str| Err(AcheError::new(ErrorCode::RequestInvalid, m.to_string()));
        if (id.is_some() || function.is_some()) && kind != "funscript" {
            return refuse("`id` and `function` apply to kind funscript only");
        }
        let mut library = self.library.lock().unwrap();
        let full = |n: usize| n >= LIBRARY_ENTRIES_MAX;
        let pattern_entry = |library: &mut Library, p: Pattern| -> Result<Value, AcheError> {
            if full(library.patterns.len()) && !library.patterns.contains_key(p.id()) {
                return Err(AcheError::new(
                    ErrorCode::RequestInvalid,
                    "the pattern library is full",
                ));
            }
            let value = json!({"kind": "pattern", "id": p.id(), "hash": p.hash(),
                "function": p.function().as_str(), "total_ms": p.total_ms()});
            library.patterns.insert(p.id().to_string(), p);
            Ok(value)
        };
        match kind {
            "pattern" => pattern_entry(&mut library, Pattern::from_json(spec)?),
            "depth" => pattern_entry(&mut library, crate::provider_tcode::depth_pattern(spec)?),
            "funscript" => {
                let (Some(id), Some(function)) = (id, function) else {
                    return refuse("kind funscript needs `id` and `function`");
                };
                let function = Function::parse(function)?;
                pattern_entry(&mut library, crate::funscript::import(id, function, spec)?)
            }
            "pack" => {
                let pack = crate::funscript::import_pack(spec)?;
                if full(library.packs.len()) && !library.packs.contains_key(&pack.id) {
                    return refuse("the pack library is full");
                }
                let value = json!({"kind": "pack", "id": pack.id, "hash": pack.hash,
                    "total_ms": pack.total_ms,
                    "channels": pack.channels.iter().map(|c| c.function.as_str()).collect::<Vec<_>>()});
                library.packs.insert(pack.id.clone(), pack);
                Ok(value)
            }
            "automation" => {
                let a = Automation::from_json(spec)?;
                if full(library.automations.len()) && !library.automations.contains_key(a.id()) {
                    return refuse("the automation library is full");
                }
                let value = json!({"kind": "automation", "id": a.id(), "hash": a.hash(),
                    "controls": a.control_ids()});
                library.automations.insert(a.id().to_string(), a);
                Ok(value)
            }
            "session" => {
                drop(library);
                let devices = self.all_devices();
                let library_guard = self.library.lock().unwrap();
                let compiled = CompiledSession::compile(spec, &library_guard, &devices)?;
                drop(library_guard);
                let mut library = self.library.lock().unwrap();
                if full(library.sessions.len()) && !library.sessions.contains_key(&compiled.id) {
                    return refuse("the session library is full");
                }
                library.sessions.insert(compiled.id.clone(), spec.clone());
                Ok(
                    json!({"kind": "session", "id": compiled.id, "hash": compiled.hash,
                    "duration_ms": compiled.duration_ms,
                    "lanes": compiled.lanes.iter().map(|l| l.binding.clone()).collect::<Vec<_>>()}),
                )
            }
            other => Err(AcheError::new(
                ErrorCode::RequestInvalid,
                format!(
                    "kind {other:?} must be pattern, funscript, pack, depth, automation, or session"
                ),
            )),
        }
    }

    /// Start one program: a loaded session, or one pattern / pack on one
    /// device (a one-binding session). Requires the armed state; the
    /// program then runs until its bound, a stop, a watchdog trip (the
    /// heartbeat is required, as for scores), or a session drop.
    fn program_run(
        &self,
        session: Option<&str>,
        pattern: Option<&str>,
        pack: Option<&str>,
        device: Option<&str>,
        function: Option<&str>,
    ) -> Result<Value, AcheError> {
        let refuse = |m: String| Err(AcheError::new(ErrorCode::RequestInvalid, m));
        let spec = {
            let library = self.library.lock().unwrap();
            match (session, pattern, pack, device) {
                (Some(id), None, None, None) if function.is_none() => {
                    match library.sessions.get(id) {
                        Some(spec) => spec.clone(),
                        None => return refuse(format!("session {id:?} is not loaded")),
                    }
                }
                (None, Some(id), None, Some(device)) => {
                    let Some(p) = library.patterns.get(id) else {
                        return refuse(format!("pattern {id:?} is not loaded"));
                    };
                    let bound = match function {
                        Some(f) => Function::parse(f)?,
                        None if p.function() == Function::Generic => {
                            return refuse("a Generic pattern needs `function`".into());
                        }
                        None => p.function(),
                    };
                    json!({"format": SESSION_FORMAT, "id": format!("run.{id}"), "duration_ms": p.total_ms(),
                        "bindings": [{"id": "main", "device": device, "function": bound, "source": {"pattern": id}}]})
                }
                (None, None, Some(id), Some(device)) if function.is_none() => {
                    let Some(p) = library.packs.get(id) else {
                        return refuse(format!("pack {id:?} is not loaded"));
                    };
                    json!({"format": SESSION_FORMAT, "id": format!("run.{id}"), "duration_ms": p.total_ms,
                        "bindings": [{"id": "main", "device": device, "source": {"pack": id}}]})
                }
                _ => {
                    return refuse(
                        "give `session`, or `pattern` + `device` (+ `function`), or `pack` + `device`".into(),
                    );
                }
            }
        };
        let devices = self.all_devices();
        let compiled = {
            let library = self.library.lock().unwrap();
            Arc::new(CompiledSession::compile(&spec, &library, &devices)?)
        };
        let _output = self.output_lock.lock().unwrap();
        if self.score_running() || self.program_running() || self.lock_state().output_active() {
            return refuse("stop active output before starting a program".into());
        }
        // The state half of the gate, up front: the same refusal every
        // command would meet, before a thread exists.
        let snapshot = self.snapshot();
        if !snapshot.target_configured {
            return Err(AcheError::new(
                ErrorCode::TargetUnconfigured,
                "no target is configured; output is impossible",
            ));
        }
        if !snapshot.armed {
            return Err(AcheError::new(
                ErrorCode::NotArmed,
                "the master toggle is off; call lovense_arm first (arming is always explicit)",
            ));
        }
        let run = Arc::new(ProgramRun {
            cancelled: Mutex::new(false),
            wake: Condvar::new(),
            events: Mutex::new(Vec::new()),
            session: Arc::clone(&compiled),
            report: Mutex::new(ProgramReport::default()),
        });
        *self.program.lock().unwrap() = Some(Arc::clone(&run));
        self.lock_state().mark_output_started();
        let worker = ProgramWorker {
            run: Arc::clone(&run),
            output_lock: Arc::clone(&self.output_lock),
            state: Arc::clone(&self.state),
            ceiling: self.config.intensity_ceiling,
            lovense: Arc::clone(&self.lovense),
            buttplug: Arc::clone(&self.buttplug),
            server_api: Arc::clone(&self.server_api),
            tcode: Arc::clone(&self.tcode),
        };
        if let Err(error) = std::thread::Builder::new()
            .name("ache-program".into())
            .spawn(move || worker.run())
        {
            run.report
                .lock()
                .unwrap()
                .finish(format!("worker failed: {error}"), Vec::new());
            self.lock_state().on_session_drop();
            self.send_stop_best_effort();
            return refuse(format!("program worker failed: {error}"));
        }
        Ok(json!({
            "started": true,
            "gate": {"decision": "allow"},
            "session": compiled.id,
            "session_hash": compiled.hash,
            "duration_ms": compiled.duration_ms,
            "lanes": compiled.lanes.iter().map(|l| json!({
                "binding": l.binding, "device": compiled.devices[l.device].id,
                "function": l.function.as_str()})).collect::<Vec<_>>(),
            "note": "heartbeat required: any tool call within the watchdog window keeps the \
                program alive; silence trips the watchdog and zeroes every output",
        }))
    }

    /// Stop the program: ache's synchronous stop (every output zeroed,
    /// master latched off), then the run artefact.
    fn program_stop(&self) -> Result<Value, AcheError> {
        let stop_sent = self.zero_output("program stop: output zeroed")?;
        let run = self.program.lock().unwrap().clone();
        Ok(json!({
            "armed": false,
            "output_active": false,
            "stop_sent": stop_sent,
            "program": run.map(|run| run.report.lock().unwrap().to_json(&run.session)),
        }))
    }

    /// Feed one named event to the running program. A stop input or the
    /// stop button is ache's synchronous stop, here and now — it never
    /// waits for a tick and no program can intercept it.
    fn program_event(
        &self,
        kind: &str,
        name: &str,
        value: Option<f64>,
    ) -> Result<Value, AcheError> {
        let run = self.program.lock().unwrap().clone();
        let Some(run) = run.filter(|_| self.program_running()) else {
            return Err(AcheError::new(
                ErrorCode::RequestInvalid,
                "no program is running",
            ));
        };
        let stop = (kind == "external" && value.is_none() && run.session.is_stop_input(name))
            || (kind == "press" && value.is_none() && run.session.is_stop_control(name));
        if stop {
            let stop_sent = self.zero_output("stop input: output zeroed")?;
            return Ok(json!({"stopped": true, "stop_sent": stop_sent, "armed": false}));
        }
        let event = run.session.event(kind, name, value)?;
        let mut events = run.events.lock().unwrap();
        if events.len() >= PROGRAM_EVENTS_QUEUED_MAX {
            return Err(AcheError::new(
                ErrorCode::RequestInvalid,
                "too many events are waiting for the next tick",
            ));
        }
        events.push(event);
        Ok(json!({"queued": true, "delivered_at": "the next 250 ms program tick"}))
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
        let _output = self.output_lock.lock().unwrap();
        let tripped = self
            .lock_state()
            .trip_watchdog_if_expired(self.config.watchdog_secs);
        if tripped {
            eprintln!("ache: watchdog expired with output active; output zeroed, re-arm required");
            self.send_stop_best_effort();
        }
        tripped
    }

    pub fn output_active(&self) -> bool {
        self.lock_state().output_active()
    }

    /// The explicit shutdown path: input ended while output is active.
    /// Zero it and say so on stderr — no skipped cleanup.
    pub fn zero_on_shutdown(&self) -> bool {
        let _output = self.output_lock.lock().unwrap();
        if !self.output_active() {
            return false;
        }
        self.lock_state().zero_and_disarm("shutdown: output zeroed");
        self.send_stop_best_effort();
        true
    }
}

/// The live executor of one program. It walks the pure `SessionRun` on
/// the wall clock (deadline-based, so it does not drift), and for every
/// proposal takes the output lock, re-reads the safety state, passes
/// the proposal through the gate, and only then hands the plan to its
/// provider. A stop takes the same lock, so once a stop returns no
/// program command can follow it.
struct ProgramWorker {
    run: Arc<ProgramRun>,
    output_lock: Arc<Mutex<()>>,
    state: Arc<Mutex<SafetyState>>,
    ceiling: u8,
    lovense: Arc<LovenseProvider>,
    buttplug: Arc<dyn DeviceProvider>,
    server_api: Arc<dyn DeviceProvider>,
    tcode: Arc<TcodeProvider>,
}

enum Halt {
    Stopped,
    Finished(String),
    Denied(AcheError),
    Dropped(AcheError),
}

impl ProgramWorker {
    fn cancelled(&self) -> bool {
        *self.run.cancelled.lock().unwrap()
    }

    fn zero_all(&self) -> bool {
        let failed = [
            self.lovense.zero().is_err(),
            self.buttplug.zero().is_err(),
            self.server_api.zero().is_err(),
            self.tcode.zero().is_err(),
        ];
        failed.iter().any(|f| *f)
    }

    fn execute(&self, gated: &Gated) -> Result<(), AcheError> {
        match gated {
            Gated::Output { provider, plan, .. } => match provider {
                ProviderId::Lovense => self.lovense.send_gate_body(&lovense_wire(plan).0),
                ProviderId::Buttplug => self.buttplug.execute(plan),
                ProviderId::ServerApi => self.server_api.execute(plan),
                ProviderId::Tcode => self.tcode.execute(plan),
            },
            Gated::Axes { frame, .. } => self.tcode.execute_axes(frame),
        }
    }

    fn run(self) {
        let session = Arc::clone(&self.run.session);
        let mut run = SessionRun::new(&session);
        let start = Instant::now();
        let halt = loop {
            // Wait for this quantum's deadline, waking at once on a stop.
            let due = start + Duration::from_millis(run.time_ms());
            {
                let mut cancelled = self.run.cancelled.lock().unwrap();
                while !*cancelled {
                    let now = Instant::now();
                    if now >= due {
                        break;
                    }
                    cancelled = self.run.wake.wait_timeout(cancelled, due - now).unwrap().0;
                }
            }
            let _output = self.output_lock.lock().unwrap();
            if self.cancelled() {
                break Halt::Stopped;
            }
            let t = run.time_ms();
            let events = if t.is_multiple_of(TICK_MS) {
                std::mem::take(&mut *self.run.events.lock().unwrap())
            } else {
                Vec::new()
            };
            let Some(proposals) = run.step(&events) else {
                break Halt::Finished(run.stop_reason.clone().unwrap_or_else(|| "complete".into()));
            };
            let snapshot = self.state.lock().unwrap().snapshot(true, self.ceiling);
            let mut halt = None;
            for proposed in &proposals {
                let view = AxisView {
                    supported: self.tcode.kind().axes(),
                    limits: self.tcode.limits(),
                    last: self.tcode.last(),
                };
                let gated = match gate_proposed(&session, proposed, &snapshot, Some(&view)) {
                    Ok(gated) => gated,
                    Err(error) => {
                        halt = Some(Halt::Denied(error));
                        break;
                    }
                };
                if let Err(error) = self.execute(&gated) {
                    halt = Some(Halt::Dropped(error));
                    break;
                }
                let clamps = match &gated {
                    Gated::Output { plan, .. } => u32::from(plan.clamped()),
                    Gated::Axes { frame, .. } => frame.clamps,
                };
                self.run
                    .report
                    .lock()
                    .unwrap()
                    .record(&artefact_line(&session, t, &gated), clamps);
            }
            self.run.report.lock().unwrap().t_ms = t;
            if let Some(halt) = halt {
                break halt;
            }
        };
        // Whoever stopped us already zeroed every provider under the
        // output lock; every other ending zeroes here, under it.
        let reason = match halt {
            Halt::Stopped => "stopped".to_string(),
            Halt::Finished(reason) => {
                let _output = self.output_lock.lock().unwrap();
                if !self.cancelled() {
                    if self.zero_all() {
                        self.state.lock().unwrap().on_session_drop();
                    } else {
                        self.state
                            .lock()
                            .unwrap()
                            .zero_and_disarm("program completed; re-arm required");
                    }
                }
                reason
            }
            Halt::Denied(error) => {
                let _output = self.output_lock.lock().unwrap();
                self.zero_all();
                self.state
                    .lock()
                    .unwrap()
                    .zero_and_disarm("program halted by the gate; re-arm required");
                format!("gate_denied: {}: {}", error.code.as_str(), error.message)
            }
            Halt::Dropped(error) => {
                let _output = self.output_lock.lock().unwrap();
                self.state.lock().unwrap().on_session_drop();
                self.zero_all();
                format!("session_drop: {}", error.message)
            }
        };
        *self.run.cancelled.lock().unwrap() = true;
        self.run.report.lock().unwrap().finish(reason, run.log());
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
        ProviderId::ServerApi => device
            .id
            .strip_prefix("serverapi:")
            .unwrap_or(&device.id)
            .to_string(),
        ProviderId::Buttplug | ProviderId::Tcode => device.id.clone(),
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

pub(crate) fn tool_list() -> Value {
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
    let score_tool = json!({
        "name": "ache_score_start",
        "description": "Atomically plan every part, then start concurrent per-device streams. Any refused part refuses the whole score. Stop with lovense_stop; heartbeat required. Stop active output before starting.",
        "inputSchema": {
            "type": "object", "additionalProperties": false, "required": ["parts"],
            "properties": {"parts": {"type": "array", "minItems": 1, "maxItems": 32,
                "items": {"type": "object", "additionalProperties": false,
                    "required": ["device", "segments"], "properties": {
                        "device": {"type": "string"},
                        "segments": {"type": "array", "minItems": 1, "maxItems": 128,
                            "items": {"type": "object", "additionalProperties": false,
                                "required": ["action", "level", "duration"], "properties": {
                                    "action": {"type": "string"},
                                    "level": {"type": "integer", "minimum": 0},
                                    "duration": {"type": "integer", "minimum": 1, "maximum": 3600}
                                }}}
                    }}}}
        },
        "annotations": {"title": "Start score", "readOnlyHint": false,
            "destructiveHint": false, "idempotentHint": false, "openWorldHint": true}
    });
    let program_load = json!({
        "name": "ache_program_load",
        "description": "Load one frozen spec into the program library: validated whole, \
            hashed, never executed. kind: pattern (ache.pattern/v1: Square/Linear/Sine \
            blocks or a bounded scripted expression), funscript (a .funscript document, \
            with `id` and `function`), pack (ache.pack/v1: the KoiSync six-axis funscript \
            pack), depth (ache.depth/v1: a depth curve and calibration, compiled to an L0 \
            pattern), automation (ache.automation/v1: closed triggers/actions, bounded \
            jobs and queues, typed clamped variables, a mandatory stop button), or \
            session (ache.session/v1: device/function/source bindings, checked against \
            the devices present now). Any rule violation refuses the whole spec. Causes \
            no output; safe while disarmed.",
        "inputSchema": {"type": "object", "additionalProperties": false, "required": ["kind", "spec"],
            "properties": {
                "kind": {"type": "string", "enum": ["pattern", "funscript", "pack", "depth", "automation", "session"]},
                "spec": {"type": "object", "description": "The spec document."},
                "id": {"type": "string", "description": "funscript only: the pattern id to create."},
                "function": {"type": "string",
                    "enum": Function::ALL.iter().map(|f| f.as_str()).collect::<Vec<_>>(),
                    "description": "funscript only: the function the pattern drives."}
            }},
        "annotations": {"title": "Load program spec", "readOnlyHint": false,
            "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
    });
    let program_run = json!({
        "name": "ache_program_run",
        "description": "Run one program: a loaded `session`, or a loaded `pattern` on one \
            `device` (+ `function` for a Generic pattern), or a loaded `pack` on one TCode \
            `device`. Requires the armed state. Every command the program proposes passes \
            the decide() gate at emission time (ceiling clamp, per-axis windows and slew); \
            feature lanes evaluate every 250 ms, TCode axes stream at 50 Hz. Heartbeat \
            required: tool-call silence past the watchdog window zeroes everything. One \
            program at a time; stop active output first. Stop with ache_program_stop or \
            lovense_stop.",
        "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
            "session": {"type": "string"}, "pattern": {"type": "string"}, "pack": {"type": "string"},
            "device": {"type": "string", "description": "Unified device id, e.g. \"buttplug:0\" or \"tcode:0\"."},
            "function": {"type": "string", "enum": Function::ALL.iter().map(|f| f.as_str()).collect::<Vec<_>>()}
        }},
        "annotations": {"title": "Run program", "readOnlyHint": false,
            "destructiveHint": false, "idempotentHint": false, "openWorldHint": true}
    });
    let program_stop = json!({
        "name": "ache_program_stop",
        "description": "Stop the running program with ache's synchronous stop: every output \
            on every provider zeroed (TCode devices get DSTOP), the master latched off, then \
            the run artefact (commands, clamps, stop reason, stream hash). Always available.",
        "inputSchema": no_arguments.clone(),
        "annotations": {"title": "Stop program", "readOnlyHint": false,
            "destructiveHint": false, "idempotentHint": true, "openWorldHint": true}
    });
    let program_event = json!({
        "name": "ache_program_event",
        "description": "Send one named event to the running program: a control (kind \
            press/release/toggle, or slider with `value`, clamped to the variable's range) \
            or an external input (kind external) the session pre-registered, or a device \
            presence report (toy_connected / toy_disconnected, by kind name) — a name and \
            no value; intensities never arrive from outside. A session stop input or the \
            automation's stop button is ache's synchronous stop, applied at once. Other \
            events are delivered at the next 250 ms tick.",
        "inputSchema": {"type": "object", "additionalProperties": false, "required": ["kind", "name"],
            "properties": {
                "kind": {"type": "string", "enum": ["press", "release", "toggle", "slider", "external", "toy_connected", "toy_disconnected"]},
                "name": {"type": "string"},
                "value": {"type": "number", "description": "slider only."}
            }},
        "annotations": {"title": "Program event", "readOnlyHint": false,
            "destructiveHint": false, "idempotentHint": false, "openWorldHint": true}
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
            "description": "Panic zero: stop every function on every device on every \
                provider, synchronously, and latch the master off so recovery is a \
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
            "description": "Discover every device across every provider (Lovense \
                Standard API, embedded Buttplug, and the opt-in Server API), in one unified model: id, \
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
        score_tool,
        {
            "name": "ache_pairing",
            "description": "Server API pairing surface (D-1451). action \"status\": report \
                pairing state — whether the developer token and the paired user id are \
                present in the password store (presence only, never a value). action \
                \"request_qr\": ask Lovense for a QR code for the Owner to scan in the \
                Remote app — an explicit act that pairs nothing by itself and can cause \
                no device output. Pairing completes in the app; the resulting user id \
                is placed in the password store as the final step. A disabled Server \
                API provider refuses with provider_unavailable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "request_qr"],
                        "description": "The pairing action."
                    }
                },
                "required": ["action"],
                "additionalProperties": false,
            },
            "annotations": {
                "title": "Server API pairing",
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": true,
            },
        },
        program_load,
        program_run,
        program_stop,
        program_event,
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

/// Whether `name` is one of the sixteen served tools. The era guard
/// asks before forwarding a `tools/call`; dispatch trusts it.
pub(crate) fn is_known_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}

pub(crate) fn clip(text: &str) -> &str {
    let mut cut = text.len().min(ARGUMENT_NAME_BYTES_MAX);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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

    /// Call a tool through the dispatch entry the rmcp service
    /// uses, wrapped in the response envelope the assertions were
    /// written against. (Envelope-level behavior — framing, era
    /// refusal, unknown tools — is gated in `guard.rs` and the stdio
    /// integration tests, where it now lives.)
    fn call(server: &Server, name: &str, arguments: Value) -> Value {
        match server.dispatch_call(name, Some(arguments)) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": 7, "result": result}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": 7, "error": {"code": code, "message": message}})
            }
        }
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
    fn tools_list_is_exactly_the_served_tools_with_full_schemas() {
        let listed = tool_list();
        let tools = listed.as_array().unwrap();
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
        assert_eq!(status["server"]["version"], "1.4.0");
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

    #[test]
    fn watchdog_fires_during_silence_without_a_next_request() {
        // The production watchdog is a timer task polling
        // `trip_watchdog` (service.rs); this test drives that same
        // entry point the way the poller does and asserts the trip
        // lands — output zeroed, master latched off — with no
        // further request involved.
        let config = Config {
            watchdog_secs: 1,
            ..Config::default()
        };
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(config, Some(Box::new(mock)));
        arm(&server);
        payload(&call(&server, "lovense_function", function_args(6)));
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0]["action"], "Vibrate:6");
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !server.trip_watchdog() {
            assert!(
                std::time::Instant::now() < deadline,
                "the watchdog must trip within its window plus slack"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 2);
            assert_eq!(sent[1]["action"], "Stop", "the trip sends the wire zero");
        }
        let status = status_of(&server);
        assert_eq!(status["watchdog_trips"], 1);
        assert_eq!(status["output_active"], false);
        assert_eq!(status["armed"], false);
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
        // `zero_on_shutdown` is the exact call the process runs when
        // stdin reaches EOF (main.rs); the EOF-at-the-binary level is
        // gated in the stdio integration tests.
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let server = Server::new(Config::default(), Some(Box::new(mock)));
        arm(&server);
        payload(&call(&server, "lovense_function", function_args(7)));
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert!(server.zero_on_shutdown(), "active output is zeroed");
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

    // --- step 4: the Server API provider (D-1451) --------------------

    use crate::provider_server_api::FakeServerApi;

    /// The wire bodies the fixture provider recorded, in order.
    fn bodies(fixture: &FakeServerApi) -> Vec<Value> {
        fixture.sent_log().lock().unwrap().clone()
    }

    /// Poll until the fixture records a body (score streams start
    /// on their own threads), bounded so a broken stream fails the
    /// test instead of hanging it.
    fn wait_for_bodies(fixture: &FakeServerApi) {
        for _ in 0..400 {
            if !bodies(fixture).is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the fixture recorded no body within the bound");
    }

    fn server_with_fixture_server_api() -> (Server, FakeServerApi) {
        let fixture = FakeServerApi::new();
        let server = Server::new(Config::default(), Some(Box::new(MockTransport::new())))
            .with_server_api(Box::new(fixture.clone()))
            .with_pairing(Box::new(fixture.clone()));
        (server, fixture)
    }

    #[test]
    fn serverapi_disabled_by_default_reports_typed_and_blocks_nothing_else() {
        let server = server_for(Config::default());
        let status = status_of(&server);
        assert_eq!(status["providers"]["serverapi"]["available"], false);
        assert_eq!(status["providers"]["serverapi"]["state"], "disabled");
        // Enumeration still succeeds through the other providers,
        // with the disabled provider's typed error in its own entry.
        let (devices, is_error) = payload(&call(&server, "ache_devices", json!({})));
        assert!(!is_error);
        assert_eq!(
            devices["providers"]["serverapi"]["error"]["code"],
            "provider_unavailable"
        );
        // Pairing refuses cleanly while disabled.
        let (body, is_error) = payload(&call(
            &server,
            "ache_pairing",
            json!({"action": "request_qr"}),
        ));
        assert!(is_error);
        assert_eq!(body["error"]["code"], "provider_unavailable");
    }

    #[test]
    fn serverapi_output_routes_through_the_gate_and_stop_zeroes_it() {
        let (server, fixture) = server_with_fixture_server_api();
        arm(&server);
        let (body, is_error) = payload(&call(
            &server,
            "lovense_function",
            json!({"feature": "Vibrate", "strength": 99, "time_sec": 5,
                    "device": "serverapi:fixture-lush"}),
        ));
        assert!(!is_error, "{body}");
        assert_eq!(body["provider"], "serverapi");
        assert_eq!(body["effective"]["strength"], 20);
        assert_eq!(body["gate"]["clamped"], true);
        {
            let bodies = bodies(&fixture);
            assert_eq!(bodies.len(), 1);
            assert_eq!(bodies[0]["command"], "Function");
            assert_eq!(bodies[0]["action"], "Vibrate:20");
            assert_eq!(bodies[0]["toy"], "fixture-lush");
            assert_eq!(bodies[0]["token"], "fixture-token");
            assert_eq!(bodies[0]["uid"], "fixture-uid");
        }
        let (_, is_error) = payload(&call(&server, "lovense_stop", json!({})));
        assert!(!is_error);
        let bodies = bodies(&fixture);
        assert_eq!(bodies.last().unwrap()["action"], "Stop");
    }

    #[test]
    fn serverapi_pairing_status_and_qr_come_from_the_fixture() {
        let (server, _fixture) = server_with_fixture_server_api();
        let (state, is_error) =
            payload(&call(&server, "ache_pairing", json!({"action": "status"})));
        assert!(!is_error);
        assert_eq!(state["provider"], "serverapi");
        assert_eq!(state["token_present"], true);
        assert_eq!(state["uid_present"], true);
        assert_eq!(state["paired"], true);
        assert_eq!(state["qr_issued"], false);
        let (issued, is_error) = payload(&call(
            &server,
            "ache_pairing",
            json!({"action": "request_qr"}),
        ));
        assert!(!is_error, "{issued}");
        assert_eq!(issued["qr"]["code"], "000000");
        let (state, _) = payload(&call(&server, "ache_pairing", json!({"action": "status"})));
        assert_eq!(state["qr_issued"], true);
    }

    #[test]
    fn adversarial_serverapi_output_while_disarmed_sends_nothing() {
        let (server, fixture) = server_with_fixture_server_api();
        let (body, is_error) = payload(&call(
            &server,
            "lovense_function",
            json!({"feature": "Vibrate", "strength": 5, "time_sec": 5,
                    "device": "serverapi:fixture-lush"}),
        ));
        assert!(is_error);
        assert_eq!(body["error"]["code"], "not_armed");
        assert!(bodies(&fixture).is_empty());
    }

    #[test]
    fn adversarial_serverapi_send_failure_drops_session_and_still_zeroes() {
        let (server, fixture) = server_with_fixture_server_api();
        arm(&server);
        fixture
            .fail_flag()
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (body, is_error) = payload(&call(
            &server,
            "lovense_function",
            json!({"feature": "Vibrate", "strength": 5, "time_sec": 5,
                    "device": "serverapi:fixture-lush"}),
        ));
        assert!(is_error);
        assert_eq!(body["error"]["code"], "target_unreachable");
        // The failed command still drove the zero onto the wire:
        // the fixture recorded the function body, then the stop.
        let bodies = bodies(&fixture);
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[1]["action"], "Stop");
        assert!(!server.lock_state().armed());
    }

    #[test]
    fn adversarial_serverapi_pairing_refuses_unknown_actions_and_fields() {
        let (server, _fixture) = server_with_fixture_server_api();
        // An unknown action is an operational refusal: there is no
        // "complete pairing" verb to smuggle a uid in through.
        let (body, is_error) = payload(&call(
            &server,
            "ache_pairing",
            json!({"action": "complete"}),
        ));
        assert!(is_error);
        assert_eq!(body["error"]["code"], "request_invalid");
        // Extra or missing fields are protocol misuse.
        for args in [json!({"action": "status", "uid": "attacker"}), json!({})] {
            let response = call(&server, "ache_pairing", args);
            assert_eq!(response["error"]["code"], INVALID_PARAMS, "{response}");
        }
    }

    #[test]
    fn serverapi_score_part_streams_and_stops_with_the_ensemble() {
        let (server, fixture) = server_with_fixture_server_api();
        arm(&server);
        let (_, is_error) = payload(&call(
            &server,
            "ache_score_start",
            json!({"parts": [{"device": "serverapi:fixture-lush",
                "segments": [{"action": "Vibrate", "level": 5, "duration": 30}]}]}),
        ));
        assert!(!is_error);
        wait_for_bodies(&fixture);
        assert_eq!(bodies(&fixture)[0]["command"], "Function");
        assert_eq!(bodies(&fixture)[0]["action"], "Vibrate:5");
        let (_, is_error) = payload(&call(&server, "lovense_stop", json!({})));
        assert!(!is_error);
        let bodies = bodies(&fixture);
        assert_eq!(bodies.last().unwrap()["action"], "Stop");
    }
}

// ---------------------------------------------------------------------
// AC-2 — "Stop is immediate and never gated" (plan ache.md, AC-2).
//
// The battery below closes the state enumeration for the stop
// guarantee. The pre-existing tests already proved several states
// (gate-level allow in decide.rs, idempotent stop, mid-pattern stop
// latency, watchdog expiry, session drop, shutdown, cross-provider
// zero); this module adds the states that battery did not name:
//
//   * a single stop matrix over every idle/active session state,
//     each with the same oracle: the wire zero is present BEFORE the
//     stop call answers (synchronous), the call lands inside the
//     estate's 1 s CI-safe immediacy bound, the master latches off,
//     a second stop is equally available, and no output can follow
//     without a fresh explicit arm;
//   * heartbeat HEALTHY (inside the watchdog window): output is
//     not zeroed early, and stop is still the next wire command;
//   * heartbeat LOST, both ways it can be observed: the watchdog
//     timer path zeroes with no further request at all, and a stop
//     that is itself the first call after the loss is not swallowed
//     by the watchdog trip that dispatch performs first;
//   * stop after a session drop (degraded session): degradation
//     does not gate stop;
//   * stop with no target configured at all: a local zero, reported
//     honestly as `stop_sent: false`, never a refusal;
//   * the gate-level matrix completed to all four armed x target
//     combinations (the pre-existing test covered three);
//   * unified (Buttplug fixture) sessions: stop from an active
//     function and from an active pattern, with pattern cancel.
//
// Everything here is silent and hardware-free: the Lovense side is
// the mock transport's recorded wire log and the Buttplug side is
// the fixture provider. No real device is contacted, nothing is
// actuated, and no sound is produced. The stop/gate semantics under
// test are NOT changed by this module — it only observes them.
#[cfg(test)]
mod ac2_stop_tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use super::Server;
    use crate::config::{Config, TransportKind};
    use crate::decide::{Decision, GateSnapshot, OutputIntent, decide};
    use crate::provider::FakeButtplug;
    use crate::transport::{MockTransport, from_config};

    /// The immediacy bound the pre-existing mid-pattern stop test
    /// established (1 s, CI-safe). AC-2 holds every state to it.
    const STOP_BOUND: Duration = Duration::from_secs(1);

    fn call(server: &Server, name: &str, arguments: Value) -> Value {
        match server.dispatch_call(name, Some(arguments)) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": 7, "result": result}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": 7, "error": {"code": code, "message": message}})
            }
        }
    }

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
        json!({"feature": "Vibrate", "strength": strength, "time_sec": 30})
    }

    fn mock_server() -> (Server, Arc<Mutex<Vec<Value>>>) {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        (Server::new(Config::default(), Some(Box::new(mock))), sent)
    }

    /// The AC-2 oracle, applied identically from every state: stop
    /// answers successfully inside the bound, the wire zero is
    /// already recorded when it answers (synchronous — not queued,
    /// not deferred), the state is zeroed and latched off, a repeat
    /// stop is just as available, and output afterwards is denied
    /// `not_armed` and sends nothing.
    fn assert_stop_zeroes(
        server: &Server,
        sent: &Arc<Mutex<Vec<Value>>>,
        wire_before: usize,
        state: &str,
    ) {
        assert_eq!(
            sent.lock().unwrap().len(),
            wire_before,
            "{state}: wire count before stop"
        );
        let started = Instant::now();
        let (stopped, is_error) = payload(&call(server, "lovense_stop", json!({})));
        let elapsed = started.elapsed();
        assert!(!is_error, "{state}: stop must never be refused");
        assert_eq!(stopped["stop_sent"], true, "{state}: a zero was sent");
        assert_eq!(stopped["armed"], false, "{state}: stop result latches off");
        assert_eq!(stopped["output_active"], false, "{state}");
        assert!(
            elapsed < STOP_BOUND,
            "{state}: stop immediacy bound exceeded: {elapsed:?}"
        );
        {
            let sent = sent.lock().unwrap();
            assert_eq!(
                sent.len(),
                wire_before + 1,
                "{state}: the zero is on the wire before stop answers"
            );
            assert_eq!(sent[wire_before]["command"], "Function", "{state}");
            assert_eq!(sent[wire_before]["action"], "Stop", "{state}");
            assert_eq!(sent[wire_before]["timeSec"], 0, "{state}");
        }
        let status = status_of(server);
        assert_eq!(
            status["armed"], false,
            "{state}: stop latches the master off"
        );
        assert_eq!(status["output_active"], false, "{state}");

        // Idempotent from the zeroed state too, on the same terms.
        let (stopped, is_error) = payload(&call(server, "lovense_stop", json!({})));
        assert!(!is_error, "{state}: a repeat stop must never be refused");
        assert_eq!(stopped["stop_sent"], true, "{state}");
        assert_eq!(sent.lock().unwrap().len(), wire_before + 2, "{state}");

        // Nothing resumes and nothing is queued behind the stop:
        // output without a fresh explicit arm is denied and silent.
        let (error, is_error) = payload(&call(server, "lovense_function", function_args(5)));
        assert!(is_error, "{state}");
        assert_eq!(error["error"]["code"], "not_armed", "{state}");
        assert_eq!(
            sent.lock().unwrap().len(),
            wire_before + 2,
            "{state}: a denied call sends nothing"
        );
    }

    // --- the state matrix -------------------------------------------

    #[test]
    fn ac2_stop_matrix_from_every_idle_and_active_state() {
        // Idle, never armed.
        let (server, sent) = mock_server();
        assert_stop_zeroes(&server, &sent, 0, "idle, never armed");

        // Idle, armed, no output started.
        let (server, sent) = mock_server();
        arm(&server);
        assert_stop_zeroes(&server, &sent, 0, "idle, armed");

        // Active function, heartbeat healthy (just started).
        let (server, sent) = mock_server();
        arm(&server);
        payload(&call(&server, "lovense_function", function_args(9)));
        assert_eq!(status_of(&server)["output_active"], true);
        assert_stop_zeroes(&server, &sent, 1, "active function");

        // Active preset.
        let (server, sent) = mock_server();
        arm(&server);
        payload(&call(
            &server,
            "lovense_preset",
            json!({"name": "pulse", "time_sec": 30}),
        ));
        assert_eq!(status_of(&server)["output_active"], true);
        assert_stop_zeroes(&server, &sent, 1, "active preset");

        // Mid-pattern: a 30 s pattern whose output is still active
        // when stop arrives.
        let (server, sent) = mock_server();
        arm(&server);
        payload(&call(
            &server,
            "lovense_pattern",
            json!({"features": "v", "interval_ms": 400, "strengths": [10, 10], "time_sec": 30}),
        ));
        assert_eq!(status_of(&server)["output_active"], true);
        assert_stop_zeroes(&server, &sent, 1, "mid-pattern");

        // Already disarmed (disarm sent its own zero): an explicit
        // stop afterwards is still available and still sends.
        let (server, sent) = mock_server();
        arm(&server);
        payload(&call(&server, "lovense_function", function_args(9)));
        payload(&call(&server, "lovense_disarm", json!({})));
        assert_stop_zeroes(&server, &sent, 2, "after disarm");
    }

    #[test]
    fn ac2_gate_allows_stop_in_all_four_state_combinations() {
        // decide.rs covered three of the four armed x target
        // combinations; the armed-without-a-target corner completes
        // the gate matrix, at both ceiling edges.
        for armed in [false, true] {
            for target_configured in [false, true] {
                for ceiling in [0, 20] {
                    let snapshot = GateSnapshot {
                        armed,
                        target_configured,
                        ceiling,
                    };
                    let Decision::Allow { body, .. } = decide(&snapshot, &OutputIntent::Stop)
                    else {
                        panic!(
                            "stop denied for armed={armed} target={target_configured} \
                             ceiling={ceiling}"
                        );
                    };
                    assert_eq!(body["action"], "Stop");
                    assert_eq!(body["timeSec"], 0);
                }
            }
        }
    }

    // --- heartbeat states ---------------------------------------------

    #[test]
    fn ac2_heartbeat_healthy_keeps_output_and_stop_is_next() {
        let (server, sent) = mock_server();
        arm(&server);
        payload(&call(&server, "lovense_function", function_args(8)));
        // Inside the 5 s window: no trip, output survives.
        server.lock_state().backdate_activity(4);
        let status = status_of(&server);
        assert_eq!(status["watchdog_trips"], 0);
        assert_eq!(
            status["output_active"], true,
            "a healthy heartbeat must not zero early"
        );
        assert_eq!(sent.lock().unwrap().len(), 1, "no zero was sent early");
        // A fresh sign of life, then stop on the usual terms.
        server.lock_state().note_activity();
        assert_stop_zeroes(&server, &sent, 1, "heartbeat healthy");
    }

    #[test]
    fn ac2_heartbeat_loss_zeroes_with_no_further_call_then_stop_still_works() {
        // The timer path (service.rs polls `trip_watchdog`): silence
        // alone stops the device — heartbeat loss MUST stop it, with
        // no tool call involved.
        let (server, sent) = mock_server();
        arm(&server);
        payload(&call(&server, "lovense_function", function_args(8)));
        server.lock_state().backdate_activity(6);
        assert!(
            server.trip_watchdog(),
            "the watchdog must trip on heartbeat loss"
        );
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 2);
            assert_eq!(
                sent[1]["action"], "Stop",
                "heartbeat loss sends the wire zero"
            );
        }
        let status = status_of(&server);
        assert_eq!(status["watchdog_trips"], 1);
        assert_eq!(status["output_active"], false);
        assert_eq!(status["armed"], false);
        // And after the trip, an explicit stop is still available.
        assert_stop_zeroes(&server, &sent, 2, "after watchdog trip");
        assert_eq!(
            status_of(&server)["watchdog_trips"],
            1,
            "stop does not re-trip"
        );
    }

    #[test]
    fn ac2_stop_as_first_call_after_heartbeat_loss_is_not_swallowed() {
        // Dispatch trips an expired watchdog BEFORE routing any
        // call. When that call is itself lovense_stop, the trip must
        // not consume or replace it: the trip's best-effort zero and
        // the stop's own synchronous zero both land, in that order,
        // and stop answers as a success.
        let (server, sent) = mock_server();
        arm(&server);
        payload(&call(&server, "lovense_function", function_args(8)));
        server.lock_state().backdate_activity(6);
        let started = Instant::now();
        let (stopped, is_error) = payload(&call(&server, "lovense_stop", json!({})));
        assert!(started.elapsed() < STOP_BOUND);
        assert!(!is_error, "stop after heartbeat loss must not be refused");
        assert_eq!(stopped["stop_sent"], true);
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 3, "function, watchdog zero, stop zero");
            assert_eq!(sent[1]["action"], "Stop");
            assert_eq!(sent[2]["action"], "Stop");
        }
        let status = status_of(&server);
        assert_eq!(status["watchdog_trips"], 1);
        assert_eq!(status["armed"], false);
        assert_eq!(status["output_active"], false);
    }

    // --- degraded and unconfigured states ------------------------------

    #[test]
    fn ac2_stop_after_session_drop_is_not_gated_by_degradation() {
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
        // The transport recovers; the session stays degraded. A
        // degraded session is exactly when a caller reaches for
        // stop — degradation must not gate it.
        fail.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(status_of(&server)["session_health"], "degraded");
        assert_stop_zeroes(&server, &sent, 2, "after session drop (degraded)");
        assert_eq!(
            status_of(&server)["session_health"],
            "degraded",
            "stop works in a degraded session; it does not silently heal it"
        );
    }

    #[test]
    fn ac2_stop_with_no_target_is_a_local_zero_not_a_refusal() {
        // Real transport, no host: nothing can reach any wire. Arm
        // is refused (no target), but stop is not gated on a target
        // existing — it zeroes locally, reports honestly that no
        // zero could be sent, and succeeds.
        let config = Config {
            transport: TransportKind::Real,
            host: String::new(),
            ..Config::default()
        };
        let transport = from_config(&config);
        assert!(transport.is_none(), "no host means no transport");
        let server = Server::new(config, transport);
        let (error, is_error) = payload(&call(&server, "lovense_arm", json!({})));
        assert!(is_error);
        assert_eq!(error["error"]["code"], "target_unconfigured");
        let started = Instant::now();
        let (stopped, is_error) = payload(&call(&server, "lovense_stop", json!({})));
        assert!(started.elapsed() < STOP_BOUND);
        assert!(!is_error, "stop with no target must not be refused");
        assert_eq!(
            stopped["stop_sent"], false,
            "no target: nothing could be sent"
        );
        assert_eq!(stopped["armed"], false);
        assert_eq!(stopped["output_active"], false);
        let status = status_of(&server);
        assert_eq!(status["armed"], false);
        assert_eq!(status["output_active"], false);
    }

    // --- unified provider states ----------------------------------------

    #[test]
    fn ac2_stop_zeroes_a_unified_buttplug_function_session() {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let fake = FakeButtplug::new();
        let zeros = fake.zero_log();
        let server =
            Server::new(Config::default(), Some(Box::new(mock))).with_buttplug(Box::new(fake));
        arm(&server);
        let args =
            json!({"feature": "Vibrate", "strength": 8, "time_sec": 30, "device": "buttplug:0"});
        let (_, is_error) = payload(&call(&server, "lovense_function", args));
        assert!(!is_error);
        assert_eq!(*zeros.lock().unwrap(), 0, "no zero before stop");
        // The Lovense half of the oracle still applies (stop zeroes
        // BOTH providers synchronously); the Buttplug half is the
        // zero count.
        assert_stop_zeroes(&server, &sent, 0, "unified buttplug function");
        assert!(
            *zeros.lock().unwrap() >= 1,
            "the buttplug provider was zeroed by stop"
        );
    }

    #[test]
    fn ac2_stop_cancels_a_unified_buttplug_pattern() {
        let fake = FakeButtplug::new();
        let zeros = fake.zero_log();
        let cancelled = fake.pattern_cancelled_flag();
        let server = Server::new(Config::default(), Some(Box::new(MockTransport::new())))
            .with_buttplug(Box::new(fake));
        arm(&server);
        let args = json!({
            "features": "v", "interval_ms": 400, "strengths": [10, 10],
            "time_sec": 30, "device": "buttplug:0"
        });
        let (_, is_error) = payload(&call(&server, "lovense_pattern", args));
        assert!(!is_error);
        assert!(!*cancelled.lock().unwrap(), "the pattern is still running");
        let (stopped, is_error) = payload(&call(&server, "lovense_stop", json!({})));
        assert!(!is_error);
        assert_eq!(stopped["stop_sent"], true);
        assert!(*zeros.lock().unwrap() >= 1, "the provider was zeroed");
        assert!(
            *cancelled.lock().unwrap(),
            "stop cancels the running pattern, it does not let it play out"
        );
        assert_eq!(status_of(&server)["output_active"], false);
        assert_eq!(status_of(&server)["armed"], false);
    }
}

#[cfg(test)]
mod score_tests {
    use super::*;
    use crate::decide::{GateSnapshot, ScoreSegment, TargetCaps};
    use crate::provider::{Capability, ProviderProbe, bind_capabilities};
    use crate::transport::MockTransport;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    #[derive(Clone)]
    struct Quartet {
        log: Arc<Mutex<Vec<AllowedOutput>>>,
        fail: Arc<AtomicBool>,
    }
    impl Quartet {
        fn new() -> Self {
            Self {
                log: Arc::default(),
                fail: Arc::default(),
            }
        }
    }
    fn devices() -> Vec<Device> {
        [
            ("Lush 4", "Vibrate"),
            ("Domi 2", "Vibrate"),
            ("Flexer", "Rotate"),
            ("Nora", "Rotate"),
        ]
        .iter()
        .map(|(name, action)| {
            let mut capabilities = vec![Capability {
                feature: action.to_string(),
                max: Some(20),
                native: action.to_string(),
                steps: None,
            }];
            bind_capabilities(&format!("Lovense {name}"), &mut capabilities);
            Device {
                id: format!("buttplug:{name}"),
                provider: ProviderId::Buttplug,
                name: name.to_string(),
                kind: Some(format!("Lovense {name}")),
                battery: None,
                capabilities,
                capability_source: "fixture",
            }
        })
        .collect()
    }
    impl DeviceProvider for Quartet {
        fn probe(&self) -> ProviderProbe {
            ProviderProbe {
                provider: ProviderId::Buttplug,
                state: "mock",
                available: true,
                note: "quartet fixture".into(),
            }
        }
        fn discover(&self) -> Result<Vec<Device>, AcheError> {
            Ok(devices())
        }
        fn execute(&self, plan: &AllowedOutput) -> Result<(), AcheError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(AcheError::new(
                    ErrorCode::TargetUnreachable,
                    "fixture disconnected",
                ));
            }
            self.log.lock().unwrap().push(plan.clone());
            Ok(())
        }
        fn zero(&self) -> Result<bool, AcheError> {
            let mut log = self.log.lock().unwrap();
            for device in devices() {
                log.push(AllowedOutput::Function {
                    feature: "Vibrate".into(),
                    strength: 0,
                    time_sec: 1,
                    toy: device.id,
                    clamped: false,
                });
            }
            Ok(true)
        }
    }
    fn parts(duration: i64) -> Vec<ScorePart> {
        devices()
            .iter()
            .zip(["Vibrate", "Vibrate", "Fingering", "Rotate"])
            .enumerate()
            .map(|(i, (device, action))| ScorePart {
                device: device.id.clone(),
                segments: vec![ScoreSegment {
                    action: action.into(),
                    level: (i + 1) as i64 * 4,
                    duration,
                }],
            })
            .collect()
    }
    fn targets() -> Vec<(String, String, TargetCaps)> {
        devices()
            .into_iter()
            .map(|d| (d.id.clone(), d.id.clone(), d.caps()))
            .collect()
    }
    fn snapshot(armed: bool, ceiling: u8) -> GateSnapshot {
        GateSnapshot {
            armed,
            ceiling,
            target_configured: true,
        }
    }
    fn setup() -> (Server, Quartet, Arc<Mutex<Vec<Value>>>) {
        let mock = MockTransport::new();
        let sent = mock.sent_log();
        let fixture = Quartet::new();
        let server = Server::new(Config::default(), Some(Box::new(mock)))
            .with_buttplug(Box::new(fixture.clone()));
        server.arm().unwrap();
        (server, fixture, sent)
    }
    fn wait_for(predicate: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for score worker"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn nonzero(log: &[AllowedOutput]) -> usize {
        log.iter()
            .filter(|p| matches!(p, AllowedOutput::Function { strength: 1.., .. }))
            .count()
    }
    fn assert_zeroed(fixture: &Quartet) {
        let log = fixture.log.lock().unwrap();
        for d in devices() {
            let last = log
                .iter()
                .rev()
                .find(|p| matches!(p, AllowedOutput::Function {toy, ..} if toy == &d.id))
                .unwrap();
            assert!(
                matches!(last, AllowedOutput::Function { strength: 0, .. }),
                "{} not zeroed",
                d.id
            );
        }
    }
    // Validation (10): the intended behavior and the existing-command boundary.
    #[test]
    fn validation_quartet_plans_differentiated_reported_actions() {
        let plan = decide_score(&snapshot(true, 20), &parts(2), &targets()).unwrap();
        assert_eq!(plan.len(), 4);
        for (part, expected) in plan.iter().zip([4, 8, 12, 16]) {
            assert!(
                matches!(part.segments[0], AllowedOutput::Function {strength, ..} if strength == expected)
            );
        }
        assert!(
            matches!(&plan[2].segments[0], AllowedOutput::Function {feature, ..} if feature == "Fingering")
        );
    }
    #[test]
    fn validation_ceiling_applies_to_every_part() {
        let plan = decide_score(&snapshot(true, 6), &parts(1), &targets()).unwrap();
        for (part, expected) in plan.iter().zip([4, 6, 6, 6]) {
            assert!(
                matches!(part.segments[0], AllowedOutput::Function {strength, ..} if strength == expected)
            );
        }
    }
    #[test]
    fn validation_one_part_matches_single_command_and_wire() {
        let (server, fixture, _) = setup();
        let part = parts(2).remove(0);
        let intent = OutputIntent::Function(FunctionIntent {
            feature: "Vibrate".into(),
            strength: 4,
            time_sec: 2,
            toy: Some(part.device.clone()),
        });
        let expected = decide_plan(&server.snapshot(), &intent, &devices()[0].caps()).unwrap();
        server.start_score(&[part]).unwrap();
        wait_for(|| !fixture.log.lock().unwrap().is_empty());
        assert_eq!(fixture.log.lock().unwrap()[0], expected);
        let lovense_parts = [ScorePart {
            device: "lovense:mock-toy-1".into(),
            segments: vec![ScoreSegment {
                action: "Vibrate".into(),
                level: 4,
                duration: 2,
            }],
        }];
        let plan = decide_score(
            &snapshot(true, 20),
            &lovense_parts,
            &[(
                "lovense:mock-toy-1".into(),
                "mock-toy-1".into(),
                devices()[0].caps(),
            )],
        )
        .unwrap();
        let single = decide_plan(
            &snapshot(true, 20),
            &OutputIntent::Function(FunctionIntent {
                feature: "Vibrate".into(),
                strength: 4,
                time_sec: 2,
                toy: Some("mock-toy-1".into()),
            }),
            &devices()[0].caps(),
        )
        .unwrap();
        assert_eq!(lovense_wire(&plan[0].segments[0]), lovense_wire(&single));
        server.stop().unwrap();
    }
    #[test]
    fn validation_concurrent_streams_interleave_before_any_part_finishes() {
        let (server, fixture, _) = setup();
        let mut score = parts(1);
        for part in &mut score {
            part.segments.push(part.segments[0].clone());
        }
        server.start_score(&score).unwrap();
        wait_for(|| nonzero(&fixture.log.lock().unwrap()) >= 4);
        {
            let log = fixture.log.lock().unwrap();
            let ids: std::collections::HashSet<_> = log[..4]
                .iter()
                .map(|p| match p {
                    AllowedOutput::Function { toy, .. } => toy.clone(),
                    _ => unreachable!(),
                })
                .collect();
            assert_eq!(
                ids.len(),
                4,
                "all first segments precede any second segment"
            );
        }
        wait_for(|| nonzero(&fixture.log.lock().unwrap()) >= 8);
        server.stop().unwrap();
        assert_zeroed(&fixture);
    }
    #[test]
    fn validation_completion_zeroes_and_reports_idle() {
        let (server, fixture, _) = setup();
        server.start_score(&parts(1)).unwrap();
        wait_for(|| !server.output_active());
        assert_zeroed(&fixture);
        assert_eq!(server.status()["score_running"], false);
    }
    #[test]
    fn validation_stop_zeroes_every_device_before_return() {
        let (server, fixture, sent) = setup();
        server.start_score(&parts(30)).unwrap();
        wait_for(|| nonzero(&fixture.log.lock().unwrap()) == 4);
        server.stop().unwrap();
        assert_zeroed(&fixture);
        assert_eq!(sent.lock().unwrap().last().unwrap()["action"], "Stop");
        assert!(!server.lock_state().armed());
        assert!(!server.score_running());
    }
    #[test]
    fn validation_healthy_heartbeat_preserves_ensemble() {
        let (server, fixture, _) = setup();
        server.start_score(&parts(30)).unwrap();
        wait_for(|| nonzero(&fixture.log.lock().unwrap()) == 4);
        server.lock_state().backdate_activity(4);
        assert!(!server.trip_watchdog());
        assert!(server.score_running());
        server.stop().unwrap();
    }
    #[test]
    fn validation_tool_returns_clamped_plan_with_one_new_surface() {
        let (mut server, fixture, _) = setup();
        server.config.intensity_ceiling = 2;
        let result = server.dispatch_call("ache_score_start", Some(json!({"parts": [
            {"device": "buttplug:Flexer", "segments": [{"action": "Fingering", "level": 12, "duration": 1}]}]}))).unwrap();
        assert_eq!(result["isError"], false);
        let body: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["parts"][0]["segments"][0]["effective"]["strength"], 2);
        assert_eq!(body["parts"][0]["segments"][0]["clamped"], true);
        // 1.4.0: the four ache_program_* tools append after the twelve.
        assert_eq!(TOOL_NAMES.len(), 16);
        server.stop().unwrap();
        assert_zeroed(&fixture);
    }
    // Adversarial (10): no partial starts, no stale output after a latch.
    #[test]
    fn adversarial_bad_parts_refuse_all_and_send_nothing_on_both_routes() {
        let (server, fixture, sent) = setup();
        let mut score = parts(1);
        score[2].segments[0].action = "Heating".into();
        score[3].segments[0].action = "Fingering".into();
        let error = server.start_score(&score).unwrap_err();
        let message = format!("{error:?}");
        for name in ["Flexer", "Nora", "Heating", "Fingering"] {
            assert!(message.contains(name), "{message}");
        }
        assert!(fixture.log.lock().unwrap().is_empty());
        assert!(sent.lock().unwrap().is_empty());
        assert!(!server.output_active());
    }
    #[test]
    fn adversarial_disarmed_refuses_every_part_without_send() {
        let (server, fixture, sent) = setup();
        server.lock_state().zero_and_disarm("test");
        let error = server.start_score(&parts(1)).unwrap_err();
        assert!(
            error.to_json().to_string().contains("NotArmed")
                || error.to_json().to_string().contains("not_armed")
        );
        assert!(fixture.log.lock().unwrap().is_empty());
        assert!(sent.lock().unwrap().is_empty());
    }
    #[test]
    fn adversarial_bounds_unknown_devices_and_reduced_capabilities() {
        let mut cases = vec![vec![], parts(0), parts(3601), parts(i64::MAX)];
        let mut unknown = parts(1);
        unknown[0].device = "buttplug:missing".into();
        cases.push(unknown);
        let mut negative = parts(1);
        negative[0].segments[0].level = -1;
        cases.push(negative);
        let mut reduced = parts(1);
        reduced[0].segments[0].action = "Oscillate".into();
        cases.push(reduced);
        let mut empty = parts(1);
        empty[0].segments.clear();
        cases.push(empty);
        for case in cases {
            assert!(decide_score(&snapshot(true, 20), &case, &targets()).is_err());
        }
    }
    #[test]
    fn adversarial_duplicate_and_conflicting_output_refused() {
        let (server, fixture, _) = setup();
        let mut score = parts(1);
        score.push(score[0].clone());
        assert!(server.start_score(&score).is_err());
        assert!(fixture.log.lock().unwrap().is_empty());
        server.start_score(&parts(30)).unwrap();
        assert!(server.start_score(&parts(1)).is_err());
        assert!(
            server
                .run_output(
                    OutputIntent::Function(FunctionIntent {
                        feature: "Vibrate".into(),
                        strength: 2,
                        time_sec: 1,
                        toy: None
                    }),
                    None
                )
                .is_err()
        );
        server.stop().unwrap();
    }
    #[test]
    fn adversarial_watchdog_cancels_whole_ensemble_and_no_resume() {
        let (server, fixture, _) = setup();
        server.start_score(&parts(30)).unwrap();
        wait_for(|| nonzero(&fixture.log.lock().unwrap()) == 4);
        server.lock_state().backdate_activity(6);
        assert!(server.trip_watchdog());
        assert_zeroed(&fixture);
        assert!(!server.score_running());
        assert!(server.start_score(&parts(1)).is_err());
    }
    #[test]
    fn adversarial_disconnect_reconnect_never_resumes_score() {
        let (server, fixture, _) = setup();
        fixture.fail.store(true, Ordering::SeqCst);
        server.start_score(&parts(1)).unwrap();
        wait_for(|| !server.output_active());
        fixture.fail.store(false, Ordering::SeqCst);
        let before = nonzero(&fixture.log.lock().unwrap());
        assert!(server.start_score(&parts(1)).is_err());
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(nonzero(&fixture.log.lock().unwrap()), before);
        assert_zeroed(&fixture);
    }
    #[test]
    fn adversarial_stop_then_rearm_does_not_revive_waiting_streams() {
        let (server, fixture, _) = setup();
        let mut score = parts(1);
        for part in &mut score {
            part.segments.push(part.segments[0].clone());
        }
        server.start_score(&score).unwrap();
        wait_for(|| nonzero(&fixture.log.lock().unwrap()) == 4);
        server.stop().unwrap();
        let before = fixture.log.lock().unwrap().len();
        server.arm().unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(fixture.log.lock().unwrap().len(), before);
        assert_zeroed(&fixture);
    }
    #[test]
    fn adversarial_malformed_score_rejects_unknown_fields_without_send() {
        let (server, fixture, sent) = setup();
        for args in [
            json!({"parts": null}),
            json!({"parts": [], "override": true}),
            json!({"parts": [{"device": "buttplug:Lush 4", "segments": [{"action": "Vibrate", "level": 1, "duration": 1, "override": true}]}]}),
        ] {
            assert!(
                server
                    .dispatch_call("ache_score_start", Some(args))
                    .is_err()
            );
        }
        assert!(fixture.log.lock().unwrap().is_empty());
        assert!(sent.lock().unwrap().is_empty());
    }
    #[test]
    fn validation_mixed_providers_start_and_stop_together() {
        let (server, fixture, sent) = setup();
        let mut score = parts(30);
        score.push(ScorePart {
            device: "lovense:mock-toy-1".into(),
            segments: vec![ScoreSegment {
                action: "Vibrate".into(),
                level: 7,
                duration: 30,
            }],
        });
        server.start_score(&score).unwrap();
        wait_for(|| nonzero(&fixture.log.lock().unwrap()) == 4 && sent.lock().unwrap().len() == 1);
        assert_eq!(sent.lock().unwrap()[0]["toy"], "mock-toy-1");
        server.stop().unwrap();
        assert_zeroed(&fixture);
        assert_eq!(sent.lock().unwrap().last().unwrap()["action"], "Stop");
    }
    #[test]
    fn validation_action_transition_zeroes_previous_action() {
        let (server, fixture, _) = setup();
        let score = [ScorePart {
            device: "buttplug:Flexer".into(),
            segments: vec![
                ScoreSegment {
                    action: "Fingering".into(),
                    level: 9,
                    duration: 1,
                },
                ScoreSegment {
                    action: "Rotate".into(),
                    level: 3,
                    duration: 1,
                },
            ],
        }];
        server.start_score(&score).unwrap();
        wait_for(|| nonzero(&fixture.log.lock().unwrap()) == 2);
        let log = fixture.log.lock().unwrap();
        assert!(
            matches!(&log[1], AllowedOutput::Function {feature, strength: 0, ..} if feature == "Fingering")
        );
        assert!(
            matches!(&log[2], AllowedOutput::Function {feature, strength: 3, ..} if feature == "Rotate")
        );
        drop(log);
        server.stop().unwrap();
    }
    #[test]
    fn adversarial_stop_failure_still_zeroes_other_provider_and_cancels() {
        let mock = MockTransport::new();
        let fail = mock.fail_flag();
        let fixture = Quartet::new();
        let server = Server::new(Config::default(), Some(Box::new(mock)))
            .with_buttplug(Box::new(fixture.clone()));
        server.arm().unwrap();
        server.start_score(&parts(30)).unwrap();
        wait_for(|| nonzero(&fixture.log.lock().unwrap()) == 4);
        fail.store(true, Ordering::SeqCst);
        assert!(server.stop().is_err());
        assert_zeroed(&fixture);
        assert!(!server.score_running());
        assert!(!server.lock_state().armed());
    }
    #[test]
    fn adversarial_shutdown_cancels_all_parts_without_resume() {
        let (server, fixture, _) = setup();
        server.start_score(&parts(30)).unwrap();
        wait_for(|| nonzero(&fixture.log.lock().unwrap()) == 4);
        assert!(server.zero_on_shutdown());
        assert_zeroed(&fixture);
        assert!(!server.score_running());
        assert!(server.start_score(&parts(1)).is_err());
    }
}

// origin_lane: ntranch; content_label: nsfw
// The program engine at the tool surface (1.4.0, KoiSync + XToys
// landings): mock transport, fixture Buttplug, recording TCode link.
// Validation half: load, run, live stream == pure replay. Adversarial
// half: stop supremacy mid-pattern / mid-job / mid-queue, watchdog trip,
// mid-command dropout, gate-bypass attempts.
#[cfg(test)]
mod program_tests {
    use super::*;
    use crate::decide::AxisLimit;
    use crate::provider::FakeButtplug;
    use crate::provider_tcode::{DeviceKind, RecordingLink};
    use crate::transport::from_config;
    use std::sync::atomic::{AtomicBool, Ordering};

    const OPEN: [AxisLimit; 6] = [AxisLimit {
        min: 0,
        max: 100,
        max_step: 100,
    }; 6];

    struct Rig {
        server: Server,
        executed: Arc<Mutex<Vec<AllowedOutput>>>,
        fail: Arc<AtomicBool>,
        link: RecordingLink,
    }

    fn rig_with(config: Config) -> Rig {
        let fixture = FakeButtplug::new();
        let executed = fixture.executed_log();
        let fail = fixture.fail_flag();
        let link = RecordingLink::new();
        let tcode = TcodeProvider::new("mock", DeviceKind::Sr6, OPEN, Box::new(link.clone()));
        let transport = from_config(&config);
        let server = Server::new(config, transport)
            .with_buttplug(Box::new(fixture))
            .with_tcode(tcode);
        Rig {
            server,
            executed,
            fail,
            link,
        }
    }

    fn rig() -> Rig {
        rig_with(Config::default())
    }

    fn call(server: &Server, name: &str, arguments: Value) -> (Value, bool) {
        let result = server
            .dispatch_call(name, Some(arguments))
            .expect("no protocol error");
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            serde_json::from_str(text).unwrap(),
            result["isError"].as_bool().unwrap(),
        )
    }

    fn ok(server: &Server, name: &str, arguments: Value) -> Value {
        let (value, is_error) = call(server, name, arguments);
        assert!(!is_error, "{name} failed: {value}");
        value
    }

    fn wait_for(what: &str, limit_ms: u64, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < Duration::from_millis(limit_ms),
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn program(server: &Server) -> Value {
        ok(server, "lovense_status", json!({}))["program"].clone()
    }

    fn pattern(id: &str, function: &str, ms: u64) -> Value {
        json!({"format": "ache.pattern/v1", "id": id, "function": function,
            "sections": [{"blocks": [
                {"kind": "linear", "bottom": 0, "top": 100, "duration_ms": ms / 2},
                {"kind": "sine", "bottom": 20, "top": 80, "duration_ms": ms / 2, "speed": 2.0}
            ]}], "loop": "once"})
    }

    fn frames(rig: &Rig) -> usize {
        rig.link.frames().len()
    }

    // --- validation ---------------------------------------------------

    #[test]
    fn tcode_is_a_fourth_provider_in_status_and_discovery() {
        let rig = rig();
        let status = ok(&rig.server, "lovense_status", json!({}));
        assert_eq!(status["providers"]["tcode"]["available"], true);
        let devices = ok(&rig.server, "ache_devices", json!({}));
        let ids: Vec<&str> = devices["devices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"tcode:0"), "{ids:?}");
        let device = ok(&rig.server, "ache_device", json!({"device": "tcode:0"}));
        assert_eq!(device["capabilities"].as_array().unwrap().len(), 6);
        // Disabled by default in a bare server.
        let bare = Server::new(Config::default(), from_config(&Config::default()));
        let status = ok(&bare, "lovense_status", json!({}));
        assert_eq!(status["providers"]["tcode"]["state"], "disabled");
    }

    #[test]
    fn every_spec_kind_loads_hashes_and_sends_nothing() {
        let rig = rig();
        let p = ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "pattern", "spec": pattern("p1", "Generic", 1000)}),
        );
        assert_eq!(p["hash"].as_str().unwrap().len(), 64);
        let f = ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "funscript", "id": "fs", "function": "StrokePosition",
            "spec": {"actions": [{"at": 0, "pos": 0}, {"at": 500, "pos": 100}]}}),
        );
        assert_eq!(f["total_ms"], 500);
        let pk = ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "pack", "spec": {"format": "ache.pack/v1", "id": "pk",
            "channels": {"stroke": {"actions": [{"at": 0, "pos": 0}, {"at": 400, "pos": 90}]},
                         "pitch": {"actions": [{"at": 0, "pos": 50}, {"at": 400, "pos": 60}]}}}}),
        );
        assert_eq!(pk["channels"], json!(["StrokePosition", "Pitch"]));
        let d = ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "depth", "spec": {"format": "ache.depth/v1", "id": "dp",
            "calibration": {"model_m": 0.14, "player_m": 0.14, "body_height_m": 1.6, "stroke_m": 0.12, "mode": "soft"},
            "samples": [[0.0, 1.0], [0.05, 1.0], [0.1, 1.0]]}}),
        );
        assert_eq!(d["function"], "StrokePosition");
        let a = ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "automation", "spec": {"format": "ache.automation/v1", "id": "a1",
            "controls": [{"id": "stop", "kind": "stop_button"}]}}),
        );
        assert_eq!(a["controls"], json!(["stop"]));
        let s = ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "session", "spec": {"format": "ache.session/v1", "id": "s1",
            "duration_ms": 1000, "bindings": [{"id": "m", "device": "buttplug:0", "function": "Vibrate", "source": {"pattern": "p1"}}]}}),
        );
        assert_eq!(s["lanes"], json!(["m"]));
        assert!(rig.executed.lock().unwrap().is_empty());
        assert_eq!(frames(&rig), 0);
        assert_eq!(ok(&rig.server, "lovense_status", json!({}))["armed"], false);
    }

    #[test]
    fn live_run_streams_exactly_the_pure_replay_and_ends_disarmed() {
        let rig = rig();
        ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "pattern", "spec": pattern("wave", "Generic", 600)}),
        );
        let session = json!({"format": "ache.session/v1", "id": "live", "duration_ms": 600, "bindings": [
            {"id": "v", "device": "buttplug:1", "function": "Vibrate", "source": {"pattern": "wave"}},
            {"id": "t", "device": "tcode:0", "function": "Surge", "source": {"pattern": "wave"}}]});
        ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "session", "spec": session.clone()}),
        );
        ok(&rig.server, "lovense_arm", json!({}));
        let started = ok(&rig.server, "ache_program_run", json!({"session": "live"}));
        assert_eq!(started["started"], true);
        wait_for("program end", 3_000, || {
            program(&rig.server)["running"] == false
        });
        let report = program(&rig.server);
        // The bound equals the pattern length, so the bound ends it.
        assert_eq!(report["stop_reason"], "session_duration", "{report}");
        // The pure replay of the same compiled session, same gate inputs.
        let compiled = {
            let library = rig.server.library.lock().unwrap();
            CompiledSession::compile(&session, &library, &rig.server.all_devices()).unwrap()
        };
        let replay = crate::session::tests::replay(&compiled, 20, &[], OPEN);
        assert_eq!(report["stream_hash"], json!(replay.hash), "live == replay");
        assert_eq!(report["commands"], json!(replay.lines.len()));
        println!(
            "PROGRAM live==replay hash={} commands={}",
            replay.hash,
            replay.lines.len()
        );
        let status = ok(&rig.server, "lovense_status", json!({}));
        assert_eq!(status["armed"], false, "completion latches the master off");
        assert_eq!(status["output_active"], false);
        assert_eq!(rig.link.frames().last().unwrap(), b"DSTOP\n");
    }

    #[test]
    fn a_pattern_or_pack_runs_on_one_device_without_a_session() {
        let rig = rig();
        ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "pack", "spec": {"format": "ache.pack/v1", "id": "pk",
            "channels": {"stroke": {"actions": [{"at": 0, "pos": 0}, {"at": 200, "pos": 90}]}}}}),
        );
        ok(&rig.server, "lovense_arm", json!({}));
        ok(
            &rig.server,
            "ache_program_run",
            json!({"pack": "pk", "device": "tcode:0"}),
        );
        wait_for("pack end", 3_000, || {
            program(&rig.server)["running"] == false
        });
        assert_eq!(frames(&rig), 11, "10 frames at 20 ms + DSTOP");
        ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "pattern", "spec": pattern("g", "Generic", 500)}),
        );
        ok(&rig.server, "lovense_arm", json!({}));
        let (error, is_error) = call(
            &rig.server,
            "ache_program_run",
            json!({"pattern": "g", "device": "buttplug:0"}),
        );
        assert!(
            is_error
                && error["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("Generic")
        );
        ok(
            &rig.server,
            "ache_program_run",
            json!({"pattern": "g", "device": "buttplug:0", "function": "Vibrate"}),
        );
        wait_for("pattern end", 3_000, || {
            program(&rig.server)["running"] == false
        });
        assert!(!rig.executed.lock().unwrap().is_empty());
    }

    // --- adversarial --------------------------------------------------

    #[test]
    fn a6_stop_mid_pattern_is_synchronous_and_final() {
        for stop_tool in ["ache_program_stop", "lovense_stop"] {
            let rig = rig();
            ok(
                &rig.server,
                "ache_program_load",
                json!({"kind": "pattern", "spec": pattern("long", "StrokePosition", 20_000)}),
            );
            ok(&rig.server, "lovense_arm", json!({}));
            ok(
                &rig.server,
                "ache_program_run",
                json!({"pattern": "long", "device": "tcode:0"}),
            );
            wait_for("frames flowing", 2_000, || frames(&rig) > 5);
            let t0 = Instant::now();
            let stopped = ok(&rig.server, stop_tool, json!({}));
            let latency = t0.elapsed();
            assert_eq!(stopped["armed"], false);
            let after = frames(&rig);
            assert_eq!(
                rig.link.frames().last().unwrap(),
                b"DSTOP\n",
                "{stop_tool}: the stop is the last frame"
            );
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(
                frames(&rig),
                after,
                "{stop_tool}: no emission after the stop returned"
            );
            assert_eq!(program(&rig.server)["running"], false);
            println!(
                "PROGRAM a6 stop={stop_tool} latency_us={} frames={after}",
                latency.as_micros()
            );
        }
    }

    fn automation_session(rig: &Rig) {
        ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "automation", "spec": {"format": "ache.automation/v1", "id": "jq",
            "controls": [{"id": "halt", "kind": "stop_button"}, {"id": "go", "kind": "button"}],
            "triggers": [{"id": "s", "on": "session_start"}, {"id": "g", "on": {"control_pressed": "go"}}],
            "jobs": [{"id": "tick", "trigger": "s", "repeat": {"every": {"ms": 250}}, "max_runs": 1000, "max_span_ms": 60000,
                      "actions": [{"set_function": {"target": "m", "value": "20 + (time * 40) % 60"}}]},
                     {"id": "start_q", "trigger": "g", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
                      "actions": [{"start_queue": "q"}]}],
            "queues": [{"id": "q", "loop": {"repeat": 50}, "actions": [
                {"set_function": {"target": "ax", "value": "30"}}, {"delay": {"ms": 250}},
                {"set_function": {"target": "ax", "value": "60"}}, {"delay": {"ms": 250}}]}]}}),
        );
        ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "session", "spec": {"format": "ache.session/v1", "id": "jq",
            "duration_ms": 60000, "automation": "jq", "inputs": [{"id": "voice"}, {"id": "safeword", "stop": true}],
            "bindings": [{"id": "m", "device": "buttplug:0", "function": "Vibrate", "source": "automation"},
                         {"id": "ax", "device": "tcode:0", "function": "Surge", "source": "automation"}]}}),
        );
        ok(&rig.server, "lovense_arm", json!({}));
        ok(&rig.server, "ache_program_run", json!({"session": "jq"}));
        ok(
            &rig.server,
            "ache_program_event",
            json!({"kind": "press", "name": "go"}),
        );
        wait_for("job and queue both emitting", 3_000, || {
            !rig.executed.lock().unwrap().is_empty() && frames(rig) > 3
        });
    }

    #[test]
    fn a6_stop_button_and_stop_input_outrank_a_running_job_and_queue() {
        for (kind, name) in [("press", "halt"), ("external", "safeword")] {
            let rig = rig();
            automation_session(&rig);
            let stopped = ok(
                &rig.server,
                "ache_program_event",
                json!({"kind": kind, "name": name}),
            );
            assert_eq!(stopped["stopped"], true, "{name}");
            let (executed, sent) = (rig.executed.lock().unwrap().len(), frames(&rig));
            assert_eq!(rig.link.frames().last().unwrap(), b"DSTOP\n");
            std::thread::sleep(Duration::from_millis(400));
            assert_eq!(
                rig.executed.lock().unwrap().len(),
                executed,
                "{name}: job emitted after stop"
            );
            assert_eq!(frames(&rig), sent, "{name}: queue emitted after stop");
            assert_eq!(ok(&rig.server, "lovense_status", json!({}))["armed"], false);
        }
    }

    #[test]
    fn a6_watchdog_trip_during_an_automation_run_zeroes_everything() {
        let rig = rig_with(Config {
            watchdog_secs: 1,
            ..Config::default()
        });
        automation_session(&rig);
        // Silence: no tool calls. Drive the production watchdog poll.
        let start = Instant::now();
        while !rig.server.trip_watchdog() {
            assert!(
                start.elapsed() < Duration::from_secs(4),
                "the watchdog must trip"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let (executed, sent) = (rig.executed.lock().unwrap().len(), frames(&rig));
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(rig.executed.lock().unwrap().len(), executed);
        assert_eq!(frames(&rig), sent);
        let status = ok(&rig.server, "lovense_status", json!({}));
        assert_eq!(status["watchdog_trips"], 1);
        assert_eq!(status["armed"], false);
        assert_eq!(status["program"]["running"], false);
    }

    #[test]
    fn mid_command_dropout_drops_the_session_and_never_resumes() {
        // TCode link dropout.
        let rig = rig();
        automation_session(&rig);
        rig.link.set_fail(true);
        wait_for("program halt", 2_000, || {
            program(&rig.server)["running"] == false
        });
        let report = program(&rig.server);
        assert!(
            report["stop_reason"]
                .as_str()
                .unwrap()
                .starts_with("session_drop"),
            "{report}"
        );
        let status = ok(&rig.server, "lovense_status", json!({}));
        assert_eq!(status["session_health"], "degraded");
        assert_eq!(status["armed"], false);
        rig.link.set_fail(false);
        let executed = rig.executed.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            rig.executed.lock().unwrap().len(),
            executed,
            "a reconnect resumes nothing"
        );
        // Buttplug dropout, same law.
        let rig = rig_with(Config::default());
        automation_session(&rig);
        rig.fail.store(true, Ordering::SeqCst);
        wait_for("program halt", 2_000, || {
            program(&rig.server)["running"] == false
        });
        assert!(
            program(&rig.server)["stop_reason"]
                .as_str()
                .unwrap()
                .starts_with("session_drop")
        );
        assert_eq!(
            rig.link.frames().last().unwrap(),
            b"DSTOP\n",
            "every provider is zeroed"
        );
    }

    #[test]
    fn a4_gate_bypass_attempts_are_refused_and_send_nothing() {
        let rig = rig();
        ok(
            &rig.server,
            "ache_program_load",
            json!({"kind": "pattern", "spec": pattern("p", "Vibrate", 1000)}),
        );
        // Disarmed: a run is refused before any thread exists.
        let (error, is_error) = call(
            &rig.server,
            "ache_program_run",
            json!({"pattern": "p", "device": "buttplug:0"}),
        );
        assert!(is_error);
        assert_eq!(error["error"]["code"], "not_armed");
        assert!(rig.executed.lock().unwrap().is_empty());
        // Events with no program, events carrying values, unregistered
        // sources and controls: refused.
        let (_, is_error) = call(
            &rig.server,
            "ache_program_event",
            json!({"kind": "external", "name": "voice"}),
        );
        assert!(is_error, "no program is running");
        automation_session(&rig);
        for args in [
            json!({"kind": "external", "name": "voice", "value": 99}),
            json!({"kind": "external", "name": "webhook"}),
            json!({"kind": "slider", "name": "ghost", "value": 1}),
            json!({"kind": "press", "name": "nope"}),
            json!({"kind": "intensity", "name": "m", "value": 100}),
        ] {
            let (_, is_error) = call(&rig.server, "ache_program_event", args.clone());
            assert!(is_error, "{args}");
        }
        // The direct output tools cannot run beside a program.
        for (tool, args) in [
            (
                "lovense_function",
                json!({"feature": "Vibrate", "strength": 20, "time_sec": 5, "device": "buttplug:0"}),
            ),
            (
                "lovense_pattern",
                json!({"interval_ms": 200, "strengths": [20], "time_sec": 5, "device": "buttplug:0"}),
            ),
        ] {
            let (error, is_error) = call(&rig.server, tool, args);
            assert!(
                is_error
                    && error["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("program"),
                "{tool}"
            );
        }
        let (_, is_error) = call(&rig.server, "ache_program_run", json!({"session": "jq"}));
        assert!(is_error, "one program at a time");
        // A TCode device refuses the direct tools outright.
        ok(&rig.server, "ache_program_stop", json!({}));
        ok(&rig.server, "lovense_arm", json!({}));
        let (error, is_error) = call(
            &rig.server,
            "lovense_function",
            json!({"feature": "Stroke", "strength": 50, "time_sec": 2, "device": "tcode:0"}),
        );
        assert!(is_error, "{error}");
        assert_eq!(
            ok(&rig.server, "lovense_status", json!({}))["session_health"],
            "ok",
            "a refusal is not a drop"
        );
        // Code-shaped and unbounded specs never load.
        for spec in [
            json!({"kind": "automation", "spec": {"format": "ache.automation/v1", "id": "js", "script": "xtoys.callAction()", "controls": [{"id": "s", "kind": "stop_button"}]}}),
            json!({"kind": "automation", "spec": {"format": "ache.automation/v1", "id": "nobound", "controls": [{"id": "s", "kind": "stop_button"}],
                "jobs": [{"id": "j", "repeat": {"while": "1"}, "actions": ["stop_all"], "max_span_ms": 1000}]}}),
            json!({"kind": "pattern", "spec": {"format": "ache.pattern/v1", "id": "x", "function": "Vibrate",
                "scripted": {"base": 0, "expr": "fetch('http://x')", "duration_ms": 1000}, "loop": "once"}}),
            json!({"kind": "session", "spec": {"format": "ache.session/v1", "id": "toolong", "duration_ms": 3_600_001,
                "bindings": [{"id": "m", "device": "buttplug:0", "function": "Vibrate", "source": {"pattern": "p"}}]}}),
            json!({"kind": "webhook", "spec": {}}),
        ] {
            let (_, is_error) = call(&rig.server, "ache_program_load", spec.clone());
            assert!(is_error, "{spec}");
        }
    }

    #[test]
    fn program_tool_arguments_are_shape_checked_before_anything_runs() {
        let rig = rig();
        for (tool, args) in [
            (
                "ache_program_load",
                json!({"kind": "pattern", "spec": "not an object"}),
            ),
            (
                "ache_program_load",
                json!({"kind": "pattern", "spec": {}, "exec": true}),
            ),
            ("ache_program_run", json!({"session": 5})),
            ("ache_program_stop", json!({"now": true})),
            (
                "ache_program_event",
                json!({"kind": "slider", "name": "k", "value": "high"}),
            ),
        ] {
            assert!(
                rig.server.dispatch_call(tool, Some(args.clone())).is_err(),
                "{tool} {args}"
            );
        }
        assert!(TOOL_NAMES.ends_with(&[
            "ache_program_load",
            "ache_program_run",
            "ache_program_stop",
            "ache_program_event"
        ]));
    }
}
