//! `ache.automation/v1` (XToys spec §2): closed trigger and action
//! sets, jobs with REQUIRED `max_runs` + `max_span_ms`, queues frozen at
//! load, typed range-clamped session-scoped variables, and operator
//! controls with a mandatory stop button.
//!
//! An automation is data and its runtime is a pure state machine: one
//! `step` per 250 ms tick, a function of the frozen spec, the declared
//! seed, and the ordered event log. It produces *effects* (play/stop a
//! pattern on a target, set a target's level) that the session layer
//! turns into proposed commands for the gate — the automation has no
//! path to a provider and no priority lane.
//!
//! Rejected outright (§2.3): JavaScript, webhooks in or out, runtime-
//! editable queues, persistent or externally settable variables,
//! unbounded loops. External input is an `external_event` naming a
//! pre-registered source, carrying no value.
//!
//! Tick order (normative for replay): apply last tick's deferred
//! job/queue starts and stops → apply this tick's events → evaluate
//! time and edge triggers → run `on_load` (tick 0) → start triggered
//! jobs → run jobs in declaration order → run queues in declaration
//! order. Start/stop actions take effect on the next tick, so no tick
//! can cascade without bound.

use serde::Deserialize;
use serde_json::Value;

use crate::error::{AcheError, ErrorCode, Result};
use crate::expr::{Env, Expr, RESERVED_NAMES};
use crate::pattern::{LoopPolicy, invalid, quantize, valid_id};

pub const AUTOMATION_FORMAT: &str = "ache.automation/v1";
pub const TICK_MS: u64 = 250;
pub const JOB_MAX_RUNS_MAX: u32 = 10_000;
pub const JOB_ACTIONS_MAX: usize = 64;
pub const QUEUE_ACTIONS_MAX: usize = 256;
/// Per-list bounds on declarations (not in §2.4; named here so no list
/// is unbounded).
pub const DECLARATIONS_MAX: usize = 64;
pub const NOTIFY_BYTES_MAX: usize = 256;
/// Most notes / bound-hit reasons kept in a run artefact.
pub const LOG_ENTRIES_MAX: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cmp {
    Lt,
    Le,
    Eq,
    Ne,
    Ge,
    Gt,
}

