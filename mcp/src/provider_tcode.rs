//! The TCode multi-axis provider (KoiSync landing), beside the Buttplug
//! provider: SR6 / OSR2 / SSR1 over USB serial or UDP, six axes as
//! first-class command space, per-axis windows and slew bounds, and a
//! 50 Hz emission cadence. Spec: docs/SPEC-KOISYNC.md.
//!
//! Contract, unchanged in kind: the provider executes only gate output.
//! An axis frame reaches the link only as an [`AllowedAxes`] built by
//! `decide::decide_axes`; the frame text comes from
//! `decide::tcode_wire`. `zero()` sends `DSTOP` synchronously; the
//! server's stop, disarm, watchdog, session-drop, and shutdown paths
//! all reach it. The provider is opt-in (`[tcode] enabled = true`),
//! off by default.
//!
//! The depth solver (SPEC-KOISYNC §5) lives here as a pure transform
//! from a frozen depth curve to an L0 curve: it never reads a game.

use std::io::Write;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::decide::{AllowedAxes, AllowedOutput, AxisLimit, TCODE_AXES, TCODE_STOP, tcode_wire};
use crate::error::{AcheError, ErrorCode, Result, err};
use crate::pattern::{Function, Pattern};
use crate::provider::{Capability, Device, DeviceProvider, ProviderId, ProviderProbe};

/// The 50 Hz emission law (SPEC-KOISYNC §3): one frame per 20 ms.
pub const TCODE_FRAME_MS: u64 = 20;
/// The `I` interval sent with each frame (`Devices.cs:254`).
pub const TCODE_FRAME_INTERVAL_MS: i64 = 20;
/// The single device id this provider serves.
pub const TCODE_DEVICE_ID: &str = "tcode:0";

/// The pattern functions that are TCode axes, in axis order.
pub const AXIS_FUNCTIONS: [Function; 6] = [
    Function::StrokePosition,
    Function::Surge,
    Function::Sway,
    Function::Twist,
    Function::Roll,
    Function::Pitch,
];

/// The axis index a function drives on a TCode device.
#[must_use]
pub fn axis_of(function: Function) -> Option<usize> {
    AXIS_FUNCTIONS.iter().position(|f| *f == function)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Sr6,
    Osr2,
    Ssr1,
}

impl DeviceKind {
    pub fn parse(name: &str) -> Option<DeviceKind> {
        match name {
            "SR6" => Some(DeviceKind::Sr6),
            "OSR2" => Some(DeviceKind::Osr2),
            "SSR1" => Some(DeviceKind::Ssr1),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceKind::Sr6 => "SR6",
            DeviceKind::Osr2 => "OSR2",
            DeviceKind::Ssr1 => "SSR1",
        }
    }

    /// Supported axes (`src/Shared/Motion.cs:318-323`).
    #[must_use]
    pub fn axes(self) -> &'static [usize] {
        match self {
            DeviceKind::Sr6 => &[0, 1, 2, 3, 4, 5],
            DeviceKind::Osr2 => &[0, 4, 5],
            DeviceKind::Ssr1 => &[0],
        }
    }
}

/// A byte link to a TCode device. One frame per call.
pub trait TcodeLink: Send {
    fn send(&mut self, bytes: &[u8]) -> Result<()>;
}

/// UDP: one ASCII frame per datagram, no reply (`Devices.cs:45-52`).
pub struct UdpLink {
    socket: UdpSocket,
}

impl UdpLink {
    pub fn open(host: &str, port: u16) -> Result<UdpLink> {
        let socket = UdpSocket::bind(("0.0.0.0", 0))
            .and_then(|s| s.connect((host, port)).map(|()| s))
            .map_err(|e| AcheError::new(ErrorCode::TargetUnreachable, format!("tcode udp: {e}")))?;
        Ok(UdpLink { socket })
    }
}

impl TcodeLink for UdpLink {
    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        match self.socket.send(bytes) {
            Ok(n) if n == bytes.len() => Ok(()),
            Ok(_) => err(ErrorCode::TargetUnreachable, "tcode udp: short send"),
            Err(e) => err(ErrorCode::TargetUnreachable, format!("tcode udp: {e}")),
        }
    }
}

