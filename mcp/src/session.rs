//! `ache.session/v1` (XToys spec §3): the frozen composition
//! Layout → Block → Device → Function → Source, and its runtime.
//!
//! Load-time law: one source per (device, function) — a second binding
//! of the same device function is a conflict error; devices are ache
//! devices (resolved through the providers, never claimed by the spec);
//! a function the device does not expose fails the load; packs bind
//! all-or-none; the composition is frozen. Static bounds (A2): every
//! pattern source and every queue's delay sum must fit the session
//! duration, which is required and ≤ 60 min.
//!
//! Runtime law: [`SessionRun`] is pure. Time advances in 10 ms quanta;
//! the automation and the expression tick run every 250 ms; feature
//! lanes (Lovense/Buttplug/Server API) are evaluated every 250 ms;
//! TCode axis lanes emit every 20 ms (the 50 Hz law). The run proposes
//! commands; [`gate_proposed`] is the only way a proposal becomes a
//! plan, and it is `decide_plan` / `decide_axes` — the existing gate.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::automation::{
    Automation, AutomationRun, BindContext, ControlKind, Effect, Event, TICK_MS,
};
use crate::decide::{
    AllowedAxes, AllowedOutput, AxesIntent, AxisLimit, FunctionIntent, GateSnapshot, OutputIntent,
    decide_axes, decide_plan,
};
use crate::error::{AcheError, ErrorCode, Result};
use crate::funscript::{PACK_CHANNELS, Pack};
use crate::pattern::{Function, Pattern, content_hash, invalid, quantize, valid_id};
use crate::provider::{Device, ProviderId};
use crate::provider_tcode::{TCODE_FRAME_INTERVAL_MS, TCODE_FRAME_MS, axis_of};

