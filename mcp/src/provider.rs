//! The provider seam: three device providers behind one trait, in the
//! estate's provider pattern — a read side (a pure probe, discovery)
//! and an output side that executes exactly one thing, a
//! [`AllowedOutput`] plan the `decide()` gate constructed and allowed.
//! No provider sees a raw intent, no provider validates on its own, and
//! no caller assembles a provider command: the gate is the single
//! choke point for every provider, which is what makes the safety
//! contract's cross-provider parity structural rather than promised.
//!
//! The unified device model lives here too: a device is data — an id
//! namespaced by its provider (`lovense:<toy id>`, `buttplug:<index>`,
//! `serverapi:<toy id>`),
//! a name, and its capabilities as reported values, each either mapped
//! onto the unified feature vocabulary (with its maximum on the
//! unified 0..=20 strength scale) or carried as provider-native data
//! the gate cannot drive (`max: null`).

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::config::{Config, TransportKind};
use crate::decide::{
    AllowedOutput, Decision, Feature, GateSnapshot, OutputIntent, TargetCaps, decide, lovense_wire,
};
use crate::error::{ErrorCode, Result, err};
use crate::transport::{Toy, Transport};

/// Which provider owns a device. A caller needs this only where a
/// capability genuinely exists on one provider (presets are Lovense's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderId {
    Lovense,
    Buttplug,
    /// The Lovense Server API provider (D-1451): the cloud route,
    /// opt-in and off by default, behind the same gate.
    ServerApi,
    /// The TCode multi-axis provider (KoiSync landing): opt-in, off by
    /// default; axis frames only, through the program engine.
    Tcode,
}

impl ProviderId {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Lovense => "lovense",
            ProviderId::Buttplug => "buttplug",
            ProviderId::ServerApi => "serverapi",
            ProviderId::Tcode => "tcode",
        }
    }
}

// origin_lane: ntranch; content_label: nsfw
// Evidence: COVERAGE-MATRIX.md [B], local buttplug_server_device_config 12.0.1
// device-config/protocols/lovense.yml. These are translations, NOT support
// tables: each binding also requires a runtime-enumerated native output.
pub const MAPPING_EVIDENCE: &str = "https://docs.rs/crate/buttplug_server_device_config/12.0.1/source/device-config/protocols/lovense.yml";

#[derive(Clone, Copy, Debug)]
pub struct ActionBinding {
    pub model: &'static str,
    pub action: &'static str,
    pub native: &'static str,
    pub max: u8,
    pub evidence: &'static str,
}

pub const ACTION_BINDINGS: &[ActionBinding] = &[
    ActionBinding {
        model: "Lovense Flexer",
        action: "Fingering",
        native: "Rotate",
        max: 20,
        evidence: MAPPING_EVIDENCE,
    },
    ActionBinding {
        model: "Lovense Gravity",
        action: "Thrusting",
        native: "Oscillate",
        max: 20,
        evidence: MAPPING_EVIDENCE,
    },
    ActionBinding {
        model: "Lovense Solace",
        action: "Thrusting",
        native: "Oscillate",
        max: 20,
        evidence: MAPPING_EVIDENCE,
    },
    ActionBinding {
        model: "Lovense Spinel",
        action: "Thrusting",
        native: "Oscillate",
        max: 20,
        evidence: MAPPING_EVIDENCE,
    },
    ActionBinding {
        model: "Lovense Max",
        action: "Pump",
        native: "Constrict",
        max: 3,
        evidence: MAPPING_EVIDENCE,
    },
    ActionBinding {
        model: "Lovense Velvo",
        action: "RollingBead",
        native: "Rotate",
        max: 20,
        evidence: MAPPING_EVIDENCE,
    },
];

pub fn action_binding(model: &str, action: &str) -> Option<&'static ActionBinding> {
    ACTION_BINDINGS
        .iter()
        .find(|binding| binding.model == model && binding.action == action)
}

