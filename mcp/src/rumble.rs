//! `ache.rumble/v1` — the rumble event-source input.
//!
//! Ache is otherwise output-only. This module is the inverse: an
//! external, pre-registered source (an estate game first: Torogao)
//! streams rumble state, and the router turns it into ordinary
//! function intents that pass the one `decide()` gate at the session
//! tick, never faster. The design is extracted from the Intiface Game
//! Haptics Router (BSD-3; see EXTRACTION-GHR.md in the lane record);
//! its mechanism (process injection) is excluded: ache never injects
//! into another process. Sources come to ache, by registration.
//!
//! The frozen spec, v1:
//!
//! - **Event** (one JSON object per line):
//!   `{"spec":"ache.rumble/v1","source":"<id>","heavy":0..1,"light":0..1,"t_ms":<u64>}`.
//!   `heavy` is the low-frequency motor, `light` the high-frequency
//!   one (XInput's left/right). `t_ms` is the source's own monotonic
//!   clock and must strictly increase per source.
//! - **Registration** (config only): source id, `gain_percent`
//!   (1..=500, GHR's 0..5x multiplier), `ceiling_percent` (0..=100,
//!   the highest motor value that source may send).
//! - **Mapping table** (config only): (source, motor) → (device,
//!   function, max_strength 0..=20). Several mappings landing on one
//!   (device, function) combine by maximum.
//! - **Decay law**: a value holds until updated (set-and-hold); a
//!   source silent for more than [`SILENCE_DECAY_MS`] (by ache's own
//!   receive clock, never the source's) decays to 0.
//! - **Emission**: only at the tick (`tick_ms`, 100..=1000, default
//!   100 — GHR's packet gap), only through `decide()`, each output a
//!   bounded `time_sec` = [`OUTPUT_TIME_SEC`] function, refreshed
//!   before it lapses while it holds.
//! - **Refusals**, each by name ([`Refusal::name`]): `rumble_disabled`,
//!   `malformed_event`, `unregistered_source`, `value_out_of_range`,
//!   `over_ceiling`, `stale_timestamp`, `tick_too_fast`. A refused
//!   event changes nothing — not the held value, not the silence
//!   clock — so a misbehaving source still decays.
//!
//! Master-off by default: `[rumble] enabled = false` unless the
//! operator turns it on, and even then nothing reaches a device unless
//! the server's master toggle is explicitly armed (the gate's rule).

use std::collections::BTreeMap;

use serde::Deserialize;

/// The spec identifier every event carries.
pub const SPEC: &str = "ache.rumble/v1";
/// Silence longer than this decays a source to zero, milliseconds.
pub const SILENCE_DECAY_MS: u64 = 500;
pub const TICK_MS_DEFAULT: u64 = 100;
pub const TICK_MS_MIN: u64 = 100;
pub const TICK_MS_MAX: u64 = 1_000;
/// Duration of each emitted function output, seconds: the smallest
/// bounded duration the gate allows, so a dead router stops the toy
/// within a second on its own.
pub const OUTPUT_TIME_SEC: i64 = 1;
/// A held nonzero output is re-emitted at least this often, so it
/// never lapses while the source still holds it.
pub const REFRESH_MS: u64 = 500;
pub const SOURCES_MAX: usize = 16;
pub const MAPPINGS_MAX: usize = 32;
pub const SOURCE_ID_BYTES_MAX: usize = 64;
pub const DEVICE_ID_BYTES_MAX: usize = 128;
pub const GAIN_PERCENT_MIN: u16 = 1;
pub const GAIN_PERCENT_MAX: u16 = 500;
pub const CEILING_PERCENT_MAX: u8 = 100;
pub const MAX_STRENGTH_MAX: u8 = 20;
/// Longest event line accepted, bytes.
pub const EVENT_BYTES_MAX: usize = 1_024;
/// Unix socket path limit (sun_path), bytes.
pub const SOCKET_PATH_BYTES_MAX: usize = 107;
/// Functions a rumble mapping may drive: her devices' continuous
/// actuators. Position/stroke functions are not rumble-shaped and are
/// refused at configuration.
pub const FUNCTIONS: &[&str] = &["Vibrate", "Rotate", "Oscillate", "Suction"];
/// Motor values are carried as integer thousandths: 0..=1000.
const MILLI: u32 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Motor {
    Heavy,
    Light,
}

