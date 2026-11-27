//! Recorded reaction-clip playback: Lana's frozen
//! torogao-motion-clip/2 reaction clips, rendered off her rung-1 rig
//! into grid strips (`assets/lana/clips/`, built by
//! `gui/tools/make_lana_clips.py`), played on the stage figure.
//!
//! Selection is the mapping the clips were authored against: the
//! trial's reaction frame (band x tier, see [`crate::reaction`])
//! names a clip key `lana/react/<band>.t<tier>`, and the
//! [`crate::reaction::ClipRegistry`] resolves it — exact tier first,
//! then tier 1 as the neutral fallback. All 32 band x tier clips are
//! registered (see [`register_all`]), so every state the reaction
//! engine can produce has an exact clip; the fallback exists for
//! robustness, not because coverage has holes.
//!
//! Transition law: a state-driven change never cuts mid-loop. The
//! desired clip is held as *pending* and takes over at the current
//! clip's loop boundary. Every clip's loop closure is exactly 0.0 by
//! construction (its end key duplicates its frame-0 state), so the
//! outgoing loop always completes to its own first pose and hands
//! over to the incoming clip's first pose — one authored pose to
//! another at a seam the motion was built around, never a torn pose
//! mid-transition and never a crossfade ghost of two bodies. The one
//! exception is STOP: [`Player::stop`] hard-cuts to the composed
//! loop's frame 0 in the same action that zeroes the drives, because
//! a commanded stop must not wait out a loop.
//!
//! Determinism: playback position advances only by the dt the caller
//! feeds in — same feed, same frames, always. The module never reads
//! a clock itself, which is what keeps it unit-testable.

use crate::reaction::{Band, ClipRegistry, clip_key};
use crate::{Assets, MainWindow};
use slint::{ComponentHandle, Image};

/// The authored clips' frame rate (torogao-motion-clip/2 rate 24/1).
pub const CLIP_FPS: u32 = 24;
/// Strip grid geometry, the generator's: 12 cells per row, each cell
/// one 240x480 frame (rendered at 2x, downscaled into the cell).
pub const STRIP_COLS: u32 = 12;
pub const CELL_W: u32 = 240;
pub const CELL_H: u32 = 480;

/// One rendered clip strip: its registry key, the band x tier it
/// answers, and its loop length in frames. Frame counts are the
/// frozen clips' own ranges, asserted against the rendered frames by
/// the generator when the strips were built.
pub struct ClipAsset {
    pub key: &'static str,
    pub band: Band,
    pub tier: u8,
    pub frames: u32,
}

const fn asset(key: &'static str, band: Band, tier: u8, frames: u32) -> ClipAsset {
    ClipAsset {
        key,
        band,
        tier,
        frames,
    }
}

/// Every landed clip, in band x tier order. The keys are the
/// registry keys of record (`lana/react/<band>.t<tier>`).
pub static CLIP_ASSETS: &[ClipAsset] = &[
    asset("lana/react/composed.t0", Band::Composed, 0, 96),
    asset("lana/react/composed.t1", Band::Composed, 1, 96),
    asset("lana/react/composed.t2", Band::Composed, 2, 96),
    asset("lana/react/composed.t3", Band::Composed, 3, 96),
    asset("lana/react/light_pleasure.t0", Band::LightPleasure, 0, 96),
    asset("lana/react/light_pleasure.t1", Band::LightPleasure, 1, 96),
    asset("lana/react/light_pleasure.t2", Band::LightPleasure, 2, 96),
    asset("lana/react/light_pleasure.t3", Band::LightPleasure, 3, 96),
    asset("lana/react/building.t0", Band::Building, 0, 72),
    asset("lana/react/building.t1", Band::Building, 1, 72),
    asset("lana/react/building.t2", Band::Building, 2, 72),
    asset("lana/react/building.t3", Band::Building, 3, 72),
    asset("lana/react/near_orgasm.t0", Band::NearOrgasm, 0, 96),
    asset("lana/react/near_orgasm.t1", Band::NearOrgasm, 1, 96),
    asset("lana/react/near_orgasm.t2", Band::NearOrgasm, 2, 96),
    asset("lana/react/near_orgasm.t3", Band::NearOrgasm, 3, 96),
    asset("lana/react/orgasm.t0", Band::Orgasm, 0, 120),
    asset("lana/react/orgasm.t1", Band::Orgasm, 1, 120),
    asset("lana/react/orgasm.t2", Band::Orgasm, 2, 120),
    asset("lana/react/orgasm.t3", Band::Orgasm, 3, 120),
    asset("lana/react/squirt_event.t0", Band::SquirtEvent, 0, 120),
    asset("lana/react/squirt_event.t1", Band::SquirtEvent, 1, 120),
    asset("lana/react/squirt_event.t2", Band::SquirtEvent, 2, 120),
    asset("lana/react/squirt_event.t3", Band::SquirtEvent, 3, 120),
    asset(
        "lana/react/overstimulation.t0",
        Band::Overstimulation,
        0,
        72,
    ),
    asset(
        "lana/react/overstimulation.t1",
        Band::Overstimulation,
        1,
        72,
    ),
    asset(
        "lana/react/overstimulation.t2",
        Band::Overstimulation,
        2,
        72,
    ),
    asset(
        "lana/react/overstimulation.t3",
        Band::Overstimulation,
        3,
        72,
    ),
    asset("lana/react/afterglow.t0", Band::Afterglow, 0, 144),
    asset("lana/react/afterglow.t1", Band::Afterglow, 1, 144),
    asset("lana/react/afterglow.t2", Band::Afterglow, 2, 144),
    asset("lana/react/afterglow.t3", Band::Afterglow, 3, 144),
];

