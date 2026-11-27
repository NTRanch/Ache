//! Lana's trial rig: a fully offline simulation. Virtual devices
//! only — no provider is constructed for this module, no device path
//! exists from here, and nothing in it can reach ache's `decide()`
//! gate. It exists so the user can feel out what a device *does*
//! (from its real config data) on a willing, in-character guide.
//!
//! Model discipline, same as the torogao guide's state pattern:
//! every number is an integer per-mille owned by this code; the
//! state machine disposes, text only reports. The model is
//! deterministic: same profile + same toy set + same levels + same
//! tick count = same state, bit for bit, independent of the order
//! toys were added in.
//!
//! # The effect model (named, explainable — no magic numbers
//! # without a job)
//!
//! Per tick (the UI ticks at 4 Hz):
//! 1. Each channel of each toy has a normalised drive
//!    `level * 1000 / max_step` in its config's own step range.
//! 2. Channels sum per **zone** with per-actuator pleasure/force
//!    weights ([`ActuatorKind`] tables in `weights`). Force is a
//!    per-toy quantity and stops there; pleasure goes through the
//!    zone response curve, because zones saturate and overstimulate
//!    as a whole, not per toy.
//! 3. Zone response curve ([`SensitivityProfile`]): pleasure rises
//!    linearly with zone drive (scaled by the profile gain and the
//!    zone's own sensitivity) up to the zone's **peak**; past the
//!    peak it falls at the profile's overstimulation slope; past
//!    1.6x the peak the zone's contribution goes negative — that is
//!    discomfort, and the state says so. This inversion is the whole
//!    point of the light-touch preset: strong input is not more
//!    pleasure, it is less, then worse than nothing.
//! 4. A toy's pleasure figure in the live feed is its **marginal**
//!    contribution: zone pleasure with the toy minus zone pleasure
//!    without it. Marginals depend on the set, never on add order.
//! 5. Arousal integrates total pleasure (250 ms ticks), decays when
//!    total pleasure is near zero, and crosses the profile's orgasm
//!    threshold as a **model event**: counter increments, arousal
//!    settles to a refractory level, and a squirt event is recorded
//!    when the profile allows it and the drive at the event includes
//!    internal or suction stimulation (the kinds whose placement
//!    supports it). Events are threshold outcomes of the arithmetic,
//!    never dice rolls.

use crate::features::{ActuatorKind, OutputFeature};

/// Hard limit from the commission: at most this many toys at once.
pub const MAX_TOTAL_TOYS: usize = 10;
/// Hard limit from the commission: at most this many of one kind
/// (kind = form-factor family) at once.
pub const MAX_PER_KIND: usize = 3;
/// Ticks of history kept for the live trace (4 Hz -> 30 s).
pub const HISTORY_TICKS: usize = 120;
/// Arousal ceiling, per-mille.
pub const AROUSAL_MAX: u32 = 1000;

/// The erogenous zones the trial's placements imply. A toy acts on
/// the zone(s) its family is placed at; the rabbit acts on two.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Zone {
    External,
    Internal,
    Anal,
    Nipples,
}

impl Zone {
    pub const ALL: [Zone; 4] = [Zone::External, Zone::Internal, Zone::Anal, Zone::Nipples];

    pub fn label(self) -> &'static str {
        match self {
            Zone::External => "external (clitoral)",
            Zone::Internal => "internal (vaginal)",
            Zone::Anal => "anal",
            Zone::Nipples => "nipples",
        }
    }

    fn index(self) -> usize {
        match self {
            Zone::External => 0,
            Zone::Internal => 1,
            Zone::Anal => 2,
            Zone::Nipples => 3,
        }
    }
}

/// Which zones a family acts on, with the share of its drive that
/// lands on each. Presentation truth from the form factor: an egg is
/// worn internally, a wand works externally, a rabbit does both.
pub fn family_zones(family: &str) -> &'static [(Zone, u32)] {
    match family {
        "egg" | "kegel" | "insertable" => &[(Zone::Internal, 1000)],
        "rabbit" => &[(Zone::Internal, 650), (Zone::External, 350)],
        "plug" | "beads" => &[(Zone::Anal, 1000)],
        "nipple" | "clip" => &[(Zone::Nipples, 1000)],
        _ => &[(Zone::External, 1000)],
    }
}