/// USB serial: 8N1, no flow control, 200 ms write timeout
/// (`SerialTransport.cs:49-60`). A short write is an error (`:75`).
pub struct SerialLink {
    port: Box<dyn serialport::SerialPort>,
}

impl SerialLink {
    pub fn open(path: &str, baud: u32) -> Result<SerialLink> {
        let port = serialport::new(path, baud)
            .timeout(Duration::from_millis(200))
            .open()
            .map_err(|e| {
                AcheError::new(
                    ErrorCode::TargetUnreachable,
                    format!("tcode serial {path}: {e}"),
                )
            })?;
        Ok(SerialLink { port })
    }
}

impl TcodeLink for SerialLink {
    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.port
            .write_all(bytes)
            .and_then(|()| self.port.flush())
            .map_err(|e| AcheError::new(ErrorCode::TargetUnreachable, format!("tcode serial: {e}")))
    }
}

/// The fixture link: records every frame, can inject a dropout. Used in
/// mock mode (labelled `fixture`) and by the replay and gate suites.
#[derive(Clone, Default)]
pub struct RecordingLink {
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
    fail: Arc<Mutex<bool>>,
}

impl RecordingLink {
    #[must_use]
    pub fn new() -> RecordingLink {
        RecordingLink::default()
    }

    #[cfg(test)]
    pub fn frames(&self) -> Vec<Vec<u8>> {
        self.frames.lock().unwrap().clone()
    }

    #[cfg(test)]
    pub fn set_fail(&self, fail: bool) {
        *self.fail.lock().unwrap() = fail;
    }
}

impl TcodeLink for RecordingLink {
    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        if *self.fail.lock().unwrap_or_else(|p| p.into_inner()) {
            return err(
                ErrorCode::TargetUnreachable,
                "fixture tcode: injected dropout",
            );
        }
        self.frames
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(bytes.to_vec());
        Ok(())
    }
}

struct Inner {
    link: Option<Box<dyn TcodeLink>>,
    /// The last effective frame, per axis: the slew reference.
    last: [Option<u8>; 6],
}

/// The TCode provider. Disabled, it is present but refuses (like the
/// other providers' inert state).
pub struct TcodeProvider {
    state: &'static str,
    note: String,
    kind: DeviceKind,
    limits: [AxisLimit; 6],
    inner: Mutex<Inner>,
}

impl TcodeProvider {
    pub fn disabled() -> TcodeProvider {
        TcodeProvider {
            state: "disabled",
            note: "tcode provider is disabled by configuration ([tcode] enabled = false)".into(),
            kind: DeviceKind::Sr6,
            limits: [AxisLimit {
                min: 0,
                max: 0,
                max_step: 0,
            }; 6],
            inner: Mutex::new(Inner {
                link: None,
                last: [None; 6],
            }),
        }
    }

    pub fn unavailable(note: impl Into<String>) -> TcodeProvider {
        TcodeProvider {
            state: "unavailable",
            note: note.into(),
            ..TcodeProvider::disabled()
        }
    }