/// Add aliases only when the provider actually enumerates the native output.
/// Config identity, never the user-editable display name, selects the binding.
pub fn bind_capabilities(model: &str, capabilities: &mut Vec<Capability>) {
    for binding in ACTION_BINDINGS
        .iter()
        .filter(|binding| binding.model == model)
    {
        if capabilities.iter().any(|cap| cap.feature == binding.action) {
            continue;
        }
        if let Some(cap) = capabilities
            .iter()
            .find(|cap| cap.native == binding.native && cap.max.is_some())
            .cloned()
        {
            capabilities.push(Capability {
                feature: binding.action.to_string(),
                max: Some(binding.max),
                native: binding.native.to_string(),
                steps: cap.steps,
            });
        }
    }
}

/// One capability of one device, as data. `max` is the highest
/// strength on the unified scale the gate may plan for this feature;
/// `None` means the capability is reported but not mapped onto the
/// unified model in this build (the gate refuses it, by name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability {
    /// Unified feature name where mapped (e.g. `Vibrate`); otherwise
    /// the provider-native output name (e.g. `Position`).
    pub feature: String,
    pub max: Option<u8>,
    /// The provider-native output this capability is (`scalar:Vibrate`
    /// style data for Lovense, the Buttplug output type for Buttplug).
    pub native: String,
    /// Discrete steps the device reports for the output, when known.
    pub steps: Option<u32>,
}

impl Capability {
    pub fn to_json(&self) -> Value {
        json!({
            "feature": self.feature,
            "max": self.max,
            "native": self.native,
            "steps": self.steps,
        })
    }
}

/// One device in the unified model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// Unified id: `lovense:<toy id>`, `buttplug:<index>`, or
    /// `serverapi:<toy id>`.
    pub id: String,
    pub provider: ProviderId,
    pub name: String,
    /// Provider-native kind data (a Lovense toy type, a Buttplug
    /// device's protocol name), when reported.
    pub kind: Option<String>,
    /// Percent, 0..=100, when the provider reports it.
    pub battery: Option<u8>,
    pub capabilities: Vec<Capability>,
    /// Where the capability list came from: `reported` (the device or
    /// its app said so), `unreported` (the device reported nothing), or `fixture` (test data).
    pub capability_source: &'static str,
}

impl Device {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "provider": self.provider.as_str(),
            "name": self.name,
            "kind": self.kind,
            "battery": self.battery,
            "capabilities": self.capabilities.iter().map(Capability::to_json).collect::<Vec<_>>(),
            "capability_source": self.capability_source,
            "action_bindings": self.bindings_json(),
            "route_reductions": self.reductions_json(),
        })
    }

    fn bindings_json(&self) -> Vec<Value> {
        if self.provider != ProviderId::Buttplug {
            return Vec::new();
        }
        ACTION_BINDINGS
            .iter()
            .filter(|binding| {
                self.kind.as_deref() == Some(binding.model)
                    && self.capabilities.iter().any(|cap| {
                        cap.feature == binding.action
                            && cap.native == binding.native
                            && cap.max.is_some()
                    })
            })
            .map(|binding| {
                json!({
                    "action": binding.action, "native": binding.native, "max": binding.max,
                    "evidence": binding.evidence, "origin_lane": "ntranch", "content_label": "nsfw",
                })
            })
            .collect()
    }

    fn reductions_json(&self) -> Vec<Value> {
        if self.provider != ProviderId::Buttplug {
            return Vec::new();
        }
        // Evidence describes missing route features, never grants support.
        // The Osci family identity does not verify the physical generation.
        let missing = match self.kind.as_deref() {
            Some("Lovense Osci" | "Lovense Osci 2") => Some("Oscillate"),
            Some("Lovense Vulse") => Some("Thrusting"),
            Some("Lovense Tenera 2") => Some("Suction"),
            // Generic enumeration cannot identify Tenera 2. Report the
            // limitation without claiming which physical toy connected.
            Some("Lovense Device") => {
                return vec![json!({
                    "dropped_features": ["non-vibration outputs (including Tenera 2 suction)"],
                    "status": "UNVERIFIED model identity; generic route models vibration only",
                    "evidence": MAPPING_EVIDENCE, "origin_lane": "ntranch", "content_label": "nsfw",
                })];
            }
            _ => None,
        };
        missing.filter(|action| !self.capabilities.iter().any(|cap| cap.feature == *action && cap.max.is_some()))
            .map(|action| vec![json!({
                "dropped_features": [action], "status": "not modelled on this route; physical generation UNVERIFIED",
                "evidence": MAPPING_EVIDENCE, "origin_lane": "ntranch", "content_label": "nsfw",
            })]).unwrap_or_default()
    }

    /// The gate's view of this device: mapped capabilities only, plus
    /// preset support. Presets are a Lovense-vocabulary capability:
    /// the local route and the Server API route both carry the
    /// documented Function/Pattern/Preset vocabulary (evidence:
    /// COVERAGE-MATRIX.md [L], Lovense Standard Solutions), so both
    /// Lovense providers support them; Buttplug does not.
    #[must_use]
    pub fn caps(&self) -> TargetCaps {
        TargetCaps::new(
            self.capabilities
                .iter()
                .filter_map(|cap| cap.max.map(|max| (cap.feature.clone(), max)))
                .collect(),
            matches!(self.provider, ProviderId::Lovense | ProviderId::ServerApi),
        )
    }
}

