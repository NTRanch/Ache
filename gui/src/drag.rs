//! Lana, picked up: click-and-drag where the grab point matters.
//!
//! Owner direction 2026-11-27: no black box around her — she floats
//! over the lab backdrop — and you can grab her and pick her up; where
//! you grab decides how she hangs. This module is the whole mechanic
//! as a pure, deterministic state machine: the UI feeds it pointer
//! positions in panel coordinates on a fixed 30 ms step, and it answers
//! with the figure's offset, hang angle and rotation origin. No
//! randomness, no wall clock — the same input sequence always produces
//! the same sway, which is what the tests pin.
//!
//! The hang itself is mostly geometry, not simulation: Slint rotates
//! the figure about the rotation origin, and the origin *is* the grab
//! point, so the grabbed point stays pinned under the pointer at any
//! angle and the body swings beneath it. What is simulated is only the
//! angle: a damped spring per grab region, leaned by pointer velocity
//! (a pendulum lags the hand that carries it), bounded per region.
//! There is deliberately no ragdoll — this is a doll-pickup read.
//!
//! Layout trap, learned live: the TouchArea must sit on the
//! *untransformed* drag container, with only the visual child
//! rotating. Pointer coords read through a rotated element are in
//! the rotated frame (and go stale between pointer events), which
//! made a still hold creep; on the plain container they are exact
//! figure-local coords and the pin is a plain sum.
//!
//! The spring's rest target is the *hang tilt*: a real held body
//! settles with its centre of mass under the grab point, so the full
//! pendulum angle is computed from the grab point and the figure's
//! approximate centre of mass, and each region takes a bounded
//! fraction of it (see [`Drag::begin`]). That is what makes the holds
//! differ at rest, not just in motion: a waist grab's full angle is
//! already ~0 (a level lift), a hand grab's is ~85 (she tips far
//! toward the dangle, fraction 0.45), a leg grab's is 180 (fraction
//! 0.65 lands at ~117, inverted-ish but never fully upside down).

/// Figure display size (px) and its rest position in panel coords.
/// Mirrors the layout constants in `ui/app.slint` (the room layout:
/// she floats center-stage in the 1280x820 window; the figure rests
/// at x=520, y=170).
pub const FIG_W: f32 = 240.0;
pub const FIG_H: f32 = 480.0;
pub const REST_X: f32 = 520.0;
pub const REST_Y: f32 = 170.0;

/// The figure's approximate centre of mass in figure-local px,
/// estimated from the masters' standing proportions (just below the
/// waist). Only used to derive the hang tilt in [`Drag::begin`].
pub const COM: (f32, f32) = (120.0, 249.3);

/// Pointer travel is clamped to the window plus a margin, so she
/// can be carried a little past the stage edge but never lost
/// off-window.
const POINTER_MIN_X: f32 = -60.0;
const POINTER_MAX_X: f32 = 1340.0;
const POINTER_MIN_Y: f32 = -20.0;
const POINTER_MAX_Y: f32 = 860.0;

/// Where on her body the grab landed. Derived from the grab point's
/// normalized figure coordinates by [`region_at`]; the mapping is
/// pose-independent (the strips differ per reaction band, and a
/// per-pose skeleton is far more machinery than the read needs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrabRegion {
    /// Head / hair: a short, steep hang — she dangles almost straight
    /// down from the grab, quick small sway.
    Head,
    /// Hand / forearm: the long dangle — slow, wide swing.
    Hand,
    /// Waist / torso: a level lift — stiff spring, she stays level.
    Torso,
    /// Leg / ankle: tipped over — a large bounded tilt, head down-ish.
    Leg,
}

/// Per-region hang character. Angles in degrees, spring constants in
/// 1/s^2 (stiffness) and 1/s (damping).
#[derive(Clone, Copy, Debug)]
pub struct HangProfile {
    /// Fraction of the full pendulum angle (centre of mass settled
    /// under the grab point) she actually settles to while held.
    pub hang_fraction: f32,
    /// How far pointer motion may lean her away from the base tilt.
    pub max_lean_deg: f32,
    /// Lean per unit of smoothed pointer velocity (deg per px/s).
    pub sway_gain: f32,
    pub stiffness: f32,
    pub damping: f32,
}