    pub fn new(
        state: &'static str,
        kind: DeviceKind,
        limits: [AxisLimit; 6],
        link: Box<dyn TcodeLink>,
    ) -> TcodeProvider {
        TcodeProvider {
            state,
            note: format!(
                "tcode {} link open; device behaviour is not verified",
                kind.as_str()
            ),
            kind,
            limits,
            inner: Mutex::new(Inner {
                link: Some(link),
                last: [None; 6],
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[must_use]
    pub fn kind(&self) -> DeviceKind {
        self.kind
    }

    #[must_use]
    pub fn limits(&self) -> [AxisLimit; 6] {
        self.limits
    }

    /// The slew reference for the gate.
    #[must_use]
    pub fn last(&self) -> [Option<u8>; 6] {
        self.lock().last
    }

    /// Send one approved frame; on success it becomes the slew
    /// reference.
    pub fn execute_axes(&self, frame: &AllowedAxes) -> Result<()> {
        let mut inner = self.lock();
        let Some(link) = inner.link.as_mut() else {
            return err(ErrorCode::ProviderUnavailable, self.note.clone());
        };
        link.send(tcode_wire(frame).as_bytes())?;
        for &(axis, position) in &frame.values {
            inner.last[axis] = Some(position);
        }
        Ok(())
    }
}

impl DeviceProvider for TcodeProvider {
    fn probe(&self) -> ProviderProbe {
        ProviderProbe {
            provider: ProviderId::Tcode,
            state: self.state,
            available: self.lock().link.is_some(),
            note: self.note.clone(),
        }
    }

    fn discover(&self) -> Result<Vec<Device>> {
        if self.lock().link.is_none() {
            return err(ErrorCode::ProviderUnavailable, self.note.clone());
        }
        let capabilities = self
            .kind
            .axes()
            .iter()
            .map(|&axis| Capability {
                feature: AXIS_FUNCTIONS[axis].as_str().to_string(),
                max: Some(100),
                native: format!("tcode:{}", TCODE_AXES[axis]),
                steps: Some(10_000),
            })
            .collect();
        Ok(vec![Device {
            id: TCODE_DEVICE_ID.to_string(),
            provider: ProviderId::Tcode,
            name: format!("TCode {}", self.kind.as_str()),
            kind: Some(self.kind.as_str().to_string()),
            battery: None,
            capabilities,
            capability_source: if self.state == "mock" {
                "fixture"
            } else {
                "reported"
            },
        }])
    }

    /// TCode devices take axis frames only, through the program engine;
    /// a function/pattern/preset plan is a typed refusal. Stop zeroes.
    fn execute(&self, plan: &AllowedOutput) -> Result<()> {
        if matches!(plan, AllowedOutput::Stop) {
            self.zero()?;
            return Ok(());
        }
        err(
            ErrorCode::CapabilityUnsupported,
            "tcode devices take axis frames from ache_program_run only",
        )
    }

    fn zero(&self) -> Result<bool> {
        let mut inner = self.lock();
        inner.last = [None; 6];
        match inner.link.as_mut() {
            None => Ok(false),
            Some(link) => {
                link.send(TCODE_STOP)?;
                Ok(true)
            }
        }
    }
}

// ------------------------------------------------------ depth solver

pub const DEPTH_FORMAT: &str = "ache.depth/v1";
pub const DEPTH_SAMPLES_MAX: usize = 90_000;

/// `DriveMappingMode` (`src/Shared/SixAxisDrive.cs:5`); Manual is not
/// a transform and is not admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepthMode {
    Absolute,
    Soft,
    Relative,
}

/// Per-character calibration, frozen in the spec (metres).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthCalibration {
    /// Model shaft length (`pose.Length`).
    pub model_m: f64,
    /// The user's length (`player`).
    pub player_m: f64,
    /// Game-side length (`game`). Give it, or `body_height_m` to derive
    /// it per character (`ShaftStrokeMapping.FromBodyHeight`).
    #[serde(default)]
    pub game_m: Option<f64>,
    #[serde(default)]
    pub body_height_m: Option<f64>,
    /// Device travel (`stroke`).
    pub stroke_m: f64,
    pub mode: DepthMode,
    /// Output `100 - L0` (SPEC-KOISYNC D4: polarity is an S4 fact).
    #[serde(default)]
    pub invert: bool,
}

impl DepthCalibration {
    /// The game length: given, or derived from the character's height.
    #[must_use]
    pub fn game_length(&self) -> f64 {
        match (self.game_m, self.body_height_m) {
            (Some(game), _) => game,
            (None, Some(height)) => length_from_body_height(height),
            (None, None) => f64::NAN,
        }
    }
}

/// `ache.depth/v1`: a depth curve sampled on the 20 ms grid.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthSpec {
    pub format: String,
    pub id: String,
    pub calibration: DepthCalibration,
    /// `[target_depth_m, engagement]` per 20 ms sample.
    pub samples: Vec<[f64; 2]>,
}

fn valid_length(value: f64, low: f64) -> bool {
    value.is_finite() && (low..=0.5).contains(&value)
}

/// Interaction length from body height (`ShaftStrokeMapping.cs:14-18`).
#[must_use]
pub fn length_from_body_height(height_m: f64) -> f64 {
    let scale = if height_m.is_finite() && (0.5..=5.0).contains(&height_m) {
        height_m / 1.70
    } else {
        1.0
    };
    (0.130 * scale).clamp(0.02, 0.5)
}

/// `SoftDepth` (`ShaftStrokeMapping.cs:34-40`).
#[must_use]
pub fn soft_depth(depth: f64, limit: f64) -> f64 {
    let knee = limit * 0.8;
    let shoulder = limit - knee;
    if depth <= knee {
        return depth.max(0.0);
    }
    let excess = depth - knee;
    knee + shoulder * (excess / (shoulder + excess))
}

/// One depth sample → L0 fraction (`ShaftStrokeMapping.cs:20-32`).
#[must_use]
pub fn depth_fraction(target_depth: f64, engagement: f64, cal: &DepthCalibration) -> f64 {
    if !(engagement.is_finite() && target_depth.is_finite()) || engagement <= 0.0 {
        return 0.0;
    }
    let mut depth = target_depth.max(0.0) * engagement.min(1.0);
    match cal.mode {
        DepthMode::Soft => depth *= cal.player_m / cal.model_m,
        DepthMode::Relative => depth *= cal.player_m.min(cal.game_length()) / cal.model_m,
        DepthMode::Absolute => {}
    }
    let limit = cal.player_m.min(cal.stroke_m);
    let mapped = match cal.mode {
        DepthMode::Absolute => depth.clamp(0.0, limit),
        _ => soft_depth(depth, limit),
    };
    mapped / cal.stroke_m
}

/// Compile a depth spec into an L0 (`StrokePosition`) pattern through
/// the one funscript adapter: each 20 ms sample becomes an action.
pub fn depth_pattern(value: &serde_json::Value) -> Result<Pattern> {
    let spec: DepthSpec = serde_json::from_value(value.clone()).map_err(|e| {
        AcheError::new(
            ErrorCode::RequestInvalid,
            format!("invalid depth spec: {e}"),
        )
    })?;
    if spec.format != DEPTH_FORMAT {
        return err(
            ErrorCode::RequestInvalid,
            format!("depth format must be {DEPTH_FORMAT:?}"),
        );
    }
    let cal = &spec.calibration;
    if cal.game_m.is_some() == cal.body_height_m.is_some() {
        return err(
            ErrorCode::RequestInvalid,
            "calibration needs exactly one of game_m, body_height_m",
        );
    }
    if cal
        .body_height_m
        .is_some_and(|h| !(h.is_finite() && (0.5..=5.0).contains(&h)))
    {
        return err(ErrorCode::RequestInvalid, "body_height_m must be 0.5..=5 m");
    }
    if !valid_length(cal.model_m, 0.001)
        || !valid_length(cal.player_m, 0.02)
        || !valid_length(cal.game_length(), 0.02)
        || !valid_length(cal.stroke_m, 0.02)
    {
        return err(
            ErrorCode::RequestInvalid,
            "calibration lengths: model 0.001..=0.5 m, player/game/stroke 0.02..=0.5 m",
        );
    }
    if spec.samples.len() < 2 || spec.samples.len() > DEPTH_SAMPLES_MAX {
        return err(
            ErrorCode::RequestInvalid,
            format!("depth samples must number 2..={DEPTH_SAMPLES_MAX}"),
        );
    }
    let actions: Vec<serde_json::Value> = spec
        .samples
        .iter()
        .enumerate()
        .map(|(k, [depth, engagement])| {
            let fraction = depth_fraction(*depth, *engagement, cal).clamp(0.0, 1.0);
            let pos = crate::pattern::quantize(fraction * 100.0);
            let pos = if cal.invert { 100 - pos } else { pos };
            json!({"at": k as u64 * TCODE_FRAME_MS, "pos": pos})
        })
        .collect();
    crate::funscript::import(
        &spec.id,
        Function::StrokePosition,
        &json!({"actions": actions}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::{AxesIntent, GateSnapshot, decide_axes};

    fn provider(kind: DeviceKind) -> (TcodeProvider, RecordingLink) {
        let link = RecordingLink::new();
        let limits = [AxisLimit {
            min: 0,
            max: 100,
            max_step: 100,
        }; 6];
        (
            TcodeProvider::new("mock", kind, limits, Box::new(link.clone())),
            link,
        )
    }

    fn armed() -> GateSnapshot {
        GateSnapshot {
            armed: true,
            target_configured: true,
            ceiling: 20,
        }
    }

    // --- validation ---------------------------------------------------

    #[test]
    fn discovery_reports_the_kind_axes_as_capabilities() {
        let (sr6, _) = provider(DeviceKind::Sr6);
        let devices = sr6.discover().unwrap();
        assert_eq!(devices[0].id, TCODE_DEVICE_ID);
        assert_eq!(devices[0].capabilities.len(), 6);
        let (osr2, _) = provider(DeviceKind::Osr2);
        let features: Vec<String> = osr2.discover().unwrap()[0]
            .capabilities
            .iter()
            .map(|c| c.feature.clone())
            .collect();
        assert_eq!(features, vec!["StrokePosition", "Roll", "Pitch"]);
    }

    #[test]
    fn approved_frames_reach_the_link_as_tcode_and_set_the_slew_reference() {
        let (p, link) = provider(DeviceKind::Sr6);
        let plan = decide_axes(
            &armed(),
            &AxesIntent {
                values: vec![(0, 25), (5, 75)],
                interval_ms: 20,
            },
            p.kind().axes(),
            &p.limits(),
            &p.last(),
        )
        .unwrap();
        p.execute_axes(&plan).unwrap();
        assert_eq!(link.frames(), vec![b"L02500I20 R27500I20\n".to_vec()]);
        assert_eq!(p.last()[0], Some(25));
        assert!(p.zero().unwrap());
        assert_eq!(link.frames().last().unwrap(), b"DSTOP\n");
        assert_eq!(p.last(), [None; 6], "a stop clears the slew reference");
    }

    #[test]
    fn depth_solver_follows_the_koisync_laws() {
        let cal = DepthCalibration {
            model_m: 0.14,
            player_m: 0.14,
            game_m: Some(0.14),
            body_height_m: None,
            stroke_m: 0.12,
            mode: DepthMode::Absolute,
            invert: false,
        };
        // Absolute: clamp to min(player, stroke), divide by stroke.
        assert_eq!(depth_fraction(0.06, 1.0, &cal), 0.5);
        assert_eq!(depth_fraction(0.5, 1.0, &cal), 1.0);
        assert_eq!(
            depth_fraction(0.06, 0.0, &cal),
            0.0,
            "no engagement, no depth"
        );
        // Soft: never exceeds the limit, linear to the knee.
        let soft = DepthCalibration {
            mode: DepthMode::Soft,
            ..cal.clone()
        };
        assert!(depth_fraction(10.0, 1.0, &soft) < 1.0);
        assert!((soft_depth(0.05, 0.1) - 0.05).abs() < 1e-12);
        assert!((length_from_body_height(1.70) - 0.130).abs() < 1e-12);
        assert_eq!(length_from_body_height(f64::NAN), 0.130);
        // Per-character calibration: a 1.36 m character scales the game
        // length to 0.104 m, which binds Relative mode below the player.
        let tall = DepthCalibration {
            game_m: None,
            body_height_m: Some(1.36),
            mode: DepthMode::Relative,
            ..cal.clone()
        };
        assert!((tall.game_length() - 0.104).abs() < 1e-12);
        let expected = 0.06 * (0.104 / 0.14) / 0.12;
        assert!((depth_fraction(0.06, 1.0, &tall) - expected).abs() < 1e-12);
    }

    #[test]
    fn depth_spec_compiles_to_an_l0_pattern_through_the_adapter() {
        let spec = json!({
            "format": DEPTH_FORMAT, "id": "d",
            "calibration": {"model_m": 0.14, "player_m": 0.14, "game_m": 0.14, "stroke_m": 0.12, "mode": "absolute"},
            "samples": [[0.0, 1.0], [0.06, 1.0], [0.12, 1.0]]
        });
        let p = depth_pattern(&spec).unwrap();
        assert_eq!(p.function(), Function::StrokePosition);
        assert_eq!(p.total_ms(), 40);
        assert_eq!(p.sample(20, 0), vec![0, 50]);
        let mut inverted = spec.clone();
        inverted["calibration"]["invert"] = json!(true);
        assert_eq!(
            depth_pattern(&inverted).unwrap().sample(20, 0),
            vec![100, 50]
        );
    }

    // --- adversarial --------------------------------------------------

    #[test]
    fn mid_command_dropout_is_a_typed_error_and_stop_still_attempts() {
        let (p, link) = provider(DeviceKind::Sr6);
        link.set_fail(true);
        let plan = decide_axes(
            &armed(),
            &AxesIntent {
                values: vec![(0, 10)],
                interval_ms: 20,
            },
            p.kind().axes(),
            &p.limits(),
            &p.last(),
        )
        .unwrap();
        assert_eq!(
            p.execute_axes(&plan).unwrap_err().code,
            ErrorCode::TargetUnreachable
        );
        assert_eq!(p.last()[0], None, "a failed send is not a reference");
        assert!(p.zero().is_err(), "a dead link reports, never pretends");
    }

    #[test]
    fn disabled_provider_refuses_and_plans_other_than_stop_are_refused() {
        let off = TcodeProvider::disabled();
        assert!(!off.probe().available);
        assert_eq!(
            off.discover().unwrap_err().code,
            ErrorCode::ProviderUnavailable
        );
        assert!(!off.zero().unwrap());
        let (p, link) = provider(DeviceKind::Sr6);
        let plan = AllowedOutput::Function {
            feature: "Vibrate".into(),
            strength: 5,
            time_sec: 1,
            toy: TCODE_DEVICE_ID.into(),
            clamped: false,
        };
        assert_eq!(
            p.execute(&plan).unwrap_err().code,
            ErrorCode::CapabilityUnsupported
        );
        assert!(link.frames().is_empty());
    }

    #[test]
    fn malformed_depth_specs_are_refused() {
        let base = json!({
            "format": DEPTH_FORMAT, "id": "d",
            "calibration": {"model_m": 0.14, "player_m": 0.14, "game_m": 0.14, "stroke_m": 0.12, "mode": "absolute"},
            "samples": [[0.0, 1.0], [0.06, 1.0]]
        });
        let mut bad = Vec::new();
        let mut v = base.clone();
        v["calibration"]["stroke_m"] = json!(0.0);
        bad.push(v);
        let mut v = base.clone();
        v["calibration"]["mode"] = json!("manual");
        bad.push(v);
        let mut v = base.clone();
        v["samples"] = json!([[0.0, 1.0]]);
        bad.push(v);
        let mut v = base.clone();
        v["format"] = json!("x");
        bad.push(v);
        let mut v = base.clone();
        v["calibration"]["model_m"] = json!(9.0);
        bad.push(v);
        let mut v = base.clone();
        v["calibration"]["body_height_m"] = json!(1.6);
        bad.push(v);
        let mut v = base.clone();
        v["calibration"].as_object_mut().unwrap().remove("game_m");
        bad.push(v);
        let mut v = base.clone();
        v["calibration"].as_object_mut().unwrap().remove("game_m");
        v["calibration"]["body_height_m"] = json!(9.0);
        bad.push(v);
        for spec in bad {
            assert!(depth_pattern(&spec).is_err(), "{spec}");
        }
    }
}