/// Her stage on the estate's S0-S4 scale (Kama PROFILES): Composed,
/// Warming, In use / aroused, Peak, Afterglow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    S0,
    S1,
    S2,
    S3,
    S4,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::S0 => "Composed",
            Stage::S1 => "Warming up",
            Stage::S2 => "Aroused",
            Stage::S3 => "Near her peak",
            Stage::S4 => "Afterglow",
        }
    }
}

/// A sensitivity profile: the adjustable model of how a body
/// responds. Named parameters, documented curves — see the module
/// docs. All values per-mille unless noted. Presets are data; the
/// panel's per-zone sliders edit `zone_pm` on top of a preset and
/// the profile is then labelled Custom.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SensitivityProfile {
    pub name: String,
    /// Overall responsiveness multiplier on pleasure.
    pub gain_pm: u32,
    /// Zone drive (per-mille) at which pleasure peaks, before the
    /// per-zone sensitivity scaling is applied.
    pub peak_pm: u32,
    /// How fast pleasure falls past the peak (per-mille of peak
    /// pleasure per per-mille of excess drive).
    pub overstim_slope_pm: u32,
    /// Arousal level at which an orgasm event fires.
    pub orgasm_threshold: u32,
    /// Combined drive across zones beyond which extra toys stop
    /// adding pleasure (the "too much at once" ceiling).
    pub multi_tolerance_pm: u32,
    /// Whether this profile squirts at orgasm when the drive
    /// supports it (internal/suction present).
    pub squirt: bool,
    /// Per-zone sensitivity multipliers, in [`Zone::ALL`] order.
    pub zone_pm: [u32; 4],
    /// The zone that works best for her under this profile; the
    /// light-touch orgasm rule keys on it.
    pub preferred: Zone,
}

impl SensitivityProfile {
    /// The top of the Owner's stated range: rapid climb, orgasms
    /// readily and repeatedly, every slot enjoyable at once,
    /// overstimulation rare and mild (high peak, gentle slope).
    pub fn hyper_responsive() -> Self {
        SensitivityProfile {
            name: "Very sensitive".to_string(),
            gain_pm: 1650,
            peak_pm: 2600,
            overstim_slope_pm: 250,
            orgasm_threshold: 680,
            multi_tolerance_pm: 6000,
            squirt: true,
            zone_pm: [1500, 1500, 1100, 1300],
            preferred: Zone::External,
        }
    }

    /// Quick to warm, orgasms with sustained attention, still has a
    /// real ceiling.
    pub fn responsive() -> Self {
        SensitivityProfile {
            name: "Responsive".to_string(),
            gain_pm: 1250,
            peak_pm: 1900,
            overstim_slope_pm: 500,
            orgasm_threshold: 800,
            multi_tolerance_pm: 4200,
            squirt: true,
            zone_pm: [1200, 1200, 900, 1100],
            preferred: Zone::External,
        }
    }

    /// Lana's own profile, mid-spectrum per her canon register: the
    /// toy enthusiast who loves plenty, at levels she chose.
    pub fn lana_default() -> Self {
        SensitivityProfile {
            name: "Lana (her own)".to_string(),
            gain_pm: 1000,
            peak_pm: 1500,
            overstim_slope_pm: 700,
            orgasm_threshold: 900,
            multi_tolerance_pm: 3400,
            squirt: false,
            zone_pm: [1000, 1000, 800, 1000],
            preferred: Zone::External,
        }
    }

    /// Needs time and the right zones; intensity is not a shortcut.
    pub fn slow_burn() -> Self {
        SensitivityProfile {
            name: "Slow burn".to_string(),
            gain_pm: 720,
            peak_pm: 1200,
            overstim_slope_pm: 800,
            orgasm_threshold: 950,
            multi_tolerance_pm: 2600,
            squirt: false,
            zone_pm: [900, 850, 600, 800],
            preferred: Zone::Internal,
        }
    }