impl Motor {
    pub fn parse(text: &str) -> Option<Motor> {
        match text {
            "heavy" => Some(Motor::Heavy),
            "light" => Some(Motor::Light),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRegistration {
    pub id: String,
    pub gain_percent: u16,
    pub ceiling_percent: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub source: String,
    pub motor: Motor,
    pub device: String,
    pub function: String,
    pub max_strength: u8,
}

/// The validated `[rumble]` configuration. Built only by `config.rs`
/// (which validates every field) or `Default` (disabled, empty).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RumbleConfig {
    pub enabled: bool,
    pub tick_ms: u64,
    /// Absolute path of the live input socket; `None` means no live
    /// listener (events can still be fed in-process).
    pub socket: Option<String>,
    pub sources: Vec<SourceRegistration>,
    pub mappings: Vec<Mapping>,
}

impl Default for RumbleConfig {
    fn default() -> RumbleConfig {
        RumbleConfig {
            enabled: false,
            tick_ms: TICK_MS_DEFAULT,
            socket: None,
            sources: Vec::new(),
            mappings: Vec::new(),
        }
    }
}

/// Every way the router refuses, named. The name is part of the spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Disabled,
    Malformed(String),
    UnregisteredSource(String),
    ValueOutOfRange {
        motor: Motor,
        value: String,
    },
    OverCeiling {
        source: String,
        motor: Motor,
        value_milli: u32,
        ceiling_milli: u32,
    },
    StaleTimestamp {
        source: String,
        t_ms: u64,
        last_t_ms: u64,
    },
    TickTooFast {
        now_ms: u64,
        next_ms: u64,
    },
}

impl Refusal {
    pub fn name(&self) -> &'static str {
        match self {
            Refusal::Disabled => "rumble_disabled",
            Refusal::Malformed(_) => "malformed_event",
            Refusal::UnregisteredSource(_) => "unregistered_source",
            Refusal::ValueOutOfRange { .. } => "value_out_of_range",
            Refusal::OverCeiling { .. } => "over_ceiling",
            Refusal::StaleTimestamp { .. } => "stale_timestamp",
            Refusal::TickTooFast { .. } => "tick_too_fast",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Refusal::Disabled => "the rumble input is off ([rumble] enabled = false)".into(),
            Refusal::Malformed(why) => why.clone(),
            Refusal::UnregisteredSource(id) => format!("source {id:?} is not registered"),
            Refusal::ValueOutOfRange { motor, value } => {
                format!("{motor:?} value {value} is outside 0..=1")
            }
            Refusal::OverCeiling {
                source,
                motor,
                value_milli,
                ceiling_milli,
            } => format!(
                "source {source:?} {motor:?} {value_milli}/1000 exceeds its registered ceiling {ceiling_milli}/1000"
            ),
            Refusal::StaleTimestamp {
                source,
                t_ms,
                last_t_ms,
            } => format!("source {source:?} t_ms {t_ms} does not follow {last_t_ms}"),
            Refusal::TickTooFast { now_ms, next_ms } => {
                format!("tick at {now_ms} ms precedes the next tick slot at {next_ms} ms")
            }
        }
    }

    pub fn to_error(&self) -> crate::error::AcheError {
        crate::error::AcheError::new(
            crate::error::ErrorCode::RequestInvalid,
            format!("rumble refused ({}): {}", self.name(), self.detail()),
        )
    }
}

/// One event, as received. Values are validated by [`Router::ingest`].
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RumbleEvent {
    pub spec: String,
    pub source: String,
    pub heavy: f64,
    pub light: f64,
    pub t_ms: u64,
}

impl RumbleEvent {
    /// Decode one event line. Size, JSON shape, and spec version are
    /// checked here; values and registration in [`Router::ingest`].
    pub fn parse(line: &str) -> Result<RumbleEvent, Refusal> {
        if line.len() > EVENT_BYTES_MAX {
            return Err(Refusal::Malformed(format!(
                "event line exceeds {EVENT_BYTES_MAX} bytes"
            )));
        }
        let event: RumbleEvent = serde_json::from_str(line)
            .map_err(|error| Refusal::Malformed(format!("not an {SPEC} event: {error}")))?;
        if event.spec != SPEC {
            return Err(Refusal::Malformed(format!(
                "spec {:?} is not {SPEC}",
                crate::server::clip(&event.spec)
            )));
        }
        Ok(event)
    }
}

