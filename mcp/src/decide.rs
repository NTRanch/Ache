//! The pure output gate.
//!
//! Every output the server can produce passes through [`decide`] exactly
//! once. The gate is pure: no I/O, no clock, no transport. Given a snapshot
//! of the safety state and a typed output intent, it returns either
//! `Allow` — carrying the exact wire body to send and the effective
//! (clamped) values that were put in it — or `Deny` with a structured
//! error. A deny is final: no tool schema carries an override parameter,
//! and no caller may retry a denied intent in altered form on the user's
//! behalf.
//!
//! Validation lives here too, so the gate is the single choke point: a
//! malformed pattern or preset can never reach the wire, because the wire
//! body is constructed inside the gate from validated parts, never from a
//! caller-supplied string.
//!
//! Clamping: the configured intensity ceiling clamps every outbound
//! strength in code — the function strength and each value inside a
//! pattern strength list — and a feature's own API range clamps further
//! where it is narrower (Pump and Depth top out at 3). The effective
//! values are reported back to the caller on every allow; nothing is
//! silently changed.

use serde_json::{Value, json};

use crate::error::{ErrorCode, LovenseError, Result};

/// Longest output duration the gate will emit, seconds. On the wire,
/// `timeSec: 0` means indefinite output; indefinite output does not
/// exist in this server, so durations are bounded at both ends.
pub const TIME_SEC_MAX: i64 = 3_600;
/// Pattern strength list cap from the wire contract.
pub const PATTERN_STRENGTHS_MAX: usize = 50;
/// Pattern step interval floor: the wire contract requires the interval
/// strictly above 100 ms, so 100 itself is refused.
pub const PATTERN_INTERVAL_MS_MIN: i64 = 101;
/// Pattern step interval cap (one hour), a named bound on `S:`.
pub const PATTERN_INTERVAL_MS_MAX: i64 = 3_600_000;
/// Toy ids longer than this are refused before the wire.
pub const TOY_ID_BYTES_MAX: usize = 128;

/// A snapshot of the safety state at the moment of the gate call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateSnapshot {
    /// Master toggle: output is possible only while armed.
    pub armed: bool,
    /// Whether a target exists to send output to (mock target, or a real
    /// transport with a configured host).
    pub target_configured: bool,
    /// The configured intensity ceiling (already validated to 0..=20).
    pub ceiling: u8,
}

/// A Function feature from the wire contract, with its API range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    /// Wire action name, e.g. `Vibrate`.
    pub action: &'static str,
    /// Highest strength the API accepts for this feature.
    pub api_max: u8,
}

const FEATURES: &[Feature] = &[
    Feature {
        action: "Vibrate",
        api_max: 20,
    },
    Feature {
        action: "Rotate",
        api_max: 20,
    },
    Feature {
        action: "Pump",
        api_max: 3,
    },
    Feature {
        action: "Thrusting",
        api_max: 20,
    },
    Feature {
        action: "Fingering",
        api_max: 20,
    },
    Feature {
        action: "Suction",
        api_max: 20,
    },
    Feature {
        action: "Depth",
        api_max: 3,
    },
    Feature {
        action: "Stroke",
        api_max: 100,
    },
    Feature {
        action: "Oscillate",
        api_max: 20,
    },
    Feature {
        action: "All",
        api_max: 20,
    },
];

impl Feature {
    /// Look up a feature by its exact wire action name.
    #[must_use]
    pub fn from_action(action: &str) -> Option<Feature> {
        FEATURES.iter().copied().find(|f| f.action == action)
    }

    /// Every wire action name, for tool schemas.
    #[must_use]
    pub fn action_names() -> Vec<&'static str> {
        FEATURES.iter().map(|f| f.action).collect()
    }
}

/// Pattern feature letters from the wire contract: `v r p t f s d o`.
/// An empty feature string is valid and means every function responds.
const PATTERN_FEATURE_LETTERS: &[u8] = b"vrptfsdo";