/// The asset for a registry key, if the clip has landed.
pub fn asset_for(key: &str) -> Option<&'static ClipAsset> {
    CLIP_ASSETS.iter().find(|a| a.key == key)
}

/// The idle clip: what plays when nothing is running — the composed
/// band at tier 0, her guide composure as a living loop.
pub fn idle_asset() -> &'static ClipAsset {
    asset_for("lana/react/composed.t0").expect("the idle clip is in the table")
}

/// Registers every landed clip's key. Called once at startup; this
/// is the registration the reaction lane's clips were promised to
/// the registry's lookup.
pub fn register_all(registry: &mut ClipRegistry) {
    // Startup invariant: the table covers the whole band x tier
    // space the reaction engine can produce, each asset answering
    // the key its band and tier name — so no live state can fall
    // back silently at runtime.
    for band in Band::ALL {
        for tier in 0..=3u8 {
            let clip =
                asset_for(&clip_key(band, tier)).expect("clip table covers every band x tier");
            assert_eq!(clip.band, band);
            assert_eq!(clip.tier, tier);
        }
    }
    for clip in CLIP_ASSETS {
        registry.register(clip.key);
    }
}

/// The strip cell offset (px) for a frame of a clip: grid position in
/// row-major order, 12 cells per row.
pub fn cell_offset(clip: &ClipAsset, frame: u32) -> (u32, u32) {
    let idx = frame % clip.frames;
    ((idx % STRIP_COLS) * CELL_W, (idx / STRIP_COLS) * CELL_H)
}

/// The strip image for a clip, from the window's asset global.
pub fn strip_image(window: &MainWindow, clip: &ClipAsset) -> Image {
    let assets = window.global::<Assets>();
    match clip.key {
        "lana/react/composed.t0" => assets.get_lana_clip_composed_t0(),
        "lana/react/composed.t1" => assets.get_lana_clip_composed_t1(),
        "lana/react/composed.t2" => assets.get_lana_clip_composed_t2(),
        "lana/react/composed.t3" => assets.get_lana_clip_composed_t3(),
        "lana/react/light_pleasure.t0" => assets.get_lana_clip_light_pleasure_t0(),
        "lana/react/light_pleasure.t1" => assets.get_lana_clip_light_pleasure_t1(),
        "lana/react/light_pleasure.t2" => assets.get_lana_clip_light_pleasure_t2(),
        "lana/react/light_pleasure.t3" => assets.get_lana_clip_light_pleasure_t3(),
        "lana/react/building.t0" => assets.get_lana_clip_building_t0(),
        "lana/react/building.t1" => assets.get_lana_clip_building_t1(),
        "lana/react/building.t2" => assets.get_lana_clip_building_t2(),
        "lana/react/building.t3" => assets.get_lana_clip_building_t3(),
        "lana/react/near_orgasm.t0" => assets.get_lana_clip_near_orgasm_t0(),
        "lana/react/near_orgasm.t1" => assets.get_lana_clip_near_orgasm_t1(),
        "lana/react/near_orgasm.t2" => assets.get_lana_clip_near_orgasm_t2(),
        "lana/react/near_orgasm.t3" => assets.get_lana_clip_near_orgasm_t3(),
        "lana/react/orgasm.t0" => assets.get_lana_clip_orgasm_t0(),
        "lana/react/orgasm.t1" => assets.get_lana_clip_orgasm_t1(),
        "lana/react/orgasm.t2" => assets.get_lana_clip_orgasm_t2(),
        "lana/react/orgasm.t3" => assets.get_lana_clip_orgasm_t3(),
        "lana/react/squirt_event.t0" => assets.get_lana_clip_squirt_event_t0(),
        "lana/react/squirt_event.t1" => assets.get_lana_clip_squirt_event_t1(),
        "lana/react/squirt_event.t2" => assets.get_lana_clip_squirt_event_t2(),
        "lana/react/squirt_event.t3" => assets.get_lana_clip_squirt_event_t3(),
        "lana/react/overstimulation.t0" => assets.get_lana_clip_overstimulation_t0(),
        "lana/react/overstimulation.t1" => assets.get_lana_clip_overstimulation_t1(),
        "lana/react/overstimulation.t2" => assets.get_lana_clip_overstimulation_t2(),
        "lana/react/overstimulation.t3" => assets.get_lana_clip_overstimulation_t3(),
        "lana/react/afterglow.t0" => assets.get_lana_clip_afterglow_t0(),
        "lana/react/afterglow.t1" => assets.get_lana_clip_afterglow_t1(),
        "lana/react/afterglow.t2" => assets.get_lana_clip_afterglow_t2(),
        "lana/react/afterglow.t3" => assets.get_lana_clip_afterglow_t3(),
        _ => Image::default(),
    }
}