/// One output the router asks the gate for at a tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RumbleOutput {
    pub device: String,
    pub function: String,
    /// Requested strength on the unified 0..=20 scale, after gain and
    /// the mapping's own max; the gate still clamps to the ceiling.
    pub strength: i64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Held {
    heavy: u32,
    light: u32,
    last_t_ms: Option<u64>,
    last_rx_ms: u64,
}

#[derive(Clone, Copy, Debug)]
struct Emitted {
    strength: i64,
    at_ms: u64,
}

/// The pure router: no clock, no I/O. Time is always passed in, so a
/// recorded stream replays byte-identically.
#[derive(Debug)]
pub struct Router {
    config: RumbleConfig,
    held: BTreeMap<String, Held>,
    emitted: BTreeMap<(String, String), Emitted>,
    last_tick_ms: Option<u64>,
}

fn to_milli(motor: Motor, value: f64) -> Result<u32, Refusal> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(Refusal::ValueOutOfRange {
            motor,
            value: value.to_string(),
        });
    }
    // In range by the check above, so the cast cannot truncate.
    let milli = (value * f64::from(MILLI)).round() as u32;
    debug_assert!(milli <= MILLI);
    Ok(milli)
}

impl Router {
    pub fn new(config: RumbleConfig) -> Router {
        assert!(config.sources.len() <= SOURCES_MAX);
        assert!(config.mappings.len() <= MAPPINGS_MAX);
        assert!((TICK_MS_MIN..=TICK_MS_MAX).contains(&config.tick_ms));
        Router {
            config,
            held: BTreeMap::new(),
            emitted: BTreeMap::new(),
            last_tick_ms: None,
        }
    }

    fn source(&self, id: &str) -> Option<&SourceRegistration> {
        self.config.sources.iter().find(|source| source.id == id)
    }

    /// Accept one event at receive time `now_ms`, or refuse it by name.
    /// A refused event changes no state.
    pub fn ingest(&mut self, event: &RumbleEvent, now_ms: u64) -> Result<(), Refusal> {
        if !self.config.enabled {
            return Err(Refusal::Disabled);
        }
        let Some(source) = self.source(&event.source) else {
            return Err(Refusal::UnregisteredSource(
                crate::server::clip(&event.source).to_string(),
            ));
        };
        let ceiling_milli = u32::from(source.ceiling_percent) * 10;
        let heavy = to_milli(Motor::Heavy, event.heavy)?;
        let light = to_milli(Motor::Light, event.light)?;
        for (motor, value_milli) in [(Motor::Heavy, heavy), (Motor::Light, light)] {
            if value_milli > ceiling_milli {
                return Err(Refusal::OverCeiling {
                    source: source.id.clone(),
                    motor,
                    value_milli,
                    ceiling_milli,
                });
            }
        }
        let id = source.id.clone();
        let held = self.held.entry(id.clone()).or_default();
        if let Some(last_t_ms) = held.last_t_ms
            && event.t_ms <= last_t_ms
        {
            return Err(Refusal::StaleTimestamp {
                source: id,
                t_ms: event.t_ms,
                last_t_ms,
            });
        }
        *held = Held {
            heavy,
            light,
            last_t_ms: Some(event.t_ms),
            last_rx_ms: now_ms,
        };
        Ok(())
    }