pub const SESSION_FORMAT: &str = "ache.session/v1";
/// §2.4: the session bound is required and at most 60 min.
pub const SESSION_DURATION_MS_MAX: u64 = 60 * 60 * 1000;
pub const BINDINGS_MAX: usize = 32;
pub const INPUTS_MAX: usize = 16;
/// The runtime quantum: gcd of the 250 ms tick and the 20 ms frame.
pub const QUANTUM_MS: u64 = 10;
/// A held feature level is re-sent this often so a device's own
/// time-out (the Lovense `timeSec` of 1 s) never lapses mid-hold.
pub const FEATURE_REFRESH_MS: u64 = 750;
/// Most events one tick accepts from outside.
pub const EVENTS_PER_TICK_MAX: usize = 32;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceSpec {
    Pattern(String),
    Pack(String),
    Automation,
    Manual(String),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingSpec {
    pub id: String,
    pub device: String,
    #[serde(default)]
    pub function: Option<Function>,
    pub source: SourceSpec,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSpec {
    pub id: String,
    /// A stop input (the "Say STOP" primitive): its event is ache's
    /// synchronous stop, outranking every running program.
    #[serde(default)]
    pub stop: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSpec {
    pub format: String,
    pub id: String,
    pub duration_ms: u64,
    #[serde(default)]
    pub seed: Option<u64>,
    pub bindings: Vec<BindingSpec>,
    #[serde(default)]
    pub automation: Option<String>,
    #[serde(default)]
    pub inputs: Vec<InputSpec>,
}

/// Loaded specs, by id. BTreeMap: deterministic order everywhere.
#[derive(Default)]
pub struct Library {
    pub patterns: BTreeMap<String, Pattern>,
    pub packs: BTreeMap<String, Pack>,
    pub automations: BTreeMap<String, Automation>,
    pub sessions: BTreeMap<String, Value>,
}

/// The estate seed library (library/seeds.json): estate-authored specs
/// for the XToys study's top-10 ADAPT shapes.
pub const SEEDS_JSON: &str = include_str!("../library/seeds.json");

/// Load the seeds through the same validators as any spec. Keys
/// starting with `_` are authoring comments and are stripped here only
/// (the public loaders refuse unknown keys).
pub fn seed_library() -> Result<Library> {
    let seeds: Value = serde_json::from_str(SEEDS_JSON)
        .map_err(|e| AcheError::new(ErrorCode::RequestInvalid, format!("seed library: {e}")))?;
    let strip = |v: &Value| -> Value {
        let mut v = v.clone();
        if let Some(object) = v.as_object_mut() {
            object.retain(|k, _| !k.starts_with('_'));
        }
        v
    };
    let mut library = Library::default();
    for p in seeds["patterns"].as_array().into_iter().flatten() {
        let p = Pattern::from_json(&strip(p))?;
        library.patterns.insert(p.id().to_string(), p);
    }
    for a in seeds["automations"].as_array().into_iter().flatten() {
        let a = Automation::from_json(&strip(a))?;
        library.automations.insert(a.id().to_string(), a);
    }
    Ok(library)
}

/// How one lane reaches its device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emit {
    /// A unified gate feature, with the device's max on its own scale.
    Feature { feature: &'static str, max: u8 },
    /// A TCode axis index.
    Axis(usize),
}

#[derive(Clone, Debug)]
enum LaneSource {
    Curve(usize),
    Automation(usize),
    Manual(usize),
}

#[derive(Clone, Debug)]
pub struct Lane {
    pub binding: String,
    pub device: usize,
    pub function: Function,
    pub emit: Emit,
    source: LaneSource,
}

/// A compiled, frozen session.
#[derive(Debug)]
pub struct CompiledSession {
    pub id: String,
    pub hash: String,
    pub duration_ms: u64,
    pub devices: Vec<Device>,
    pub lanes: Vec<Lane>,
    /// Lane curves: (samples, period) per curve-sourced lane.
    curves: Vec<(Vec<u8>, u64)>,
    /// Patterns the automation may play: (250 ms grid, 20 ms grid).
    playable: Vec<(Vec<u8>, Vec<u8>)>,
    automation: Option<crate::automation::CompiledAutomation>,
    pub inputs: Vec<InputSpec>,
    sources: Vec<String>,
}

fn device_of<'a>(devices: &'a [Device], id: &str) -> Result<&'a Device> {
    devices.iter().find(|d| d.id == id).ok_or_else(|| {
        AcheError::new(
            ErrorCode::DeviceNotFound,
            format!("session names device {id:?}, which no provider reports"),
        )
    })
}

/// The emit route of `function` on `device`, or a load error.
fn route(device: &Device, function: Function) -> Result<Emit> {
    if function == Function::Generic {
        return invalid("a binding names a device function; Generic is a pattern kind only");
    }
    if device.provider == ProviderId::Tcode {
        let axis = axis_of(function).filter(|_| {
            device
                .capabilities
                .iter()
                .any(|c| c.feature == function.as_str())
        });
        return axis.map(Emit::Axis).ok_or_else(|| {
            AcheError::new(
                ErrorCode::CapabilityUnsupported,
                format!("device {} does not expose {}", device.id, function.as_str()),
            )
        });
    }
    let caps = device.caps();
    match function
        .gate_feature()
        .and_then(|f| caps.max_for(f).map(|max| (f, max)))
    {
        Some((feature, max)) => Ok(Emit::Feature { feature, max }),
        None => Err(AcheError::new(
            ErrorCode::CapabilityUnsupported,
            format!("device {} does not expose {}", device.id, function.as_str()),
        )),
    }
}

impl CompiledSession {
    /// Compile a session against the library and the devices the
    /// providers report right now. Every rule is a load error.
    pub fn compile(
        value: &Value,
        library: &Library,
        devices: &[Device],
    ) -> Result<CompiledSession> {
        let spec: SessionSpec = serde_json::from_value(value.clone()).map_err(|e| {
            AcheError::new(ErrorCode::RequestInvalid, format!("invalid session: {e}"))
        })?;
        if spec.format != SESSION_FORMAT {
            return invalid(format!("session format must be {SESSION_FORMAT:?}"));
        }
        valid_id("session", &spec.id)?;
        if spec.duration_ms == 0 || spec.duration_ms > SESSION_DURATION_MS_MAX {
            return invalid(format!(
                "session duration_ms must be 1..={SESSION_DURATION_MS_MAX}"
            ));
        }
        if spec.bindings.is_empty() || spec.bindings.len() > BINDINGS_MAX {
            return invalid(format!("a session needs 1..={BINDINGS_MAX} bindings"));
        }
        if spec.inputs.len() > INPUTS_MAX {
            return invalid(format!("at most {INPUTS_MAX} inputs"));
        }
        let mut input_ids: Vec<String> = Vec::new();
        for input in &spec.inputs {
            valid_id("input", &input.id)?;
            if input_ids.contains(&input.id) {
                return invalid(format!("input {:?} is declared twice", input.id));
            }
            input_ids.push(input.id.clone());
        }
        let automation = match &spec.automation {
            None => None,
            Some(id) => Some(library.automations.get(id).ok_or_else(|| {
                AcheError::new(
                    ErrorCode::RequestInvalid,
                    format!("automation {id:?} is not loaded"),
                )
            })?),
        };
        // Devices used, in first-use order.
        let mut used: Vec<Device> = Vec::new();
        let mut lanes: Vec<Lane> = Vec::new();
        let mut curves: Vec<(Vec<u8>, u64)> = Vec::new();
        let mut binding_ids: Vec<String> = Vec::new();
        let mut targets: Vec<String> = Vec::new();
        let mut hashes: Vec<String> = Vec::new();
        let mut fan_out: BTreeMap<String, u32> = BTreeMap::new();
        for b in &spec.bindings {
            valid_id("binding", &b.id)?;
            if binding_ids.contains(&b.id) {
                return invalid(format!("binding {:?} is declared twice", b.id));
            }
            binding_ids.push(b.id.clone());
            let device = match used.iter().position(|d| d.id == b.device) {
                Some(i) => i,
                None => {
                    used.push(device_of(devices, &b.device)?.clone());
                    used.len() - 1
                }
            };
            let period = |emit: &Emit| match emit {
                Emit::Axis(_) => TCODE_FRAME_MS,
                Emit::Feature { .. } => TICK_MS,
            };
            match &b.source {
                SourceSpec::Pack(pack_id) => {
                    if b.function.is_some() {
                        return invalid(format!(
                            "binding {:?}: a pack binds its own axes; omit function",
                            b.id
                        ));
                    }
                    let pack = library.packs.get(pack_id).ok_or_else(|| {
                        AcheError::new(
                            ErrorCode::RequestInvalid,
                            format!("pack {pack_id:?} is not loaded"),
                        )
                    })?;
                    if pack.total_ms > spec.duration_ms {
                        return invalid(format!(
                            "pack {pack_id:?} ({} ms) exceeds the session bound",
                            pack.total_ms
                        ));
                    }
                    hashes.push(pack.hash.clone());
                    // All-or-none: every channel must route, or none loads.
                    for channel in &pack.channels {
                        let emit = route(&used[device], channel.function).map_err(|e| {
                            AcheError::new(
                                e.code,
                                format!("pack {pack_id:?} loads all-or-none: {}", e.message),
                            )
                        })?;
                        let marker = PACK_CHANNELS
                            .iter()
                            .find(|(_, _, f)| *f == channel.function)
                            .map_or("axis", |(m, _, _)| *m);
                        let p = period(&emit);
                        curves.push((channel.pattern.sample(p, 0), p));
                        lanes.push(Lane {
                            binding: format!("{}.{marker}", b.id),
                            device,
                            function: channel.function,
                            emit,
                            source: LaneSource::Curve(curves.len() - 1),
                        });
                    }
                }
                other => {
                    let Some(function) = b.function else {
                        return invalid(format!("binding {:?} needs a function", b.id));
                    };
                    let emit = route(&used[device], function)?;
                    let source = match other {
                        SourceSpec::Pattern(pattern_id) => {
                            let pattern = library.patterns.get(pattern_id).ok_or_else(|| {
                                AcheError::new(
                                    ErrorCode::RequestInvalid,
                                    format!("pattern {pattern_id:?} is not loaded"),
                                )
                            })?;
                            if !pattern.function().compiles_against(function) {
                                return invalid(format!(
                                    "pattern {pattern_id:?} is a {} pattern; binding {:?} is {}",
                                    pattern.function().as_str(),
                                    b.id,
                                    function.as_str()
                                ));
                            }
                            if pattern.total_ms() > spec.duration_ms {
                                return invalid(format!(
                                    "pattern {pattern_id:?} ({} ms) exceeds the session bound of {} ms",
                                    pattern.total_ms(),
                                    spec.duration_ms
                                ));
                            }
                            hashes.push(pattern.hash().to_string());
                            let index = fan_out.entry(pattern_id.clone()).or_insert(0);
                            let lane_index = *index;
                            *index += 1;
                            let p = period(&emit);
                            curves.push((pattern.sample(p, lane_index), p));
                            lanes.push(Lane {
                                binding: b.id.clone(),
                                device,
                                function,
                                emit,
                                source: LaneSource::Curve(curves.len() - 1),
                            });
                            continue;
                        }
                        SourceSpec::Automation => {
                            if automation.is_none() {
                                return invalid(format!(
                                    "binding {:?} is automation-sourced but the session has no automation",
                                    b.id
                                ));
                            }
                            targets.push(b.id.clone());
                            LaneSource::Automation(targets.len() - 1)
                        }
                        SourceSpec::Manual(control) => {
                            let var = automation.and_then(|a| a.slider_var(control)).ok_or_else(|| {
                                AcheError::new(
                                    ErrorCode::RequestInvalid,
                                    format!("binding {:?}: manual source {control:?} must name a slider of the session automation", b.id),
                                )
                            })?;
                            LaneSource::Manual(var)
                        }
                        SourceSpec::Pack(_) => unreachable!("handled above"),
                    };
                    lanes.push(Lane {
                        binding: b.id.clone(),
                        device,
                        function,
                        emit,
                        source,
                    });
                }
            }
        }
        // One source per (device, function): two lanes reaching the same
        // device output conflict, whatever their binding names.
        for (i, a) in lanes.iter().enumerate() {
            for b in &lanes[..i] {
                if a.device == b.device && a.emit == b.emit {
                    return invalid(format!(
                        "bindings {:?} and {:?} both drive {} on {}: one source per device function",
                        b.binding,
                        a.binding,
                        a.function.as_str(),
                        used[a.device].id
                    ));
                }
            }
        }
        // The automation, bound to this session's names.
        let mut playable = Vec::new();
        let mut compiled_automation = None;
        if let Some(a) = automation {
            for (queue, total) in a.queue_delay_totals() {
                if total > spec.duration_ms {
                    return invalid(format!(
                        "queue {queue:?} delays sum to {total} ms, over the session bound of {} ms",
                        spec.duration_ms
                    ));
                }
            }
            let pattern_ids: Vec<String> = library.patterns.keys().cloned().collect();
            let bound = a.bind(&BindContext {
                targets: &targets,
                patterns: &pattern_ids,
                sources: &input_ids,
                seeded: spec.seed.is_some(),
                session_ms: spec.duration_ms,
            })?;
            for id in &pattern_ids {
                let p = &library.patterns[id];
                playable.push((p.sample(TICK_MS, 0), p.sample(TCODE_FRAME_MS, 0)));
            }
            hashes.push(a.hash().to_string());
            compiled_automation = Some(bound.with_seed(spec.seed));
        }
        let canonical = serde_json::to_vec(value).expect("a JSON value serializes");
        let hash =
            content_hash(format!("{}|{}", content_hash(&canonical), hashes.join("|")).as_bytes());
        Ok(CompiledSession {
            id: spec.id,
            hash,
            duration_ms: spec.duration_ms,
            devices: used,
            lanes,
            curves,
            playable,
            automation: compiled_automation,
            sources: input_ids,
            inputs: spec.inputs,
        })
    }

    /// Resolve an operator/external event by name. Values never come
    /// from outside except a slider's position, which clamps.
    pub fn event(&self, kind: &str, name: &str, value: Option<f64>) -> Result<Event> {
        let refuse = |m: String| Err(AcheError::new(ErrorCode::RequestInvalid, m));
        if kind == "toy_connected" || kind == "toy_disconnected" {
            // The operator reports device presence by kind name only.
            if value.is_some()
                || name.is_empty()
                || name.len() > 64
                || name.chars().any(char::is_control)
            {
                return refuse(
                    "a toy event carries a device kind name (1..=64 bytes) and no value".into(),
                );
            }
            return Ok(if kind == "toy_connected" {
                Event::ToyConnected(name.to_string())
            } else {
                Event::ToyDisconnected(name.to_string())
            });
        }
        if kind == "external" {
            if value.is_some() {
                return refuse("an external event carries a name and no value".into());
            }
            return match self.sources.iter().position(|s| s == name) {
                Some(i) => Ok(Event::External(i)),
                None => refuse(format!(
                    "input source {name:?} is not registered by this session"
                )),
            };
        }
        let Some(a) = &self.automation else {
            return refuse("this session has no automation, so no controls".into());
        };
        let Some(c) = a.control_index(name) else {
            return refuse(format!("control {name:?} is not declared by this session"));
        };
        match (kind, a.control_kind(c), value) {
            ("press", ControlKind::Button, None) => Ok(Event::Press(c)),
            ("release", ControlKind::Button, None) => Ok(Event::Release(c)),
            ("toggle", ControlKind::Toggle, None) => Ok(Event::Toggle(c)),
            ("slider", ControlKind::Slider, Some(v)) if v.is_finite() => Ok(Event::Slider(c, v)),
            _ => refuse(format!("event {kind:?} does not fit control {name:?}")),
        }
    }

    /// Whether `name` is a stop input (routes to the synchronous stop).
    #[must_use]
    pub fn is_stop_input(&self, name: &str) -> bool {
        self.inputs.iter().any(|i| i.stop && i.id == name)
    }

    /// Whether `name` is a stop control (the automation's stop button).
    #[must_use]
    pub fn is_stop_control(&self, name: &str) -> bool {
        self.automation.as_ref().is_some_and(|a| {
            a.control_index(name)
                .is_some_and(|c| a.control_kind(c) == ControlKind::StopButton)
        })
    }
}

/// A proposed command: not a plan until [`gate_proposed`] says so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Proposed {
    Feature {
        device: usize,
        feature: &'static str,
        max: u8,
        percent: u8,
    },
    Axes {
        device: usize,
        values: Vec<(usize, u8)>,
    },
}