    /// The bottom of the Owner's stated range: only light stimulation
    /// is enjoyable. Low peak — anything strong inverts to
    /// discomfort — and a low tolerance for simultaneous toys.
    /// Orgasm is possible, via the preferred zone at gentle levels.
    pub fn light_touch() -> Self {
        SensitivityProfile {
            name: "Light touches only".to_string(),
            gain_pm: 620,
            peak_pm: 620,
            overstim_slope_pm: 1600,
            orgasm_threshold: 930,
            multi_tolerance_pm: 1300,
            squirt: false,
            zone_pm: [1100, 800, 450, 900],
            preferred: Zone::External,
        }
    }

    pub fn presets() -> Vec<SensitivityProfile> {
        vec![
            SensitivityProfile::hyper_responsive(),
            SensitivityProfile::responsive(),
            SensitivityProfile::lana_default(),
            SensitivityProfile::slow_burn(),
            SensitivityProfile::light_touch(),
        ]
    }

    /// The pleasure one zone returns for a total drive, per-mille.
    /// Can go negative: that is discomfort, reported as such.
    pub fn zone_pleasure(&self, zone: Zone, drive_pm: u32) -> i32 {
        let sens = self.zone_pm[zone.index()];
        let scaled_drive = drive_pm.saturating_mul(sens) / 1000;
        let peak = self.peak_pm;
        let at_peak = peak.saturating_mul(self.gain_pm) / 1000;
        let raw = if scaled_drive <= peak {
            (scaled_drive.saturating_mul(self.gain_pm) / 1000) as i32
        } else {
            let excess = scaled_drive - peak;
            let fall = excess.saturating_mul(self.overstim_slope_pm) / 1000;
            at_peak as i32 - fall.min(at_peak.saturating_mul(2)) as i32
        };
        // Per-mille is her scale: saturate at the top of it. The
        // peak and the inversion still govern — saturation is what
        // a body at its ceiling feels like.
        raw.clamp(-1000, 1000)
    }

    /// True when a zone's drive is past the discomfort line
    /// (1.6x its peak): pleasure has fully inverted there.
    pub fn zone_discomfort(&self, zone: Zone, drive_pm: u32) -> bool {
        let sens = self.zone_pm[zone.index()];
        let scaled_drive = drive_pm.saturating_mul(sens) / 1000;
        scaled_drive > self.peak_pm.saturating_mul(16) / 10
    }
}

/// (pleasure weight, force weight) per actuator kind, per-mille.
/// Vibration is nearly all pleasure and little force; oscillation
/// and position carry real mechanical force; constriction is force
/// first. Temperature and LED barely register as sensation drive.
fn weights(kind: ActuatorKind) -> (u32, u32) {
    match kind {
        ActuatorKind::Vibrate => (850, 150),
        ActuatorKind::Oscillate => (800, 850),
        ActuatorKind::Rotate => (650, 400),
        ActuatorKind::Constrict => (450, 900),
        ActuatorKind::Inflate => (400, 700),
        ActuatorKind::Position => (750, 800),
        ActuatorKind::HwPositionWithDuration => (750, 800),
        ActuatorKind::Temperature => (300, 50),
        ActuatorKind::Spray => (300, 200),
        ActuatorKind::Led => (40, 0),
    }
}

/// How much a form factor tends to land for her, per-mille — her
/// response curve per toy kind, separate from raw force (a suction
/// toy is gentle and lands high; a kegel trainer is work, not play).
fn family_sensitivity(family: &str) -> u32 {
    match family {
        "suction" => 1150,
        "rabbit" => 1100,
        "wand" => 1100,
        "egg" => 1000,
        "insertable" => 1000,
        "butterfly" => 1000,
        "external" => 1000,
        "rideon" => 1050,
        "panty" => 950,
        "bullet" => 900,
        "plug" => 900,
        "beads" => 850,
        "nipple" => 850,
        "clip" => 800,
        "kegel" => 700,
        "controller" => 500,
        _ => 800,
    }
}

/// One virtual toy on the rig: a catalogue device, simulated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialToy {
    pub brand: String,
    pub name: String,
    pub family: String,
    /// One simulated channel per config output feature; `level` is
    /// in the config's own steps, clamped to `max`.
    pub channels: Vec<TrialChannel>,
}

/// One simulated channel of a toy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialChannel {
    pub kind: ActuatorKind,
    pub max: u32,
    pub level: u32,
}

