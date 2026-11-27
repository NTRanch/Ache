//! Lana's visible reactions: animation-driven, not a static portrait.
//!
//! This module is the interface between the trial engine and her
//! figure. It computes, deterministically from trial state, one
//! [`ReactionFrame`] per tick:
//!
//! - a **band** (the state taxonomy the animation lane's clips are
//!   authored against — see BANDS.md in the lane record),
//! - an **intensity tier** (0-3) from the live feed,
//! - the toon layer's **13 expression parameters** (the exact field
//!   names of `torogao-render3d`'s expression struct: eyelid_aperture,
//!   pupil_dilate, pupil_shape, eye_gloss, tear_sheen, brow_raise,
//!   brow_angle, mouth_open, mouth_shape, blush_intensity,
//!   blush_spread, skin_flush, skin_sheen), as per-mille integers,
//! - a named **body-reaction parameter set** (breath, tremble, arch,
//!   tense, recoil, settle) the clips also target.
//!
//! The GUI plays the recorded torogao-motion-clip/2 reaction clips
//! for Lana — all 32 band x tier clips, registered in
//! [`ClipRegistry`] at startup by [`crate::clips`] and rendered off
//! her rung-1 rig — matching the current band+tier. The procedural
//! figure-strip frames + offsets this interface also computes remain
//! as the fallback for any state a clip does not cover, and the
//! parameter surface still drives the details readout.
//!
//! Determinism: every value is a pure function of the trial state
//! and its tick count. Oscillation (breath, tremble) is a triangle
//! wave of the tick count — same state, same frame, always.

use crate::trial::{Stage, Trial};

/// Reaction bands: the taxonomy clips are authored against. Crossed
/// with intensity tiers 0-3 they form the clip key space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Band {
    /// Nothing running, or running at nothing. Her guide composure.
    Composed,
    /// Pleasure registering under a low-peak (light-touch class)
    /// profile: gentle levels, enjoyed as gentle.
    LightPleasure,
    /// Arousal building through the middle bands.
    Building,
    /// High arousal, pre-crest.
    NearOrgasm,
    /// The orgasm event window (the ticks right after the model's
    /// threshold fires).
    Orgasm,
    /// The squirt event window, when the model fires one.
    SquirtEvent,
    /// A zone is past its peak: pleasure inverted, she recoils.
    Overstimulation,
    /// Post-event refractory settle: toys remain, she basks.
    Afterglow,
}

impl Band {
    pub fn key(self) -> &'static str {
        match self {
            Band::Composed => "composed",
            Band::LightPleasure => "light_pleasure",
            Band::Building => "building",
            Band::NearOrgasm => "near_orgasm",
            Band::Orgasm => "orgasm",
            Band::SquirtEvent => "squirt_event",
            Band::Overstimulation => "overstimulation",
            Band::Afterglow => "afterglow",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Band::Composed => "Composed",
            Band::LightPleasure => "Light pleasure",
            Band::Building => "Building",
            Band::NearOrgasm => "Near orgasm",
            Band::Orgasm => "Orgasm",
            Band::SquirtEvent => "Orgasm — squirt event",
            Band::Overstimulation => "Overstimulation",
            Band::Afterglow => "Afterglow",
        }
    }

    /// All bands, in taxonomy order — the rows of the clip key
    /// space (band x tier 0-3).
    pub const ALL: [Band; 8] = [
        Band::Composed,
        Band::LightPleasure,
        Band::Building,
        Band::NearOrgasm,
        Band::Orgasm,
        Band::SquirtEvent,
        Band::Overstimulation,
        Band::Afterglow,
    ];
}

/// The toon layer's 13 expression parameters, per-mille. Field names
/// mirror `torogao-render3d`'s expression struct exactly so a clip
/// or renderer consuming them needs no translation table.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Expr13 {
    pub eyelid_aperture: u32,
    pub pupil_dilate: u32,
    pub pupil_shape: u32,
    pub eye_gloss: u32,
    pub tear_sheen: u32,
    pub brow_raise: u32,
    pub brow_angle: u32,
    pub mouth_open: u32,
    pub mouth_shape: u32,
    pub blush_intensity: u32,
    pub blush_spread: u32,
    pub skin_flush: u32,
    pub skin_sheen: u32,
}