/// The clip player. Owns what is on the figure right now, what is
/// queued for the next loop boundary, and the position inside the
/// current loop (in frames, fractional — the displayed frame is its
/// floor).
pub struct Player {
    current: &'static ClipAsset,
    pending: Option<&'static ClipAsset>,
    position: f64,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    pub fn new() -> Player {
        Player {
            current: idle_asset(),
            pending: None,
            position: 0.0,
        }
    }

    pub fn current(&self) -> &'static ClipAsset {
        self.current
    }

    // Inspection surface for the queue; exercised by the player
    // tests (the stage reads `current`/`frame` only).
    #[allow(dead_code)]
    pub fn pending(&self) -> Option<&'static ClipAsset> {
        self.pending
    }

    /// The frame on the figure right now.
    pub fn frame(&self) -> u32 {
        (self.position.floor() as u32) % self.current.frames
    }

    /// State-driven selection: queue `clip` for the next loop
    /// boundary. Asking for the playing clip cancels any queue.
    pub fn desire(&mut self, clip: &'static ClipAsset) {
        if clip.key == self.current.key {
            self.pending = None;
        } else if self.pending.map(|p| p.key) != Some(clip.key) {
            self.pending = Some(clip);
        }
    }

    /// STOP: hard cut to the idle loop, frame 0, in this action —
    /// the same action that zeroes the drives.
    pub fn stop(&mut self) {
        self.current = idle_asset();
        self.pending = None;
        self.position = 0.0;
    }

    /// Advances playback by `dt_secs` of wall time. At a loop
    /// boundary a queued clip takes over; the sub-frame overshoot
    /// carries into the next loop, so no time is lost or invented at
    /// the seam.
    pub fn advance(&mut self, dt_secs: f64) {
        if dt_secs <= 0.0 || !dt_secs.is_finite() {
            return;
        }
        self.position += dt_secs * f64::from(CLIP_FPS);
        loop {
            let len = f64::from(self.current.frames);
            if self.position < len {
                break;
            }
            self.position -= len;
            if let Some(next) = self.pending.take() {
                self.current = next;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reaction::{ReactionFrame, clip_key};

    fn frame_for(band: Band, tier: u8) -> ReactionFrame {
        ReactionFrame {
            band,
            tier,
            expr: Default::default(),
            body: Default::default(),
            clip_key: clip_key(band, tier),
        }
    }

    #[test]
    fn asset_table_is_the_full_band_tier_space() {
        assert_eq!(CLIP_ASSETS.len(), 32);
        let mut seen = std::collections::HashSet::new();
        for clip in CLIP_ASSETS {
            assert!(seen.insert(clip.key), "duplicate key {}", clip.key);
            assert_eq!(clip.key, clip_key(clip.band, clip.tier));
            assert!(clip.frames > 0);
        }
        for band in Band::ALL {
            for tier in 0..=3u8 {
                assert!(
                    asset_for(&clip_key(band, tier)).is_some(),
                    "no asset for {} t{tier}",
                    band.key()
                );
            }
        }
        // The census of record: 3,264 frames across the set.
        assert_eq!(CLIP_ASSETS.iter().map(|c| c.frames).sum::<u32>(), 3264);
    }

    #[test]
    fn selection_mapping_resolves_every_state_to_its_exact_clip() {
        let mut registry = ClipRegistry::new();
        register_all(&mut registry);
        for band in Band::ALL {
            for tier in 0..=3u8 {
                let frame = frame_for(band, tier);
                let key = registry.lookup(&frame).expect("every band x tier resolves");
                assert_eq!(key, frame.clip_key, "exact tier must win");
                let clip = asset_for(&key).expect("resolved key has an asset");
                assert_eq!(clip.band, band);
                assert_eq!(clip.tier, tier);
            }
        }
    }

    #[test]
    fn selection_falls_back_to_tier_1_when_a_tier_is_missing() {
        // The registry's contract, exercised against the asset
        // table: with only tier 1 registered for a band, a tier-3
        // frame resolves to the tier-1 clip.
        let mut registry = ClipRegistry::new();
        registry.register("lana/react/building.t1");
        let frame = frame_for(Band::Building, 3);
        let key = registry.lookup(&frame).expect("tier 1 fallback");
        assert_eq!(key, "lana/react/building.t1");
        assert_eq!(asset_for(&key).map(|c| c.tier), Some(1));
    }

    #[test]
    fn cell_offsets_walk_the_grid() {
        let clip = asset_for("lana/react/afterglow.t3").unwrap();
        assert_eq!(cell_offset(clip, 0), (0, 0));
        assert_eq!(cell_offset(clip, 11), (11 * CELL_W, 0));
        assert_eq!(cell_offset(clip, 12), (0, CELL_H));
        assert_eq!(cell_offset(clip, 143), (11 * CELL_W, 11 * CELL_H));
        // Frames past the loop end wrap inside the same strip.
        assert_eq!(cell_offset(clip, 144), (0, 0));
    }

    #[test]
    fn player_starts_on_the_idle_loop() {
        let player = Player::new();
        assert_eq!(player.current().key, "lana/react/composed.t0");
        assert_eq!(player.frame(), 0);
        assert!(player.pending().is_none());
    }

    #[test]
    fn player_frame_is_the_floor_of_position() {
        let mut player = Player::new();
        player.advance(0.5 / f64::from(CLIP_FPS));
        assert_eq!(player.frame(), 0);
        player.advance(0.6 / f64::from(CLIP_FPS));
        assert_eq!(player.frame(), 1);
    }

    #[test]
    fn pending_clip_waits_for_the_loop_boundary() {
        let mut player = Player::new();
        let target = asset_for("lana/react/orgasm.t3").unwrap();
        player.advance(1.0); // 24 frames into the 96-frame idle loop
        player.desire(target);
        assert_eq!(player.current().key, "lana/react/composed.t0");
        assert_eq!(player.pending().map(|p| p.key), Some(target.key));
        // Advance to just before the boundary: still the old clip.
        player.advance(71.0 / f64::from(CLIP_FPS));
        assert_eq!(player.current().key, "lana/react/composed.t0");
        assert_eq!(player.frame(), 95);
        // Cross the boundary: the queued clip takes over at its start.
        player.advance(1.0 / f64::from(CLIP_FPS));
        assert_eq!(player.current().key, target.key);
        assert_eq!(player.frame(), 0);
        assert!(player.pending().is_none());
    }

    #[test]
    fn desire_for_the_playing_clip_cancels_the_queue() {
        let mut player = Player::new();
        player.desire(asset_for("lana/react/building.t2").unwrap());
        assert!(player.pending().is_some());
        player.desire(idle_asset());
        assert!(player.pending().is_none());
    }

    #[test]
    fn loops_wrap_without_drift() {
        let mut player = Player::new();
        let len = f64::from(player.current().frames);
        // Ten exact loops land back on frame 0.
        for _ in 0..10 {
            player.advance(len / f64::from(CLIP_FPS));
        }
        assert_eq!(player.frame(), 0);
        // And a fractional position survives loop crossings intact.
        player.advance(37.5 / f64::from(CLIP_FPS));
        assert_eq!(player.frame(), 37);
        player.advance(len / f64::from(CLIP_FPS));
        assert_eq!(player.frame(), 37);
    }

    #[test]
    fn stop_cuts_immediately_to_idle_frame_zero() {
        let mut player = Player::new();
        player.desire(asset_for("lana/react/near_orgasm.t2").unwrap());
        player.advance(4.0); // crosses into near_orgasm.t2, mid-loop
        assert_eq!(player.current().key, "lana/react/near_orgasm.t2");
        player.desire(asset_for("lana/react/orgasm.t3").unwrap());
        player.stop();
        assert_eq!(player.current().key, "lana/react/composed.t0");
        assert_eq!(player.frame(), 0);
        assert!(player.pending().is_none());
    }
}