impl TrialToy {
    /// Builds a virtual toy from a device's real config features.
    /// Devices with no matched config get a single generic vibration
    /// channel of 20 steps — the simulation's stated assumption for
    /// unlisted hardware, labelled as such wherever it is used; the
    /// guide text for such a device claims no features.
    pub fn from_features(
        brand: &str,
        name: &str,
        family: &str,
        outputs: &[OutputFeature],
    ) -> TrialToy {
        let channels = if outputs.is_empty() {
            vec![TrialChannel {
                kind: ActuatorKind::Vibrate,
                max: 20,
                level: 0,
            }]
        } else {
            outputs
                .iter()
                .map(|f| TrialChannel {
                    kind: f.kind,
                    // The channel's step count is the span of the
                    // config's range — signed ranges exist (rotation
                    // runs -20..20) and the span is the honest count.
                    max: (f.max - f.min).max(0) as u32,
                    level: 0,
                })
                .collect()
        };
        TrialToy {
            brand: brand.to_string(),
            name: name.to_string(),
            family: family.to_string(),
            channels,
        }
    }

    /// This toy's drive into each zone, per-mille, before the zone
    /// curve. Index by [`Zone::index`].
    fn zone_drive(&self) -> [u32; 4] {
        let mut drive = [0u32; 4];
        let sens = family_sensitivity(&self.family);
        for channel in &self.channels {
            if channel.max == 0 {
                continue;
            }
            let norm = channel.level.saturating_mul(1000) / channel.max;
            let (pleasure_w, _) = weights(channel.kind);
            let weighted = norm.saturating_mul(pleasure_w) / 1000;
            let weighted = weighted.saturating_mul(sens) / 1000;
            for (zone, share) in family_zones(&self.family) {
                drive[zone.index()] += weighted.saturating_mul(*share) / 1000;
            }
        }
        drive
    }

    /// This toy's force, per-mille: a direct per-toy quantity (no
    /// zone curve) — pressure is physical, not a response.
    pub fn force(&self) -> u32 {
        let mut total = 0u32;
        for channel in &self.channels {
            if channel.max == 0 {
                continue;
            }
            let norm = channel.level.saturating_mul(1000) / channel.max;
            let (_, force_w) = weights(channel.kind);
            total += norm.saturating_mul(force_w) / 1000;
        }
        total.min(1000)
    }
}

/// One toy's live telemetry row: what it is putting out, and what it
/// is doing to her, this tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToyTelemetry {
    pub toy_index: usize,
    /// Per-channel output level, per-mille of each channel's real
    /// step range, in channel order.
    pub channel_levels_pm: Vec<u32>,
    /// Marginal pleasure contribution, per-mille (may be negative:
    /// this toy is currently part of an overstimulated zone).
    pub pleasure: i32,
    /// Force/pressure, per-mille.
    pub force: u32,
}

/// One tick of history for the live trace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickRecord {
    pub arousal: u32,
    pub per_toy: Vec<(i32, u32)>, // (pleasure, force) by toy index
}

/// Why a toy could not be added. The refusal lines are Lana's, in
/// register — the limit is hers to state, per the commission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddRefusal {
    TotalLimit,
    KindLimit,
}

impl AddRefusal {
    pub fn lana_line(self) -> &'static str {
        match self {
            AddRefusal::TotalLimit => {
                "Ten is my hard limit, love — even I need to be able to feel each one. Take something off and we'll talk."
            }
            AddRefusal::KindLimit => {
                "Three of one kind is the rule — variety is the whole point of a collection like mine. Pick a different shape."
            }
        }
    }
}

/// The trial state machine. All fields are the simulation's truth;
/// the UI renders them, it does not keep its own copies.
#[derive(Clone, Debug)]
pub struct Trial {
    pub profile: SensitivityProfile,
    pub toys: Vec<TrialToy>,
    pub arousal: u32,
    pub orgasms: u32,
    pub squirts: u32,
    /// Ticks since the last orgasm event (refractory window).
    pub refractory: u32,
    /// Any zone is currently in discomfort.
    pub discomfort: bool,
    /// Tick of the most recent orgasm event (0 = none yet).
    pub last_orgasm_tick: u64,
    /// Tick of the most recent squirt event (0 = none yet).
    pub last_squirt_tick: u64,
    pub tick_count: u64,
    pub history: Vec<TickRecord>,
}