impl GrabRegion {
    pub fn profile(self) -> HangProfile {
        match self {
            GrabRegion::Head => HangProfile {
                hang_fraction: 0.5,
                max_lean_deg: 12.0,
                sway_gain: 0.010,
                stiffness: 55.0,
                damping: 9.0,
            },
            GrabRegion::Hand => HangProfile {
                hang_fraction: 0.45,
                max_lean_deg: 30.0,
                sway_gain: 0.030,
                stiffness: 22.0,
                damping: 5.5,
            },
            GrabRegion::Torso => HangProfile {
                hang_fraction: 0.25,
                max_lean_deg: 7.0,
                sway_gain: 0.006,
                stiffness: 80.0,
                damping: 13.0,
            },
            GrabRegion::Leg => HangProfile {
                hang_fraction: 0.65,
                max_lean_deg: 18.0,
                sway_gain: 0.016,
                stiffness: 30.0,
                damping: 6.5,
            },
        }
    }

    /// Her line while held by this region — flustered, in register.
    /// Shown through the existing reaction-line path while the drag
    /// lasts (see `main.rs`); the band/strip machinery is untouched.
    pub fn held_line(self) -> &'static str {
        match self {
            GrabRegion::Head => {
                "H-hey — by the hair?! Careful, careful… okay, this is kind of fun."
            }
            GrabRegion::Hand => "By the hand? How romantic. Don't let go — I'd swing for hours.",
            GrabRegion::Torso => "Ooh — lifted like a doll. Mind the merchandise; it's sensitive.",
            GrabRegion::Leg => {
                "By the ankle?! Everything's upside down — wait. This view is… interesting."
            }
        }
    }
}