/// What a probe reports about a provider. Pure: no I/O.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderProbe {
    pub provider: ProviderId,
    /// `mock`, `ready`, `unconfigured`, `unavailable`, or `disabled`.
    pub state: &'static str,
    /// Whether the provider could accept output at all right now.
    pub available: bool,
    pub note: String,
}

impl ProviderProbe {
    pub fn to_json(&self) -> Value {
        json!({
            "provider": self.provider.as_str(),
            "state": self.state,
            "available": self.available,
            "note": self.note,
        })
    }
}

/// One device provider. Implementations execute gate plans and
/// nothing else: there is deliberately no method that takes a raw
/// strength, a feature string, or a caller-built command.
pub trait DeviceProvider: Send + Sync {
    /// Describe this provider. Never performs I/O.
    fn probe(&self) -> ProviderProbe;
    /// Enumerate the devices this provider can see right now. An
    /// unreachable backend is a typed error, never an empty list
    /// pretending to be a successful enumeration.
    fn discover(&self) -> Result<Vec<Device>>;
    /// Execute one gate-approved plan. The plan's target names the
    /// device in this provider's own id space.
    fn execute(&self, plan: &AllowedOutput) -> Result<()>;
    /// Zero every output this provider owns. `Ok(true)` when a zero
    /// was actually issued, `Ok(false)` when the provider has no
    /// target to zero (the local zero still stands).
    fn zero(&self) -> Result<bool>;
}

// ------------------------------------------------------------- lovense

/// The Lovense provider: the pre-unification server, carried over.
/// Discovery and output run over the existing transport; output bodies
/// come from `decide::lovense_wire` and nowhere else.
pub struct LovenseProvider {
    transport: Option<Box<dyn Transport>>,
    kind: TransportKind,
    /// Real transport named without a host: no target exists, and the
    /// config gate refuses before any transport is touched (the rule
    /// the original server enforced, kept verbatim).
    unconfigured: bool,
}

impl LovenseProvider {
    pub fn new(config: &Config, transport: Option<Box<dyn Transport>>) -> LovenseProvider {
        LovenseProvider {
            unconfigured: config.transport == TransportKind::Real && config.host.is_empty(),
            kind: config.transport,
            transport,
        }
    }

    /// Whether output could reach a target at all.
    #[must_use]
    pub fn target_configured(&self) -> bool {
        self.transport.is_some() && !self.unconfigured
    }

    /// The transport probe the status tool has always reported.
    pub fn transport_probe(&self) -> Option<crate::transport::TransportProbe> {
        self.transport.as_ref().map(|transport| transport.probe())
    }