impl Default for Trial {
    fn default() -> Self {
        Trial::new()
    }
}

impl Trial {
    pub fn new() -> Trial {
        Trial {
            profile: SensitivityProfile::lana_default(),
            toys: Vec::new(),
            arousal: 0,
            orgasms: 0,
            squirts: 0,
            refractory: 0,
            discomfort: false,
            last_orgasm_tick: 0,
            last_squirt_tick: 0,
            tick_count: 0,
            history: Vec::new(),
        }
    }

    pub fn set_profile(&mut self, profile: SensitivityProfile) {
        self.profile = profile;
    }

    /// Adds a toy, enforcing the commission's hard limits.
    pub fn add_toy(&mut self, toy: TrialToy) -> Result<(), AddRefusal> {
        if self.toys.len() >= MAX_TOTAL_TOYS {
            return Err(AddRefusal::TotalLimit);
        }
        let same_kind = self.toys.iter().filter(|t| t.family == toy.family).count();
        if same_kind >= MAX_PER_KIND {
            return Err(AddRefusal::KindLimit);
        }
        self.toys.push(toy);
        Ok(())
    }

    pub fn remove_toy(&mut self, index: usize) {
        if index < self.toys.len() {
            self.toys.remove(index);
        }
    }

    pub fn clear_toys(&mut self) {
        self.toys.clear();
    }

    /// Sets one channel's level, clamped to the config's own steps.
    pub fn set_level(&mut self, toy: usize, channel: usize, level: u32) {
        if let Some(toy) = self.toys.get_mut(toy)
            && let Some(channel) = toy.channels.get_mut(channel)
        {
            channel.level = level.min(channel.max);
        }
    }

    /// Total drive into each zone across all toys, per-mille.
    pub fn zone_drives(&self) -> [u32; 4] {
        let mut total = [0u32; 4];
        for toy in &self.toys {
            let drive = toy.zone_drive();
            for (i, slot) in total.iter_mut().enumerate() {
                *slot += drive[i];
            }
        }
        total
    }

    /// Live telemetry for every toy, this instant.
    pub fn telemetry(&self) -> Vec<ToyTelemetry> {
        let totals = self.zone_drives();
        let combined: u32 = totals.iter().sum();
        self.toys
            .iter()
            .enumerate()
            .map(|(index, toy)| {
                let drive = toy.zone_drive();
                let mut pleasure = 0i32;
                for zone in Zone::ALL {
                    let zi = zone.index();
                    let with = self.profile.zone_pleasure(zone, totals[zi]);
                    let without = self
                        .profile
                        .zone_pleasure(zone, totals[zi].saturating_sub(drive[zi]));
                    pleasure += with - without;
                }
                // Multi-toy tolerance: past the profile's ceiling the
                // marginal pleasure of everything fades toward zero —
                // too much at once stops being more.
                if combined > self.profile.multi_tolerance_pm && pleasure > 0 {
                    let over = combined - self.profile.multi_tolerance_pm;
                    let fade = (1000u32).saturating_sub(over / 4).max(250);
                    pleasure = pleasure * fade as i32 / 1000;
                }
                ToyTelemetry {
                    toy_index: index,
                    channel_levels_pm: toy
                        .channels
                        .iter()
                        .map(|c| c.level.saturating_mul(1000).checked_div(c.max).unwrap_or(0))
                        .collect(),
                    pleasure,
                    force: toy.force(),
                }
            })
            .collect()
    }

    /// Total pleasure across all zones this instant, per-mille.
    pub fn total_pleasure(&self) -> i32 {
        let totals = self.zone_drives();
        Zone::ALL
            .iter()
            .map(|zone| self.profile.zone_pleasure(*zone, totals[zone.index()]))
            .sum()
    }

    pub fn stage(&self) -> Stage {
        if self.refractory > 0 {
            Stage::S4
        } else if self.arousal < 150 {
            Stage::S0
        } else if self.arousal < 400 {
            Stage::S1
        } else if self.arousal < 650 {
            Stage::S2
        } else {
            Stage::S3
        }
    }