#[derive(Clone, Debug, Default)]
struct LaneState {
    value: Option<u8>,
    player: Option<(usize, u64)>,
    stopped: bool,
    last: Option<(u8, u64)>,
}

/// One session run: pure, quantum by quantum.
pub struct SessionRun<'a> {
    s: &'a CompiledSession,
    t: u64,
    lanes: Vec<LaneState>,
    automation: Option<AutomationRun>,
    finished_patterns: Vec<usize>,
    pub stop_reason: Option<String>,
}

impl<'a> SessionRun<'a> {
    #[must_use]
    pub fn new(s: &'a CompiledSession) -> SessionRun<'a> {
        SessionRun {
            s,
            t: 0,
            lanes: vec![LaneState::default(); s.lanes.len()],
            automation: s.automation.clone().map(AutomationRun::new),
            finished_patterns: Vec::new(),
            stop_reason: None,
        }
    }

    #[must_use]
    pub fn time_ms(&self) -> u64 {
        self.t
    }

    /// Notes and bound-hit reasons from the automation.
    #[must_use]
    pub fn log(&self) -> Vec<String> {
        self.automation
            .as_ref()
            .map(|a| a.log.clone())
            .unwrap_or_default()
    }

    fn lane_value(&mut self, i: usize, period: u64) -> Option<u8> {
        let lane = &self.s.lanes[i];
        let t = self.t;
        let state = &mut self.lanes[i];
        if state.stopped {
            return None;
        }
        match lane.source {
            LaneSource::Curve(c) => {
                let (samples, p) = &self.s.curves[c];
                samples.get((t / p) as usize).copied()
            }
            LaneSource::Manual(var) => self.automation.as_ref().map(|a| quantize(a.var(var))),
            LaneSource::Automation(_) => {
                if let Some((pattern, start)) = state.player {
                    let (coarse, fine) = &self.s.playable[pattern];
                    let grid = if period == TCODE_FRAME_MS {
                        fine
                    } else {
                        coarse
                    };
                    match grid.get(((t - start) / period) as usize) {
                        Some(v) => return Some(*v),
                        None => {
                            state.player = None;
                            state.value = None;
                            if !self.finished_patterns.contains(&pattern) {
                                self.finished_patterns.push(pattern);
                            }
                        }
                    }
                }
                state.value
            }
        }
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            let target_lanes: Vec<usize> = match &effect {
                Effect::StopAll => (0..self.lanes.len()).collect(),
                Effect::Play { target, .. }
                | Effect::StopPattern { target }
                | Effect::Set { target, .. } => self
                    .s
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| matches!(l.source, LaneSource::Automation(x) if x == *target))
                    .map(|(i, _)| i)
                    .collect(),
            };
            for i in target_lanes {
                let state = &mut self.lanes[i];
                match &effect {
                    Effect::StopAll => {
                        state.player = None;
                        state.value = None;
                        state.stopped =
                            !matches!(self.s.lanes[i].source, LaneSource::Automation(_));
                    }
                    Effect::StopPattern { .. } => {
                        state.player = None;
                        state.value = None;
                    }
                    Effect::Set { value, .. } => {
                        state.player = None;
                        state.value = Some(*value);
                    }
                    Effect::Play { pattern, .. } => state.player = Some((*pattern, self.t)),
                }
            }
        }
    }

    /// Advance one quantum. `None` when the session is over (the bound
    /// is reached, or every source is exhausted and zeroed).
    pub fn step(&mut self, events: &[Event]) -> Option<Vec<Proposed>> {
        if self.t >= self.s.duration_ms {
            if self.stop_reason.is_none() {
                self.stop_reason = Some("session_duration".into());
                if let Some(a) = self.automation.as_mut() {
                    a.finish();
                }
            }
            return None;
        }
        let mut out = Vec::new();
        let tick = self.t.is_multiple_of(TICK_MS);
        if tick {
            if let Some(a) = self.automation.as_mut() {
                let mut all: Vec<Event> =
                    events.iter().take(EVENTS_PER_TICK_MAX).cloned().collect();
                all.extend(self.finished_patterns.drain(..).map(Event::PatternFinished));
                let effects = a.step(&all);
                self.apply(effects);
            }
            for i in 0..self.lanes.len() {
                let Emit::Feature { feature, max } = self.s.lanes[i].emit else {
                    continue;
                };
                let value = self.lane_value(i, TICK_MS).unwrap_or(0);
                let state = &mut self.lanes[i];
                let due = match state.last {
                    None => value > 0,
                    Some((last, at)) => {
                        last != value || (value > 0 && self.t - at >= FEATURE_REFRESH_MS)
                    }
                };
                if due {
                    state.last = Some((value, self.t));
                    out.push(Proposed::Feature {
                        device: self.s.lanes[i].device,
                        feature,
                        max,
                        percent: value,
                    });
                }
            }
        }
        if self.t.is_multiple_of(TCODE_FRAME_MS) {
            for device in 0..self.s.devices.len() {
                let mut values = Vec::new();
                for i in 0..self.lanes.len() {
                    if self.s.lanes[i].device != device {
                        continue;
                    }
                    if let Emit::Axis(axis) = self.s.lanes[i].emit
                        && let Some(v) = self.lane_value(i, TCODE_FRAME_MS)
                    {
                        values.push((axis, v));
                    }
                }
                if !values.is_empty() {
                    out.push(Proposed::Axes { device, values });
                }
            }
        }
        // Early completion: no automation, every curve exhausted, every
        // feature lane zeroed.
        if self.automation.is_none() && out.is_empty() && tick {
            let exhausted = self.s.lanes.iter().enumerate().all(|(i, lane)| {
                let LaneSource::Curve(c) = lane.source else {
                    return false;
                };
                let (samples, p) = &self.s.curves[c];
                let done = self.t / p >= samples.len() as u64;
                done && self.lanes[i].last.is_none_or(|(v, _)| v == 0)
            });
            if exhausted {
                self.stop_reason = Some("complete".into());
                return None;
            }
        }
        self.t += QUANTUM_MS;
        Some(out)
    }
}