/// Built-in Remote app presets from the wire contract. Preset names
/// outside this list are refused: the server does not guess at names the
/// app might or might not hold.
pub const PRESET_NAMES: &[&str] = &["pulse", "wave", "fireworks", "earthquake"];

/// One function call, as requested. Raw values are validated and
/// clamped by the gate; nothing here is wire-ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionIntent {
    pub feature: String,
    pub strength: i64,
    pub time_sec: i64,
    pub toy: Option<String>,
}

/// A pattern, as requested in structured parts. The gate validates the
/// parts and constructs the wire rule string itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternIntent {
    pub features: String,
    pub interval_ms: i64,
    pub strengths: Vec<i64>,
    pub time_sec: i64,
    pub toy: Option<String>,
}

/// A named preset call, as requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetIntent {
    pub name: String,
    pub time_sec: i64,
    pub toy: Option<String>,
}

/// Every output the server can be asked to produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputIntent {
    Function(FunctionIntent),
    Pattern(PatternIntent),
    Preset(PresetIntent),
    /// The panic zero. Allowed in every state, even disarmed and even
    /// with no target: zeroing is never gated.
    Stop,
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow {
        /// The exact JSON body to POST to `/command`.
        body: Value,
        /// The effective values the body carries, for the tool result.
        effective: Value,
        /// True when any requested value was clamped to fit.
        clamped: bool,
    },
    Deny(LovenseError),
}

fn deny(code: ErrorCode, message: impl Into<String>) -> Decision {
    Decision::Deny(LovenseError::new(code, message))
}

fn request_invalid(message: impl Into<String>) -> Decision {
    deny(ErrorCode::RequestInvalid, message)
}

/// Validate a requested duration. Zero would mean indefinite output on
/// the wire and is refused outright, never clamped into existence.
fn valid_time_sec(time_sec: i64) -> Result<i64> {
    if time_sec < 1 {
        return Err(LovenseError::new(
            ErrorCode::RequestInvalid,
            "time_sec must be at least 1; indefinite output (timeSec 0) is not permitted",
        ));
    }
    if time_sec > TIME_SEC_MAX {
        return Err(LovenseError::new(
            ErrorCode::RequestInvalid,
            format!("time_sec {time_sec} exceeds the maximum of {TIME_SEC_MAX}"),
        ));
    }
    Ok(time_sec)
}

/// Validate the toy selector and return the wire value: the empty string
/// targets every toy.
fn valid_toy(toy: &Option<String>) -> Result<String> {
    match toy {
        None => Ok(String::new()),
        Some(id) => {
            if id.len() > TOY_ID_BYTES_MAX || id.chars().any(char::is_control) {
                return Err(LovenseError::new(
                    ErrorCode::RequestInvalid,
                    format!(
                        "toy id must be at most {TOY_ID_BYTES_MAX} bytes with no control characters"
                    ),
                ));
            }
            Ok(id.clone())
        }
    }
}

/// Clamp one strength to the ceiling and, for functions, the feature's
/// own API range. Negative strengths never reach this point.
fn clamp_strength(requested: i64, ceiling: u8, api_max: u8) -> (i64, bool) {
    let cap = i64::from(ceiling.min(api_max));
    if requested > cap {
        (cap, true)
    } else {
        (requested, false)
    }
}

/// The gate. Pure: same snapshot and intent, same decision, always.
#[must_use]
pub fn decide(snapshot: &GateSnapshot, intent: &OutputIntent) -> Decision {
    match intent {
        OutputIntent::Stop => decide_stop(),
        OutputIntent::Function(f) => match gate_output(snapshot) {
            Err(denied) => denied,
            Ok(()) => decide_function(snapshot, f),
        },
        OutputIntent::Pattern(p) => match gate_output(snapshot) {
            Err(denied) => denied,
            Ok(()) => decide_pattern(snapshot, p),
        },
        OutputIntent::Preset(p) => match gate_output(snapshot) {
            Err(denied) => denied,
            Ok(()) => decide_preset(snapshot, p),
        },
    }
}