    /// Advances the simulation one tick (250 ms in the UI).
    pub fn tick(&mut self) {
        self.tick_count += 1;
        let totals = self.zone_drives();
        self.discomfort = Zone::ALL
            .iter()
            .any(|zone| self.profile.zone_discomfort(*zone, totals[zone.index()]));
        let pleasure = self.total_pleasure();
        if self.refractory > 0 {
            self.refractory -= 1;
        }
        let gain_divisor = if self.refractory > 0 { 80 } else { 40 };
        if pleasure > 30 {
            let delta = (pleasure / gain_divisor).max(0) as u32;
            self.arousal = (self.arousal + delta).min(AROUSAL_MAX);
        } else if pleasure < -30 {
            // Discomfort drains arousal fast: she is not warming to
            // this, she is enduring it, and the model says so.
            let delta = ((-pleasure) / 20).max(1) as u32;
            self.arousal = self.arousal.saturating_sub(delta);
        } else {
            self.arousal = self.arousal.saturating_sub(1);
        }
        // Orgasm: a threshold outcome of the model, not a roll.
        if self.refractory == 0 && self.arousal >= self.profile.orgasm_threshold {
            let squirt_drive = totals[Zone::Internal.index()] > 200
                && self.toys.iter().any(|t| {
                    t.family == "suction" || t.family == "insertable" || t.family == "egg"
                });
            self.orgasms += 1;
            self.last_orgasm_tick = self.tick_count;
            if self.profile.squirt && squirt_drive {
                self.squirts += 1;
                self.last_squirt_tick = self.tick_count;
            }
            self.arousal = 340;
            self.refractory = 16; // 4 s at 4 Hz
        }
        let telemetry = self.telemetry();
        self.history.push(TickRecord {
            arousal: self.arousal,
            per_toy: telemetry.iter().map(|t| (t.pleasure, t.force)).collect(),
        });
        if self.history.len() > HISTORY_TICKS {
            self.history.remove(0);
        }
    }