    /// Send one gate-constructed body over the transport, exactly as
    /// given — the same contract the transport itself holds. The
    /// server's Lovense paths use this so results can carry the body.
    pub fn send_gate_body(&self, body: &Value) -> Result<()> {
        match &self.transport {
            Some(transport) => transport.send_command(body),
            None => err(
                ErrorCode::TargetUnconfigured,
                "no transport exists; nothing was sent",
            ),
        }
    }

    /// Toy discovery in the original shape (the `lovense_toys` tool).
    pub fn toys(&self) -> Result<Vec<Toy>> {
        match &self.transport {
            Some(transport) if !self.unconfigured => transport.discover(),
            _ => err(
                ErrorCode::TargetUnconfigured,
                "transport is real but no host is configured; no request was made",
            ),
        }
    }
}

/// Map one discovered toy into the unified model.
fn toy_to_device(toy: &Toy) -> Device {
    let (capabilities, capability_source) = match &toy.functions {
        Some(names) => {
            let capabilities = names
                .iter()
                .map(|name| Capability {
                    feature: name.clone(),
                    max: Feature::from_action(name).map(|feature| feature.api_max),
                    native: format!("lovense:{name}"),
                    steps: None,
                })
                .collect();
            (capabilities, "reported")
        }
        None => (Vec::new(), "unreported"),
    };
    Device {
        id: format!("lovense:{}", toy.id),
        provider: ProviderId::Lovense,
        name: toy.name.clone(),
        kind: toy.toy_type.clone(),
        battery: toy.battery,
        capabilities,
        capability_source,
    }
}

impl DeviceProvider for LovenseProvider {
    fn probe(&self) -> ProviderProbe {
        let (state, available, note) = match (self.kind, self.transport.is_some()) {
            (TransportKind::Mock, _) => (
                "mock",
                true,
                "mock transport: fixture toys only; no device or app is contacted",
            ),
            (TransportKind::Real, false) => (
                "unconfigured",
                false,
                "real transport with no host configured; nothing can reach the wire",
            ),
            (TransportKind::Real, true) => (
                "ready",
                true,
                "target configured but not verified; discovery contacts it",
            ),
        };
        ProviderProbe {
            provider: ProviderId::Lovense,
            state,
            available,
            note: note.to_string(),
        }
    }

    fn discover(&self) -> Result<Vec<Device>> {
        Ok(self.toys()?.iter().map(toy_to_device).collect())
    }

    fn execute(&self, plan: &AllowedOutput) -> Result<()> {
        if matches!(plan, AllowedOutput::Stop) {
            self.zero()?;
            return Ok(());
        }
        let Some(transport) = &self.transport else {
            return err(
                ErrorCode::TargetUnconfigured,
                "no transport exists; nothing was sent",
            );
        };
        let (body, _) = lovense_wire(plan);
        transport.send_command(&body)
    }

    fn zero(&self) -> Result<bool> {
        if !self.target_configured() {
            return Ok(false);
        }
        let transport = self.transport.as_ref().expect("checked above");
        // The stop body comes from the gate, as it always has: stop
        // is the intent the gate allows in every state, so the
        // snapshot here is deliberately the emptiest one.
        let snapshot = GateSnapshot {
            armed: false,
            target_configured: false,
            ceiling: 0,
        };
        let Decision::Allow { body, .. } = decide(&snapshot, &OutputIntent::Stop) else {
            unreachable!("the gate always allows stop");
        };
        transport.send_command(&body)?;
        Ok(true)
    }
}

// ------------------------------------------------------- inert (off)

/// A provider that exists in the surface but cannot serve: disabled
/// by configuration, not wired by a constructor, or failed to start.
/// Every operation is a typed `provider_unavailable` — absence is
/// reported, never faked as an empty success.
pub struct InertProvider {
    provider: ProviderId,
    state: &'static str,
    note: String,
}