/// Map a grab point in normalized figure coordinates (0..1, origin
/// top-left) to a body region.
///
/// Bands, from the masters' standing proportions: head and hair fill
/// the top ~17%; her arms hang along her sides, so the lateral strips
/// from 30% to 70% height are forearm/hand territory; legs run from
/// ~70% down. Everything else is the torso mass (chest, waist, hips).
pub fn region_at(nx: f32, ny: f32) -> GrabRegion {
    let nx = nx.clamp(0.0, 1.0);
    let ny = ny.clamp(0.0, 1.0);
    if ny < 0.17 {
        GrabRegion::Head
    } else if ny >= 0.70 {
        GrabRegion::Leg
    } else if (nx <= 0.30 || nx >= 0.70) && ny >= 0.30 {
        GrabRegion::Hand
    } else {
        GrabRegion::Torso
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Rest,
    Held,
    Settling,
}

/// The drag state machine. Feed [`Drag::step`] a fixed `dt` and, while
/// held, the pointer's panel position; read the figure transform from
/// [`Drag::offset`], [`Drag::angle_deg`] and [`Drag::origin`].
pub struct Drag {
    mode: Mode,
    region: GrabRegion,
    /// Grab point in figure-local px — the rotation origin.
    grab_local: (f32, f32),
    /// Hang tilt for this grab (deg), derived in [`Drag::begin`].
    base_tilt: f32,
    /// Figure offset from rest (panel px).
    offset: (f32, f32),
    off_vel: (f32, f32),
    angle_deg: f32,
    angle_vel: f32,
    /// Last pointer position (panel) and its smoothed velocity.
    pointer: (f32, f32),
    pointer_vel: (f32, f32),
}

impl Default for Drag {
    fn default() -> Self {
        Drag {
            mode: Mode::Rest,
            region: GrabRegion::Torso,
            grab_local: (FIG_W / 2.0, FIG_H / 2.0),
            base_tilt: 0.0,
            offset: (0.0, 0.0),
            off_vel: (0.0, 0.0),
            angle_deg: 0.0,
            angle_vel: 0.0,
            pointer: (0.0, 0.0),
            pointer_vel: (0.0, 0.0),
        }
    }
}

impl Drag {
    /// True while she is held or still settling back to rest.
    pub fn active(&self) -> bool {
        self.mode != Mode::Rest
    }

    pub fn held(&self) -> bool {
        self.mode == Mode::Held
    }

    pub fn region(&self) -> GrabRegion {
        self.region
    }

    /// The hang tilt this grab settles to while held still.
    pub fn base_tilt(&self) -> f32 {
        self.base_tilt
    }

    pub fn offset(&self) -> (f32, f32) {
        self.offset
    }

    pub fn angle_deg(&self) -> f32 {
        self.angle_deg
    }

    /// Rotation origin in figure-local px (the grab point).
    pub fn origin(&self) -> (f32, f32) {
        self.grab_local
    }

    /// The figure element's current panel position.
    pub fn elem_pos(&self) -> (f32, f32) {
        (REST_X + self.offset.0, REST_Y + self.offset.1)
    }

    /// Grab her. `nx`/`ny` are the grab point in normalized figure
    /// coordinates; `pointer` is the pointer's current panel position
    /// (the caller recovers it as element position + the TouchArea's
    /// local mouse coords — the TouchArea sits on the untransformed
    /// drag container, so that sum is exact at any hang angle, and a
    /// re-grab mid-settle starts from the true pointer position).
    /// The current angle carries over — no snap on a re-grab.
    pub fn begin(&mut self, nx: f32, ny: f32, pointer: (f32, f32)) {
        self.region = region_at(nx, ny);
        self.grab_local = (nx.clamp(0.0, 1.0) * FIG_W, ny.clamp(0.0, 1.0) * FIG_H);
        self.base_tilt = hang_tilt(self.grab_local, self.region.profile().hang_fraction);
        self.pointer = clamp_pointer(pointer);
        self.pointer_vel = (0.0, 0.0);
        self.off_vel = (0.0, 0.0);
        self.mode = Mode::Held;
        self.pin_to_pointer();
    }

    /// Let go: she eases back to her floating rest position.
    pub fn release(&mut self) {
        if self.mode == Mode::Held {
            self.mode = Mode::Settling;
            self.pointer_vel = (0.0, 0.0);
        }
    }

    /// Advance one fixed step. `pointer` is the pointer's panel
    /// position while held, `None` once released.
    pub fn step(&mut self, dt: f32, pointer: Option<(f32, f32)>) {
        match self.mode {
            Mode::Rest => {}
            Mode::Held => {
                let p = clamp_pointer(pointer.unwrap_or(self.pointer));
                let inst = ((p.0 - self.pointer.0) / dt, (p.1 - self.pointer.1) / dt);
                self.pointer = p;
                // Smoothed velocity: enough lag to read as weight,
                // not so much that she trails the hand.
                self.pointer_vel.0 += (inst.0 - self.pointer_vel.0) * 0.35;
                self.pointer_vel.1 += (inst.1 - self.pointer_vel.1) * 0.35;
                self.pin_to_pointer();
                let prof = self.region.profile();
                let lean = (-self.pointer_vel.0 * prof.sway_gain)
                    .clamp(-prof.max_lean_deg, prof.max_lean_deg);
                self.spring_angle(self.base_tilt() + lean, prof.stiffness, prof.damping, dt);
            }
            Mode::Settling => {
                // One underdamped spring home for the angle, one per
                // offset axis; done when everything is near rest and
                // slow, then snapped exact so rest is bit-stable.
                self.spring_angle(0.0, 48.0, 10.0, dt);
                for axis in 0..2 {
                    let (pos, vel) = if axis == 0 {
                        (&mut self.offset.0, &mut self.off_vel.0)
                    } else {
                        (&mut self.offset.1, &mut self.off_vel.1)
                    };
                    let acc = -44.0 * *pos - 11.0 * *vel;
                    *vel += acc * dt;
                    *pos += *vel * dt;
                }
                if self.angle_deg.abs() < 0.4
                    && self.angle_vel.abs() < 6.0
                    && self.offset.0.abs() < 1.5
                    && self.offset.1.abs() < 1.5
                    && self.off_vel.0.abs() < 30.0
                    && self.off_vel.1.abs() < 30.0
                {
                    self.mode = Mode::Rest;
                    self.offset = (0.0, 0.0);
                    self.off_vel = (0.0, 0.0);
                    self.angle_deg = 0.0;
                    self.angle_vel = 0.0;
                }
            }
        }
    }

    /// Place the element so the grab point sits exactly under the
    /// pointer: because the origin is the grab point, its rendered
    /// position is `elem + grab_local` at any angle.
    fn pin_to_pointer(&mut self) {
        self.offset = (
            self.pointer.0 - self.grab_local.0 - REST_X,
            self.pointer.1 - self.grab_local.1 - REST_Y,
        );
    }

    fn spring_angle(&mut self, target: f32, stiffness: f32, damping: f32, dt: f32) {
        let acc = stiffness * (target - self.angle_deg) - damping * self.angle_vel;
        self.angle_vel += acc * dt;
        self.angle_deg = (self.angle_deg + self.angle_vel * dt).clamp(-160.0, 160.0);
    }
}

/// The tilt a grab settles to: the full pendulum angle that would
/// put the centre of mass directly under the grab point, scaled by
/// the region's hang fraction. Screen coordinates (y down): straight
/// down from the grab point is +90 deg. A grab at the centre of mass
/// itself has no defined hang direction and reads as level.
pub fn hang_tilt(grab_local: (f32, f32), fraction: f32) -> f32 {
    let d = (COM.0 - grab_local.0, COM.1 - grab_local.1);
    if d.0 * d.0 + d.1 * d.1 < 16.0 {
        return 0.0;
    }
    let dir = d.1.atan2(d.0).to_degrees();
    let mut full = 90.0 - dir;
    while full > 180.0 {
        full -= 360.0;
    }
    while full < -180.0 {
        full += 360.0;
    }
    // Deep-inversion grabs (centre of mass well above the grab
    // point, |full| near 180) sit on a sign discontinuity: a grab a
    // pixel left of the centreline computes +179, a pixel right
    // computes -179, and the scaled tilts would point opposite
    // ways. The doll read wants one continuous choice there, so
    // deep grabs always tip clockwise — her head falls toward the
    // window's open side rather than out of it.
    if full.abs() > 135.0 {
        full = full.abs();
    }
    full * fraction
}

fn clamp_pointer(p: (f32, f32)) -> (f32, f32) {
    (
        p.0.clamp(POINTER_MIN_X, POINTER_MAX_X),
        p.1.clamp(POINTER_MIN_Y, POINTER_MAX_Y),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 0.03;

    #[test]
    fn regions_map_by_body_part() {
        assert_eq!(region_at(0.5, 0.08), GrabRegion::Head);
        assert_eq!(region_at(0.15, 0.02), GrabRegion::Head);
        assert_eq!(region_at(0.08, 0.50), GrabRegion::Hand);
        assert_eq!(region_at(0.92, 0.45), GrabRegion::Hand);
        assert_eq!(region_at(0.5, 0.45), GrabRegion::Torso);
        assert_eq!(region_at(0.5, 0.25), GrabRegion::Torso);
        assert_eq!(region_at(0.85, 0.20), GrabRegion::Torso);
        assert_eq!(region_at(0.5, 0.85), GrabRegion::Leg);
        assert_eq!(region_at(0.1, 0.95), GrabRegion::Leg);
        // Out-of-range input clamps instead of panicking.
        assert_eq!(region_at(-1.0, 2.0), GrabRegion::Leg);
    }

    #[test]
    fn leg_grab_settles_to_its_base_tilt_while_held() {
        let mut d = Drag::default();
        d.begin(0.5, 0.9, (200.0, 330.0));
        for _ in 0..200 {
            d.step(DT, Some((200.0, 330.0)));
        }
        assert!(d.held());
        assert!(
            (d.base_tilt() - 117.0).abs() < 1.0,
            "leg base tilt {} should be ~117",
            d.base_tilt()
        );
        assert!(
            (d.angle_deg() - d.base_tilt()).abs() < 3.0,
            "angle {} should hang near the leg base tilt",
            d.angle_deg()
        );
    }

    #[test]
    fn torso_grab_stays_near_level_under_pointer_motion() {
        let mut d = Drag::default();
        d.begin(0.5, 0.45, (144.0, 162.0));
        // A brisk side-to-side carry for five seconds.
        for i in 0..160 {
            let x = 144.0 + (i as f32 * 0.35).sin() * 90.0;
            d.step(DT, Some((x, 162.0)));
            assert!(d.angle_deg().abs() < 20.0, "torso lift should stay level");
        }
    }

    #[test]
    fn sway_is_bounded_under_violent_motion() {
        for (nx, ny) in [(0.5f32, 0.08f32), (0.08, 0.5), (0.5, 0.45), (0.5, 0.9)] {
            let mut d = Drag::default();
            d.begin(nx, ny, (144.0, 180.0));
            let prof = d.region().profile();
            let bound = d.base_tilt().abs() + prof.max_lean_deg + 30.0;
            for i in 0..400 {
                let x = 144.0 + if i % 2 == 0 { 140.0 } else { -140.0 };
                d.step(DT, Some((x, 180.0)));
                assert!(d.angle_deg().is_finite());
                assert!(
                    d.angle_deg().abs() < bound,
                    "region {:?}: angle {} exceeds bound {bound}",
                    d.region(),
                    d.angle_deg()
                );
            }
        }
    }

    #[test]
    fn hand_grab_dangles_toward_the_held_side() {
        // Left hand (figure-local x ~14): she tips clockwise, body
        // trailing under the hand; the right hand mirrors it.
        let mut d = Drag::default();
        d.begin(0.08, 0.5, (86.0, 357.0));
        assert!(
            d.base_tilt() > 30.0 && d.base_tilt() < 46.0,
            "tilt {}",
            d.base_tilt()
        );
        for _ in 0..200 {
            d.step(DT, Some((86.0, 357.0)));
        }
        assert!((d.angle_deg() - d.base_tilt()).abs() < 3.0);
        let mut d2 = Drag::default();
        d2.begin(0.92, 0.5, (238.0, 357.0));
        assert!(d2.base_tilt() < -30.0 && d2.base_tilt() > -46.0);
    }

    #[test]
    fn head_and_waist_grabs_hang_near_level() {
        let mut d = Drag::default();
        d.begin(0.5, 0.08, (144.0, 205.0));
        assert!(d.base_tilt().abs() < 1.0);
        let mut d2 = Drag::default();
        d2.begin(0.5, 0.45, (144.0, 339.0));
        assert!(d2.base_tilt().abs() < 1.0);
    }

    #[test]
    fn leg_grabs_tip_clockwise_on_both_sides() {
        // Left leg, dead centre, a hair right of centre (the wrap
        // trap), right leg: one continuous clockwise tip.
        for nx in [0.36, 0.5, 0.54, 0.64] {
            let mut d = Drag::default();
            d.begin(nx, 0.9, (150.0, 449.0));
            assert!(d.base_tilt() > 90.0, "nx {nx}: tilt {}", d.base_tilt());
        }
    }

    #[test]
    fn still_hold_is_a_fixed_point() {
        // A perfectly still pointer holds her perfectly still:
        // the offset converges to pointer - grab - rest and then
        // does not creep, and the angle settles on the hang tilt.
        let mut d = Drag::default();
        d.begin(0.54, 0.9, (160.0, 355.0));
        for _ in 0..300 {
            d.step(DT, Some((170.0, 360.0)));
        }
        let (off1, ang1) = (d.offset(), d.angle_deg());
        for _ in 0..300 {
            d.step(DT, Some((170.0, 360.0)));
        }
        assert!((d.offset().0 - off1.0).abs() < 0.01 && (d.offset().1 - off1.1).abs() < 0.01);
        assert!((d.angle_deg() - ang1).abs() < 0.01);
        let want = (170.0 - 0.54 * FIG_W - REST_X, 360.0 - 0.9 * FIG_H - REST_Y);
        assert!(
            (d.offset().0 - want.0).abs() < 0.5 && (d.offset().1 - want.1).abs() < 0.5,
            "offset {:?} want {want:?}",
            d.offset()
        );
    }

    #[test]
    fn release_settles_back_to_exact_rest() {
        let mut d = Drag::default();
        d.begin(0.5, 0.9, (240.0, 340.0));
        for _ in 0..60 {
            d.step(DT, Some((240.0, 340.0)));
        }
        d.release();
        for _ in 0..400 {
            d.step(DT, None);
        }
        assert!(!d.active());
        assert_eq!(d.offset(), (0.0, 0.0));
        assert_eq!(d.angle_deg(), 0.0);
    }

    #[test]
    fn the_same_carry_replays_bit_identically() {
        let run = || {
            let mut d = Drag::default();
            d.begin(0.08, 0.5, (100.0, 200.0));
            let mut trace = Vec::new();
            for i in 0..120 {
                let p = (
                    100.0 + (i as f32 * 0.22).sin() * 70.0,
                    200.0 + (i as f32) * 0.4,
                );
                d.step(DT, Some(p));
                trace.push((d.offset(), d.angle_deg()));
            }
            trace
        };
        assert_eq!(run(), run());
    }
}
