//! The safety state machine.
//!
//! One value holds everything the safety contract says about the
//! session: whether the master toggle is armed, whether output is
//! currently active, when the last sign of life arrived, how often the
//! watchdog has fired, and whether the session has been degraded by a
//! transport drop. The server mutates it only through these methods, so
//! every path that can zero output — stop, disarm, watchdog, session
//! drop, shutdown — does so the same way: state first, synchronously,
//! then the wire.
//!
//! Latching rules, mirroring the estate bridge semantics:
//! - Stop zeroes output AND disarms; recovery is a fresh explicit arm.
//! - A watchdog trip zeroes output AND disarms.
//! - A session drop zeroes output AND disarms, and marks the session
//!   degraded. A reconnect therefore resumes nothing: after any of
//!   these events, output is impossible until `lovense_arm` is called
//!   again on purpose.

use std::time::{Duration, Instant};

use crate::decide::GateSnapshot;

/// The session's safety state.
pub struct SafetyState {
    armed: bool,
    output_active: bool,
    last_activity: Instant,
    watchdog_trips: u64,
    session_degraded: bool,
    last_event: Option<String>,
}

impl SafetyState {
    /// A fresh session: master off, nothing active. The safe state is
    /// the default state.
    #[must_use]
    pub fn new() -> Self {
        SafetyState {
            armed: false,
            output_active: false,
            last_activity: Instant::now(),
            watchdog_trips: 0,
            session_degraded: false,
            last_event: None,
        }
    }

    #[must_use]
    pub fn armed(&self) -> bool {
        self.armed
    }

    #[must_use]
    pub fn output_active(&self) -> bool {
        self.output_active
    }

    #[must_use]
    pub fn watchdog_trips(&self) -> u64 {
        self.watchdog_trips
    }

    /// `"ok"`, or `"degraded"` after a session drop.
    #[must_use]
    pub fn session_health(&self) -> &'static str {
        if self.session_degraded {
            "degraded"
        } else {
            "ok"
        }
    }

    #[must_use]
    pub fn last_event(&self) -> Option<&str> {
        self.last_event.as_deref()
    }

    /// The gate's view of the world for one decision.
    #[must_use]
    pub fn snapshot(&self, target_configured: bool, ceiling: u8) -> GateSnapshot {
        GateSnapshot {
            armed: self.armed,
            target_configured,
            ceiling,
        }
    }

    /// Record a sign of life. Every tool call is a heartbeat; the
    /// watchdog measures silence from the most recent one.
    pub fn note_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Arm the master toggle. Arming starts no output by itself.
    pub fn arm(&mut self) {
        self.armed = true;
        self.last_event = Some("armed".to_string());
        self.note_activity();
    }

    /// Output reached the wire: it is now active, and the call that
    /// started it is the latest sign of life.
    pub fn mark_output_started(&mut self) {
        self.output_active = true;
        self.note_activity();
    }

    /// Zero the local output state and latch the master off. Shared by
    /// stop and disarm; the caller sends the wire zero first where the
    /// contract demands it synchronous.
    pub fn zero_and_disarm(&mut self, event: &str) {
        self.output_active = false;
        self.armed = false;
        self.last_event = Some(event.to_string());
        self.note_activity();
    }

    /// Whether the watchdog window has elapsed with output active.
    #[must_use]
    pub fn watchdog_expired(&self, watchdog_secs: u8) -> bool {
        self.output_active
            && self.last_activity.elapsed() > Duration::from_secs(u64::from(watchdog_secs))
    }

    /// Time left before the watchdog fires, when output is active;
    /// `None` when it cannot fire. A zero duration means fire now.
    #[must_use]
    pub fn watchdog_remaining(&self, watchdog_secs: u8) -> Option<Duration> {
        if !self.output_active {
            return None;
        }
        let window = Duration::from_secs(u64::from(watchdog_secs));
        Some(window.saturating_sub(self.last_activity.elapsed()))
    }

    /// Fire the watchdog: zero output, latch off, count the trip.
    /// Returns true exactly when this call tripped, so the caller sends
    /// the wire zero once.
    pub fn trip_watchdog_if_expired(&mut self, watchdog_secs: u8) -> bool {
        if !self.watchdog_expired(watchdog_secs) {
            return false;
        }
        self.output_active = false;
        self.armed = false;
        self.watchdog_trips += 1;
        self.last_event = Some("watchdog_expired: output zeroed; re-arm required".to_string());
        true
    }

    /// A transport send failed mid-session: the session is dropped.
    /// Output zeroes and the master latches off, so nothing can resume
    /// when a transport comes back; the session reports degraded until
    /// the server restarts.
    pub fn on_session_drop(&mut self) {
        self.output_active = false;
        self.armed = false;
        self.session_degraded = true;
        self.last_event = Some("session_drop: output zeroed; re-arm required".to_string());
        self.note_activity();
    }

    /// Test seam: pretend the last sign of life was `secs` ago, so the
    /// watchdog battery needs no real sleeps.
    #[cfg(test)]
    pub(crate) fn backdate_activity(&mut self, secs: u64) {
        self.last_activity = Instant::now() - Duration::from_secs(secs);
    }
}

impl Default for SafetyState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- validation ---------------------------------------------------

    #[test]
    fn fresh_state_is_disarmed_and_inactive() {
        let state = SafetyState::new();
        assert!(!state.armed());
        assert!(!state.output_active());
        assert_eq!(state.watchdog_trips(), 0);
        assert_eq!(state.session_health(), "ok");
        assert_eq!(state.last_event(), None);
        assert!(!state.snapshot(true, 20).armed);
    }

    #[test]
    fn watchdog_cannot_fire_without_active_output() {
        let mut state = SafetyState::new();
        state.backdate_activity(3_600);
        assert!(!state.watchdog_expired(5));
        assert_eq!(state.watchdog_remaining(5), None);
        assert!(!state.trip_watchdog_if_expired(5));
    }

    // --- adversarial --------------------------------------------------

    #[test]
    fn watchdog_trips_once_and_latches_off() {
        let mut state = SafetyState::new();
        state.arm();
        state.mark_output_started();
        state.backdate_activity(6);
        assert!(state.watchdog_expired(5));
        assert!(state.trip_watchdog_if_expired(5));
        assert!(!state.output_active());
        assert!(!state.armed(), "a trip latches the master off");
        assert_eq!(state.watchdog_trips(), 1);
        // Already zeroed: a second check does not trip again.
        assert!(!state.trip_watchdog_if_expired(5));
        assert_eq!(state.watchdog_trips(), 1);
    }

    #[test]
    fn activity_inside_the_window_keeps_output_alive() {
        let mut state = SafetyState::new();
        state.arm();
        state.mark_output_started();
        state.backdate_activity(4);
        state.note_activity();
        assert!(!state.watchdog_expired(5));
        assert!(state.output_active());
    }

    #[test]
    fn session_drop_zeroes_and_blocks_any_resume() {
        let mut state = SafetyState::new();
        state.arm();
        state.mark_output_started();
        state.on_session_drop();
        assert!(!state.output_active());
        assert!(!state.armed(), "recovery needs a fresh explicit arm");
        assert_eq!(state.session_health(), "degraded");
        assert!(!state.snapshot(true, 20).armed);
    }
}