    /// The session tick: apply the decay law, compute every mapped
    /// output, and return the ones to send now (sorted by device then
    /// function). Refused when called before the next tick slot.
    pub fn tick(&mut self, now_ms: u64) -> Result<Vec<RumbleOutput>, Refusal> {
        if !self.config.enabled {
            return Err(Refusal::Disabled);
        }
        if let Some(last) = self.last_tick_ms {
            let next_ms = last.saturating_add(self.config.tick_ms);
            if now_ms < next_ms {
                return Err(Refusal::TickTooFast { now_ms, next_ms });
            }
        }
        self.last_tick_ms = Some(now_ms);
        for held in self.held.values_mut() {
            if now_ms.saturating_sub(held.last_rx_ms) > SILENCE_DECAY_MS {
                held.heavy = 0;
                held.light = 0;
            }
        }
        let mut wanted: BTreeMap<(String, String), i64> = BTreeMap::new();
        for mapping in &self.config.mappings {
            let source = self
                .source(&mapping.source)
                .expect("config validation binds every mapping to a registered source");
            let value = self
                .held
                .get(&mapping.source)
                .map_or(0, |held| match mapping.motor {
                    Motor::Heavy => held.heavy,
                    Motor::Light => held.light,
                });
            let scaled = (value * u32::from(source.gain_percent) / 100).min(MILLI);
            let max = u32::from(mapping.max_strength);
            let strength = i64::from((scaled * max + MILLI / 2) / MILLI);
            debug_assert!(strength <= i64::from(MAX_STRENGTH_MAX));
            let slot = wanted
                .entry((mapping.device.clone(), mapping.function.clone()))
                .or_insert(0);
            *slot = (*slot).max(strength);
        }
        let mut outputs = Vec::new();
        for (key, strength) in wanted {
            let send = match self.emitted.get(&key) {
                None => strength > 0,
                Some(last) if last.strength != strength => true,
                Some(last) => strength > 0 && now_ms.saturating_sub(last.at_ms) >= REFRESH_MS,
            };
            if send {
                self.emitted.insert(
                    key.clone(),
                    Emitted {
                        strength,
                        at_ms: now_ms,
                    },
                );
                outputs.push(RumbleOutput {
                    device: key.0,
                    function: key.1,
                    strength,
                });
            }
        }
        Ok(outputs)
    }

    /// Forget everything held and emitted: the stop / disarm / drop
    /// path. A source must send again before anything is emitted.
    pub fn reset(&mut self) {
        self.held.clear();
        self.emitted.clear();
    }
}

// ------------------------------------------------------------ live input

/// The live input: a Unix socket the operator names, owner-only
/// (0600) — the filesystem is the authentication, the registration
/// table is the allowlist — and a ticker thread that drives the
/// session tick. Both run only when `[rumble] enabled = true` and a
/// socket is configured.
pub mod live {
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::EVENT_BYTES_MAX;
    use crate::server::Server;

    /// Most concurrent source connections.
    pub const CONNECTIONS_MAX: usize = 4;

