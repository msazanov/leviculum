//! When the mailbox is drained without being asked: the schedule of
//! `docs/src/concepts/loquax-mailbox.md`, section 1, "When to sync".
//!
//! A pure function of events and a monotonic clock in milliseconds, with no
//! timer of its own: the shell reads its clock, hands the reading over, and
//! asks whether a sync is due ([`Schedule::due`]). The schedule never runs a
//! sync either; it is told when one starts ([`Schedule::begin`]) and how it
//! ended ([`SyncEvent::Done`], [`SyncEvent::Failed`]), so one sync at a time
//! is the caller's guard and the schedule's assumption.
//!
//! # The rule
//!
//! - Start, resume and the manual key make a sync due at once; so does an
//!   announce of the selected node, unless a sync began within
//!   [`ANNOUNCE_GAP_MS`], so a chatty node cannot drive syncs.
//! - The configured interval, the bound (default [`DEFAULT_BOUND_MS`]), is
//!   never exceeded: the next sync begins no later than the bound after the
//!   last one began, whatever else happened. A sync running longer than the
//!   bound is the only way past it, and the next one is due when it ends.
//! - Activity (a sync that returned something, or an outbound send) syncs
//!   every [`FAST_MS`] for [`ACTIVITY_WINDOW_MS`] after it, then the bound
//!   again. Below the default bound both shrink with it, at the same ratio:
//!   a burst is always five syncs, whatever the bound costs.
//! - An empty sync changes nothing: the interval is never lengthened.
//! - A failed sync retries after [`RETRY_FIRST_MS`], doubling with each
//!   failure in a row, and never later than the bound.
//! - The timer off (a bound of `None`) leaves start, resume and the key;
//!   the announce trigger, the activity cadence and the retries are timer
//!   syncs and go with it.

use alloc::string::String;

/// The default bound, fifteen minutes: the decision of 2026-08-10.
pub const DEFAULT_BOUND_MS: u64 = 15 * 60_000;
/// The activity cadence at the default bound and above.
pub const FAST_MS: u64 = 2 * 60_000;
/// How long the activity cadence lasts after the activity, at the default
/// bound and above.
pub const ACTIVITY_WINDOW_MS: u64 = 10 * 60_000;
/// The least time between the start of a sync and an announce of the
/// selected node that starts another.
pub const ANNOUNCE_GAP_MS: u64 = 60_000;
/// The first retry after a failed sync; each failure in a row doubles it.
pub const RETRY_FIRST_MS: u64 = 30_000;
/// The activity cadence's floor, for a bound of a few seconds.
const FAST_FLOOR_MS: u64 = 1_000;
/// A wall clock that ran this much further than the monotonic one between
/// two readings slept in between ([`resumed`]).
pub const RESUME_GAP_MS: u64 = 30_000;

/// What the schedule is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncEvent {
    /// The messenger is up and its engine ready.
    Start,
    /// The machine woke from suspend ([`resumed`]).
    Resume,
    /// The reader asked.
    Key,
    /// The selected mailbox node announced.
    Announce,
    /// The running sync finished; `new` messages were new.
    Done { new: usize },
    /// The running sync failed.
    Failed,
    /// A message went out.
    Sent,
}

/// Why a sync is due, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Start,
    Resume,
    Key,
    Announce,
    /// The bound, or the activity cadence, or a retry.
    Timer,
}

impl Reason {
    pub fn word(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Resume => "resume",
            Self::Key => "key",
            Self::Announce => "announce",
            Self::Timer => "timer",
        }
    }
}

/// The schedule's state between events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    bound: Option<u64>,
    /// A sync an event asked for, not yet begun.
    owed: Option<Reason>,
    running: bool,
    /// When the last sync began.
    last_start: Option<u64>,
    /// When the last sync ended.
    last_end: Option<u64>,
    /// The last activity.
    activity: Option<u64>,
    /// Failures in a row.
    failures: u32,
}

impl Schedule {
    /// A schedule with this bound in milliseconds; `None` is the timer off.
    pub fn new(bound_ms: Option<u64>) -> Self {
        Self {
            bound: bound_ms.filter(|ms| *ms > 0),
            owed: None,
            running: false,
            last_start: None,
            last_end: None,
            activity: None,
            failures: 0,
        }
    }

