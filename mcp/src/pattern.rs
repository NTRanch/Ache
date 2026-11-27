//! `ache.pattern/v1` — the frozen declarative pattern spec (XToys spec
//! §1.1 basic, §1.2 scripted) and its compiler.
//!
//! A pattern is data. It is parsed with unknown fields refused,
//! validated whole at load (every violation is a hard error and the
//! pattern never reaches `decide()`), hashed over its canonical form,
//! and compiled once into a sampled curve: a `Vec<u8>` of 0..=100
//! setpoints on a fixed grid. Nothing is interpreted at run time — the
//! runtime walks the samples, and every sample still passes the gate.
//!
//! Float law (§4.1): evaluation in f64, rounding only at the final
//! clamp step, so the compiled curve is bit-stable on one build.

use serde::{Deserialize, Serialize};

use crate::error::{AcheError, ErrorCode, Result};
use crate::expr::{Env, Expr};

pub const PATTERN_FORMAT: &str = "ache.pattern/v1";
/// §2.4: pattern total duration ≤ 30 min.
pub const PATTERN_DURATION_MS_MAX: u64 = 30 * 60 * 1000;
/// §1.2: the scripted evaluation tick, fixed, not author-settable.
pub const EXPRESSION_TICK_MS: u64 = 250;
/// A bound on blocks across a whole pattern (a 30 min funscript at the
/// 50 Hz law is 90 000 actions; this admits it and nothing pathological).
pub const PATTERN_BLOCKS_MAX: usize = 100_000;
pub const SPEC_ID_BYTES_MAX: usize = 64;
/// Longest provenance text carried (title/creator), bytes.
pub const PROVENANCE_BYTES_MAX: usize = 256;

/// Output functions (§1.1 + §3.3): the Buttplug/Lovense-side set, the
/// six TCode axes, and `Generic` (compiles against any function).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Function {
    Vibrate,
    AirPulse,
    Suction,
    Rotate,
    StrokeSpeed,
    StrokePosition,
    Inflate,
    Generic,
    Surge,
    Sway,
    Twist,
    Roll,
    Pitch,
}

impl Function {
    pub const ALL: [Function; 13] = [
        Function::Vibrate,
        Function::AirPulse,
        Function::Suction,
        Function::Rotate,
        Function::StrokeSpeed,
        Function::StrokePosition,
        Function::Inflate,
        Function::Generic,
        Function::Surge,
        Function::Sway,
        Function::Twist,
        Function::Roll,
        Function::Pitch,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Function::Vibrate => "Vibrate",
            Function::AirPulse => "AirPulse",
            Function::Suction => "Suction",
            Function::Rotate => "Rotate",
            Function::StrokeSpeed => "StrokeSpeed",
            Function::StrokePosition => "StrokePosition",
            Function::Inflate => "Inflate",
            Function::Generic => "Generic",
            Function::Surge => "Surge",
            Function::Sway => "Sway",
            Function::Twist => "Twist",
            Function::Roll => "Roll",
            Function::Pitch => "Pitch",
        }
    }

    pub fn parse(name: &str) -> Result<Function> {
        Function::ALL
            .iter()
            .copied()
            .find(|f| f.as_str() == name)
            .ok_or_else(|| {
                AcheError::new(
                    ErrorCode::RequestInvalid,
                    format!("unknown function {name:?}"),
                )
            })
    }

    /// The unified gate feature this function drives on a
    /// Lovense/Buttplug/Server API device (the existing vocabulary in
    /// `decide.rs`). `None` for the TCode-only axes and for `Generic`,
    /// which is not a device function. `StrokePosition` maps to the
    /// Lovense `Stroke` feature here and to L0 on a TCode device.
    #[must_use]
    pub fn gate_feature(self) -> Option<&'static str> {
        match self {
            Function::Vibrate => Some("Vibrate"),
            Function::AirPulse | Function::Suction => Some("Suction"),
            Function::Rotate => Some("Rotate"),
            Function::StrokeSpeed => Some("Thrusting"),
            Function::StrokePosition => Some("Stroke"),
            Function::Inflate => Some("Pump"),
            Function::Generic
            | Function::Surge
            | Function::Sway
            | Function::Twist
            | Function::Roll
            | Function::Pitch => None,
        }
    }

    /// Whether a pattern for `self` may drive a binding of `target`.
    #[must_use]
    pub fn compiles_against(self, target: Function) -> bool {
        self == Function::Generic || self == target
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Square,
    Linear,
    Sine,
}