/// The body-reaction parameter set the animation lane's clips
/// target, per-mille unless noted. `breath_wave` and `tremble_wave`
/// are signed phase outputs (-1000..=1000) computed from the tick.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BodyParams {
    /// Breaths per minute x10 (120 = a calm 12/min, 340 = gasping).
    pub breath_rate: u32,
    pub breath_depth: u32,
    pub tremble_amp: u32,
    pub arch: u32,
    pub tense: u32,
    pub recoil: u32,
    pub settle: u32,
    pub breath_wave: i32,
    pub tremble_wave: i32,
}

/// One tick's complete reaction: what her figure should show.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReactionFrame {
    pub band: Band,
    /// 0 calm, 1 gentle, 2 strong, 3 max — from the live feed.
    pub tier: u8,
    pub expr: Expr13,
    pub body: BodyParams,
    /// The clip key this frame would play, if a clip is registered.
    pub clip_key: String,
}

/// Registered reaction clips (torogao-motion-clip/2 keys). Startup
/// registers every landed clip (see `clips::register_all`); a state
/// whose key is not registered falls back to the procedural figure.
#[derive(Clone, Debug, Default)]
pub struct ClipRegistry {
    keys: Vec<String>,
}

impl ClipRegistry {
    pub fn new() -> ClipRegistry {
        ClipRegistry { keys: Vec::new() }
    }

    /// Registers a landed clip's key.
    pub fn register(&mut self, key: &str) {
        if !self.keys.iter().any(|k| k == key) {
            self.keys.push(key.to_string());
        }
    }

    /// The clip to play for this frame, if one is registered for
    /// its band+tier (exact tier first, then tier 1 as the neutral
    /// fallback a clip set is expected to always carry).
    pub fn lookup(&self, frame: &ReactionFrame) -> Option<String> {
        if self.keys.contains(&frame.clip_key) {
            return Some(frame.clip_key.clone());
        }
        let neutral = clip_key(frame.band, 1);
        if self.keys.contains(&neutral) {
            return Some(neutral);
        }
        Option::None
    }
}

/// The clip key for a band+tier: `lana/react/<band>.t<tier>`.
pub fn clip_key(band: Band, tier: u8) -> String {
    format!("lana/react/{}.t{}", band.key(), tier)
}

/// Triangle wave of the tick: period in ticks, output -1000..=1000.
/// The deterministic oscillator for breath and tremble.
fn triangle(tick: u64, period: u64) -> i32 {
    let period = period.max(1);
    let phase = (tick % period) as i32;
    let half = (period / 2).max(1) as i32;
    if phase < half {
        phase * 2000 / half - 1000
    } else {
        1000 - (phase - half) * 2000 / half
    }
}

/// True for low-peak profiles (the light-touch class): their
/// pleasure band reads differently from a mid/high profile's build.
fn is_light_class(trial: &Trial) -> bool {
    trial.profile.peak_pm <= 700
}

/// Computes the current band from trial state.
pub fn band_of(trial: &Trial) -> Band {
    if trial.discomfort {
        return Band::Overstimulation;
    }
    if trial.last_squirt_tick > 0 && trial.tick_count - trial.last_squirt_tick <= 8 {
        return Band::SquirtEvent;
    }
    if trial.last_orgasm_tick > 0 && trial.tick_count - trial.last_orgasm_tick <= 8 {
        return Band::Orgasm;
    }
    if trial.refractory > 0 {
        return Band::Afterglow;
    }
    let pleasure = trial.total_pleasure();
    if is_light_class(trial) && pleasure > 30 && trial.arousal < 650 {
        return Band::LightPleasure;
    }
    match trial.stage() {
        Stage::S0 => Band::Composed,
        Stage::S1 | Stage::S2 => Band::Building,
        Stage::S3 => {
            if trial.arousal >= 850 {
                Band::NearOrgasm
            } else {
                Band::Building
            }
        }
        Stage::S4 => Band::Afterglow,
    }
}

/// Intensity tier from the live feed: total pleasure, 0-3.
pub fn tier_of(trial: &Trial) -> u8 {
    let pleasure = trial.total_pleasure().max(0) as u32;
    match pleasure {
        0..=30 => 0,
        31..=300 => 1,
        301..=650 => 2,
        _ => 3,
    }
}