    /// The bound, `None` with the timer off.
    pub fn bound_ms(&self) -> Option<u64> {
        self.bound
    }

    /// The activity cadence: [`FAST_MS`], or the bound's share of it below
    /// the default bound.
    pub fn fast_ms(&self) -> Option<u64> {
        self.bound
            .map(|b| scaled(b, FAST_MS).max(FAST_FLOOR_MS).min(b))
    }

    /// How long the activity cadence lasts.
    pub fn window_ms(&self) -> Option<u64> {
        self.bound.map(|b| scaled(b, ACTIVITY_WINDOW_MS))
    }

    /// Whether a sync is running.
    pub fn running(&self) -> bool {
        self.running
    }

    /// Failed syncs in a row.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Fold one event in, at `now_ms`.
    pub fn on(&mut self, event: SyncEvent, now_ms: u64) {
        match event {
            SyncEvent::Start => self.owe(Reason::Start),
            SyncEvent::Resume => self.owe(Reason::Resume),
            SyncEvent::Key => self.owe(Reason::Key),
            SyncEvent::Announce => {
                let recent = self
                    .last_start
                    .is_some_and(|at| now_ms.saturating_sub(at) < ANNOUNCE_GAP_MS);
                if self.bound.is_some() && !self.running && !recent {
                    self.owe(Reason::Announce);
                }
            }
            SyncEvent::Done { new } => {
                self.end(now_ms);
                self.failures = 0;
                if new > 0 {
                    self.activity = Some(now_ms);
                }
            }
            SyncEvent::Failed => {
                self.end(now_ms);
                self.failures = self.failures.saturating_add(1);
            }
            SyncEvent::Sent => self.activity = Some(now_ms),
        }
    }

    fn owe(&mut self, reason: Reason) {
        if self.owed.is_none() {
            self.owed = Some(reason);
        }
    }

    fn end(&mut self, now_ms: u64) {
        self.running = false;
        self.last_end = Some(now_ms);
    }

    /// A sync is due at `now_ms`, and why; `None` when none is, or one is
    /// running.
    pub fn due(&self, now_ms: u64) -> Option<Reason> {
        if self.running {
            return None;
        }
        if let Some(reason) = self.owed {
            return Some(reason);
        }
        self.next_ms()
            .filter(|next| *next <= now_ms)
            .map(|_| Reason::Timer)
    }

    /// A sync begins at `now_ms`.
    pub fn begin(&mut self, now_ms: u64) {
        self.running = true;
        self.owed = None;
        self.last_start = Some(now_ms);
    }

    /// When the timer's next sync is due: `None` with the timer off, before
    /// the first sync, or while one runs. An owed sync is due now, and the
    /// caller asks [`Schedule::due`] for that.
    pub fn next_ms(&self) -> Option<u64> {
        if self.running {
            return None;
        }
        let bound = self.bound?;
        let last = self.last_start?;
        let mut next = last.saturating_add(bound);
        if let (Some(at), Some(fast), Some(window)) =
            (self.activity, self.fast_ms(), self.window_ms())
        {
            let fast_next = last.max(at).saturating_add(fast);
            if fast_next <= at.saturating_add(window) {
                next = next.min(fast_next);
            }
        }
        if self.failures > 0 {
            let end = self.last_end.unwrap_or(last);
            next = next.min(end.saturating_add(self.retry_ms(bound)));
        }
        Some(next)
    }

    /// The wait after the latest failure in a row, never more than `bound`.
    fn retry_ms(&self, bound: u64) -> u64 {
        let doublings = self.failures.saturating_sub(1).min(32);
        RETRY_FIRST_MS.saturating_mul(1u64 << doublings).min(bound)
    }
}

/// `value` at the default bound, shrunk by `bound / DEFAULT_BOUND_MS` below
/// it.
fn scaled(bound: u64, value: u64) -> u64 {
    if bound >= DEFAULT_BOUND_MS {
        return value;
    }
    let shrunk = u128::from(value) * u128::from(bound) / u128::from(DEFAULT_BOUND_MS);
    u64::try_from(shrunk).unwrap_or(value)
}

