//! What a press of a board's user button means (Codeberg #459).
//!
//! A short press sends the board's own announces, the delivery destination
//! and the propagation node destination, so somebody holding the board can
//! make it findable now instead of waiting out the cadence. A long press is
//! not assigned here: on the T1000-E it is the power button, and on the
//! WisMesh Pocket V2 it is the display's own (`leviculum_screen::power`).
//!
//! Two gates stand between a contact and an announce, and both are here
//! because both are timing that a thumb on a board cannot reproduce:
//!
//! * [`Debouncer`]: a contact chatters for a few milliseconds on each edge,
//!   and every chatter is a GPIOTE edge. A level counts once it has held
//!   for [`BUTTON_DEBOUNCE_MS`] with no further edge.
//! * [`PressLimiter`]: a press announces at most once every
//!   [`BUTTON_ANNOUNCE_MIN_INTERVAL_MS`]; one inside the window is reported
//!   and dropped, never queued.
//!
//! [`ButtonGate`] is the two in the order the board runs them, and the
//! board does nothing with a press that this module did not decide.

/// How long a level must hold with no further edge before it counts.
///
/// Mechanical tact switches of the kind on these boards bounce for a few
/// milliseconds, rarely past 10 ms, and a press a person means lasts well
/// over 100 ms. Fifty sits between the two with margin on both sides: long
/// enough that the worst bounce is absorbed, short enough that no deliberate
/// press is mistaken for one. One constant for every board, because the
/// switches are the same kind and a per-board value would be a guess per
/// board.
pub const BUTTON_DEBOUNCE_MS: u64 = 50;

/// At most one press-triggered announce in this window (Lew, 2026-10-20).
///
/// The press is a person asking to be found, and the first announce answers
/// it; a second within half a minute reaches the same neighbours with the
/// same content. A thumb repeating the press, or a button pressed in a
/// pocket, would otherwise put one announce set on the air per press. The
/// window is fixed from the last announcing press, so a press inside it
/// does not extend it.
pub const BUTTON_ANNOUNCE_MIN_INTERVAL_MS: u64 = 30 * 1_000;

/// The shortest press that is a long one, and therefore not an announce.
///
/// The same two seconds the Pocket V2's display turns on at
/// (`leviculum_screen::power::LONG_PRESS_MIN_MS`, asserted equal in
/// `leviculum-nrf/src/button.rs`), so that on that board one press means one
/// thing: below it a press announces, at or above it the display wakes and
/// nothing is announced.
pub const LONG_PRESS_MIN_MS: u64 = 2_000;

/// A level the debouncer has accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// The button went down and stayed down.
    Down,
    /// The button came up after being down for `held_ms`, measured from the
    /// first edge of the press to the first edge of the release.
    Up {
        /// How long the button was held.
        held_ms: u64,
    },
    /// Edges arrived on a released button and it settled released again:
    /// a glitch, not a press.
    Bounce,
}

/// Accepts a level once it has held for [`BUTTON_DEBOUNCE_MS`] with no
/// further edge.
///
/// Fed by the GPIO task: [`edge`](Self::edge) on every edge it wakes for,
/// [`settle`](Self::settle) when [`deadline_ms`](Self::deadline_ms) has
/// passed. `pressed` is always the pin as read at that moment, so an edge
/// pair too fast for the task to see both halves of still restarts the
/// quiet period, and a level that changed while nothing woke is caught at
/// the settle.
#[derive(Debug, Clone, Copy, Default)]
pub struct Debouncer {
    /// The level as last read.
    raw: bool,
    /// The level last accepted.
    stable: bool,
    /// When the quiet period ends, `None` while nothing is pending.
    quiet_until_ms: Option<u64>,
    /// The first edge of the pending burst: the moment a press or a
    /// release actually began.
    burst_start_ms: u64,
    /// When the accepted press began.
    pressed_at_ms: u64,
}

impl Debouncer {
    /// A released button with nothing pending.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            raw: false,
            stable: false,
            quiet_until_ms: None,
            burst_start_ms: 0,
            pressed_at_ms: 0,
        }
    }

    /// An edge woke the task and the pin now reads `pressed`: restart the
    /// quiet period.
    pub fn edge(&mut self, now_ms: u64, pressed: bool) {
        if self.quiet_until_ms.is_none() {
            self.burst_start_ms = now_ms;
        }
        self.raw = pressed;
        self.quiet_until_ms = Some(now_ms.saturating_add(BUTTON_DEBOUNCE_MS));
    }

    /// When [`settle`](Self::settle) is next worth calling, `None` when
    /// nothing is pending and only an edge can change anything.
    #[must_use]
    pub const fn deadline_ms(&self) -> Option<u64> {
        self.quiet_until_ms
    }

    /// The quiet period may have ended and the pin reads `pressed`.
    ///
    /// `None` while it has not ended, and when the burst left the accepted
    /// level where it was on a held button (contact chatter while held is
    /// not an event).
    pub fn settle(&mut self, now_ms: u64, pressed: bool) -> Option<Settled> {
        let until = self.quiet_until_ms?;
        if pressed != self.raw {
            // A change no edge woke us for: it is an edge, and it restarts
            // the quiet period like one.
            self.edge(now_ms, pressed);
            return None;
        }
        if now_ms < until {
            return None;
        }
        self.quiet_until_ms = None;
        if self.raw == self.stable {
            return (!self.stable).then_some(Settled::Bounce);
        }
        self.stable = self.raw;
        if self.stable {
            self.pressed_at_ms = self.burst_start_ms;
            Some(Settled::Down)
        } else {
            Some(Settled::Up {
                held_ms: self.burst_start_ms.saturating_sub(self.pressed_at_ms),
            })
        }
    }
}