/// The state half of the gate, shared by every non-stop output: armed,
/// and a target to send to. Returns `Err(deny)` on failure.
fn gate_output(snapshot: &GateSnapshot) -> std::result::Result<(), Decision> {
    if !snapshot.target_configured {
        return Err(deny(
            ErrorCode::TargetUnconfigured,
            "no target is configured; output is impossible",
        ));
    }
    if !snapshot.armed {
        return Err(deny(
            ErrorCode::NotArmed,
            "the master toggle is off; call lovense_arm first (arming is always explicit)",
        ));
    }
    Ok(())
}

/// Stop is the documented exception to every gate: a `Function` body
/// with action `Stop` and `timeSec 0`, zeroing every feature on every
/// toy. It is allowed in any state so a caller can always make the
/// world stop.
fn decide_stop() -> Decision {
    let body = json!({
        "command": "Function",
        "action": "Stop",
        "timeSec": 0,
        "toy": "",
        "apiVer": 1,
    });
    let effective = json!({
        "action": "Stop",
        "strength": 0,
        "time_sec": 0,
        "toy": "",
    });
    Decision::Allow {
        body,
        effective,
        clamped: false,
    }
}

fn decide_function(snapshot: &GateSnapshot, intent: &FunctionIntent) -> Decision {
    let Some(feature) = Feature::from_action(&intent.feature) else {
        return request_invalid(format!(
            "unknown function {:?}; expected one of {}",
            intent.feature,
            Feature::action_names().join(", ")
        ));
    };
    if intent.strength < 0 {
        return request_invalid("strength must not be negative");
    }
    let time_sec = match valid_time_sec(intent.time_sec) {
        Ok(t) => t,
        Err(e) => return Decision::Deny(e),
    };
    let toy = match valid_toy(&intent.toy) {
        Ok(t) => t,
        Err(e) => return Decision::Deny(e),
    };
    let (strength, clamped) = clamp_strength(intent.strength, snapshot.ceiling, feature.api_max);
    let body = json!({
        "command": "Function",
        "action": format!("{}:{}", feature.action, strength),
        "timeSec": time_sec,
        "toy": toy,
        "apiVer": 1,
    });
    let effective = json!({
        "feature": feature.action,
        "strength": strength,
        "time_sec": time_sec,
        "toy": toy,
    });
    Decision::Allow {
        body,
        effective,
        clamped,
    }
}

fn decide_pattern(snapshot: &GateSnapshot, intent: &PatternIntent) -> Decision {
    if intent.features.len() > PATTERN_FEATURE_LETTERS.len()
        || !intent
            .features
            .bytes()
            .all(|b| PATTERN_FEATURE_LETTERS.contains(&b))
    {
        return request_invalid(format!(
            "pattern features {:?} must use only the letters v, r, p, t, f, s, d, o",
            intent.features
        ));
    }
    let mut seen = [false; 256];
    if intent.features.bytes().any(|b| {
        let dup = seen[usize::from(b)];
        seen[usize::from(b)] = true;
        dup
    }) {
        return request_invalid("pattern features must not repeat a letter");
    }
    if intent.interval_ms < PATTERN_INTERVAL_MS_MIN {
        return request_invalid(format!(
            "pattern interval {} ms is at or below the 100 ms floor; the minimum is {PATTERN_INTERVAL_MS_MIN}",
            intent.interval_ms
        ));
    }
    if intent.interval_ms > PATTERN_INTERVAL_MS_MAX {
        return request_invalid(format!(
            "pattern interval {} ms exceeds the maximum of {PATTERN_INTERVAL_MS_MAX}",
            intent.interval_ms
        ));
    }
    if intent.strengths.is_empty() {
        return request_invalid("pattern strengths must not be empty");
    }
    if intent.strengths.len() > PATTERN_STRENGTHS_MAX {
        return request_invalid(format!(
            "pattern carries {} strengths; the wire contract allows at most {PATTERN_STRENGTHS_MAX}",
            intent.strengths.len()
        ));
    }
    if intent.strengths.iter().any(|s| *s < 0) {
        return request_invalid("pattern strengths must not be negative");
    }
    let time_sec = match valid_time_sec(intent.time_sec) {
        Ok(t) => t,
        Err(e) => return Decision::Deny(e),
    };
    let toy = match valid_toy(&intent.toy) {
        Ok(t) => t,
        Err(e) => return Decision::Deny(e),
    };
    let mut clamped = false;
    let effective_strengths: Vec<i64> = intent
        .strengths
        .iter()
        .map(|s| {
            let (v, c) = clamp_strength(*s, snapshot.ceiling, 20);
            clamped |= c;
            v
        })
        .collect();
    let strength_string = effective_strengths
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(";");
    let rule = format!("V:1;F:{};S:{}#", intent.features, intent.interval_ms);
    let body = json!({
        "command": "Pattern",
        "rule": rule,
        "strength": strength_string,
        "timeSec": time_sec,
        "toy": toy,
        "apiVer": 2,
    });
    let effective = json!({
        "rule": rule,
        "strengths": effective_strengths,
        "time_sec": time_sec,
        "toy": toy,
    });
    Decision::Allow {
        body,
        effective,
        clamped,
    }
}