/// What the gate approved, ready for a provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gated {
    Output {
        device: usize,
        provider: ProviderId,
        plan: AllowedOutput,
    },
    Axes {
        device: usize,
        frame: AllowedAxes,
    },
}

/// The selector a device's provider resolves (the server's `retarget`
/// rule): the raw toy id for Lovense routes, the unified id otherwise.
fn selector(device: &Device) -> String {
    match device.provider {
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
    }
}

/// The TCode view the axis gate needs, read from the provider.
pub struct AxisView<'a> {
    pub supported: &'a [usize],
    pub limits: [AxisLimit; 6],
    pub last: [Option<u8>; 6],
}

/// The ONLY path from a proposal to a plan: the existing gate. A deny
/// here is final for the command (and, for not-armed, for the run).
pub fn gate_proposed(
    session: &CompiledSession,
    proposed: &Proposed,
    snapshot: &GateSnapshot,
    axes: Option<&AxisView<'_>>,
) -> Result<Gated> {
    match proposed {
        Proposed::Feature {
            device,
            feature,
            max,
            percent,
        } => {
            let d = &session.devices[*device];
            let strength = (i64::from(*percent) * i64::from(*max) + 50) / 100;
            let intent = OutputIntent::Function(FunctionIntent {
                feature: (*feature).to_string(),
                strength,
                time_sec: 1,
                toy: Some(selector(d)),
            });
            let plan = decide_plan(snapshot, &intent, &d.caps())?;
            Ok(Gated::Output {
                device: *device,
                provider: d.provider,
                plan,
            })
        }
        Proposed::Axes { device, values } => {
            let view = axes.ok_or_else(|| {
                AcheError::new(
                    ErrorCode::ProviderUnavailable,
                    "no tcode provider view for an axis frame",
                )
            })?;
            let intent = AxesIntent {
                values: values.iter().map(|(a, v)| (*a, i64::from(*v))).collect(),
                interval_ms: TCODE_FRAME_INTERVAL_MS,
            };
            let frame = decide_axes(snapshot, &intent, view.supported, &view.limits, &view.last)?;
            Ok(Gated::Axes {
                device: *device,
                frame,
            })
        }
    }
}