/// Whether the machine slept between two readings of the wall clock (in
/// seconds) and the monotonic one (in milliseconds, which stands still in
/// suspend): the wall clock ran [`RESUME_GAP_MS`] or more ahead of it.
pub fn resumed(before: (i64, u64), after: (i64, u64)) -> bool {
    let wall_ms = after.0.saturating_sub(before.0).saturating_mul(1000);
    let mono_ms = i64::try_from(after.1.saturating_sub(before.1)).unwrap_or(i64::MAX);
    wall_ms.saturating_sub(mono_ms) >= RESUME_GAP_MS as i64
}

/// A sync interval as the config file and `--sync-interval` say it: `off` or
/// `0` for the timer off, else a number with `s`, `m` or `h`, minutes
/// without one. In milliseconds.
pub fn parse_interval(text: &str) -> Result<Option<u64>, String> {
    let text = text.trim();
    if text == "off" {
        return Ok(None);
    }
    let (digits, unit) = match text.char_indices().last() {
        Some((at, 's')) => (&text[..at], 1_000),
        Some((at, 'm')) => (&text[..at], 60_000),
        Some((at, 'h')) => (&text[..at], 3_600_000),
        _ => (text, 60_000),
    };
    let count: u64 = digits
        .trim()
        .parse()
        .map_err(|_| alloc::format!("{text:?} is not off or a number of s, m or h"))?;
    match count.checked_mul(unit) {
        Some(0) => Ok(None),
        Some(ms) => Ok(Some(ms)),
        None => Err(alloc::format!("{text:?} is too long")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;

    /// Run a schedule to `until`, a sync at every due instant ending
    /// `duration` later with `new` messages; the starts.
    fn run(schedule: &mut Schedule, from: u64, until: u64, step: u64, new: usize) -> Vec<u64> {
        let mut starts = Vec::new();
        let mut now = from;
        while now <= until {
            if schedule.due(now).is_some() {
                schedule.begin(now);
                starts.push(now);
                schedule.on(SyncEvent::Done { new }, now + 1_000);
            }
            now += step;
        }
        starts
    }

    #[test]
    fn start_syncs_at_once() {
        let mut s = Schedule::new(Some(DEFAULT_BOUND_MS));
        assert_eq!(s.due(0), None, "nothing before the start");
        s.on(SyncEvent::Start, 5);
        assert_eq!(s.due(5), Some(Reason::Start));
        s.begin(5);
        assert_eq!(s.due(5), None, "one sync at a time");
        s.on(SyncEvent::Done { new: 0 }, 2_000);
        assert_eq!(s.next_ms(), Some(5 + DEFAULT_BOUND_MS));
    }

    /// The promise: over a long empty run the gap between two starts is
    /// the bound, never more, never stretched by the emptiness.
    #[test]
    fn the_bound_is_never_exceeded_across_a_long_empty_run() {
        for bound in [30_000, 5 * MIN, DEFAULT_BOUND_MS, 60 * MIN] {
            let mut s = Schedule::new(Some(bound));
            s.on(SyncEvent::Start, 0);
            let starts = run(&mut s, 0, 24 * 60 * MIN, 250, 0);
            assert!(starts.len() > 20, "{bound}: {}", starts.len());
            for pair in starts.windows(2) {
                let gap = pair[1] - pair[0];
                assert!(gap <= bound, "{bound}: a gap of {gap}");
                assert!(
                    gap + 250 > bound,
                    "{bound}: empty syncs shortened it: {gap}"
                );
            }
        }
    }

    /// The same promise under every event the schedule knows, at random.
    #[test]
    fn no_mix_of_events_stretches_the_bound() {
        let bound = DEFAULT_BOUND_MS;
        let mut s = Schedule::new(Some(bound));
        s.on(SyncEvent::Start, 0);
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut last_start = None;
        let mut ends_at = None;
        let mut now = 0;
        while now < 48 * 60 * MIN {
            if let Some(end) = ends_at {
                if now >= end {
                    ends_at = None;
                    let event = match next() % 3 {
                        0 => SyncEvent::Failed,
                        1 => SyncEvent::Done { new: 0 },
                        _ => SyncEvent::Done { new: 2 },
                    };
                    s.on(event, now);
                }
            }
            match next() % 2_000 {
                0 => s.on(SyncEvent::Sent, now),
                1 => s.on(SyncEvent::Announce, now),
                _ => {}
            }
            if s.due(now).is_some() {
                if let Some(last) = last_start {
                    assert!(now - last <= bound + 60_000, "a gap of {}", now - last);
                }
                s.begin(now);
                last_start = Some(now);
                // A sync of up to a minute.
                ends_at = Some(now + next() % 60_000);
            }
            if ends_at.is_none() {
                if let (Some(last), Some(due)) = (last_start, s.next_ms()) {
                    assert!(
                        due <= last + bound,
                        "due {} after the last start",
                        due - last
                    );
                }
            }
            now += 1_000;
        }
    }

    #[test]
    fn activity_shortens_the_interval_and_it_decays_back() {
        let mut s = Schedule::new(Some(DEFAULT_BOUND_MS));
        s.on(SyncEvent::Start, 0);
        s.begin(0);
        s.on(SyncEvent::Done { new: 1 }, 1_000);
        let starts = run(&mut s, 1_000, 40 * MIN, 1_000, 0);
        let gaps: Vec<u64> = core::iter::once(starts[0])
            .chain(starts.windows(2).map(|p| p[1] - p[0]))
            .collect();
        // Two minutes after the activity, then every two minutes inside the
        // ten, then the bound.
        assert_eq!(starts[0], 2 * MIN + 1_000, "{starts:?}");
        assert!(gaps[1..5].iter().all(|g| *g == 2 * MIN), "{gaps:?}");
        assert!(gaps[5..].iter().all(|g| *g == DEFAULT_BOUND_MS), "{gaps:?}");
    }

    #[test]
    fn a_send_is_activity_and_never_pushes_past_the_bound() {
        let mut s = Schedule::new(Some(DEFAULT_BOUND_MS));
        s.on(SyncEvent::Start, 0);
        s.begin(0);
        s.on(SyncEvent::Done { new: 0 }, 1_000);
        s.on(SyncEvent::Sent, 5 * MIN);
        assert_eq!(s.next_ms(), Some(7 * MIN));
        // A send a minute before the bound keeps the bound.
        let mut s = Schedule::new(Some(DEFAULT_BOUND_MS));
        s.on(SyncEvent::Start, 0);
        s.begin(0);
        s.on(SyncEvent::Done { new: 0 }, 1_000);
        s.on(SyncEvent::Sent, 14 * MIN);
        assert_eq!(s.next_ms(), Some(DEFAULT_BOUND_MS));
    }

    #[test]
    fn the_activity_cadence_shrinks_with_a_short_bound() {
        let s = Schedule::new(Some(30_000));
        assert_eq!(s.fast_ms(), Some(4_000));
        assert_eq!(s.window_ms(), Some(20_000));
        let s = Schedule::new(Some(60 * MIN));
        assert_eq!(s.fast_ms(), Some(FAST_MS));
        assert_eq!(s.window_ms(), Some(ACTIVITY_WINDOW_MS));
        let s = Schedule::new(Some(2_000));
        assert_eq!(s.fast_ms(), Some(1_000));
    }

    #[test]
    fn a_failure_backs_off_but_not_past_the_bound() {
        let bound = 5 * MIN;
        let mut s = Schedule::new(Some(bound));
        s.on(SyncEvent::Start, 0);
        let mut now = 0;
        let mut waits = Vec::new();
        for _ in 0..8 {
            s.begin(now);
            now += 1_000;
            s.on(SyncEvent::Failed, now);
            let next = s.next_ms().expect("a retry");
            waits.push(next - now);
            now = next;
        }
        assert_eq!(&waits[..4], &[30_000, 60_000, 120_000, 240_000]);
        // The bound from the start, a second before the failure.
        assert!(waits[4..].iter().all(|w| *w == bound - 1_000), "{waits:?}");
        s.begin(now);
        s.on(SyncEvent::Done { new: 0 }, now + 1_000);
        assert_eq!(s.next_ms(), Some(now + bound), "a success ends the backoff");
    }

    #[test]
    fn a_resume_syncs_at_once() {
        let mut s = Schedule::new(Some(DEFAULT_BOUND_MS));
        s.on(SyncEvent::Start, 0);
        s.begin(0);
        s.on(SyncEvent::Done { new: 0 }, 1_000);
        assert_eq!(s.due(2_000), None);
        s.on(SyncEvent::Resume, 2_000);
        assert_eq!(s.due(2_000), Some(Reason::Resume));
    }

    #[test]
    fn an_announce_syncs_once_within_the_gap() {
        let mut s = Schedule::new(Some(DEFAULT_BOUND_MS));
        s.on(SyncEvent::Start, 0);
        s.begin(0);
        s.on(SyncEvent::Done { new: 0 }, 1_000);
        s.on(SyncEvent::Announce, 30_000);
        assert_eq!(s.due(30_000), None, "inside the gap after the start");
        s.on(SyncEvent::Announce, ANNOUNCE_GAP_MS);
        assert_eq!(s.due(ANNOUNCE_GAP_MS), Some(Reason::Announce));
        s.begin(ANNOUNCE_GAP_MS);
        s.on(SyncEvent::Announce, ANNOUNCE_GAP_MS + 500);
        s.on(SyncEvent::Done { new: 0 }, ANNOUNCE_GAP_MS + 1_000);
        for at in [ANNOUNCE_GAP_MS + 2_000, ANNOUNCE_GAP_MS + 59_000] {
            s.on(SyncEvent::Announce, at);
            assert_eq!(s.due(at), None, "a second announce at {at}");
        }
    }

    #[test]
    fn a_disabled_timer_still_syncs_on_start_and_key() {
        let mut s = Schedule::new(None);
        s.on(SyncEvent::Start, 0);
        assert_eq!(s.due(0), Some(Reason::Start));
        s.begin(0);
        s.on(SyncEvent::Done { new: 3 }, 1_000);
        assert_eq!(s.next_ms(), None);
        assert_eq!(s.due(24 * 60 * MIN), None, "no timer");
        s.on(SyncEvent::Announce, 10 * MIN);
        assert_eq!(s.due(10 * MIN), None, "the announce trigger is the timer's");
        s.on(SyncEvent::Key, 11 * MIN);
        assert_eq!(s.due(11 * MIN), Some(Reason::Key));
        s.begin(11 * MIN);
        s.on(SyncEvent::Failed, 11 * MIN + 1_000);
        assert_eq!(s.due(60 * MIN), None, "no retry without the timer");
        assert_eq!(Schedule::new(Some(0)).bound_ms(), None);
    }

    #[test]
    fn a_sleep_reads_as_a_resume_and_a_slow_tick_does_not() {
        assert!(!resumed((1_000, 0), (1_001, 1_000)));
        assert!(!resumed((1_000, 0), (1_020, 1_000)), "20 s of drift");
        assert!(resumed((1_000, 0), (1_031, 1_000)));
        assert!(resumed((1_000, 0), (4_600, 250)), "an hour asleep");
        assert!(!resumed((1_000, 0), (900, 250)), "a wall clock set back");
    }

    #[test]
    fn intervals_parse_with_their_units() {
        assert_eq!(parse_interval("15"), Ok(Some(15 * MIN)));
        assert_eq!(parse_interval("15m"), Ok(Some(15 * MIN)));
        assert_eq!(parse_interval("30s"), Ok(Some(30_000)));
        assert_eq!(parse_interval("2h"), Ok(Some(120 * MIN)));
        assert_eq!(parse_interval(" off "), Ok(None));
        assert_eq!(parse_interval("0"), Ok(None));
        assert_eq!(parse_interval("0s"), Ok(None));
        assert!(parse_interval("soon").is_err());
        assert!(parse_interval("-5m").is_err());
        assert!(parse_interval("").is_err());
    }
}