fn decide_preset(_snapshot: &GateSnapshot, intent: &PresetIntent) -> Decision {
    if !PRESET_NAMES.contains(&intent.name.as_str()) {
        return request_invalid(format!(
            "unknown preset {:?}; the Remote app built-ins are {}",
            intent.name,
            PRESET_NAMES.join(", ")
        ));
    }
    let time_sec = match valid_time_sec(intent.time_sec) {
        Ok(t) => t,
        Err(e) => return Decision::Deny(e),
    };
    let toy = match valid_toy(&intent.toy) {
        Ok(t) => t,
        Err(e) => return Decision::Deny(e),
    };
    let body = json!({
        "command": "Preset",
        "name": intent.name,
        "timeSec": time_sec,
        "toy": toy,
        "apiVer": 1,
    });
    let effective = json!({
        "name": intent.name,
        "time_sec": time_sec,
        "toy": toy,
    });
    Decision::Allow {
        body,
        effective,
        clamped: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed() -> GateSnapshot {
        GateSnapshot {
            armed: true,
            target_configured: true,
            ceiling: 20,
        }
    }

    fn function(strength: i64) -> OutputIntent {
        OutputIntent::Function(FunctionIntent {
            feature: "Vibrate".to_string(),
            strength,
            time_sec: 10,
            toy: None,
        })
    }

    fn pattern(strengths: Vec<i64>, interval_ms: i64) -> OutputIntent {
        OutputIntent::Pattern(PatternIntent {
            features: "v".to_string(),
            interval_ms,
            strengths,
            time_sec: 10,
            toy: None,
        })
    }

    fn allow(decision: Decision) -> (Value, Value, bool) {
        match decision {
            Decision::Allow {
                body,
                effective,
                clamped,
            } => (body, effective, clamped),
            Decision::Deny(e) => panic!(
                "expected allow, got deny {}: {}",
                e.code.as_str(),
                e.message
            ),
        }
    }

    fn deny_code(decision: Decision) -> ErrorCode {
        match decision {
            Decision::Deny(e) => e.code,
            Decision::Allow { .. } => panic!("expected deny, got allow"),
        }
    }

    // --- validation ---------------------------------------------------

    #[test]
    fn function_at_ceiling_passes_unclamped_with_exact_wire_body() {
        let (body, effective, clamped) = allow(decide(&armed(), &function(20)));
        assert!(!clamped);
        assert_eq!(body["command"], "Function");
        assert_eq!(body["action"], "Vibrate:20");
        assert_eq!(body["timeSec"], 10);
        assert_eq!(body["toy"], "");
        assert_eq!(body["apiVer"], 1);
        assert_eq!(effective["strength"], 20);
    }

    #[test]
    fn pattern_builds_the_documented_rule_and_strength_strings() {
        let (body, effective, clamped) = allow(decide(&armed(), &pattern(vec![20, 20, 5], 1_000)));
        assert!(!clamped);
        assert_eq!(body["command"], "Pattern");
        assert_eq!(body["rule"], "V:1;F:v;S:1000#");
        assert_eq!(body["strength"], "20;20;5");
        assert_eq!(body["apiVer"], 2);
        assert_eq!(effective["strengths"], json!([20, 20, 5]));
    }

    #[test]
    fn pattern_with_empty_features_targets_all_functions() {
        let intent = OutputIntent::Pattern(PatternIntent {
            features: String::new(),
            interval_ms: 500,
            strengths: vec![7],
            time_sec: 3,
            toy: None,
        });
        let (body, _, _) = allow(decide(&armed(), &intent));
        assert_eq!(body["rule"], "V:1;F:;S:500#");
    }

    #[test]
    fn preset_wave_builds_the_documented_body() {
        let intent = OutputIntent::Preset(PresetIntent {
            name: "wave".to_string(),
            time_sec: 9,
            toy: Some("toy-9".to_string()),
        });
        let (body, effective, clamped) = allow(decide(&armed(), &intent));
        assert!(!clamped);
        assert_eq!(body["command"], "Preset");
        assert_eq!(body["name"], "wave");
        assert_eq!(body["toy"], "toy-9");
        assert_eq!(body["apiVer"], 1);
        assert_eq!(effective["name"], "wave");
    }

    #[test]
    fn stop_body_is_the_documented_zero() {
        let (body, effective, _) = allow(decide(&armed(), &OutputIntent::Stop));
        assert_eq!(body["command"], "Function");
        assert_eq!(body["action"], "Stop");
        assert_eq!(body["timeSec"], 0);
        assert_eq!(effective["strength"], 0);
    }

    #[test]
    fn narrow_feature_range_clamps_below_the_ceiling() {
        // Pump's API range is 0..=3; a request for 10 under a ceiling of
        // 20 lands on the wire as 3, and the result says it was clamped.
        let intent = OutputIntent::Function(FunctionIntent {
            feature: "Pump".to_string(),
            strength: 10,
            time_sec: 5,
            toy: None,
        });
        let (body, effective, clamped) = allow(decide(&armed(), &intent));
        assert!(clamped);
        assert_eq!(body["action"], "Pump:3");
        assert_eq!(effective["strength"], 3);
    }

    #[test]
    fn lower_configured_ceiling_clamps() {
        let snapshot = GateSnapshot {
            ceiling: 5,
            ..armed()
        };
        let (_, effective, clamped) = allow(decide(&snapshot, &function(12)));
        assert!(clamped);
        assert_eq!(effective["strength"], 5);
    }

    // --- adversarial --------------------------------------------------

    #[test]
    fn strength_above_ceiling_is_clamped_never_sent_raw() {
        // The probe's proven shape: an armed request for 25 hits the
        // wire as 20.
        let (body, effective, clamped) = allow(decide(&armed(), &function(25)));
        assert!(clamped);
        assert_eq!(body["action"], "Vibrate:20");
        assert_eq!(effective["strength"], 20);
    }

    #[test]
    fn pattern_strengths_clamp_per_value_inside_the_list() {
        let (body, effective, clamped) = allow(decide(&armed(), &pattern(vec![3, 99, 20], 250)));
        assert!(clamped);
        assert_eq!(body["strength"], "3;20;20");
        assert_eq!(effective["strengths"], json!([3, 20, 20]));
    }

    #[test]
    fn output_while_disarmed_is_denied() {
        let snapshot = GateSnapshot {
            armed: false,
            ..armed()
        };
        assert_eq!(
            deny_code(decide(&snapshot, &function(1))),
            ErrorCode::NotArmed
        );
        assert_eq!(
            deny_code(decide(&snapshot, &pattern(vec![1], 500))),
            ErrorCode::NotArmed
        );
    }

    #[test]
    fn output_without_a_target_is_denied() {
        let snapshot = GateSnapshot {
            target_configured: false,
            ..armed()
        };
        assert_eq!(
            deny_code(decide(&snapshot, &function(1))),
            ErrorCode::TargetUnconfigured
        );
    }

    #[test]
    fn stop_is_allowed_in_every_state() {
        for snapshot in [
            GateSnapshot {
                armed: false,
                target_configured: false,
                ceiling: 20,
            },
            GateSnapshot {
                armed: false,
                target_configured: true,
                ceiling: 20,
            },
            armed(),
        ] {
            assert!(matches!(
                decide(&snapshot, &OutputIntent::Stop),
                Decision::Allow { .. }
            ));
        }
    }

    #[test]
    fn indefinite_output_is_refused_not_clamped() {
        let mut intent = function(5);
        if let OutputIntent::Function(f) = &mut intent {
            f.time_sec = 0;
        }
        assert_eq!(
            deny_code(decide(&armed(), &intent)),
            ErrorCode::RequestInvalid
        );
        if let OutputIntent::Function(f) = &mut intent {
            f.time_sec = TIME_SEC_MAX + 1;
        }
        assert_eq!(
            deny_code(decide(&armed(), &intent)),
            ErrorCode::RequestInvalid
        );
    }

    #[test]
    fn fifty_one_strengths_are_refused() {
        let intent = pattern(vec![1; 51], 500);
        assert_eq!(
            deny_code(decide(&armed(), &intent)),
            ErrorCode::RequestInvalid
        );
        let intent = pattern(vec![1; 50], 500);
        assert!(matches!(decide(&armed(), &intent), Decision::Allow { .. }));
    }

    #[test]
    fn interval_at_or_below_100_ms_is_refused() {
        for ms in [0, 50, 100] {
            assert_eq!(
                deny_code(decide(&armed(), &pattern(vec![1], ms))),
                ErrorCode::RequestInvalid
            );
        }
        assert!(matches!(
            decide(&armed(), &pattern(vec![1], 101)),
            Decision::Allow { .. }
        ));
    }

    #[test]
    fn malformed_patterns_are_refused() {
        let bad_features = ["x", "vv", "vibrate", "v r", "V"];
        for features in bad_features {
            let intent = OutputIntent::Pattern(PatternIntent {
                features: features.to_string(),
                interval_ms: 500,
                strengths: vec![1],
                time_sec: 5,
                toy: None,
            });
            assert_eq!(
                deny_code(decide(&armed(), &intent)),
                ErrorCode::RequestInvalid,
                "features {features:?}"
            );
        }
        let empty = pattern(vec![], 500);
        assert_eq!(
            deny_code(decide(&armed(), &empty)),
            ErrorCode::RequestInvalid
        );
        let negative = pattern(vec![-1], 500);
        assert_eq!(
            deny_code(decide(&armed(), &negative)),
            ErrorCode::RequestInvalid
        );
    }

    #[test]
    fn unknown_function_and_preset_are_refused() {
        let intent = OutputIntent::Function(FunctionIntent {
            feature: "Explode".to_string(),
            strength: 1,
            time_sec: 5,
            toy: None,
        });
        assert_eq!(
            deny_code(decide(&armed(), &intent)),
            ErrorCode::RequestInvalid
        );
        let intent = OutputIntent::Preset(PresetIntent {
            name: "custom-thing".to_string(),
            time_sec: 5,
            toy: None,
        });
        assert_eq!(
            deny_code(decide(&armed(), &intent)),
            ErrorCode::RequestInvalid
        );
    }

    #[test]
    fn negative_strength_and_oversized_toy_are_refused() {
        assert_eq!(
            deny_code(decide(&armed(), &function(-1))),
            ErrorCode::RequestInvalid
        );
        let intent = OutputIntent::Function(FunctionIntent {
            feature: "Vibrate".to_string(),
            strength: 1,
            time_sec: 5,
            toy: Some("x".repeat(TOY_ID_BYTES_MAX + 1)),
        });
        assert_eq!(
            deny_code(decide(&armed(), &intent)),
            ErrorCode::RequestInvalid
        );
    }
}