/// One announcing press per [`BUTTON_ANNOUNCE_MIN_INTERVAL_MS`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PressLimiter {
    last_announce_ms: Option<u64>,
}

impl PressLimiter {
    /// No press yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_announce_ms: None,
        }
    }

    /// A short press at `now_ms`: `true` when it announces, which also
    /// starts the window; `false` inside the window, which leaves it alone.
    pub fn press(&mut self, now_ms: u64) -> bool {
        if let Some(last) = self.last_announce_ms {
            if now_ms < last.saturating_add(BUTTON_ANNOUNCE_MIN_INTERVAL_MS) {
                return false;
            }
        }
        self.last_announce_ms = Some(now_ms);
        true
    }
}

/// What a finished press does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressAction {
    /// A short press outside the window: send the announces.
    Announce,
    /// A short press inside the window: report it, send nothing.
    RateLimited,
    /// Edges that settled released: not a press.
    Debounced,
    /// A press of at least [`LONG_PRESS_MIN_MS`]: no announce. Left to
    /// whatever else the board does with a long press.
    Long,
}

impl PressAction {
    /// The `action=` token of the board's `BTN_PRESS` line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Announce => "announce",
            Self::RateLimited => "rate-limited",
            Self::Debounced => "debounced",
            Self::Long => "long",
        }
    }
}

/// A press the gate has classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    /// What the board does with it.
    pub action: PressAction,
    /// How long the button was held, `0` for [`PressAction::Debounced`].
    pub held_ms: u64,
}

/// The debouncer and the rate limit, in the order the board runs them.
#[derive(Debug, Clone, Copy, Default)]
pub struct ButtonGate {
    debounce: Debouncer,
    limit: PressLimiter,
}