/// The canonical artefact line of one gated command (replay hashing).
#[must_use]
pub fn artefact_line(session: &CompiledSession, t_ms: u64, gated: &Gated) -> String {
    match gated {
        Gated::Output { device, plan, .. } => {
            let wire = match plan {
                AllowedOutput::Function {
                    feature, strength, ..
                } => format!("{feature}:{strength}"),
                other => format!("{other:?}"),
            };
            format!("{t_ms} {} {wire}", session.devices[*device].id)
        }
        Gated::Axes { device, frame } => format!(
            "{t_ms} {} {}",
            session.devices[*device].id,
            crate::decide::tcode_wire(frame).trim_end()
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::funscript::import_pack;
    use crate::provider::DeviceProvider;
    use crate::provider::FakeButtplug;
    use crate::provider_tcode::{DeviceKind, RecordingLink, TcodeProvider};
    use serde_json::json;

    pub(crate) fn devices() -> Vec<Device> {
        let mut d = FakeButtplug::fixture_devices();
        let limits = [AxisLimit {
            min: 0,
            max: 100,
            max_step: 100,
        }; 6];
        let tcode = TcodeProvider::new(
            "mock",
            DeviceKind::Sr6,
            limits,
            Box::new(RecordingLink::new()),
        );
        d.extend(tcode.discover().unwrap());
        d
    }

    pub(crate) fn armed() -> GateSnapshot {
        GateSnapshot {
            armed: true,
            target_configured: true,
            ceiling: 20,
        }
    }

    pub(crate) struct Report {
        pub lines: Vec<String>,
        pub hash: String,
        pub clamps: u32,
        pub stop_reason: String,
        pub log: Vec<String>,
    }

    /// Replay a compiled session against a recording sink, with an
    /// event log keyed by tick time; the gate runs on every command.
    pub(crate) fn replay(
        s: &CompiledSession,
        ceiling: u8,
        events: &[(u64, Event)],
        limits: [AxisLimit; 6],
    ) -> Report {
        let mut run = SessionRun::new(s);
        let mut lines = Vec::new();
        let mut clamps = 0;
        let mut last = [None; 6];
        let snapshot = GateSnapshot { ceiling, ..armed() };
        loop {
            let t = run.time_ms();
            let now: Vec<Event> = events
                .iter()
                .filter(|(at, _)| *at == t)
                .map(|(_, e)| e.clone())
                .collect();
            let Some(proposals) = run.step(&now) else {
                break;
            };
            for p in proposals {
                let view = AxisView {
                    supported: DeviceKind::Sr6.axes(),
                    limits,
                    last,
                };
                let gated = gate_proposed(s, &p, &snapshot, Some(&view))
                    .expect("armed fixture gate allows");
                match &gated {
                    Gated::Output { plan, .. } => clamps += u32::from(plan.clamped()),
                    Gated::Axes { frame, .. } => {
                        clamps += frame.clamps;
                        for &(a, v) in &frame.values {
                            last[a] = Some(v);
                        }
                    }
                }
                lines.push(artefact_line(s, t, &gated));
            }
        }
        let hash = content_hash(lines.join("\n").as_bytes());
        Report {
            lines,
            hash,
            clamps,
            stop_reason: run.stop_reason.clone().unwrap_or_default(),
            log: run.log(),
        }
    }

    pub(crate) fn library(patterns: &[Value], automation: Option<Value>) -> Library {
        let mut lib = Library::default();
        for p in patterns {
            let p = Pattern::from_json(p).unwrap();
            lib.patterns.insert(p.id().to_string(), p);
        }
        if let Some(a) = automation {
            let a = Automation::from_json(&a).unwrap();
            lib.automations.insert(a.id().to_string(), a);
        }
        lib
    }

    fn square(id: &str, function: &str, top: u8, ms: u64) -> Value {
        json!({"format": "ache.pattern/v1", "id": id, "function": function,
            "sections": [{"blocks": [{"kind": "square", "top": top, "duration_ms": ms}]}], "loop": "once"})
    }

    fn session(duration: u64, bindings: Value) -> Value {
        json!({"format": SESSION_FORMAT, "id": "s", "duration_ms": duration, "bindings": bindings})
    }

    // --- validation (V5) ---------------------------------------------

    #[test]
    fn v5_fan_out_drives_every_bound_function() {
        let scripted = json!({"format": "ache.pattern/v1", "id": "fan", "function": "Generic",
            "scripted": {"base": 0, "expr": "20 + index * 30", "duration_ms": 500}, "loop": "once"});
        let lib = library(&[scripted], None);
        let s = CompiledSession::compile(&session(1000, json!([
            {"id": "a", "device": "buttplug:0", "function": "Vibrate", "source": {"pattern": "fan"}},
            {"id": "b", "device": "buttplug:1", "function": "Vibrate", "source": {"pattern": "fan"}},
            {"id": "c", "device": "buttplug:1", "function": "Rotate", "source": {"pattern": "fan"}},
            {"id": "d", "device": "tcode:0", "function": "Twist", "source": {"pattern": "fan"}}
        ])), &lib, &devices()).unwrap();
        let r = replay(
            &s,
            20,
            &[],
            [AxisLimit {
                min: 0,
                max: 100,
                max_step: 100,
            }; 6],
        );
        // index 0 → 20 %, 1 → 50 %, 2 → 80 % of 20 = 4, 10, 16.
        assert!(
            r.lines.contains(&"0 buttplug:0 Vibrate:4".to_string()),
            "{:?}",
            r.lines
        );
        assert!(r.lines.contains(&"0 buttplug:1 Vibrate:10".to_string()));
        assert!(r.lines.contains(&"0 buttplug:1 Rotate:16".to_string()));
        assert!(
            r.lines.contains(&"0 tcode:0 R09999I20".to_string()),
            "index 3 → 110 → 100 %"
        );
        // The pattern ends at 500 ms: every feature lane is zeroed.
        assert!(r.lines.contains(&"500 buttplug:0 Vibrate:0".to_string()));
        assert_eq!(r.stop_reason, "complete");
    }

    #[test]
    fn v5_a_pack_binds_six_axes_and_streams_at_50_hz() {
        let mut lib = Library::default();
        let script =
            |a: i64, b: i64| json!({"actions": [{"at": 0, "pos": a}, {"at": 200, "pos": b}]});
        let pack = import_pack(
            &json!({"format": "ache.pack/v1", "id": "scene", "channels": {
            "stroke": script(0, 100), "surge": script(50, 50), "sway": script(50, 60),
            "twist": script(50, 40), "roll": script(50, 50), "pitch": script(50, 55)}}),
        )
        .unwrap();
        lib.packs.insert("scene".into(), pack);
        let s = CompiledSession::compile(
            &session(
                1000,
                json!([
                    {"id": "p", "device": "tcode:0", "source": {"pack": "scene"}}
                ]),
            ),
            &lib,
            &devices(),
        )
        .unwrap();
        assert_eq!(s.lanes.len(), 6);
        let r = replay(
            &s,
            20,
            &[],
            [AxisLimit {
                min: 0,
                max: 100,
                max_step: 100,
            }; 6],
        );
        assert_eq!(r.lines.len(), 10, "200 ms at 20 ms per frame");
        assert_eq!(
            r.lines[0],
            "0 tcode:0 L00000I20 L15000I20 L25000I20 R05000I20 R15000I20 R25000I20"
        );
        assert!(r.lines[5].starts_with("100 tcode:0 L05000I20"));
    }

    // --- adversarial (V5 conflict, A2, A4, A5) -----------------------

    #[test]
    fn v5_conflicting_bindings_refuse_to_load() {
        let lib = library(
            &[
                square("a", "Vibrate", 10, 500),
                square("b", "Generic", 10, 500),
            ],
            None,
        );
        for bindings in [
            json!([
                {"id": "x", "device": "buttplug:0", "function": "Vibrate", "source": {"pattern": "a"}},
                {"id": "y", "device": "buttplug:0", "function": "Vibrate", "source": {"pattern": "b"}}
            ]),
            // The same axis twice under different binding names.
            json!([
                {"id": "x", "device": "tcode:0", "function": "Surge", "source": {"pattern": "b"}},
                {"id": "y", "device": "tcode:0", "function": "Surge", "source": {"pattern": "b"}}
            ]),
        ] {
            let error =
                CompiledSession::compile(&session(1000, bindings), &lib, &devices()).unwrap_err();
            assert!(
                error.message.contains("one source per device function"),
                "{}",
                error.message
            );
        }
    }

    #[test]
    fn a5_unresolvable_compositions_refuse_to_load() {
        let lib = library(&[square("v", "Vibrate", 10, 500)], None);
        for (bindings, why) in [
            (
                json!([{"id": "x", "device": "buttplug:9", "function": "Vibrate", "source": {"pattern": "v"}}]),
                "unknown device",
            ),
            (
                json!([{"id": "x", "device": "buttplug:0", "function": "Rotate", "source": {"pattern": "v"}}]),
                "function not exposed",
            ),
            (
                json!([{"id": "x", "device": "buttplug:1", "function": "Rotate", "source": {"pattern": "v"}}]),
                "pattern/function mismatch",
            ),
            (
                json!([{"id": "x", "device": "buttplug:0", "function": "Vibrate", "source": {"pattern": "ghost"}}]),
                "unknown pattern",
            ),
            (
                json!([{"id": "x", "device": "buttplug:0", "function": "Vibrate", "source": "automation"}]),
                "no automation",
            ),
            (
                json!([{"id": "x", "device": "buttplug:0", "function": "Generic", "source": {"pattern": "v"}}]),
                "generic binding",
            ),
            (
                json!([{"id": "x", "device": "tcode:0", "function": "Vibrate", "source": {"pattern": "v"}}]),
                "vibrate on tcode",
            ),
            (json!([]), "no bindings"),
        ] {
            assert!(
                CompiledSession::compile(&session(1000, bindings), &lib, &devices()).is_err(),
                "{why}"
            );
        }
        assert!(CompiledSession::compile(&session(0, json!([{"id": "x", "device": "buttplug:0", "function": "Vibrate", "source": {"pattern": "v"}}])), &lib, &devices()).is_err());
        assert!(CompiledSession::compile(&session(SESSION_DURATION_MS_MAX + 1, json!([{"id": "x", "device": "buttplug:0", "function": "Vibrate", "source": {"pattern": "v"}}])), &lib, &devices()).is_err());
    }

    #[test]
    fn a5_a_pack_missing_an_axis_on_the_device_loads_none() {
        let mut lib = Library::default();
        let script = json!({"actions": [{"at": 0, "pos": 0}, {"at": 200, "pos": 100}]});
        let pack = import_pack(&json!({"format": "ache.pack/v1", "id": "pk", "channels": {"stroke": script, "surge": script}})).unwrap();
        lib.packs.insert("pk".into(), pack);
        // An OSR2 has no surge: the whole pack refuses.
        let mut devs = FakeButtplug::fixture_devices();
        let limits = [AxisLimit {
            min: 0,
            max: 100,
            max_step: 100,
        }; 6];
        devs.extend(
            TcodeProvider::new(
                "mock",
                DeviceKind::Osr2,
                limits,
                Box::new(RecordingLink::new()),
            )
            .discover()
            .unwrap(),
        );
        let error = CompiledSession::compile(
            &session(
                1000,
                json!([{"id": "p", "device": "tcode:0", "source": {"pack": "pk"}}]),
            ),
            &lib,
            &devs,
        )
        .unwrap_err();
        assert!(error.message.contains("all-or-none"));
    }

    #[test]
    fn a2_pieces_legal_alone_but_over_the_session_bound_refuse_at_load() {
        let lib = library(&[square("long", "Vibrate", 10, 2000)], None);
        let error = CompiledSession::compile(&session(1000, json!([
            {"id": "x", "device": "buttplug:0", "function": "Vibrate", "source": {"pattern": "long"}}
        ])), &lib, &devices()).unwrap_err();
        assert!(error.message.contains("exceeds the session bound"));
        // A queue whose delays are each legal but sum past the bound.
        let automation = json!({"format": "ache.automation/v1", "id": "a",
            "controls": [{"id": "stop", "kind": "stop_button"}],
            "queues": [{"id": "q", "loop": {"repeat": 3}, "actions": [{"delay": {"ms": 400}}]}]});
        let lib = library(&[square("v", "Vibrate", 10, 500)], Some(automation));
        let mut spec = session(
            1000,
            json!([{"id": "x", "device": "buttplug:0", "function": "Vibrate", "source": "automation"}]),
        );
        spec["automation"] = json!("a");
        let error = CompiledSession::compile(&spec, &lib, &devices()).unwrap_err();
        assert!(error.message.contains("delays sum"), "{}", error.message);
    }

    #[test]
    fn a4_events_with_values_or_unregistered_names_are_refused() {
        let automation = json!({"format": "ache.automation/v1", "id": "a",
            "variables": [{"id": "lvl", "type": "number", "init": 0, "min": 0, "max": 50}],
            "controls": [{"id": "stop", "kind": "stop_button"}, {"id": "dial", "kind": "slider", "var": "lvl"}]});
        let lib = library(&[], Some(automation));
        let mut spec = session(
            1000,
            json!([{"id": "m", "device": "buttplug:0", "function": "Vibrate", "source": {"manual": "dial"}}]),
        );
        spec["automation"] = json!("a");
        spec["inputs"] = json!([{"id": "voice"}, {"id": "safeword", "stop": true}]);
        let s = CompiledSession::compile(&spec, &lib, &devices()).unwrap();
        assert!(
            s.event("external", "voice", Some(90.0)).is_err(),
            "intensity payload"
        );
        assert!(
            s.event("external", "webhook", None).is_err(),
            "unregistered source"
        );
        assert!(
            s.event("slider", "ghost", Some(10.0)).is_err(),
            "unregistered control"
        );
        assert!(
            s.event("press", "dial", None).is_err(),
            "wrong kind for control"
        );
        assert!(s.event("slider", "dial", Some(f64::NAN)).is_err());
        assert!(s.event("external", "voice", None).is_ok());
        assert!(s.is_stop_input("safeword") && !s.is_stop_input("voice"));
        assert!(s.is_stop_control("stop"));
        // Slider writes clamp to the variable's range; the gate clamps
        // to the ceiling after that.
        let events = [(0, s.event("slider", "dial", Some(1e9)).unwrap())];
        let r = replay(
            &s,
            5,
            &events,
            [AxisLimit {
                min: 0,
                max: 100,
                max_step: 100,
            }; 6],
        );
        assert_eq!(r.lines[0], "0 buttplug:0 Vibrate:5");
        assert!(r.clamps >= 1);
    }
}