impl InertProvider {
    pub fn disabled(provider: ProviderId, note: impl Into<String>) -> InertProvider {
        InertProvider {
            provider,
            state: "disabled",
            note: note.into(),
        }
    }

    pub fn unavailable(provider: ProviderId, note: impl Into<String>) -> InertProvider {
        InertProvider {
            provider,
            state: "unavailable",
            note: note.into(),
        }
    }

    fn unavailable_error<T>(&self) -> Result<T> {
        err(ErrorCode::ProviderUnavailable, self.note.clone())
    }
}

impl DeviceProvider for InertProvider {
    fn probe(&self) -> ProviderProbe {
        ProviderProbe {
            provider: self.provider,
            state: self.state,
            available: false,
            note: self.note.clone(),
        }
    }

    fn discover(&self) -> Result<Vec<Device>> {
        self.unavailable_error()
    }

    fn execute(&self, _plan: &AllowedOutput) -> Result<()> {
        self.unavailable_error()
    }

    fn zero(&self) -> Result<bool> {
        Ok(false)
    }
}

// ---------------------------------------------------- fixture (mock)

/// The Buttplug fixture provider: the mock transport's counterpart for
/// the unified surface. Serves fixture devices, records every plan it
/// is handed (so tests can assert exactly what the gate allowed), and
/// can inject execute failures to exercise the session-drop path. Used
/// wherever the server runs in mock mode, and labelled `fixture`
/// everywhere its data appears.
pub struct FakeButtplug {
    executed: Arc<Mutex<Vec<AllowedOutput>>>,
    zero_calls: Arc<Mutex<u32>>,
    pattern_active: Arc<Mutex<bool>>,
    pattern_cancelled: Arc<Mutex<bool>>,
    fail_execute: Arc<AtomicBool>,
}

impl Default for FakeButtplug {
    fn default() -> FakeButtplug {
        FakeButtplug::new()
    }
}

impl FakeButtplug {
    pub fn new() -> FakeButtplug {
        FakeButtplug {
            executed: Arc::new(Mutex::new(Vec::new())),
            zero_calls: Arc::new(Mutex::new(0)),
            pattern_active: Arc::new(Mutex::new(false)),
            pattern_cancelled: Arc::new(Mutex::new(false)),
            fail_execute: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The fixture devices: a vibrate-only unit and a vibrate+rotate
    /// unit, mirroring the Lovense mock's two-toy fixture.
    pub fn fixture_devices() -> Vec<Device> {
        let capability = |feature: &str, native: &str| Capability {
            feature: feature.to_string(),
            max: Some(20),
            native: native.to_string(),
            steps: Some(20),
        };
        vec![
            Device {
                id: "buttplug:0".to_string(),
                provider: ProviderId::Buttplug,
                name: "Fixture Vibrator".to_string(),
                kind: Some("fixture".to_string()),
                battery: None,
                capabilities: vec![capability("Vibrate", "Vibrate")],
                capability_source: "fixture",
            },
            Device {
                id: "buttplug:1".to_string(),
                provider: ProviderId::Buttplug,
                name: "Fixture Duo".to_string(),
                kind: Some("fixture".to_string()),
                battery: Some(63),
                capabilities: vec![
                    capability("Vibrate", "Vibrate"),
                    capability("Rotate", "Rotate"),
                ],
                capability_source: "fixture",
            },
        ]
    }

    /// A shared handle on the log of executed plans, in order.
    #[cfg(test)]
    pub fn executed_log(&self) -> Arc<Mutex<Vec<AllowedOutput>>> {
        Arc::clone(&self.executed)
    }

    /// A shared handle on the zero-call count.
    #[cfg(test)]
    pub fn zero_log(&self) -> Arc<Mutex<u32>> {
        Arc::clone(&self.zero_calls)
    }

    /// A shared handle on the pattern-cancellation flag: set when a
    /// zero lands while a pattern is active.
    #[cfg(test)]
    pub fn pattern_cancelled_flag(&self) -> Arc<Mutex<bool>> {
        Arc::clone(&self.pattern_cancelled)
    }

    /// A shared handle on the failure flag, so a test can inject
    /// execute failures (the session-drop path) after the provider
    /// has been handed to the server.
    #[cfg(test)]
    pub fn fail_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.fail_execute)
    }