/// The full reaction frame for the trial's current state.
pub fn reaction_frame(trial: &Trial) -> ReactionFrame {
    let band = band_of(trial);
    let tier = tier_of(trial);
    let arousal = trial.arousal;
    let t = tier as u32;
    // Expression targets per band; tier scales the mobile params.
    let expr = match band {
        Band::Composed => Expr13 {
            eyelid_aperture: 900,
            pupil_dilate: 250,
            pupil_shape: 0,
            eye_gloss: 300,
            tear_sheen: 0,
            brow_raise: 350,
            brow_angle: 150,
            mouth_open: 60,
            mouth_shape: 350,
            blush_intensity: 60,
            blush_spread: 100,
            skin_flush: 40,
            skin_sheen: 120,
        },
        Band::LightPleasure => Expr13 {
            eyelid_aperture: 760,
            pupil_dilate: 420,
            pupil_shape: 120,
            eye_gloss: 480,
            tear_sheen: 0,
            brow_raise: 480,
            brow_angle: 260,
            mouth_open: 180 + 60 * t,
            mouth_shape: 520,
            blush_intensity: 260,
            blush_spread: 320,
            skin_flush: 180,
            skin_sheen: 260,
        },
        Band::Building => Expr13 {
            eyelid_aperture: 680,
            pupil_dilate: 550,
            pupil_shape: 300,
            eye_gloss: 560,
            tear_sheen: 60,
            brow_raise: 560,
            brow_angle: 380,
            mouth_open: 260 + 90 * t,
            mouth_shape: 580,
            blush_intensity: 420,
            blush_spread: 480,
            skin_flush: 320,
            skin_sheen: 380,
        },
        Band::NearOrgasm => Expr13 {
            eyelid_aperture: 520,
            pupil_dilate: 760,
            pupil_shape: 620,
            eye_gloss: 700,
            tear_sheen: 240,
            brow_raise: 680,
            brow_angle: 560,
            mouth_open: 480 + 110 * t,
            mouth_shape: 660,
            blush_intensity: 640,
            blush_spread: 660,
            skin_flush: 560,
            skin_sheen: 560,
        },
        Band::Orgasm | Band::SquirtEvent => Expr13 {
            eyelid_aperture: 260,
            pupil_dilate: 920,
            pupil_shape: 900,
            eye_gloss: 820,
            tear_sheen: 420,
            brow_raise: 760,
            brow_angle: 700,
            mouth_open: 760,
            mouth_shape: 720,
            blush_intensity: 820,
            blush_spread: 840,
            skin_flush: 780,
            skin_sheen: 740,
        },
        Band::Overstimulation => Expr13 {
            eyelid_aperture: 940,
            pupil_dilate: 680,
            pupil_shape: 0,
            eye_gloss: 620,
            tear_sheen: 520,
            brow_raise: 880,
            brow_angle: 860,
            mouth_open: 420,
            mouth_shape: 180,
            blush_intensity: 380,
            blush_spread: 400,
            skin_flush: 460,
            skin_sheen: 420,
        },
        Band::Afterglow => Expr13 {
            eyelid_aperture: 480,
            pupil_dilate: 480,
            pupil_shape: 260,
            eye_gloss: 640,
            tear_sheen: 120,
            brow_raise: 300,
            brow_angle: 120,
            mouth_open: 220,
            mouth_shape: 640,
            blush_intensity: 520,
            blush_spread: 620,
            skin_flush: 520,
            skin_sheen: 620,
        },
    };
    // Body: breath quickens with arousal; tremble grows near the
    // crest and spikes at events; recoil belongs to overstimulation
    // alone; settle belongs to afterglow alone.
    let breath_rate = 120 + arousal / 4 + t * 30;
    let breath_period = (2400 / breath_rate.max(60)).max(4) as u64;
    let tremble_amp = match band {
        Band::NearOrgasm => 380 + 120 * t,
        Band::Orgasm | Band::SquirtEvent => 900,
        Band::Overstimulation => 550,
        Band::Building => 120 + 60 * t,
        Band::LightPleasure => 60,
        _ => 0,
    };
    let body = BodyParams {
        breath_rate,
        breath_depth: 220 + arousal / 3,
        tremble_amp,
        arch: match band {
            Band::NearOrgasm => 620,
            Band::Orgasm | Band::SquirtEvent => 840,
            Band::Building => arousal / 2,
            _ => 60,
        },
        tense: match band {
            Band::Overstimulation => 780,
            Band::NearOrgasm | Band::Orgasm | Band::SquirtEvent => 640,
            Band::Building => 300,
            _ => 80,
        },
        recoil: if band == Band::Overstimulation {
            720
        } else {
            0
        },
        settle: if band == Band::Afterglow { 820 } else { 0 },
        breath_wave: triangle(trial.tick_count, breath_period),
        tremble_wave: triangle(trial.tick_count, 3),
    };
    ReactionFrame {
        band,
        tier,
        expr,
        body,
        clip_key: clip_key(band, tier),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::OutputFeature;
    use crate::trial::{SensitivityProfile, TrialToy};

    fn running_trial(profile: SensitivityProfile, level: u32) -> Trial {
        let mut trial = Trial::new();
        trial.set_profile(profile);
        let mut toy = TrialToy::from_features(
            "Test",
            "Wand",
            "wand",
            &[OutputFeature {
                kind: crate::features::ActuatorKind::Vibrate,
                min: 0,
                max: 100,
                descriptor: String::new(),
            }],
        );
        toy.channels[0].level = level;
        trial.add_toy(toy).unwrap();
        trial
    }

    #[test]
    fn idle_trial_is_composed() {
        let trial = Trial::new();
        let frame = reaction_frame(&trial);
        assert_eq!(frame.band, Band::Composed);
        assert_eq!(frame.tier, 0);
        assert_eq!(frame.body.tremble_amp, 0);
        assert_eq!(frame.body.recoil, 0);
    }

    #[test]
    fn overstimulation_band_and_recoil() {
        let mut trial = running_trial(SensitivityProfile::light_touch(), 100);
        trial.tick();
        let frame = reaction_frame(&trial);
        assert_eq!(frame.band, Band::Overstimulation);
        assert!(frame.body.recoil > 0);
        assert!(frame.expr.tear_sheen > 0);
        assert_eq!(frame.expr.pupil_shape, 0, "no pleasure tells in recoil");
    }

    #[test]
    fn light_pleasure_band_for_gentle_light_touch() {
        let mut trial = running_trial(SensitivityProfile::light_touch(), 15);
        trial.tick();
        assert_eq!(reaction_frame(&trial).band, Band::LightPleasure);
    }

    #[test]
    fn orgasm_then_afterglow_bands() {
        let mut trial = running_trial(SensitivityProfile::hyper_responsive(), 90);
        let mut saw_orgasm_band = false;
        let mut saw_afterglow = false;
        for _ in 0..300 {
            trial.tick();
            match reaction_frame(&trial).band {
                Band::Orgasm | Band::SquirtEvent => saw_orgasm_band = true,
                Band::Afterglow => saw_afterglow = true,
                _ => {}
            }
            if saw_orgasm_band && saw_afterglow {
                break;
            }
        }
        assert!(saw_orgasm_band, "event band must show at the crest");
        assert!(saw_afterglow, "afterglow must follow the event window");
    }

    #[test]
    fn reaction_is_deterministic() {
        let build = || {
            let mut trial = running_trial(SensitivityProfile::lana_default(), 55);
            for _ in 0..37 {
                trial.tick();
            }
            reaction_frame(&trial)
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn clip_registry_falls_back_to_neutral_tier() {
        let mut trial = running_trial(SensitivityProfile::lana_default(), 55);
        for _ in 0..10 {
            trial.tick();
        }
        let frame = reaction_frame(&trial);
        let mut registry = ClipRegistry::new();
        assert_eq!(registry.lookup(&frame), Option::None);
        registry.register(&clip_key(frame.band, 1));
        assert_eq!(registry.lookup(&frame), Some(clip_key(frame.band, 1)));
        registry.register(&frame.clip_key);
        assert_eq!(registry.lookup(&frame), Some(frame.clip_key.clone()));
    }

    #[test]
    fn expr_params_stay_in_per_mille_range() {
        for profile in SensitivityProfile::presets() {
            for level in [0, 25, 60, 100] {
                let mut trial = running_trial(profile.clone(), level);
                for _ in 0..50 {
                    trial.tick();
                    let frame = reaction_frame(&trial);
                    for value in [
                        frame.expr.eyelid_aperture,
                        frame.expr.pupil_dilate,
                        frame.expr.pupil_shape,
                        frame.expr.mouth_open,
                        frame.expr.blush_intensity,
                        frame.body.breath_depth,
                        frame.body.tremble_amp,
                        frame.body.arch,
                    ] {
                        assert!(value <= 1000, "param {value} out of range");
                    }
                }
            }
        }
    }
}