    /// Her current reaction line, in register, chosen
    /// deterministically from state (stage, events, discomfort).
    pub fn reaction_line(&self) -> &'static str {
        if self.discomfort {
            return "Too much — that's past my peak, it's not pleasure any more, it's just… loud. Ease it down and find the gentle spot again.";
        }
        if self.refractory > 0 {
            return "Mmm — give me a second, love. Afterglow rules: everything stays, nothing gets tidied away, we just… bask.";
        }
        match self.stage() {
            Stage::S0 => {
                "Composed and curious. Show me what you've brought — and yes, I have opinions about all of them."
            }
            Stage::S1 => {
                "Warming up nicely. That's the teaser stage — present, chosen, and just getting started."
            }
            Stage::S2 => "Oh, that's properly in use now. Keep the pacing — build is everything.",
            Stage::S3 => "Close — so close. Steady hands, love, don't rush the peak…",
            Stage::S4 => "Basking. That was earned.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::OutputFeature;

    fn toy(family: &str, kind: ActuatorKind, max: u32, level: u32) -> TrialToy {
        let mut toy = TrialToy::from_features(
            "Test",
            "Toy",
            family,
            &[OutputFeature {
                kind,
                min: 0,
                max: max as i32,
                descriptor: String::new(),
            }],
        );
        toy.channels[0].level = level;
        toy
    }

    #[test]
    fn zero_level_means_zero_effect() {
        let mut trial = Trial::new();
        trial
            .add_toy(toy("wand", ActuatorKind::Vibrate, 100, 0))
            .unwrap();
        let telemetry = trial.telemetry();
        assert_eq!(telemetry[0].pleasure, 0);
        assert_eq!(telemetry[0].force, 0);
        for _ in 0..40 {
            trial.tick();
        }
        assert_eq!(trial.arousal, 0);
        assert_eq!(trial.orgasms, 0);
    }

    #[test]
    fn limits_are_enforced() {
        let mut trial = Trial::new();
        for _ in 0..MAX_PER_KIND {
            trial
                .add_toy(toy("wand", ActuatorKind::Vibrate, 100, 10))
                .unwrap();
        }
        assert_eq!(
            trial.add_toy(toy("wand", ActuatorKind::Vibrate, 100, 10)),
            Err(AddRefusal::KindLimit)
        );
        for family in [
            "egg", "plug", "suction", "bullet", "rabbit", "kegel", "clip",
        ] {
            trial
                .add_toy(toy(family, ActuatorKind::Vibrate, 100, 10))
                .unwrap();
        }
        assert_eq!(trial.toys.len(), MAX_TOTAL_TOYS);
        assert_eq!(
            trial.add_toy(toy("egg", ActuatorKind::Vibrate, 100, 10)),
            Err(AddRefusal::TotalLimit)
        );
    }

    #[test]
    fn effect_is_independent_of_add_order() {
        let build = |order: &[&str]| {
            let mut trial = Trial::new();
            for family in order {
                trial
                    .add_toy(toy(family, ActuatorKind::Vibrate, 100, 60))
                    .unwrap();
            }
            for _ in 0..30 {
                trial.tick();
            }
            trial
        };
        let a = build(&["wand", "egg", "plug"]);
        let b = build(&["plug", "wand", "egg"]);
        assert_eq!(a.zone_drives(), b.zone_drives());
        assert_eq!(a.arousal, b.arousal);
        assert_eq!(a.total_pleasure(), b.total_pleasure());
    }

    #[test]
    fn determinism_same_inputs_same_state() {
        let run = || {
            let mut trial = Trial::new();
            trial
                .add_toy(toy("rabbit", ActuatorKind::Vibrate, 20, 14))
                .unwrap();
            for _ in 0..100 {
                trial.tick();
            }
            (trial.arousal, trial.orgasms, trial.tick_count)
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn light_touch_profile_inverts_under_a_maxed_wand() {
        let mut trial = Trial::new();
        trial.set_profile(SensitivityProfile::light_touch());
        trial
            .add_toy(toy("wand", ActuatorKind::Vibrate, 100, 100))
            .unwrap();
        let telemetry = trial.telemetry();
        assert!(
            telemetry[0].pleasure < 0,
            "maxed wand under light-touch must read as too much, got {}",
            telemetry[0].pleasure
        );
        for _ in 0..20 {
            trial.tick();
        }
        assert!(trial.discomfort, "the state must name the discomfort");
        assert_eq!(trial.arousal, 0, "discomfort must not build arousal");
    }

    #[test]
    fn light_touch_profile_enjoys_a_gentle_wand() {
        let mut trial = Trial::new();
        trial.set_profile(SensitivityProfile::light_touch());
        trial
            .add_toy(toy("wand", ActuatorKind::Vibrate, 100, 18))
            .unwrap();
        let telemetry = trial.telemetry();
        assert!(telemetry[0].pleasure > 0);
        assert!(!trial.discomfort);
    }

    #[test]
    fn hyper_responsive_profile_reaches_orgasm_events() {
        let mut trial = Trial::new();
        trial.set_profile(SensitivityProfile::hyper_responsive());
        for family in ["egg", "wand", "suction"] {
            trial
                .add_toy(toy(family, ActuatorKind::Vibrate, 100, 80))
                .unwrap();
        }
        for _ in 0..400 {
            trial.tick();
        }
        assert!(trial.orgasms >= 1, "sustained multi-toy input must crest");
        assert!(
            trial.squirts >= 1,
            "hyper profile + internal/suction drive squirts"
        );
    }

    #[test]
    fn levels_clamp_to_the_config_step_range() {
        let mut trial = Trial::new();
        trial
            .add_toy(toy("egg", ActuatorKind::Vibrate, 20, 0))
            .unwrap();
        trial.set_level(0, 0, 999);
        assert_eq!(trial.toys[0].channels[0].level, 20);
    }

    #[test]
    fn force_and_pleasure_are_separate_quantities() {
        // A maxed constrict channel: force near the top of its
        // range, pleasure well below a vibrator's at the same level.
        let mut trial = Trial::new();
        trial
            .add_toy(toy("clip", ActuatorKind::Constrict, 100, 100))
            .unwrap();
        let telemetry = trial.telemetry();
        assert!(telemetry[0].force >= 800, "force {}", telemetry[0].force);
        let mut trial2 = Trial::new();
        trial2
            .add_toy(toy("clip", ActuatorKind::Vibrate, 100, 100))
            .unwrap();
        assert!(trial2.telemetry()[0].pleasure > telemetry[0].pleasure);
    }
}
