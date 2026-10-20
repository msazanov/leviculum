//! Whether the GNSS receiver should be awake (Codeberg #457, step two).
//!
//! The board's accelerometer says `moving` or `still`
//! (`leviculum-motion`); this gate turns that into a power decision for
//! the receiver. A still board's position is the one it already has, so
//! the receiver sleeps on `still` and wakes on `moving`. The movement
//! proof of #401 then gets no new fixes while the board is still, which is
//! what a still board would have produced anyway
//! (`docs/src/concepts/announce-cadence.md`).
//!
//! Two things keep the receiver awake whatever the accelerometer says:
//!
//! - **No motion value at all.** A board without the part, or one whose
//!   part did not answer, never publishes one, and the gate never sleeps:
//!   the receiver runs exactly as it did before this module.
//! - **The first-fix grace.** A board that boots and lies still would
//!   otherwise sleep 30 s in and never learn where it is. Until the first
//!   fix, or until [`GNSS_FIRST_FIX_GRACE_S`] has passed, `still` keeps
//!   the receiver awake; after either, `still` means sleep.
//!
//! Pure and host-tested like [`crate::PresenceMachine`]; the GNSS task
//! acts on the [`PowerEdge`]s, prints them and owns the pins.

use core::fmt;

/// How long after boot `still` cannot put the receiver to sleep unless a
/// fix has arrived first, in seconds.
///
/// Ten minutes covers a cold start with margin. The AG3335 modules'
/// datasheets give a full cold start of 26-29 s in open sky (Quectel
/// LC29H hardware design 1.3, table 3) and 24 s (Delincomm EM1612L5,
/// AG3335M), and a weak indoor or window-sill signal takes several times
/// that. A board that has not
/// fixed in ten minutes is not going to fix where it lies, and keeping a
/// receiver awake longer only spends the battery a still board was meant
/// to save; the next `moving` wakes it for another try.
pub const GNSS_FIRST_FIX_GRACE_S: u64 = 600;

const GRACE_MS: u64 = GNSS_FIRST_FIX_GRACE_S * 1_000;

/// What the accelerometer says, as the GNSS side reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Moving,
    Still,
}

impl Motion {
    /// The `reason=` token.
    pub fn as_str(self) -> &'static str {
        match self {
            Motion::Moving => "moving",
            Motion::Still => "still",
        }
    }
}

/// The receiver's power as the gate wants it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Awake,
    Asleep,
}

impl Power {
    /// The `state=` token: the act, not the state, because the line is
    /// printed on the edge.
    pub fn as_str(self) -> &'static str {
        match self {
            Power::Awake => "wake",
            Power::Asleep => "sleep",
        }
    }
}

/// A change the GNSS task must act on, printed as
/// `GNSS_POWER <this> t=<ms>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerEdge {
    pub power: Power,
    /// The motion state that decided it: `still` for every sleep,
    /// `moving` for every wake.
    pub reason: Motion,
}

impl fmt::Display for PowerEdge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "state={} reason={}",
            self.power.as_str(),
            self.reason.as_str()
        )
    }
}

/// The sleep/wake decision. Feed it motion edges, the first fix and time;
/// it returns an edge only when the wanted power changes.
#[derive(Clone, Debug)]
pub struct PowerGate {
    boot_ms: u64,
    fixed: bool,
    motion: Option<Motion>,
    power: Power,
}

impl PowerGate {
    /// Awake, with no motion value and the grace running from `now_ms`.
    pub fn new(now_ms: u64) -> Self {
        Self {
            boot_ms: now_ms,
            fixed: false,
            motion: None,
            power: Power::Awake,
        }
    }

    /// What the gate currently wants.
    pub fn power(&self) -> Power {
        self.power
    }

    /// The accelerometer's state, on each of its edges.
    pub fn on_motion(&mut self, motion: Motion, now_ms: u64) -> Option<PowerEdge> {
        self.motion = Some(motion);
        self.decide(now_ms)
    }

    /// The receiver has a fix. The first one ends the grace; a later one
    /// changes nothing.
    pub fn on_fix(&mut self, now_ms: u64) -> Option<PowerEdge> {
        self.fixed = true;
        self.decide(now_ms)
    }

    /// Time alone: the grace running out while the board is still.
    pub fn poll(&mut self, now_ms: u64) -> Option<PowerEdge> {
        self.decide(now_ms)
    }

    fn grace_over(&self, now_ms: u64) -> bool {
        self.fixed || now_ms.saturating_sub(self.boot_ms) >= GRACE_MS
    }