fn default_one_u32() -> u32 {
    1
}
fn default_speed() -> f64 {
    1.0
}
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub kind: BlockKind,
    pub top: u8,
    #[serde(default)]
    pub bottom: u8,
    pub duration_ms: u64,
    #[serde(default = "default_speed")]
    pub speed: f64,
    #[serde(default = "default_one_u32")]
    pub loops: u32,
    /// Linear only: ramp from `top` down to `bottom` instead of up.
    /// (Spec extension S-1: §1.1 Linear ramps bottom→top only, which
    /// cannot express a falling funscript segment without `bottom >
    /// top`, itself a hard error. Recorded in the build RECORD.)
    #[serde(default, skip_serializing_if = "is_false")]
    pub falling: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub blocks: Vec<Block>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopPolicy {
    Once,
    Repeat(u32),
}

impl LoopPolicy {
    #[must_use]
    pub fn count(self) -> u32 {
        match self {
            LoopPolicy::Once => 1,
            LoopPolicy::Repeat(n) => n,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scripted {
    pub base: u8,
    pub expr: String,
    pub duration_ms: u64,
}

/// Carried from an import (funscript metadata) — never semantics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternSpec {
    pub format: String,
    pub id: String,
    pub function: Function,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<Section>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripted: Option<Scripted>,
    #[serde(rename = "loop")]
    pub loop_policy: LoopPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// A validated, hashed pattern. Only `Pattern::load` constructs one.
#[derive(Clone, Debug)]
pub struct Pattern {
    spec: PatternSpec,
    expr: Option<Expr>,
    hash: String,
    total_ms: u64,
}

pub(crate) fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(AcheError::new(ErrorCode::RequestInvalid, message))
}

/// SHA-256 of `bytes`, lowercase hex. The identity of a frozen spec.
#[must_use]
pub fn content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A spec id: 1..=64 bytes of `[A-Za-z0-9_.-]`.
pub fn valid_id(kind: &str, id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= SPEC_ID_BYTES_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok {
        Ok(())
    } else {
        invalid(format!(
            "{kind} id {id:?} must be 1..={SPEC_ID_BYTES_MAX} bytes of letters, digits, `_`, `-`, `.`"
        ))
    }
}

/// The scripted-pattern env names, in slot order (§1.2).
pub const SCRIPTED_NAMES: &[&str] = &["time", "intensity", "index", "loop"];

impl Pattern {
    /// Parse and validate a pattern from JSON. Every §1.1/§1.2 rule is
    /// a hard error here.
    pub fn from_json(value: &serde_json::Value) -> Result<Pattern> {
        let spec: PatternSpec = serde_json::from_value(value.clone()).map_err(|e| {
            AcheError::new(ErrorCode::RequestInvalid, format!("invalid pattern: {e}"))
        })?;
        Pattern::load(spec)
    }

    pub fn load(spec: PatternSpec) -> Result<Pattern> {
        if spec.format != PATTERN_FORMAT {
            return invalid(format!(
                "pattern format {:?} must be {PATTERN_FORMAT:?}",
                spec.format
            ));
        }
        valid_id("pattern", &spec.id)?;
        if let Some(p) = &spec.provenance {
            for text in [&p.source, &p.title, &p.creator].into_iter().flatten() {
                if text.len() > PROVENANCE_BYTES_MAX || text.chars().any(char::is_control) {
                    return invalid("provenance text too long or has control characters");
                }
            }
        }
        let repeat = spec.loop_policy.count();
        if repeat == 0 {
            return invalid("loop repeat must be at least 1; loops are finite only");
        }
        let (per_loop_ms, expr) = match (&spec.sections, &spec.scripted) {
            (Some(sections), None) => (validate_sections(sections)?, None),
            (None, Some(scripted)) => {
                if scripted.base > 100 {
                    return invalid("scripted base must be within 0..=100");
                }
                if scripted.duration_ms == 0 {
                    return invalid("scripted duration_ms must be at least 1");
                }
                let expr = Expr::parse(&scripted.expr, SCRIPTED_NAMES, false)?;
                (scripted.duration_ms, Some(expr))
            }
            (Some(_), Some(_)) => {
                return invalid("a pattern is either basic (sections) or scripted, not both");
            }
            (None, None) => return invalid("a pattern needs sections or a scripted body"),
        };
        let total_ms = per_loop_ms.saturating_mul(u64::from(repeat));
        if total_ms > PATTERN_DURATION_MS_MAX {
            return invalid(format!(
                "pattern total duration {total_ms} ms exceeds the bound of {PATTERN_DURATION_MS_MAX} ms"
            ));
        }
        let canonical = serde_json::to_vec(&spec).expect("a typed spec always serializes");
        let hash = content_hash(&canonical);
        Ok(Pattern {
            spec,
            expr,
            hash,
            total_ms,
        })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.spec.id
    }

    #[must_use]
    pub fn function(&self) -> Function {
        self.spec.function
    }

    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    #[must_use]
    pub fn total_ms(&self) -> u64 {
        self.total_ms
    }

    #[cfg(test)]
    pub fn spec(&self) -> &PatternSpec {
        &self.spec
    }

    /// Compile to a sampled curve on a `period_ms` grid. `index` is the
    /// fan-out target index (§3.2), read by scripted patterns only.
    /// Sample `k` is the setpoint at `t = k × period_ms`; the curve has
    /// `ceil(total / period)` samples and ends at the pattern's total.
    #[must_use]
    pub fn sample(&self, period_ms: u64, index: u32) -> Vec<u8> {
        assert!(period_ms > 0, "the sampling period is a positive constant");
        let count = self.total_ms.div_ceil(period_ms);
        match (&self.spec.sections, &self.expr, &self.spec.scripted) {
            (Some(sections), _, _) => {
                let per_loop = self.total_ms / u64::from(self.spec.loop_policy.count());
                (0..count)
                    .map(|k| sample_sections(sections, (k * period_ms) % per_loop))
                    .collect()
            }
            (None, Some(expr), Some(scripted)) => {
                let ticks = scripted_ticks(expr, scripted, self.spec.loop_policy.count(), index);
                (0..count)
                    .map(|k| {
                        let tick = (k * period_ms / EXPRESSION_TICK_MS) as usize;
                        ticks[tick.min(ticks.len() - 1)]
                    })
                    .collect()
            }
            _ => unreachable!("load() admits exactly one body"),
        }
    }
}

fn validate_sections(sections: &[Section]) -> Result<u64> {
    if sections.is_empty() {
        return invalid("pattern sections must not be empty");
    }
    let mut total: u64 = 0;
    let mut blocks_seen = 0usize;
    for (s, section) in sections.iter().enumerate() {
        if section.blocks.is_empty() {
            return invalid(format!("section {s}: blocks must not be empty"));
        }
        blocks_seen += section.blocks.len();
        if blocks_seen > PATTERN_BLOCKS_MAX {
            return invalid(format!("pattern exceeds {PATTERN_BLOCKS_MAX} blocks"));
        }
        for (b, block) in section.blocks.iter().enumerate() {
            let at = format!("section {s} block {b}");
            if block.top > 100 || block.bottom > 100 {
                return invalid(format!("{at}: top and bottom must be within 0..=100"));
            }
            if block.bottom > block.top {
                return invalid(format!("{at}: bottom {} > top {}", block.bottom, block.top));
            }
            if block.loops == 0 {
                return invalid(format!("{at}: loops must be at least 1"));
            }
            if block.duration_ms == 0 {
                return invalid(format!("{at}: duration_ms must be at least 1"));
            }
            if !block.speed.is_finite() || block.speed <= 0.0 {
                return invalid(format!("{at}: speed must be finite and positive"));
            }
            match block.kind {
                BlockKind::Square => {
                    if block.bottom != 0 {
                        return invalid(format!(
                            "{at}: a Square block holds `top`; bottom must be 0"
                        ));
                    }
                    if block.speed != 1.0 {
                        return invalid(format!("{at}: Square speed must be 1.0"));
                    }
                }
                BlockKind::Linear => {}
                BlockKind::Sine => {}
            }
            if block.falling && block.kind != BlockKind::Linear {
                return invalid(format!("{at}: `falling` applies to Linear blocks only"));
            }
            let span = block.duration_ms.saturating_mul(u64::from(block.loops));
            total = total.saturating_add(span);
            if total > PATTERN_DURATION_MS_MAX {
                return invalid(format!(
                    "pattern total duration exceeds the bound of {PATTERN_DURATION_MS_MAX} ms"
                ));
            }
        }
    }
    Ok(total)
}

/// The §1.1 closed-form law of one block at local time `tau_ms` within
/// one iteration (`0 <= tau < duration`).
#[must_use]
pub fn block_value(block: &Block, tau_ms: u64) -> f64 {
    let top = f64::from(block.top);
    let bottom = f64::from(block.bottom);
    let duration = block.duration_ms as f64;
    let tau = tau_ms as f64;
    match block.kind {
        BlockKind::Square => top,
        BlockKind::Linear => {
            let ramp = duration / block.speed;
            let fraction = if tau < ramp { tau / ramp } else { 1.0 };
            if block.falling {
                top - (top - bottom) * fraction
            } else {
                bottom + (top - bottom) * fraction
            }
        }
        BlockKind::Sine => {
            let phase = 2.0 * std::f64::consts::PI * block.speed * tau / duration;
            bottom + (top - bottom) * (0.5 - 0.5 * phase.cos())
        }
    }
}

/// Round and clamp — the only rounding step in the compiler.
#[must_use]
pub fn quantize(value: f64) -> u8 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(0.0, 100.0) as u8
}

fn sample_sections(sections: &[Section], mut t_ms: u64) -> u8 {
    for section in sections {
        for block in &section.blocks {
            let span = block.duration_ms * u64::from(block.loops);
            if t_ms < span {
                return quantize(block_value(block, t_ms % block.duration_ms));
            }
            t_ms -= span;
        }
    }
    unreachable!("t is reduced modulo the per-loop total before sampling")
}

/// The scripted tick table for all loops: `intensity(k)` per §1.2.
/// Within a loop, `k` restarts at 0 and `intensity(0) = base`; `loop`
/// counts completed loops.
fn scripted_ticks(expr: &Expr, scripted: &Scripted, repeat: u32, index: u32) -> Vec<u8> {
    let per_loop = scripted.duration_ms.div_ceil(EXPRESSION_TICK_MS);
    let mut ticks = Vec::with_capacity((per_loop * u64::from(repeat)) as usize);
    for completed in 0..repeat {
        let mut previous = f64::from(scripted.base);
        for k in 0..per_loop {
            let time = (k * EXPRESSION_TICK_MS) as f64 / 1000.0;
            let slots = [time, previous, f64::from(index), f64::from(completed)];
            let value = quantize(expr.eval(&Env {
                slots: &slots,
                seed: None,
            }));
            ticks.push(value);
            previous = f64::from(value);
        }
    }
    ticks
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn basic(blocks: serde_json::Value) -> serde_json::Value {
        json!({
            "format": PATTERN_FORMAT, "id": "t", "function": "Vibrate",
            "sections": [{"blocks": blocks}], "loop": "once"
        })
    }

    // --- validation (V1, V2) -----------------------------------------

    #[test]
    fn v1_each_block_kind_matches_its_closed_form_law() {
        let p = Pattern::from_json(&basic(json!([
            {"kind": "square", "top": 40, "duration_ms": 1000},
            {"kind": "linear", "top": 100, "bottom": 0, "duration_ms": 1000, "speed": 2.0},
            {"kind": "sine", "top": 80, "bottom": 20, "duration_ms": 2000, "speed": 1.0}
        ])))
        .unwrap();
        assert_eq!(p.total_ms(), 4000);
        let s = p.sample(250, 0);
        assert_eq!(s.len(), 16);
        // Square: held top.
        assert!(s[0..4].iter().all(|v| *v == 40));
        // Linear at speed 2: ramp over 500 ms, then hold top.
        assert_eq!(&s[4..8], &[0, 50, 100, 100]);
        // Sine: bottom at t=0, top at half period, back toward bottom.
        assert_eq!(s[8], 20);
        assert_eq!(s[12], 80);
        for (k, v) in s[8..16].iter().enumerate() {
            let tau = (k as f64) * 250.0;
            let law = 20.0 + 60.0 * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * tau / 2000.0).cos());
            assert_eq!(*v, law.round() as u8, "sine sample {k}");
        }
    }

    #[test]
    fn block_loops_and_pattern_repeat_extend_the_total() {
        let mut spec =
            basic(json!([{"kind": "square", "top": 10, "duration_ms": 500, "loops": 3}]));
        spec["loop"] = json!({"repeat": 2});
        let p = Pattern::from_json(&spec).unwrap();
        assert_eq!(p.total_ms(), 3000);
        assert_eq!(p.sample(20, 0).len(), 150);
    }

    #[test]
    fn falling_linear_ramps_down() {
        let p = Pattern::from_json(&basic(json!([
            {"kind": "linear", "top": 80, "bottom": 0, "duration_ms": 1000, "falling": true}
        ])))
        .unwrap();
        assert_eq!(p.sample(250, 0), vec![80, 60, 40, 20]);
    }

    #[test]
    fn v2_scripted_matches_a_hand_computed_tick_table() {
        // Uses every §1.2 function. time advances 0.25 s per tick.
        let expr = "select(time < 0.5, intensity + 10, min(max(abs(-intensity), 0), clamp(pow(2, 3) * 10, 0, 100))) \
            + floor(0.5) + ceil(0) + round(0.4) + sin(0) + cos(0) - 1 + sqrt(0) + index";
        let spec = json!({
            "format": PATTERN_FORMAT, "id": "s", "function": "Vibrate",
            "scripted": {"base": 20, "expr": expr, "duration_ms": 1000}, "loop": "once"
        });
        let p = Pattern::from_json(&spec).unwrap();
        // k0: t=0 → 20+10=30; k1: t=.25 → 40; k2: t=.5 → min(40, 80)=40; k3: 40.
        assert_eq!(p.sample(250, 0), vec![30, 40, 40, 40]);
        // index shifts every tick by 1 (fan-out law, §3.2).
        assert_eq!(p.sample(250, 1), vec![31, 42, 43, 44]);
        // 50 Hz resampling is a zero-order hold of the 250 ms ticks.
        let fine = p.sample(20, 0);
        assert_eq!(fine.len(), 50);
        assert_eq!(fine[12], 30);
        assert_eq!(fine[13], 40);
    }

    #[test]
    fn hash_is_stable_and_content_addressed() {
        let a = Pattern::from_json(&basic(
            json!([{"kind": "square", "top": 10, "duration_ms": 500}]),
        ))
        .unwrap();
        let b = Pattern::from_json(&basic(
            json!([{"kind": "square", "top": 10, "duration_ms": 500}]),
        ))
        .unwrap();
        let c = Pattern::from_json(&basic(
            json!([{"kind": "square", "top": 11, "duration_ms": 500}]),
        ))
        .unwrap();
        assert_eq!(a.hash(), b.hash());
        assert_ne!(a.hash(), c.hash());
        assert_eq!(a.hash().len(), 64);
    }

    // --- adversarial (A2, A5) ----------------------------------------

    #[test]
    fn a5_malformed_patterns_are_load_errors() {
        let cases = [
            json!([]),
            json!([{"kind": "linear", "top": 10, "bottom": 20, "duration_ms": 100}]),
            json!([{"kind": "square", "top": 10, "duration_ms": 100, "loops": 0}]),
            json!([{"kind": "square", "top": 10, "duration_ms": 0}]),
            json!([{"kind": "sine", "top": 10, "duration_ms": 100, "speed": 0.0}]),
            json!([{"kind": "sine", "top": 10, "duration_ms": 100, "speed": -1.0}]),
            json!([{"kind": "square", "top": 10, "bottom": 5, "duration_ms": 100}]),
            json!([{"kind": "square", "top": 101, "duration_ms": 100}]),
            json!([{"kind": "sine", "top": 10, "duration_ms": 100, "falling": true}]),
            json!([{"kind": "pulse", "top": 10, "duration_ms": 100}]),
            json!([{"kind": "square", "top": 10, "duration_ms": 100, "script": "x"}]),
        ];
        for blocks in cases {
            assert!(
                Pattern::from_json(&basic(blocks.clone())).is_err(),
                "{blocks}"
            );
        }
        let mut spec = basic(json!([{"kind": "square", "top": 1, "duration_ms": 10}]));
        spec["function"] = json!("Explode");
        assert!(Pattern::from_json(&spec).is_err());
        spec["function"] = json!("Vibrate");
        spec["format"] = json!("ache.pattern/v2");
        assert!(Pattern::from_json(&spec).is_err());
        spec["format"] = json!(PATTERN_FORMAT);
        spec["loop"] = json!({"repeat": 0});
        assert!(Pattern::from_json(&spec).is_err());
        spec["loop"] = json!("forever");
        assert!(Pattern::from_json(&spec).is_err());
        spec["loop"] = json!("once");
        spec["id"] = json!("../etc");
        assert!(Pattern::from_json(&spec).is_err());
    }

    #[test]
    fn a2_computed_total_over_the_bound_is_refused_at_load() {
        // Each block legal, the sum over 30 min.
        let block = json!({"kind": "square", "top": 5, "duration_ms": 600_000});
        assert!(Pattern::from_json(&basic(json!([block, block, block]))).is_ok());
        assert!(Pattern::from_json(&basic(json!([block, block, block, block]))).is_err());
        // Repeat multiplies past the bound.
        let mut spec = basic(json!([block]));
        spec["loop"] = json!({"repeat": 4});
        assert!(Pattern::from_json(&spec).is_err());
        // Overflowing arithmetic saturates into a refusal, never wraps.
        let huge =
            json!({"kind": "square", "top": 5, "duration_ms": u64::MAX, "loops": 4_000_000_000u32});
        assert!(Pattern::from_json(&basic(json!([huge]))).is_err());
    }

    #[test]
    fn a3_expression_output_is_clamped_into_range_every_tick() {
        // intensity + 50 every tick: clamps at 100, never above.
        let spec = json!({
            "format": PATTERN_FORMAT, "id": "s", "function": "Vibrate",
            "scripted": {"base": 0, "expr": "intensity + 50", "duration_ms": 2000}, "loop": "once"
        });
        let s = Pattern::from_json(&spec).unwrap().sample(250, 0);
        assert_eq!(s[..3], [50, 100, 100]);
        assert!(s.iter().all(|v| *v <= 100));
        let spec = json!({
            "format": PATTERN_FORMAT, "id": "s", "function": "Vibrate",
            "scripted": {"base": 0, "expr": "0 - 1000", "duration_ms": 500}, "loop": "once"
        });
        assert!(
            Pattern::from_json(&spec)
                .unwrap()
                .sample(250, 0)
                .iter()
                .all(|v| *v == 0)
        );
    }

    #[test]
    fn scripted_patterns_refuse_code_and_both_bodies() {
        let spec = json!({
            "format": PATTERN_FORMAT, "id": "s", "function": "Vibrate",
            "scripted": {"base": 0, "expr": "function onToyMove(t) { return 5 }", "duration_ms": 500},
            "loop": "once"
        });
        assert!(Pattern::from_json(&spec).is_err());
        let spec = json!({
            "format": PATTERN_FORMAT, "id": "s", "function": "Vibrate",
            "sections": [{"blocks": [{"kind": "square", "top": 1, "duration_ms": 10}]}],
            "scripted": {"base": 0, "expr": "1", "duration_ms": 500}, "loop": "once"
        });
        assert!(Pattern::from_json(&spec).is_err());
        let spec = json!({
            "format": PATTERN_FORMAT, "id": "s", "function": "Vibrate",
            "scripted": {"base": 0, "expr": "1", "duration_ms": 500, "tick_ms": 10}, "loop": "once"
        });
        assert!(
            Pattern::from_json(&spec).is_err(),
            "the tick is not author-settable"
        );
    }
}