impl Cmp {
    fn holds(self, a: f64, b: f64) -> bool {
        match self {
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
            Cmp::Eq => a == b,
            Cmp::Ne => a != b,
            Cmp::Ge => a >= b,
            Cmp::Gt => a > b,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TriggerKind {
    SessionStart,
    SessionStop,
    ControlPressed(String),
    ControlHeld { control: String, ms: u64 },
    TimerElapsed(String),
    VariableCrossed { var: String, cmp: Cmp, value: f64 },
    ToyConnected(String),
    ToyDisconnected(String),
    PatternFinished(String),
    ExternalEvent(String),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerSpec {
    pub id: String,
    pub on: TriggerKind,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionSpec {
    PlayPattern {
        pattern: String,
        target: String,
    },
    StopPattern {
        target: String,
    },
    StopAll,
    /// Spec extension S-2: §2.1 gives `u8`; ache admits a §1.2
    /// expression (a literal is the common case) so variable-driven
    /// levels exist — and are clamped like everything else.
    SetFunction {
        target: String,
        value: String,
    },
    SetVariable {
        var: String,
        expr: String,
    },
    AddVariable {
        var: String,
        expr: String,
    },
    StartJob(String),
    StopJob(String),
    StartQueue(String),
    StopQueue(String),
    Delay {
        ms: u64,
    },
    Notify(String),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Repeat {
    Once,
    Every { ms: u64 },
    While(String),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    pub id: String,
    #[serde(default)]
    pub trigger: Option<String>,
    pub actions: Vec<ActionSpec>,
    pub repeat: Repeat,
    /// REQUIRED: no default, no infinity.
    pub max_runs: u32,
    /// REQUIRED wall-clock bound.
    pub max_span_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueSpec {
    pub id: String,
    pub actions: Vec<ActionSpec>,
    #[serde(rename = "loop")]
    pub loop_policy: LoopPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VarType {
    Number,
    Bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariableSpec {
    pub id: String,
    #[serde(rename = "type")]
    pub var_type: VarType,
    pub init: Value,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Slider,
    Button,
    Toggle,
    StopButton,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlSpec {
    pub id: String,
    pub kind: ControlKind,
    #[serde(default)]
    pub var: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSpec {
    pub format: String,
    pub id: String,
    #[serde(default)]
    pub triggers: Vec<TriggerSpec>,
    #[serde(default)]
    pub jobs: Vec<JobSpec>,
    #[serde(default)]
    pub queues: Vec<QueueSpec>,
    #[serde(default)]
    pub variables: Vec<VariableSpec>,
    pub controls: Vec<ControlSpec>,
    #[serde(default)]
    pub on_load: Vec<ActionSpec>,
}

/// A validated automation, not yet bound to a session's targets.
#[derive(Clone, Debug)]
pub struct Automation {
    spec: AutomationSpec,
    hash: String,
}

/// Names a session supplies when binding an automation.
pub struct BindContext<'a> {
    pub targets: &'a [String],
    pub patterns: &'a [String],
    pub sources: &'a [String],
    pub seeded: bool,
    pub session_ms: u64,
}

// ------------------------------------------------------ compiled form

#[derive(Clone, Debug)]
enum Trig {
    SessionStart,
    SessionStop,
    Pressed(usize),
    Held(usize, u64),
    Timer(usize),
    Crossed(usize, Cmp, f64),
    ToyConnected(String),
    ToyDisconnected(String),
    PatternFinished(usize),
    External(usize),
}

#[derive(Clone, Debug)]
enum Act {
    Play(usize, usize),
    StopPattern(usize),
    StopAll,
    Set(usize, Expr),
    SetVar(usize, Expr),
    AddVar(usize, Expr),
    StartJob(usize),
    StopJob(usize),
    StartQueue(usize),
    StopQueue(usize),
    Delay(u64),
    Notify(String),
}

#[derive(Clone, Debug)]
enum Rep {
    Once,
    Every(u64),
    While(Expr),
}

#[derive(Clone, Debug)]
struct Job {
    id: String,
    trigger: Option<usize>,
    actions: Vec<Act>,
    repeat: Rep,
    max_runs: u32,
    max_span_ticks: u64,
}

#[derive(Clone, Debug)]
struct Queue {
    actions: Vec<Act>,
    repeat: u32,
}

#[derive(Clone, Debug)]
struct Var {
    min: f64,
    max: f64,
    boolean: bool,
    init: f64,
}

#[derive(Clone, Debug)]
pub struct CompiledAutomation {
    triggers: Vec<Trig>,
    jobs: Vec<Job>,
    queues: Vec<Queue>,
    vars: Vec<Var>,
    controls: Vec<(ControlKind, Option<usize>)>,
    control_ids: Vec<String>,
    on_load: Vec<Act>,
    seed: Option<u64>,
}

/// What the session layer must do. Targets and patterns are indices
/// into the session's own lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Play { pattern: usize, target: usize },
    StopPattern { target: usize },
    StopAll,
    Set { target: usize, value: u8 },
}

/// An input to one tick. Every variant names something pre-declared;
/// none carries an intensity, a pattern, or an action.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Press(usize),
    Release(usize),
    Slider(usize, f64),
    Toggle(usize),
    External(usize),
    ToyConnected(String),
    ToyDisconnected(String),
    PatternFinished(usize),
}

fn id_list<'a>(kind: &str, ids: impl Iterator<Item = &'a str>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        valid_id(kind, id)?;
        if out.iter().any(|known| known == id) {
            return invalid(format!("{kind} id {id:?} is declared twice"));
        }
        out.push(id.to_string());
    }
    if out.len() > DECLARATIONS_MAX {
        return invalid(format!("more than {DECLARATIONS_MAX} {kind}s"));
    }
    Ok(out)
}

fn index_of(kind: &str, list: &[String], id: &str) -> Result<usize> {
    list.iter().position(|known| known == id).ok_or_else(|| {
        AcheError::new(
            ErrorCode::RequestInvalid,
            format!("unknown {kind} id {id:?}"),
        )
    })
}

fn ticks_ceil(ms: u64) -> u64 {
    ms.div_ceil(TICK_MS)
}

impl Automation {
    pub fn from_json(value: &Value) -> Result<Automation> {
        let spec: AutomationSpec = serde_json::from_value(value.clone()).map_err(|e| {
            AcheError::new(
                ErrorCode::RequestInvalid,
                format!("invalid automation: {e}"),
            )
        })?;
        if spec.format != AUTOMATION_FORMAT {
            return invalid(format!("automation format must be {AUTOMATION_FORMAT:?}"));
        }
        valid_id("automation", &spec.id)?;
        let automation = Automation {
            hash: crate::pattern::content_hash(
                serde_json::to_vec(value)
                    .expect("a JSON value serializes")
                    .as_slice(),
            ),
            spec,
        };
        // Structural validation without a session: every internal id
        // resolves, every bound is present; targets/patterns/sources are
        // taken as declared by their use and re-checked at session bind.
        let targets = automation.referenced(|a| match a {
            ActionSpec::PlayPattern { target, .. }
            | ActionSpec::StopPattern { target }
            | ActionSpec::SetFunction { target, .. } => Some(target.clone()),
            _ => None,
        });
        let patterns = automation.referenced(|a| match a {
            ActionSpec::PlayPattern { pattern, .. } => Some(pattern.clone()),
            _ => None,
        });
        let mut sources: Vec<String> = automation
            .spec
            .triggers
            .iter()
            .filter_map(|t| match &t.on {
                TriggerKind::ExternalEvent(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        sources.dedup();
        let mut patterns = patterns;
        for t in &automation.spec.triggers {
            if let TriggerKind::PatternFinished(p) = &t.on {
                patterns.push(p.clone());
            }
        }
        patterns.sort();
        patterns.dedup();
        sources.sort();
        sources.dedup();
        automation.bind(&BindContext {
            targets: &targets,
            patterns: &patterns,
            sources: &sources,
            seeded: true,
            session_ms: crate::session::SESSION_DURATION_MS_MAX,
        })?;
        Ok(automation)
    }

    fn referenced(&self, pick: impl Fn(&ActionSpec) -> Option<String>) -> Vec<String> {
        let lists = std::iter::once(&self.spec.on_load)
            .chain(self.spec.jobs.iter().map(|j| &j.actions))
            .chain(self.spec.queues.iter().map(|q| &q.actions));
        let mut out: Vec<String> = lists.flatten().filter_map(&pick).collect();
        out.sort();
        out.dedup();
        out
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.spec.id
    }

    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// Ids of the controls, in order (the operator surface).
    #[must_use]
    pub fn control_ids(&self) -> Vec<String> {
        self.spec.controls.iter().map(|c| c.id.clone()).collect()
    }

    /// The variable a slider control writes, by control id.
    pub fn slider_var(&self, control: &str) -> Option<usize> {
        let var = self
            .spec
            .controls
            .iter()
            .find(|c| c.id == control && c.kind == ControlKind::Slider)?
            .var
            .as_ref()?;
        self.spec.variables.iter().position(|v| &v.id == var)
    }

    /// The static duration of each queue's delays, ms (A2 static sum).
    #[must_use]
    pub fn queue_delay_totals(&self) -> Vec<(String, u64)> {
        self.spec
            .queues
            .iter()
            .map(|q| {
                let per_loop: u64 = q
                    .actions
                    .iter()
                    .map(|a| match a {
                        ActionSpec::Delay { ms } => *ms,
                        _ => 0,
                    })
                    .fold(0u64, u64::saturating_add);
                (
                    q.id.clone(),
                    per_loop.saturating_mul(u64::from(q.loop_policy.count())),
                )
            })
            .collect()
    }

    /// Resolve every id against a session and compile.
    pub fn bind(&self, ctx: &BindContext<'_>) -> Result<CompiledAutomation> {
        let spec = &self.spec;
        let trigger_ids = id_list("trigger", spec.triggers.iter().map(|t| t.id.as_str()))?;
        let job_ids = id_list("job", spec.jobs.iter().map(|j| j.id.as_str()))?;
        let queue_ids = id_list("queue", spec.queues.iter().map(|q| q.id.as_str()))?;
        let var_ids = id_list("variable", spec.variables.iter().map(|v| v.id.as_str()))?;
        let control_ids = id_list("control", spec.controls.iter().map(|c| c.id.as_str()))?;
        for id in &var_ids {
            if RESERVED_NAMES.contains(&id.as_str()) || matches!(id.as_str(), "time" | "seed") {
                return invalid(format!("variable id {id:?} is a reserved name"));
            }
            if !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || id.as_bytes()[0].is_ascii_digit()
            {
                return invalid(format!("variable id {id:?} must be an identifier"));
            }
        }
        // Variables.
        let mut vars = Vec::new();
        for v in &spec.variables {
            let var = match v.var_type {
                VarType::Number => {
                    let (Some(min), Some(max)) = (v.min, v.max) else {
                        return invalid(format!("number variable {:?} needs min and max", v.id));
                    };
                    if !min.is_finite() || !max.is_finite() || min > max {
                        return invalid(format!(
                            "variable {:?}: min/max must be finite, min <= max",
                            v.id
                        ));
                    }
                    let Some(init) = v.init.as_f64().filter(|x| x.is_finite()) else {
                        return invalid(format!("variable {:?}: init must be a number", v.id));
                    };
                    Var {
                        min,
                        max,
                        boolean: false,
                        init: init.clamp(min, max),
                    }
                }
                VarType::Bool => {
                    if v.min.is_some() || v.max.is_some() {
                        return invalid(format!("bool variable {:?} takes no range", v.id));
                    }
                    let Some(init) = v.init.as_bool() else {
                        return invalid(format!("variable {:?}: init must be a bool", v.id));
                    };
                    Var {
                        min: 0.0,
                        max: 1.0,
                        boolean: true,
                        init: f64::from(u8::from(init)),
                    }
                }
            };
            vars.push(var);
        }
        let mut names: Vec<&str> = var_ids.iter().map(String::as_str).collect();
        names.push("time");
        let parse = |source: &str| Expr::parse(source, &names, ctx.seeded);
        // Controls.
        let mut controls = Vec::new();
        let mut stop_buttons = 0;
        for c in &spec.controls {
            let var = match (c.kind, &c.var) {
                (ControlKind::Slider, Some(v)) => {
                    let index = index_of("variable", &var_ids, v)?;
                    if vars[index].boolean {
                        return invalid(format!("slider {:?} needs a number variable", c.id));
                    }
                    Some(index)
                }
                (ControlKind::Toggle, Some(v)) => {
                    let index = index_of("variable", &var_ids, v)?;
                    if !vars[index].boolean {
                        return invalid(format!("toggle {:?} needs a bool variable", c.id));
                    }
                    Some(index)
                }
                (ControlKind::Button | ControlKind::StopButton, None) => None,
                _ => {
                    return invalid(format!(
                        "control {:?}: var is required for sliders/toggles only",
                        c.id
                    ));
                }
            };
            if c.kind == ControlKind::StopButton {
                stop_buttons += 1;
            }
            controls.push((c.kind, var));
        }
        if stop_buttons == 0 {
            return invalid("every automation declares a stop_button control (§2.1)");
        }
        // Triggers.
        let mut triggers = Vec::new();
        for t in &spec.triggers {
            triggers.push(match &t.on {
                TriggerKind::SessionStart => Trig::SessionStart,
                TriggerKind::SessionStop => Trig::SessionStop,
                TriggerKind::ControlPressed(c) => {
                    let index = index_of("control", &control_ids, c)?;
                    if controls[index].0 != ControlKind::Button {
                        return invalid(format!("control_pressed {c:?} must name a button"));
                    }
                    Trig::Pressed(index)
                }
                TriggerKind::ControlHeld { control, ms } => {
                    let index = index_of("control", &control_ids, control)?;
                    if controls[index].0 != ControlKind::Button || *ms == 0 || *ms > ctx.session_ms
                    {
                        return invalid(format!(
                            "control_held {control:?}: a button and 1..=session ms"
                        ));
                    }
                    Trig::Held(index, ticks_ceil(*ms))
                }
                TriggerKind::TimerElapsed(j) => Trig::Timer(index_of("job", &job_ids, j)?),
                TriggerKind::VariableCrossed { var, cmp, value } => {
                    if !value.is_finite() {
                        return invalid("variable_crossed value must be finite");
                    }
                    Trig::Crossed(index_of("variable", &var_ids, var)?, *cmp, *value)
                }
                TriggerKind::ToyConnected(k) => Trig::ToyConnected(k.clone()),
                TriggerKind::ToyDisconnected(k) => Trig::ToyDisconnected(k.clone()),
                TriggerKind::PatternFinished(p) => {
                    Trig::PatternFinished(index_of("pattern", ctx.patterns, p)?)
                }
                TriggerKind::ExternalEvent(s) => {
                    Trig::External(index_of("input source", ctx.sources, s)?)
                }
            });
        }
        let compile_actions = |actions: &[ActionSpec],
                               allow_delay: bool,
                               cap: usize,
                               what: &str|
         -> Result<Vec<Act>> {
            if actions.len() > cap {
                return invalid(format!("{what} exceeds {cap} actions"));
            }
            actions
                .iter()
                .map(|a| {
                    Ok(match a {
                        ActionSpec::PlayPattern { pattern, target } => Act::Play(
                            index_of("pattern", ctx.patterns, pattern)?,
                            index_of("target", ctx.targets, target)?,
                        ),
                        ActionSpec::StopPattern { target } => {
                            Act::StopPattern(index_of("target", ctx.targets, target)?)
                        }
                        ActionSpec::StopAll => Act::StopAll,
                        ActionSpec::SetFunction { target, value } => {
                            Act::Set(index_of("target", ctx.targets, target)?, parse(value)?)
                        }
                        ActionSpec::SetVariable { var, expr } => {
                            Act::SetVar(index_of("variable", &var_ids, var)?, parse(expr)?)
                        }
                        ActionSpec::AddVariable { var, expr } => {
                            Act::AddVar(index_of("variable", &var_ids, var)?, parse(expr)?)
                        }
                        ActionSpec::StartJob(j) => Act::StartJob(index_of("job", &job_ids, j)?),
                        ActionSpec::StopJob(j) => Act::StopJob(index_of("job", &job_ids, j)?),
                        ActionSpec::StartQueue(q) => {
                            Act::StartQueue(index_of("queue", &queue_ids, q)?)
                        }
                        ActionSpec::StopQueue(q) => {
                            Act::StopQueue(index_of("queue", &queue_ids, q)?)
                        }
                        ActionSpec::Delay { ms } => {
                            if !allow_delay {
                                return invalid(format!("{what}: delay is queue-internal only"));
                            }
                            if *ms == 0 || *ms > ctx.session_ms {
                                return invalid(format!("{what}: delay must be 1..=session ms"));
                            }
                            Act::Delay(ticks_ceil(*ms))
                        }
                        ActionSpec::Notify(text) => {
                            if text.len() > NOTIFY_BYTES_MAX || text.chars().any(char::is_control) {
                                return invalid(format!(
                                    "{what}: notify text too long or has control characters"
                                ));
                            }
                            Act::Notify(text.clone())
                        }
                    })
                })
                .collect()
        };
        let mut jobs = Vec::new();
        for j in &spec.jobs {
            let what = format!("job {:?}", j.id);
            if j.actions.is_empty() {
                return invalid(format!("{what}: actions must not be empty"));
            }
            if j.max_runs == 0 || j.max_runs > JOB_MAX_RUNS_MAX {
                return invalid(format!("{what}: max_runs must be 1..={JOB_MAX_RUNS_MAX}"));
            }
            if j.max_span_ms == 0 || j.max_span_ms > ctx.session_ms {
                return invalid(format!("{what}: max_span_ms must be 1..=the session bound"));
            }
            let repeat = match &j.repeat {
                Repeat::Once => Rep::Once,
                Repeat::Every { ms } => {
                    if *ms < TICK_MS || *ms > ctx.session_ms {
                        return invalid(format!("{what}: every ms must be {TICK_MS}..=session ms"));
                    }
                    Rep::Every(ticks_ceil(*ms))
                }
                Repeat::While(source) => Rep::While(parse(source)?),
            };
            jobs.push(Job {
                id: j.id.clone(),
                trigger: match &j.trigger {
                    None => None,
                    Some(t) => Some(index_of("trigger", &trigger_ids, t)?),
                },
                actions: compile_actions(&j.actions, false, JOB_ACTIONS_MAX, &what)?,
                repeat,
                max_runs: j.max_runs,
                max_span_ticks: ticks_ceil(j.max_span_ms),
            });
        }
        let mut queues = Vec::new();
        for q in &spec.queues {
            let what = format!("queue {:?}", q.id);
            if q.actions.is_empty() {
                return invalid(format!("{what}: actions must not be empty"));
            }
            let repeat = q.loop_policy.count();
            if repeat == 0 || repeat > JOB_MAX_RUNS_MAX {
                return invalid(format!(
                    "{what}: loop repeat must be 1..={JOB_MAX_RUNS_MAX}"
                ));
            }
            queues.push(Queue {
                actions: compile_actions(&q.actions, true, QUEUE_ACTIONS_MAX, &what)?,
                repeat,
            });
        }
        let on_load = compile_actions(&spec.on_load, false, JOB_ACTIONS_MAX, "on_load")?;
        Ok(CompiledAutomation {
            triggers,
            jobs,
            queues,
            vars,
            controls,
            control_ids,
            on_load,
            seed: None,
        })
    }
}

impl CompiledAutomation {
    #[must_use]
    pub fn with_seed(mut self, seed: Option<u64>) -> CompiledAutomation {
        self.seed = seed;
        self
    }

    pub fn control_index(&self, id: &str) -> Option<usize> {
        self.control_ids.iter().position(|c| c == id)
    }

    #[must_use]
    pub fn control_kind(&self, index: usize) -> ControlKind {
        self.controls[index].0
    }
}

// ------------------------------------------------------------ runtime

#[derive(Clone, Debug, Default)]
struct JobState {
    running: bool,
    started: u64,
    runs: u32,
    next: u64,
}

#[derive(Clone, Debug, Default)]
struct QueueState {
    running: bool,
    pc: usize,
    wait_until: u64,
    loops_done: u32,
}

#[derive(Clone, Copy, Debug)]
enum Deferred {
    StartJob(usize),
    StopJob(usize),
    StartQueue(usize),
    StopQueue(usize),
}

/// One automation run. Pure: same spec, seed, and event log → same
/// effects, tick for tick.
pub struct AutomationRun {
    a: CompiledAutomation,
    tick: u64,
    vars: Vec<f64>,
    jobs: Vec<JobState>,
    queues: Vec<QueueState>,
    deferred: Vec<Deferred>,
    crossed: Vec<bool>,
    pressed_at: Vec<Option<u64>>,
    timers: Vec<usize>,
    /// Notes (Notify) and bound-hit reasons, for the run artefact.
    pub log: Vec<String>,
}

impl AutomationRun {
    #[must_use]
    pub fn new(a: CompiledAutomation) -> AutomationRun {
        let vars: Vec<f64> = a.vars.iter().map(|v| v.init).collect();
        let crossed = a
            .triggers
            .iter()
            .map(|t| match t {
                Trig::Crossed(v, cmp, value) => cmp.holds(vars[*v], *value),
                _ => false,
            })
            .collect();
        AutomationRun {
            jobs: vec![JobState::default(); a.jobs.len()],
            queues: vec![QueueState::default(); a.queues.len()],
            pressed_at: vec![None; a.controls.len()],
            deferred: Vec::new(),
            timers: Vec::new(),
            log: Vec::new(),
            tick: 0,
            vars,
            crossed,
            a,
        }
    }

    /// The current value of a variable (manual sources read sliders).
    #[must_use]
    pub fn var(&self, index: usize) -> f64 {
        self.vars[index]
    }

    fn note(&mut self, text: String) {
        if self.log.len() < LOG_ENTRIES_MAX {
            self.log.push(format!("t={}ms {text}", self.tick * TICK_MS));
        }
    }

    fn write_var(&mut self, index: usize, value: f64) {
        let v = &self.a.vars[index];
        let value = if v.boolean {
            f64::from(u8::from(value != 0.0))
        } else {
            value.clamp(v.min, v.max)
        };
        self.vars[index] = value;
    }

    fn eval(&self, expr: &Expr) -> f64 {
        let mut slots = self.vars.clone();
        slots.push((self.tick * TICK_MS) as f64 / 1000.0);
        expr.eval(&Env {
            slots: &slots,
            seed: self.a.seed,
        })
    }

    fn fire(&mut self, trigger: usize, fired: &mut Vec<usize>) {
        if !fired.contains(&trigger) {
            fired.push(trigger);
        }
    }

    fn start_job(&mut self, j: usize) {
        self.jobs[j] = JobState {
            running: true,
            started: self.tick,
            runs: 0,
            next: self.tick,
        };
    }

    fn exec(&mut self, actions: &[Act], effects: &mut Vec<Effect>) -> Option<u64> {
        for act in actions {
            match act {
                Act::Play(pattern, target) => effects.push(Effect::Play {
                    pattern: *pattern,
                    target: *target,
                }),
                Act::StopPattern(target) => effects.push(Effect::StopPattern { target: *target }),
                Act::StopAll => effects.push(Effect::StopAll),
                Act::Set(target, expr) => {
                    let value = quantize(self.eval(expr));
                    effects.push(Effect::Set {
                        target: *target,
                        value,
                    });
                }
                Act::SetVar(v, expr) => {
                    let value = self.eval(expr);
                    self.write_var(*v, value);
                }
                Act::AddVar(v, expr) => {
                    let value = self.vars[*v] + self.eval(expr);
                    self.write_var(*v, value);
                }
                Act::StartJob(j) => self.deferred.push(Deferred::StartJob(*j)),
                Act::StopJob(j) => self.deferred.push(Deferred::StopJob(*j)),
                Act::StartQueue(q) => self.deferred.push(Deferred::StartQueue(*q)),
                Act::StopQueue(q) => self.deferred.push(Deferred::StopQueue(*q)),
                Act::Delay(ticks) => return Some(*ticks),
                Act::Notify(text) => {
                    let text = format!("notify: {text}");
                    self.note(text);
                }
            }
        }
        None
    }

    /// Advance one 250 ms tick.
    pub fn step(&mut self, events: &[Event]) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut fired: Vec<usize> = Vec::new();
        // 1. Deferred starts and stops from the previous tick.
        for d in std::mem::take(&mut self.deferred) {
            match d {
                Deferred::StartJob(j) => self.start_job(j),
                Deferred::StopJob(j) => self.jobs[j].running = false,
                Deferred::StartQueue(q) => {
                    self.queues[q] = QueueState {
                        running: true,
                        pc: 0,
                        wait_until: self.tick,
                        loops_done: 0,
                    };
                }
                Deferred::StopQueue(q) => self.queues[q].running = false,
            }
        }
        // 2. Events.
        for event in events {
            match event {
                Event::Slider(c, value) => {
                    if let (ControlKind::Slider, Some(v)) = self.a.controls[*c] {
                        self.write_var(v, if value.is_finite() { *value } else { 0.0 });
                    }
                }
                Event::Toggle(c) => {
                    if let (ControlKind::Toggle, Some(v)) = self.a.controls[*c] {
                        let flipped = if self.vars[v] == 0.0 { 1.0 } else { 0.0 };
                        self.write_var(v, flipped);
                    }
                }
                Event::Press(c) => {
                    self.pressed_at[*c] = Some(self.tick);
                    for (t, trig) in self.a.triggers.clone().iter().enumerate() {
                        if matches!(trig, Trig::Pressed(x) if x == c) {
                            self.fire(t, &mut fired);
                        }
                    }
                }
                Event::Release(c) => self.pressed_at[*c] = None,
                Event::External(s) => {
                    for (t, trig) in self.a.triggers.clone().iter().enumerate() {
                        if matches!(trig, Trig::External(x) if x == s) {
                            self.fire(t, &mut fired);
                        }
                    }
                }
                Event::ToyConnected(kind) | Event::ToyDisconnected(kind) => {
                    let connected = matches!(event, Event::ToyConnected(_));
                    for (t, trig) in self.a.triggers.clone().iter().enumerate() {
                        let hit = match trig {
                            Trig::ToyConnected(k) => connected && k == kind,
                            Trig::ToyDisconnected(k) => !connected && k == kind,
                            _ => false,
                        };
                        if hit {
                            self.fire(t, &mut fired);
                        }
                    }
                }
                Event::PatternFinished(p) => {
                    for (t, trig) in self.a.triggers.clone().iter().enumerate() {
                        if matches!(trig, Trig::PatternFinished(x) if x == p) {
                            self.fire(t, &mut fired);
                        }
                    }
                }
            }
        }
        // 3. Time and edge triggers.
        let timers = std::mem::take(&mut self.timers);
        for (t, trig) in self.a.triggers.clone().iter().enumerate() {
            let hit = match trig {
                Trig::SessionStart => self.tick == 0,
                Trig::Held(c, ticks) => {
                    self.pressed_at[*c].is_some_and(|at| self.tick == at + ticks)
                }
                Trig::Timer(j) => timers.contains(j),
                Trig::Crossed(v, cmp, value) => {
                    let now = cmp.holds(self.vars[*v], *value);
                    let edge = now && !self.crossed[t];
                    self.crossed[t] = now;
                    edge
                }
                _ => false,
            };
            if hit {
                self.fire(t, &mut fired);
            }
        }
        // 4. on_load at tick 0, then triggered jobs start now.
        if self.tick == 0 {
            let actions = self.a.on_load.clone();
            self.exec(&actions, &mut effects);
        }
        for j in 0..self.a.jobs.len() {
            if let Some(t) = self.a.jobs[j].trigger
                && fired.contains(&t)
                && !self.jobs[j].running
            {
                self.start_job(j);
            }
        }
        // 5. Jobs.
        for j in 0..self.a.jobs.len() {
            if !self.jobs[j].running {
                continue;
            }
            let job = self.a.jobs[j].clone();
            let state = self.jobs[j].clone();
            if self.tick - state.started >= job.max_span_ticks {
                self.jobs[j].running = false;
                self.note(format!("job {} stopped: max_span", job.id));
                continue;
            }
            if state.runs >= job.max_runs {
                self.jobs[j].running = false;
                self.note(format!("job {} stopped: max_runs", job.id));
                continue;
            }
            let run = match &job.repeat {
                Rep::Once | Rep::Every(_) => self.tick >= state.next,
                Rep::While(expr) => {
                    if self.eval(expr) == 0.0 {
                        self.jobs[j].running = false;
                        continue;
                    }
                    true
                }
            };
            if !run {
                continue;
            }
            self.exec(&job.actions, &mut effects);
            self.timers.push(j);
            let s = &mut self.jobs[j];
            s.runs += 1;
            match job.repeat {
                Rep::Once => s.running = false,
                Rep::Every(ticks) => s.next = self.tick + ticks,
                Rep::While(_) => {}
            }
        }
        // 6. Queues: run until a delay or the end of one pass.
        for q in 0..self.a.queues.len() {
            let state = self.queues[q].clone();
            if !state.running || self.tick < state.wait_until {
                continue;
            }
            let queue = self.a.queues[q].clone();
            let rest = &queue.actions[state.pc..];
            let consumed = rest.iter().position(|a| matches!(a, Act::Delay(_)));
            match consumed {
                Some(offset) => {
                    let delay = self
                        .exec(&rest[..=offset], &mut effects)
                        .expect("ends at a delay");
                    let s = &mut self.queues[q];
                    s.pc += offset + 1;
                    s.wait_until = self.tick + delay;
                }
                None => {
                    self.exec(rest, &mut effects);
                    let s = &mut self.queues[q];
                    s.loops_done += 1;
                    s.pc = 0;
                    s.wait_until = self.tick + 1;
                    if s.loops_done >= queue.repeat {
                        s.running = false;
                    }
                }
            }
            // A queue whose last action is a delay finishes its pass
            // when the delay elapses.
            let s = &mut self.queues[q];
            if s.running && s.pc >= queue.actions.len() {
                s.pc = 0;
                s.loops_done += 1;
                if s.loops_done >= queue.repeat {
                    s.running = false;
                }
            }
        }
        self.tick += 1;
        effects
    }

    /// Session end: `session_stop` triggers run their jobs' actions
    /// once, for notes only — every output is being zeroed regardless,
    /// so device effects at stop are discarded by construction.
    pub fn finish(&mut self) {
        for j in 0..self.a.jobs.len() {
            let job = self.a.jobs[j].clone();
            if let Some(t) = job.trigger
                && matches!(self.a.triggers[t], Trig::SessionStop)
            {
                let mut discarded = Vec::new();
                self.exec(&job.actions, &mut discarded);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn automation(body: Value) -> Value {
        let mut base = json!({
            "format": AUTOMATION_FORMAT, "id": "a",
            "controls": [{"id": "stop", "kind": "stop_button"}]
        });
        for (k, v) in body.as_object().unwrap() {
            if k == "controls" {
                base["controls"]
                    .as_array_mut()
                    .unwrap()
                    .extend(v.as_array().unwrap().clone());
            } else {
                base[k] = v.clone();
            }
        }
        base
    }

    fn run(body: Value, targets: &[&str], patterns: &[&str], sources: &[&str]) -> AutomationRun {
        let a = Automation::from_json(&automation(body)).unwrap();
        let s = |l: &[&str]| l.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let (t, p, src) = (s(targets), s(patterns), s(sources));
        let compiled = a
            .bind(&BindContext {
                targets: &t,
                patterns: &p,
                sources: &src,
                seeded: false,
                session_ms: 600_000,
            })
            .unwrap();
        AutomationRun::new(compiled)
    }

    // --- validation (V4) ---------------------------------------------

    #[test]
    fn v4_triggers_jobs_queues_variables_and_controls_produce_the_expected_effects() {
        let mut r = run(
            json!({
                "variables": [{"id": "level", "type": "number", "init": 10, "min": 0, "max": 60}],
                "controls": [{"id": "click", "kind": "button"}, {"id": "dial", "kind": "slider", "var": "level"}],
                "triggers": [
                    {"id": "start", "on": "session_start"},
                    {"id": "clicked", "on": {"control_pressed": "click"}},
                    {"id": "hot", "on": {"variable_crossed": {"var": "level", "cmp": "ge", "value": 50}}},
                    {"id": "ext", "on": {"external_event": "voice"}},
                    {"id": "done", "on": {"pattern_finished": "p1"}}
                ],
                "jobs": [
                    {"id": "ramp", "trigger": "start", "repeat": {"every": {"ms": 500}}, "max_runs": 3, "max_span_ms": 60000,
                     "actions": [{"add_variable": {"var": "level", "expr": "10"}}, {"set_function": {"target": "main", "value": "level"}}]},
                    {"id": "reward", "trigger": "clicked", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
                     "actions": [{"play_pattern": {"pattern": "p1", "target": "main"}}]},
                    {"id": "cool", "trigger": "hot", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
                     "actions": [{"start_queue": "q"}]},
                    {"id": "deny", "trigger": "ext", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
                     "actions": ["stop_all"]},
                    {"id": "after", "trigger": "done", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
                     "actions": [{"notify": "pattern done"}]}
                ],
                "queues": [{"id": "q", "loop": "once", "actions": [
                    {"set_function": {"target": "main", "value": "5"}}, {"delay": {"ms": 500}}, {"stop_pattern": {"target": "main"}}
                ]}]
            }),
            &["main"],
            &["p1"],
            &["voice"],
        );
        let set = |v| Effect::Set {
            target: 0,
            value: v,
        };
        assert_eq!(r.step(&[]), vec![set(20)], "t0: session_start ramp run 1");
        assert_eq!(
            r.step(&[Event::Press(1)]),
            vec![Effect::Play {
                pattern: 0,
                target: 0
            }],
            "t1: click → reward"
        );
        assert_eq!(r.step(&[]), vec![set(30)], "t2: ramp run 2");
        // t3: the slider write crosses 50 (edge) → `cool` runs, its
        // start_queue takes effect next tick.
        assert_eq!(
            r.step(&[Event::Slider(2, 55.0)]),
            vec![],
            "t3: slider writes level"
        );
        // t4: ramp run 3 (clamped to 60), then the queue's first action
        // up to its delay.
        assert_eq!(r.step(&[]), vec![set(60), set(5)]);
        assert_eq!(r.step(&[]), vec![], "t5: ramp hit max_runs; queue waits");
        assert_eq!(
            r.step(&[]),
            vec![Effect::StopPattern { target: 0 }],
            "t6: delay elapsed"
        );
        assert!(r.log.iter().any(|l| l.contains("ramp stopped: max_runs")));
        assert_eq!(
            r.step(&[Event::External(0)]),
            vec![Effect::StopAll],
            "t7: external → deny"
        );
        r.step(&[Event::PatternFinished(0)]);
        assert!(r.log.iter().any(|l| l.contains("notify: pattern done")));
        assert_eq!(r.var(0), 60.0, "slider and adds both clamp to max 60");
    }

    #[test]
    fn hold_toggle_timer_and_toy_triggers_fire() {
        let mut r = run(
            json!({
                "variables": [{"id": "on", "type": "bool", "init": false}],
                "controls": [{"id": "b", "kind": "button"}, {"id": "t", "kind": "toggle", "var": "on"}],
                "triggers": [
                    {"id": "held", "on": {"control_held": {"control": "b", "ms": 500}}},
                    {"id": "tick", "on": {"timer_elapsed": "pulse"}},
                    {"id": "toy", "on": {"toy_connected": "SR6"}}
                ],
                "jobs": [
                    {"id": "pulse", "trigger": "held", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
                     "actions": [{"set_function": {"target": "m", "value": "on * 70"}}]},
                    {"id": "echo", "trigger": "tick", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
                     "actions": [{"set_function": {"target": "m", "value": "1"}}]},
                    {"id": "hello", "trigger": "toy", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
                     "actions": [{"set_function": {"target": "m", "value": "2"}}]}
                ]
            }),
            &["m"],
            &[],
            &[],
        );
        assert!(r.step(&[Event::Press(1), Event::Toggle(2)]).is_empty());
        assert!(r.step(&[]).is_empty());
        assert_eq!(
            r.step(&[]),
            vec![Effect::Set {
                target: 0,
                value: 70
            }],
            "held 500 ms"
        );
        // Jobs run in declaration order: echo (timer) before hello (toy).
        assert_eq!(
            r.step(&[Event::ToyConnected("SR6".into())]),
            vec![
                Effect::Set {
                    target: 0,
                    value: 1
                },
                Effect::Set {
                    target: 0,
                    value: 2
                },
            ]
        );
    }

    #[test]
    fn seeded_hash01_is_replayable() {
        let body = json!({
            "variables": [{"id": "r", "type": "number", "init": 0, "min": 0, "max": 100}],
            "triggers": [{"id": "s", "on": "session_start"}],
            "jobs": [{"id": "j", "trigger": "s", "repeat": {"every": {"ms": 250}}, "max_runs": 8, "max_span_ms": 10000,
                "actions": [{"set_function": {"target": "m", "value": "hash01(time * 4) * 100"}}]}]
        });
        let make = |seed| {
            let a = Automation::from_json(&automation(body.clone())).unwrap();
            let t = vec!["m".to_string()];
            let c = a
                .bind(&BindContext {
                    targets: &t,
                    patterns: &[],
                    sources: &[],
                    seeded: true,
                    session_ms: 600_000,
                })
                .unwrap();
            let mut r = AutomationRun::new(c.with_seed(Some(seed)));
            (0..8).flat_map(|_| r.step(&[])).collect::<Vec<_>>()
        };
        assert_eq!(make(7), make(7));
        assert_ne!(make(7), make(8));
        // Without a session seed, hash01 does not even bind.
        let a = Automation::from_json(&automation(body)).unwrap();
        let t = vec!["m".to_string()];
        assert!(
            a.bind(&BindContext {
                targets: &t,
                patterns: &[],
                sources: &[],
                seeded: false,
                session_ms: 600_000
            })
            .is_err()
        );
    }

    // --- adversarial (A1, A3, A4, A5) --------------------------------

    #[test]
    fn a1_runaway_while_is_stopped_at_max_runs_with_the_reason_logged() {
        let mut r = run(
            json!({
                "triggers": [{"id": "s", "on": "session_start"}],
                "jobs": [{"id": "forever", "trigger": "s", "repeat": {"while": "1"}, "max_runs": 5, "max_span_ms": 600000,
                    "actions": [{"set_function": {"target": "m", "value": "9"}}]}]
            }),
            &["m"],
            &[],
            &[],
        );
        let effects: usize = (0..20).map(|_| r.step(&[]).len()).sum();
        assert_eq!(effects, 5);
        assert!(
            r.log
                .iter()
                .any(|l| l.contains("forever stopped: max_runs"))
        );
        let mut r = run(
            json!({
                "triggers": [{"id": "s", "on": "session_start"}],
                "jobs": [{"id": "span", "trigger": "s", "repeat": {"while": "1"}, "max_runs": 10000, "max_span_ms": 1000,
                    "actions": [{"set_function": {"target": "m", "value": "9"}}]}]
            }),
            &["m"],
            &[],
            &[],
        );
        let effects: usize = (0..20).map(|_| r.step(&[]).len()).sum();
        assert_eq!(effects, 4, "1000 ms span = 4 ticks");
        assert!(r.log.iter().any(|l| l.contains("span stopped: max_span")));
    }

    #[test]
    fn a1_queue_job_mutual_restart_is_bounded_per_tick() {
        // q starts j, j starts q: restarts are deferred to the next tick,
        // so each tick does bounded work and only the session bound
        // (the session layer's clock) ends it.
        let mut r = run(
            json!({
                "triggers": [{"id": "s", "on": "session_start"}],
                "jobs": [{"id": "j", "trigger": "s", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
                    "actions": [{"start_queue": "q"}, {"set_function": {"target": "m", "value": "3"}}]}],
                "queues": [{"id": "q", "loop": "once", "actions": [{"start_job": "j"}]}]
            }),
            &["m"],
            &[],
            &[],
        );
        let per_tick: Vec<usize> = (0..12).map(|_| r.step(&[]).len()).collect();
        assert!(per_tick.iter().all(|n| *n <= 1), "{per_tick:?}");
        assert!(
            per_tick.iter().filter(|n| **n == 1).count() >= 3,
            "it keeps cycling: {per_tick:?}"
        );
    }

    #[test]
    fn a3_add_variable_drift_clamps_at_the_declared_range() {
        let mut r = run(
            json!({
                "variables": [{"id": "x", "type": "number", "init": 0, "min": 0, "max": 40}],
                "triggers": [{"id": "s", "on": "session_start"}],
                "jobs": [{"id": "drift", "trigger": "s", "repeat": {"every": {"ms": 250}}, "max_runs": 100, "max_span_ms": 600000,
                    "actions": [{"add_variable": {"var": "x", "expr": "7"}}, {"set_function": {"target": "m", "value": "x * 10"}}]}]
            }),
            &["m"],
            &[],
            &[],
        );
        let values: Vec<u8> = (0..30)
            .flat_map(|_| r.step(&[]))
            .map(|e| match e {
                Effect::Set { value, .. } => value,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(r.var(0), 40.0);
        // x*10 = 400 is clamped to 100 by quantize; the gate clamps again.
        assert!(values.iter().all(|v| *v <= 100));
        assert_eq!(*values.last().unwrap(), 100);
    }

    #[test]
    fn a5_malformed_automations_are_load_errors() {
        let job = |extra: Value| {
            let mut j = json!({"id": "j", "repeat": "once", "max_runs": 1, "max_span_ms": 1000,
                "actions": [{"set_function": {"target": "m", "value": "1"}}]});
            for (k, v) in extra.as_object().unwrap() {
                if v.is_null() {
                    j.as_object_mut().unwrap().remove(k);
                } else {
                    j[k] = v.clone();
                }
            }
            json!({"jobs": [j]})
        };
        let cases = vec![
            job(json!({"max_runs": null})),
            job(json!({"max_span_ms": null})),
            job(json!({"max_runs": 0})),
            job(json!({"max_runs": 10001})),
            job(json!({"max_span_ms": 0})),
            job(json!({"trigger": "nope"})),
            job(json!({"actions": []})),
            job(json!({"actions": [{"delay": {"ms": 10}}]})),
            job(json!({"actions": [{"start_job": "ghost"}]})),
            job(json!({"actions": [{"webhook": "http://x"}]})),
            job(json!({"actions": [{"set_variable": {"var": "ghost", "expr": "1"}}]})),
            job(json!({"repeat": {"every": {"ms": 10}}})),
            job(json!({"repeat": {"while": "eval('x')"}})),
            job(json!({"repeat": "forever"})),
            json!({"variables": [{"id": "v", "type": "number", "init": 0}]}),
            json!({"variables": [{"id": "time", "type": "number", "init": 0, "min": 0, "max": 1}]}),
            json!({"variables": [{"id": "b", "type": "bool", "init": 3}]}),
            json!({"variables": [{"id": "s", "type": "string", "init": "x"}]}),
            json!({"queues": [{"id": "q", "loop": {"repeat": 0}, "actions": [{"delay": {"ms": 250}}]}]}),
            json!({"queues": [{"id": "q", "loop": "once", "actions": []}]}),
            json!({"triggers": [{"id": "t", "on": {"heart_rate": 120}}]}),
            json!({"triggers": [{"id": "t", "on": {"webhook": "id"}}]}),
            json!({"script": "xtoys.callAction()"}),
        ];
        for body in cases {
            let doc = automation(body.clone());
            assert!(Automation::from_json(&doc).is_err(), "{body}");
        }
        // No stop button: refused.
        let doc = json!({"format": AUTOMATION_FORMAT, "id": "a", "controls": [{"id": "b", "kind": "button"}]});
        assert!(Automation::from_json(&doc).is_err());
        // Queue with 257 actions: refused.
        let actions = vec![json!("stop_all"); QUEUE_ACTIONS_MAX + 1];
        assert!(
            Automation::from_json(&automation(
                json!({"queues": [{"id": "q", "loop": "once", "actions": actions}]})
            ))
            .is_err()
        );
    }

    #[test]
    fn a4_events_cannot_carry_values_and_unknown_sources_do_not_bind() {
        // An external source not registered with the session refuses at
        // bind; the event type itself has no value field to carry.
        let a = Automation::from_json(&automation(json!({
            "triggers": [{"id": "e", "on": {"external_event": "webhook"}}]
        })))
        .unwrap();
        let err = a
            .bind(&BindContext {
                targets: &[],
                patterns: &[],
                sources: &[],
                seeded: false,
                session_ms: 600_000,
            })
            .unwrap_err();
        assert!(err.message.contains("input source"));
        // A trigger kind with a payload shape is not in the closed set.
        assert!(Automation::from_json(&automation(json!({
            "triggers": [{"id": "e", "on": {"external_event": {"source": "x", "intensity": 90}}}]
        })))
        .is_err());
    }
}