    /// Bind the socket and start the listener and ticker threads.
    /// Refuses to replace an existing path.
    pub fn spawn(server: Arc<Server>, socket: &str, tick_ms: u64) -> std::io::Result<()> {
        let path = Path::new(socket);
        if path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("rumble socket {socket} already exists; refusing to replace it"),
            ));
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let epoch = Instant::now();
        let now_ms = move || u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        let ticker = Arc::clone(&server);
        std::thread::Builder::new()
            .name("ache-rumble-tick".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(tick_ms));
                    let _ = ticker.rumble_tick(now_ms());
                }
            })?;
        let open = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("ache-rumble-listen".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    if open.fetch_add(1, Ordering::SeqCst) >= CONNECTIONS_MAX {
                        open.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                    let server = Arc::clone(&server);
                    let held_open = Arc::clone(&open);
                    let reader = std::thread::Builder::new()
                        .name("ache-rumble-source".into())
                        .spawn(move || {
                            // Bound each line: a hostile peer cannot
                            // make the reader buffer without limit.
                            let limit = u64::try_from(EVENT_BYTES_MAX + 1).unwrap_or(u64::MAX);
                            let mut reader = BufReader::new(stream);
                            let mut line = String::new();
                            loop {
                                line.clear();
                                match (&mut reader).take(limit).read_line(&mut line) {
                                    Ok(0) | Err(_) => break,
                                    Ok(_) => {}
                                }
                                if !line.ends_with('\n') {
                                    eprintln!("ache: rumble source sent an overlong line; closed");
                                    break;
                                }
                                if let Err(error) = server.rumble_ingest(line.trim_end(), now_ms())
                                {
                                    eprintln!("ache: {error}");
                                }
                            }
                            held_open.fetch_sub(1, Ordering::SeqCst);
                        });
                    if reader.is_err() {
                        // The closure (and its decrement) never ran.
                        open.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RumbleConfig {
        RumbleConfig {
            enabled: true,
            tick_ms: 100,
            socket: None,
            sources: vec![SourceRegistration {
                id: "torogao".into(),
                gain_percent: 100,
                ceiling_percent: 100,
            }],
            mappings: vec![
                Mapping {
                    source: "torogao".into(),
                    motor: Motor::Heavy,
                    device: "buttplug:1".into(),
                    function: "Vibrate".into(),
                    max_strength: 20,
                },
                Mapping {
                    source: "torogao".into(),
                    motor: Motor::Light,
                    device: "buttplug:0".into(),
                    function: "Vibrate".into(),
                    max_strength: 10,
                },
            ],
        }
    }

    fn event(source: &str, heavy: f64, light: f64, t_ms: u64) -> RumbleEvent {
        RumbleEvent {
            spec: SPEC.into(),
            source: source.into(),
            heavy,
            light,
            t_ms,
        }
    }

    fn out(device: &str, strength: i64) -> RumbleOutput {
        RumbleOutput {
            device: device.into(),
            function: "Vibrate".into(),
            strength,
        }
    }

    // --- validation ---------------------------------------------------

    #[test]
    fn heavy_and_light_map_to_their_own_devices_with_scaling() {
        let mut router = Router::new(config());
        router.ingest(&event("torogao", 0.5, 1.0, 1), 0).unwrap();
        let outputs = router.tick(0).unwrap();
        // heavy 0.5 x max 20 = 10 on buttplug:1; light 1.0 x max 10 = 10.
        assert_eq!(outputs, vec![out("buttplug:0", 10), out("buttplug:1", 10)]);
    }

    #[test]
    fn values_hold_and_refresh_but_do_not_resend_every_tick() {
        let mut router = Router::new(config());
        router.ingest(&event("torogao", 0.25, 0.0, 1), 0).unwrap();
        assert_eq!(router.tick(0).unwrap(), vec![out("buttplug:1", 5)]);
        for now in [100, 200, 300, 400] {
            router
                .ingest(&event("torogao", 0.25, 0.0, now), now)
                .unwrap();
            assert!(router.tick(now).unwrap().is_empty(), "unchanged at {now}");
        }
        router
            .ingest(&event("torogao", 0.25, 0.0, 500), 500)
            .unwrap();
        assert_eq!(router.tick(500).unwrap(), vec![out("buttplug:1", 5)]);
    }

    #[test]
    fn silence_beyond_500_ms_decays_to_zero_once() {
        let mut router = Router::new(config());
        router.ingest(&event("torogao", 1.0, 0.0, 1), 0).unwrap();
        assert_eq!(router.tick(0).unwrap(), vec![out("buttplug:1", 20)]);
        // 500 ms of silence still holds (refresh due exactly at 500).
        assert_eq!(router.tick(500).unwrap(), vec![out("buttplug:1", 20)]);
        // 600 ms: decayed; the zero is emitted once, then nothing.
        assert_eq!(router.tick(600).unwrap(), vec![out("buttplug:1", 0)]);
        assert!(router.tick(700).unwrap().is_empty());
        assert!(router.tick(2_000).unwrap().is_empty());
    }

    #[test]
    fn gain_scales_and_saturates_at_full() {
        let mut cfg = config();
        cfg.sources[0].gain_percent = 300;
        let mut router = Router::new(cfg);
        router.ingest(&event("torogao", 0.1, 0.5, 1), 0).unwrap();
        // heavy 0.1 x 3 = 0.3 -> 6; light 0.5 x 3 saturates at 1.0 -> 10.
        assert_eq!(
            router.tick(0).unwrap(),
            vec![out("buttplug:0", 10), out("buttplug:1", 6)]
        );
    }

    #[test]
    fn mappings_onto_one_function_combine_by_maximum() {
        let mut cfg = config();
        cfg.mappings[1].device = "buttplug:1".into();
        cfg.mappings[1].max_strength = 20;
        let mut router = Router::new(cfg);
        router.ingest(&event("torogao", 0.2, 0.7, 1), 0).unwrap();
        assert_eq!(router.tick(0).unwrap(), vec![out("buttplug:1", 14)]);
    }

    #[test]
    fn event_lines_parse_with_the_spec_tag() {
        let line = r#"{"spec":"ache.rumble/v1","source":"torogao","heavy":0.5,"light":0,"t_ms":7}"#;
        assert_eq!(
            RumbleEvent::parse(line).unwrap(),
            event("torogao", 0.5, 0.0, 7)
        );
    }

    // --- adversarial --------------------------------------------------

    #[test]
    fn unregistered_source_is_refused_by_name_and_changes_nothing() {
        let mut router = Router::new(config());
        let refusal = router
            .ingest(&event("stranger", 1.0, 1.0, 1), 0)
            .unwrap_err();
        assert_eq!(refusal.name(), "unregistered_source");
        assert!(router.tick(0).unwrap().is_empty());
    }

    #[test]
    fn over_ceiling_event_is_refused_by_name_and_does_not_keep_alive() {
        let mut cfg = config();
        cfg.sources[0].ceiling_percent = 60;
        let mut router = Router::new(cfg);
        router.ingest(&event("torogao", 0.6, 0.0, 1), 0).unwrap();
        assert_eq!(router.tick(0).unwrap(), vec![out("buttplug:1", 12)]);
        let refusal = router
            .ingest(&event("torogao", 0.61, 0.0, 2), 300)
            .unwrap_err();
        assert_eq!(refusal.name(), "over_ceiling");
        assert!(refusal.to_error().message.contains("over_ceiling"));
        // The refused event neither raised the value nor reset the
        // silence clock: the source decays on schedule from t=0.
        assert_eq!(router.tick(600).unwrap(), vec![out("buttplug:1", 0)]);
    }

    #[test]
    fn out_of_range_and_non_finite_values_are_refused() {
        let mut router = Router::new(config());
        for (heavy, light) in [
            (1.01, 0.0),
            (-0.1, 0.0),
            (0.0, f64::NAN),
            (f64::INFINITY, 0.0),
        ] {
            let refusal = router
                .ingest(&event("torogao", heavy, light, 1), 0)
                .unwrap_err();
            assert_eq!(refusal.name(), "value_out_of_range");
        }
        assert!(router.tick(0).unwrap().is_empty());
    }

    #[test]
    fn timestamps_must_strictly_increase_per_source() {
        let mut router = Router::new(config());
        router.ingest(&event("torogao", 0.5, 0.0, 10), 0).unwrap();
        for t_ms in [10, 9, 0] {
            let refusal = router
                .ingest(&event("torogao", 1.0, 0.0, t_ms), 50)
                .unwrap_err();
            assert_eq!(refusal.name(), "stale_timestamp");
        }
        assert_eq!(router.tick(100).unwrap(), vec![out("buttplug:1", 10)]);
    }

    #[test]
    fn ticks_faster_than_the_session_tick_are_refused() {
        let mut router = Router::new(config());
        router.ingest(&event("torogao", 0.5, 0.0, 1), 0).unwrap();
        router.tick(0).unwrap();
        let refusal = router.tick(99).unwrap_err();
        assert_eq!(refusal.name(), "tick_too_fast");
        assert!(router.tick(100).is_ok());
    }

    #[test]
    fn disabled_router_refuses_everything_by_name() {
        let mut router = Router::new(RumbleConfig {
            enabled: false,
            ..config()
        });
        assert_eq!(
            router
                .ingest(&event("torogao", 0.5, 0.0, 1), 0)
                .unwrap_err()
                .name(),
            "rumble_disabled"
        );
        assert_eq!(router.tick(0).unwrap_err().name(), "rumble_disabled");
    }

    #[test]
    fn malformed_lines_are_refused_by_name() {
        for line in [
            "",
            "not json",
            r#"{"spec":"ache.rumble/v2","source":"torogao","heavy":0,"light":0,"t_ms":1}"#,
            r#"{"spec":"ache.rumble/v1","source":"torogao","heavy":0,"light":0}"#,
            r#"{"spec":"ache.rumble/v1","source":"torogao","heavy":0,"light":0,"t_ms":1,"strength":20}"#,
            r#"{"spec":"ache.rumble/v1","source":"torogao","heavy":"1","light":0,"t_ms":1}"#,
            r#"{"spec":"ache.rumble/v1","source":"torogao","heavy":0,"light":0,"t_ms":-1}"#,
        ] {
            assert_eq!(
                RumbleEvent::parse(line).unwrap_err().name(),
                "malformed_event",
                "line {line:?}"
            );
        }
        let long = format!(
            r#"{{"spec":"ache.rumble/v1","source":"{}","heavy":0,"light":0,"t_ms":1}}"#,
            "x".repeat(EVENT_BYTES_MAX)
        );
        assert_eq!(
            RumbleEvent::parse(&long).unwrap_err().name(),
            "malformed_event"
        );
    }

    #[test]
    fn reset_forgets_held_values() {
        let mut router = Router::new(config());
        router.ingest(&event("torogao", 1.0, 1.0, 1), 0).unwrap();
        router.reset();
        assert!(router.tick(0).unwrap().is_empty());
    }
}
