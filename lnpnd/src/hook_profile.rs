//! Where a [`CoreProcessor`](leviculum_std::driver::CoreProcessor) hook
//! spends its time (leviculum order 1009, Codeberg #384, #427).
//!
//! The driver reports a hook that ran past
//! [`PROCESSOR_TICK_BUDGET`] as `CORE_PROCESSOR_OVER_BUDGET` with the total
//! only. miauhaus logged 4520 of them in ten hours on both hooks, and the
//! total does not say which part of the engine paid for it. This splits one
//! hook invocation into its sections and emits one debug line when the
//! invocation alone ran past the budget:
//!
//! ```text
//! LNPND_HOOK_PROFILE hook=on_tick arm=- total_us=5810 ready_us=2 drain_us=0 sync_us=5402 mailbox_us=0 peer_table_us=0 event_us=0 announce_us=0 tick_us=0 links_us=3 peering_us=401 appends=1
//! ```
//!
//! Stable keys, zero for a section that did not run, `arm` the
//! [`NodeEvent`](leviculum_core::NodeEvent) variant an `on_event` call
//! handled. Debug level, so a field node switches it on with the env filter
//! (`RUST_LOG=lnpnd=debug`) and no rebuild.
//!
//! The driver times a whole dispatch, which can hand `on_event` several
//! events under one lock and one budget (`run_event_tap`,
//! `leviculum-std/src/driver/processor.rs`). This line is per invocation,
//! so a dispatch that ran over on many cheap calls shows as an
//! `events=` count above one on the driver's line and no profile line here.

use std::time::Instant;

use leviculum_std::driver::PROCESSOR_TICK_BUDGET;

/// The sections one hook invocation is split into. Disjoint: each
/// microsecond of the invocation is charged to exactly one of them, so they
/// sum to `total_us` up to the cost of reading the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    /// `take_ready`: the role's registration check.
    Ready,
    /// The store queue apart from sync slices: taking finished validations
    /// off the worker, client uploads (packet or resource), classifying a
    /// completed resource.
    Drain,
    /// One slice of a validated peer's inbound batch.
    Sync,
    /// The operator mailbox's router.
    Mailbox,
    /// `on_event`: a propagation announce into the peer table.
    PeerTable,
    /// `on_event`: every other arm of the event match.
    Event,
    /// `on_tick`: the node's own announce.
    Announce,
    /// `on_tick`: `PropagationNode::tick` (expiry and eviction) and the
    /// store's heartbeat line.
    Tick,
    /// `on_tick`: closing idle inbound links.
    Links,
    /// `on_tick`: the outbound sync state machine, rotation and cull.
    Peering,
}

impl Section {
    const ALL: [Section; 10] = [
        Section::Ready,
        Section::Drain,
        Section::Sync,
        Section::Mailbox,
        Section::PeerTable,
        Section::Event,
        Section::Announce,
        Section::Tick,
        Section::Links,
        Section::Peering,
    ];

    fn index(self) -> usize {
        self as usize
    }
}

/// One finished hook invocation, in microseconds per section.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HookTimes {
    pub(crate) total_us: u64,
    pub(crate) section_us: [u64; Section::ALL.len()],
    /// Messages the invocation offered to the store.
    pub(crate) appends: usize,
}

impl HookTimes {
    pub(crate) fn of(&self, section: Section) -> u64 {
        self.section_us[section.index()]
    }
}

/// The clock of one hook invocation.
#[derive(Debug)]
pub(crate) struct HookProfile {
    hook: &'static str,
    arm: &'static str,
    started: Instant,
    lap: Instant,
    times: HookTimes,
}

impl HookProfile {
    pub(crate) fn start(hook: &'static str) -> Self {
        let now = Instant::now();
        Self {
            hook,
            arm: "-",
            started: now,
            lap: now,
            times: HookTimes::default(),
        }
    }

    pub(crate) fn set_arm(&mut self, arm: &'static str) {
        self.arm = arm;
    }

    pub(crate) fn add_appends(&mut self, appends: usize) {
        self.times.appends += appends;
    }

    /// Charge the time since the previous lap to `section`.
    pub(crate) fn lap(&mut self, section: Section) {
        let now = Instant::now();
        self.times.section_us[section.index()] += now.duration_since(self.lap).as_micros() as u64;
        self.lap = now;
    }

    /// Close the invocation: the remainder goes to `section`, the line goes
    /// out when the invocation ran past the budget.
    pub(crate) fn finish(mut self, section: Section) -> HookTimes {
        self.lap(section);
        let total = self.started.elapsed();
        self.times.total_us = total.as_micros() as u64;
        if total > PROCESSOR_TICK_BUDGET {
            let us = |section: Section| self.times.of(section);
            tracing::debug!(
                "LNPND_HOOK_PROFILE hook={} arm={} total_us={} ready_us={} drain_us={} \
                 sync_us={} mailbox_us={} peer_table_us={} event_us={} announce_us={} \
                 tick_us={} links_us={} peering_us={} appends={}",
                self.hook,
                self.arm,
                self.times.total_us,
                us(Section::Ready),
                us(Section::Drain),
                us(Section::Sync),
                us(Section::Mailbox),
                us(Section::PeerTable),
                us(Section::Event),
                us(Section::Announce),
                us(Section::Tick),
                us(Section::Links),
                us(Section::Peering),
                self.times.appends,
            );
        }
        self.times
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_are_disjoint_and_sum_to_the_total() {
        let mut profile = HookProfile::start("on_tick");
        std::thread::sleep(std::time::Duration::from_millis(2));
        profile.lap(Section::Drain);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let times = profile.finish(Section::Peering);
        assert!(times.of(Section::Drain) >= 2000);
        assert!(times.of(Section::Peering) >= 1000);
        assert_eq!(times.of(Section::Tick), 0);
        let sum: u64 = times.section_us.iter().sum();
        assert!(
            times.total_us >= sum && times.total_us - sum < 1000,
            "sections {sum} us against a total of {} us",
            times.total_us
        );
    }
}