    fn record(&self, plan: &AllowedOutput) {
        let mut executed = self
            .executed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        executed.push(plan.clone());
    }
}

impl DeviceProvider for FakeButtplug {
    fn probe(&self) -> ProviderProbe {
        ProviderProbe {
            provider: ProviderId::Buttplug,
            state: "mock",
            available: true,
            note: "fixture provider: fixture devices only; no device or app is contacted"
                .to_string(),
        }
    }

    fn discover(&self) -> Result<Vec<Device>> {
        Ok(FakeButtplug::fixture_devices())
    }

    fn execute(&self, plan: &AllowedOutput) -> Result<()> {
        // Record before the failure check: an attempted execute is a
        // fact the battery asserts on, even when the drop swallows it.
        self.record(plan);
        if self.fail_execute.load(Ordering::SeqCst) {
            return err(
                ErrorCode::TargetUnreachable,
                "fixture buttplug: execute failed (injected session drop)",
            );
        }
        if matches!(plan, AllowedOutput::Pattern { .. }) {
            let mut active = self
                .pattern_active
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            *active = true;
        }
        Ok(())
    }

    fn zero(&self) -> Result<bool> {
        let mut calls = self
            .zero_calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *calls += 1;
        let mut active = self
            .pattern_active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *active {
            *active = false;
            let mut cancelled = self
                .pattern_cancelled
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            *cancelled = true;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    fn lovense_mock() -> LovenseProvider {
        LovenseProvider::new(&Config::default(), Some(Box::new(MockTransport::new())))
    }

    // --- validation ---------------------------------------------------

    #[test]
    fn lovense_devices_carry_reported_capabilities_as_data() {
        let provider = lovense_mock();
        let devices = provider.discover().unwrap();
        assert_eq!(devices.len(), 2);
        let lush = &devices[0];
        assert_eq!(lush.id, "lovense:mock-toy-1");
        assert_eq!(lush.provider, ProviderId::Lovense);
        assert_eq!(lush.capability_source, "reported");
        assert_eq!(lush.capabilities.len(), 1);
        assert_eq!(lush.capabilities[0].feature, "Vibrate");
        assert_eq!(lush.capabilities[0].max, Some(20));
        // The gate's caps for the device hold exactly the reported set.
        let caps = lush.caps();
        assert_eq!(caps.max_for("Vibrate"), Some(20));
        assert_eq!(caps.max_for("Rotate"), None);
        assert!(caps.supports_presets());
    }

    #[test]
    fn fixture_devices_cover_two_capability_shapes() {
        let devices = FakeButtplug::fixture_devices();
        assert_eq!(devices[0].caps().max_for("Rotate"), None);
        assert_eq!(devices[1].caps().max_for("Rotate"), Some(20));
        assert!(!devices[0].caps().supports_presets());
    }

    #[test]
    fn inert_provider_reports_unavailable_never_empty_success() {
        let provider = InertProvider::unavailable(ProviderId::Buttplug, "boom");
        assert!(!provider.probe().available);
        assert_eq!(
            provider.discover().unwrap_err().code,
            ErrorCode::ProviderUnavailable
        );
        assert!(!provider.zero().unwrap());
    }
}

// origin_lane: ntranch; content_label: nsfw
// Evidence: dispatch COVERAGE-MATRIX.md verdict queue; mock-only tests.
#[cfg(test)]
mod step2_tests {
    use super::*;
    use crate::decide::{FunctionIntent, decide_plan};
    use crate::transport::MockTransport;

    fn cap(native: &str, max: u8) -> Capability {
        Capability {
            feature: native.to_string(),
            native: native.to_string(),
            max: Some(max),
            steps: Some(20),
        }
    }
    fn device(model: &str, capabilities: Vec<Capability>) -> Device {
        Device {
            kind: Some(model.to_string()),
            name: model.to_string(),
            capabilities,
            ..FakeButtplug::fixture_devices().remove(0)
        }
    }
    fn toy(functions: Option<Vec<String>>) -> Toy {
        Toy {
            id: "runtime".into(),
            name: "unverified".into(),
            toy_type: None,
            battery: None,
            functions,
        }
    }
    fn snapshot() -> GateSnapshot {
        GateSnapshot {
            armed: true,
            target_configured: true,
            ceiling: 20,
        }
    }
    fn intent(action: &str, strength: i64, time_sec: i64) -> OutputIntent {
        OutputIntent::Function(FunctionIntent {
            feature: action.into(),
            strength,
            time_sec,
            toy: None,
        })
    }

    // Validation (5).
    #[test]
    fn reported_depth_stroke_preserve_their_api_ranges() {
        let d = toy_to_device(&toy(Some(vec!["Depth".into(), "Stroke".into()])));
        assert_eq!(d.caps().max_for("Depth"), Some(3));
        assert_eq!(d.caps().max_for("Stroke"), Some(100));
        assert_eq!(d.capability_source, "reported");
    }
    #[test]
    fn named_bindings_require_and_preserve_runtime_outputs() {
        for binding in ACTION_BINDINGS {
            let mut caps = vec![cap(binding.native, binding.max)];
            bind_capabilities(binding.model, &mut caps);
            let d = device(binding.model, caps);
            assert_eq!(d.caps().max_for(binding.action), Some(binding.max));
            let json = d.to_json();
            assert_eq!(json["action_bindings"][0]["evidence"], binding.evidence);
            assert_eq!(json["action_bindings"][0]["origin_lane"], "ntranch");
        }
    }
    #[test]
    fn mapped_actions_have_equal_clamping_and_execute_on_mock_providers() {
        let transport = MockTransport::new();
        let sent = transport.sent_log();
        let lovense = LovenseProvider::new(&Config::default(), Some(Box::new(transport)));
        let buttplug = FakeButtplug::new();
        for binding in ACTION_BINDINGS
            .iter()
            .filter(|binding| binding.action != "RollingBead")
        {
            let mut caps = vec![cap(binding.native, binding.max)];
            // Real enumeration maps Constrict to Pump before alias binding.
            if binding.native == "Constrict" {
                caps[0].feature = "Pump".into();
            }
            bind_capabilities(binding.model, &mut caps);
            let ble = device(binding.model, caps);
            let gm = toy_to_device(&toy(Some(vec![binding.action.into()])));
            for strength in [0, 2, 3, 20, 99] {
                let output = intent(binding.action, strength, 5);
                let a = decide_plan(&snapshot(), &output, &gm.caps()).unwrap();
                let b = decide_plan(&snapshot(), &output, &ble.caps()).unwrap();
                assert_eq!(a, b);
                lovense.execute(&a).unwrap();
                buttplug.execute(&b).unwrap();
            }
        }
        assert_eq!(sent.lock().unwrap().len(), 25);
    }
    #[test]
    fn named_route_reductions_appear_in_device_data() {
        for (model, missing) in [
            ("Lovense Osci", "Oscillate"),
            ("Lovense Osci 2", "Oscillate"),
            ("Lovense Vulse", "Thrusting"),
            ("Lovense Tenera 2", "Suction"),
        ] {
            let json = device(model, vec![cap("Vibrate", 20)]).to_json();
            assert_eq!(json["route_reductions"][0]["dropped_features"][0], missing);
            assert_eq!(json["route_reductions"][0]["evidence"], MAPPING_EVIDENCE);
        }
    }
    #[test]
    fn rolling_bead_is_allowed_only_by_the_named_ble_binding() {
        let mut caps = vec![cap("Rotate", 20)];
        bind_capabilities("Lovense Velvo", &mut caps);
        let plan = decide_plan(
            &snapshot(),
            &intent("RollingBead", 99, 5),
            &device("Lovense Velvo", caps).caps(),
        )
        .unwrap();
        assert!(plan.clamped());
        assert_eq!(
            action_binding("Lovense Velvo", "RollingBead")
                .unwrap()
                .native,
            "Rotate"
        );
    }

    // Adversarial (5).
    #[test]
    fn absent_functions_are_unreported_not_assumed() {
        let d = toy_to_device(&toy(None));
        assert!(d.capabilities.is_empty());
        assert_eq!(d.capability_source, "unreported");
        for action in ["Depth", "Stroke", "Vibrate"] {
            assert_eq!(
                decide_plan(&snapshot(), &intent(action, 1, 5), &d.caps())
                    .unwrap_err()
                    .code,
                ErrorCode::CapabilityUnsupported
            );
        }
    }
    #[test]
    fn mappings_never_create_an_unenumerated_output_or_use_display_name() {
        for binding in ACTION_BINDINGS {
            let mut caps = vec![cap("Vibrate", 20)];
            bind_capabilities(binding.model, &mut caps);
            assert_eq!(caps.len(), 1);
            let mut caps = vec![cap(binding.native, binding.max)];
            bind_capabilities("unknown", &mut caps);
            assert_eq!(caps.len(), 1);
        }
        let mut d = device("unknown", vec![cap("Rotate", 20)]);
        d.name = "Lovense Velvo".into();
        assert!(d.bindings_json().is_empty());
    }
    #[test]
    fn named_refusals_are_equal_and_send_nothing_on_mock_providers() {
        let transport = MockTransport::new();
        let sent = transport.sent_log();
        let lovense = LovenseProvider::new(&Config::default(), Some(Box::new(transport)));
        let ble = FakeButtplug::new();
        for action in ["Heating", "Tapping", "RollingBead"] {
            for provider in [&lovense as &dyn DeviceProvider, &ble] {
                let caps = provider.discover().unwrap()[0].caps();
                let error = decide_plan(&snapshot(), &intent(action, 1, 5), &caps).unwrap_err();
                assert_eq!(error.code, ErrorCode::CapabilityUnsupported);
                assert!(error.message.contains(action));
            }
        }
        assert!(sent.lock().unwrap().is_empty());
        assert!(ble.executed_log().lock().unwrap().is_empty());
    }
    #[test]
    fn mapped_actions_refuse_invalid_duration_negative_strength_and_disarmed_equally() {
        for binding in ACTION_BINDINGS.iter().filter(|b| b.action != "RollingBead") {
            let caps = TargetCaps::new(vec![(binding.action.into(), binding.max)], false);
            for bad in [
                intent(binding.action, -1, 5),
                intent(binding.action, 1, 0),
                intent(binding.action, 1, 3601),
            ] {
                let a = decide_plan(&snapshot(), &bad, &TargetCaps::lovense()).unwrap_err();
                let b = decide_plan(&snapshot(), &bad, &caps).unwrap_err();
                assert_eq!(a.code, ErrorCode::RequestInvalid);
                assert_eq!(a.code, b.code);
            }
            let disarmed = GateSnapshot {
                armed: false,
                ..snapshot()
            };
            assert_eq!(
                decide_plan(&disarmed, &intent(binding.action, 1, 5), &caps)
                    .unwrap_err()
                    .code,
                ErrorCode::NotArmed
            );
        }
    }
    #[test]
    fn generic_model_reduction_does_not_claim_tenera_identity_or_suction() {
        let d = device("Lovense Device", vec![cap("Vibrate", 20)]);
        let json = d.to_json();
        assert!(
            json["route_reductions"][0]["status"]
                .as_str()
                .unwrap()
                .contains("UNVERIFIED")
        );
        assert_eq!(d.caps().max_for("Suction"), None);
        let restored = device("Lovense Osci 2", vec![cap("Oscillate", 20)]);
        assert!(restored.reductions_json().is_empty());
    }
}