    fn decide(&mut self, now_ms: u64) -> Option<PowerEdge> {
        let (want, reason) = match self.motion {
            Some(Motion::Still) if self.grace_over(now_ms) => (Power::Asleep, Motion::Still),
            Some(Motion::Still) | None => (Power::Awake, Motion::Still),
            Some(Motion::Moving) => (Power::Awake, Motion::Moving),
        };
        if want == self.power {
            return None;
        }
        self.power = want;
        Some(PowerEdge {
            power: want,
            reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: u64 = GNSS_FIRST_FIX_GRACE_S * 1_000;

    fn sleep() -> Option<PowerEdge> {
        Some(PowerEdge {
            power: Power::Asleep,
            reason: Motion::Still,
        })
    }

    fn wake() -> Option<PowerEdge> {
        Some(PowerEdge {
            power: Power::Awake,
            reason: Motion::Moving,
        })
    }

    #[test]
    fn grace_is_ten_minutes() {
        assert_eq!(GNSS_FIRST_FIX_GRACE_S, 600);
    }

    #[test]
    fn still_before_the_grace_keeps_awake() {
        let mut g = PowerGate::new(0);
        assert_eq!(g.on_motion(Motion::Still, 30_000), None);
        assert_eq!(g.poll(GRACE - 1), None);
        assert_eq!(g.power(), Power::Awake);
    }

    #[test]
    fn still_through_the_grace_sleeps_when_it_ends() {
        let mut g = PowerGate::new(0);
        assert_eq!(g.on_motion(Motion::Still, 30_000), None);
        assert_eq!(g.poll(GRACE), sleep());
        assert_eq!(g.power(), Power::Asleep);
        // Stated once: the next poll is no edge.
        assert_eq!(g.poll(GRACE + 1_000), None);
    }

    #[test]
    fn still_after_the_grace_sleeps_at_once() {
        let mut g = PowerGate::new(0);
        assert_eq!(g.poll(GRACE + 5_000), None);
        assert_eq!(g.on_motion(Motion::Still, GRACE + 5_000), sleep());
    }

    #[test]
    fn moving_wakes() {
        let mut g = PowerGate::new(0);
        g.on_motion(Motion::Still, GRACE);
        assert_eq!(g.power(), Power::Asleep);
        assert_eq!(g.on_motion(Motion::Moving, GRACE + 60_000), wake());
        assert_eq!(g.power(), Power::Awake);
        // And the next still sleeps again: the grace is a boot affair.
        assert_eq!(g.on_motion(Motion::Still, GRACE + 90_000), sleep());
    }

    #[test]
    fn moving_while_awake_is_no_edge() {
        let mut g = PowerGate::new(0);
        assert_eq!(g.on_motion(Motion::Moving, 1_000), None);
        assert_eq!(g.on_motion(Motion::Still, 31_000), None);
        assert_eq!(g.on_motion(Motion::Moving, 32_000), None);
    }

    #[test]
    fn no_motion_value_never_sleeps() {
        let mut g = PowerGate::new(0);
        assert_eq!(g.on_fix(5_000), None);
        for t in [GRACE, 10 * GRACE, u64::MAX] {
            assert_eq!(g.poll(t), None);
        }
        assert_eq!(g.power(), Power::Awake);
    }

    #[test]
    fn a_fix_ends_the_grace_early() {
        let mut g = PowerGate::new(0);
        assert_eq!(g.on_motion(Motion::Still, 30_000), None);
        assert_eq!(g.on_fix(45_000), sleep());
    }

    #[test]
    fn a_fix_while_moving_lets_the_next_still_sleep() {
        let mut g = PowerGate::new(0);
        g.on_motion(Motion::Moving, 1_000);
        assert_eq!(g.on_fix(20_000), None);
        assert_eq!(g.on_motion(Motion::Still, 50_000), sleep());
    }

    #[test]
    fn the_grace_counts_from_construction() {
        let mut g = PowerGate::new(1_000_000);
        g.on_motion(Motion::Still, 1_030_000);
        assert_eq!(g.poll(1_000_000 + GRACE - 1), None);
        assert_eq!(g.poll(1_000_000 + GRACE), sleep());
    }

    #[test]
    fn the_line_reads_as_documented() {
        let s = sleep().map(|e| e.to_string());
        assert_eq!(s.as_deref(), Some("state=sleep reason=still"));
        let w = wake().map(|e| e.to_string());
        assert_eq!(w.as_deref(), Some("state=wake reason=moving"));
    }
}