impl ButtonGate {
    /// A released button, no press yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            debounce: Debouncer::new(),
            limit: PressLimiter::new(),
        }
    }

    /// See [`Debouncer::edge`].
    pub fn edge(&mut self, now_ms: u64, pressed: bool) {
        self.debounce.edge(now_ms, pressed);
    }

    /// See [`Debouncer::deadline_ms`].
    #[must_use]
    pub const fn deadline_ms(&self) -> Option<u64> {
        self.debounce.deadline_ms()
    }

    /// See [`Debouncer::settle`]; a press is classified at its release,
    /// the only moment its length is known.
    pub fn settle(&mut self, now_ms: u64, pressed: bool) -> Option<Press> {
        match self.debounce.settle(now_ms, pressed)? {
            Settled::Down => None,
            Settled::Bounce => Some(Press {
                action: PressAction::Debounced,
                held_ms: 0,
            }),
            Settled::Up { held_ms } => {
                let action = if held_ms >= LONG_PRESS_MIN_MS {
                    PressAction::Long
                } else if self.limit.press(now_ms) {
                    PressAction::Announce
                } else {
                    PressAction::RateLimited
                };
                Some(Press { action, held_ms })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the gate the way the GPIO task does: each `(t, level)` is an
    /// edge, and between edges the task settles at every deadline that
    /// falls before the next one. Returns every press, with the moment it
    /// was classified.
    fn run(gate: &mut ButtonGate, edges: &[(u64, bool)], until_ms: u64) -> Vec<(u64, Press)> {
        let mut out = Vec::new();
        let mut level = false;
        let mut settle_until = |gate: &mut ButtonGate, level: bool, before: u64| {
            while let Some(d) = gate.deadline_ms() {
                if d >= before {
                    break;
                }
                if let Some(p) = gate.settle(d, level) {
                    out.push((d, p));
                }
            }
        };
        for &(t, pressed) in edges {
            settle_until(gate, level, t);
            level = pressed;
            gate.edge(t, pressed);
        }
        settle_until(gate, level, until_ms);
        out
    }

    /// A clean press of `held` ms starting at `t`, bouncing three times on
    /// each edge within 6 ms.
    fn bouncy_press(t: u64, held: u64) -> Vec<(u64, bool)> {
        let up = t + held;
        vec![
            (t, true),
            (t + 2, false),
            (t + 4, true),
            (t + 6, false),
            (t + 7, true),
            (up, false),
            (up + 1, true),
            (up + 3, false),
            (up + 5, true),
            (up + 6, false),
        ]
    }

    #[test]
    fn bounce_inside_the_window_is_one_press() {
        let mut gate = ButtonGate::new();
        let presses = run(&mut gate, &bouncy_press(1_000, 200), 10_000);
        assert_eq!(
            presses,
            vec![(
                1_200 + 6 + BUTTON_DEBOUNCE_MS,
                Press {
                    action: PressAction::Announce,
                    held_ms: 200,
                }
            )],
            "ten edges are one press, classified once the release is quiet, \
             timed from the first edge of each burst"
        );
    }

    #[test]
    fn a_glitch_shorter_than_the_window_is_debounced_not_a_press() {
        let mut gate = ButtonGate::new();
        let presses = run(&mut gate, &[(500, true), (520, false)], 10_000);
        assert_eq!(
            presses,
            vec![(
                520 + BUTTON_DEBOUNCE_MS,
                Press {
                    action: PressAction::Debounced,
                    held_ms: 0,
                }
            )]
        );
        // And it did not spend the rate-limit window.
        let presses = run(&mut gate, &bouncy_press(1_000, 150), 10_000);
        assert_eq!(presses.len(), 1);
        assert_eq!(presses[0].1.action, PressAction::Announce);
    }

    #[test]
    fn two_presses_inside_the_window_announce_once_and_the_third_after_it_again() {
        let mut gate = ButtonGate::new();
        let mut edges = bouncy_press(1_000, 200);
        edges.extend(bouncy_press(11_000, 200));
        edges.extend(bouncy_press(
            1_000 + BUTTON_ANNOUNCE_MIN_INTERVAL_MS + 1_000,
            200,
        ));
        let actions: Vec<PressAction> = run(&mut gate, &edges, 100_000)
            .into_iter()
            .map(|(_, p)| p.action)
            .collect();
        assert_eq!(
            actions,
            vec![
                PressAction::Announce,
                PressAction::RateLimited,
                PressAction::Announce,
            ]
        );
    }

    #[test]
    fn a_dropped_press_does_not_extend_the_window() {
        let mut limit = PressLimiter::new();
        assert!(limit.press(0));
        assert!(!limit.press(BUTTON_ANNOUNCE_MIN_INTERVAL_MS - 1));
        assert!(limit.press(BUTTON_ANNOUNCE_MIN_INTERVAL_MS));
    }

    #[test]
    fn a_long_press_announces_nothing_and_keeps_the_window_free() {
        let mut gate = ButtonGate::new();
        let mut edges = bouncy_press(1_000, LONG_PRESS_MIN_MS);
        edges.extend(bouncy_press(5_000, 100));
        let presses = run(&mut gate, &edges, 100_000);
        assert_eq!(
            presses.iter().map(|(_, p)| p.action).collect::<Vec<_>>(),
            vec![PressAction::Long, PressAction::Announce]
        );
        assert_eq!(presses[0].1.held_ms, LONG_PRESS_MIN_MS);
        let mut gate = ButtonGate::new();
        let presses = run(
            &mut gate,
            &bouncy_press(1_000, LONG_PRESS_MIN_MS - 1),
            100_000,
        );
        assert_eq!(presses[0].1.action, PressAction::Announce);
    }

    #[test]
    fn chatter_while_held_is_not_an_event() {
        let mut gate = ButtonGate::new();
        let presses = run(
            &mut gate,
            &[(0, true), (300, false), (310, true), (900, false)],
            10_000,
        );
        assert_eq!(
            presses,
            vec![(
                900 + BUTTON_DEBOUNCE_MS,
                Press {
                    action: PressAction::Announce,
                    held_ms: 900,
                }
            )]
        );
    }

    #[test]
    fn a_change_no_edge_reported_restarts_the_quiet_period() {
        let mut d = Debouncer::new();
        d.edge(0, true);
        // At the deadline the pin reads released: the press half was never
        // quiet, so nothing is accepted and the wait starts again.
        assert_eq!(d.settle(BUTTON_DEBOUNCE_MS, false), None);
        assert_eq!(d.deadline_ms(), Some(2 * BUTTON_DEBOUNCE_MS));
        assert_eq!(
            d.settle(2 * BUTTON_DEBOUNCE_MS, false),
            Some(Settled::Bounce)
        );
        assert_eq!(d.deadline_ms(), None);
    }

    #[test]
    fn nothing_settles_before_the_quiet_period_ends() {
        let mut d = Debouncer::new();
        d.edge(100, true);
        assert_eq!(d.settle(100 + BUTTON_DEBOUNCE_MS - 1, true), None);
        assert_eq!(
            d.settle(100 + BUTTON_DEBOUNCE_MS, true),
            Some(Settled::Down)
        );
    }

    #[test]
    fn the_action_tokens_are_stable() {
        assert_eq!(PressAction::Announce.as_str(), "announce");
        assert_eq!(PressAction::RateLimited.as_str(), "rate-limited");
        assert_eq!(PressAction::Debounced.as_str(), "debounced");
        assert_eq!(PressAction::Long.as_str(), "long");
    }
}
