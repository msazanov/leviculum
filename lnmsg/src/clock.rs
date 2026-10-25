//! The time the send and fetch runners measure their budgets on.
//!
//! A runner needs two things from a clock: how long it has been running, to
//! hold its budget, and a way to wait one poll interval when the event queue
//! is empty. [`Clock`] is exactly those two and nothing more: a monotonic
//! reading in milliseconds and an async sleep for a number of milliseconds.
//! No `sleep_until`: both runners sleep a fixed poll and re-read the clock,
//! so a deadline-shaped call would only move the same subtraction into every
//! host (`docs/src/concepts/lnmsg-architecture.md`, "Plan: the lnmsg core
//! without std", pass 3).
//!
//! The terminal's clock is tokio's ([`Tokio`]), so a test that pauses the
//! runtime clock still drives a thirty-second budget through in
//! milliseconds and exercises the real expiry path. A board brings its own
//! timer.

use core::future::Future;
use core::time::Duration;

/// A monotonic clock and a sleep, both in milliseconds.
pub trait Clock {
    /// Milliseconds since an origin of the clock's choosing. Only differences
    /// are meaningful; the reading never goes backwards.
    fn now_ms(&self) -> u64;
    /// Wait `ms` milliseconds.
    fn sleep_ms(&self, ms: u64) -> impl Future<Output = ()>;
}

/// How long ago `start_ms` (an earlier [`Clock::now_ms`]) was.
pub(crate) fn since(clock: &impl Clock, start_ms: u64) -> Duration {
    Duration::from_millis(clock.now_ms().saturating_sub(start_ms))
}

/// Sleep one poll interval on `clock`.
pub(crate) async fn sleep(clock: &impl Clock, poll: Duration) {
    clock
        .sleep_ms(u64::try_from(poll.as_millis()).unwrap_or(u64::MAX))
        .await;
}

/// The terminal's clock: tokio's, so `tokio::time::pause` drives it.
#[cfg(feature = "std")]
pub struct Tokio {
    origin: tokio::time::Instant,
}

#[cfg(feature = "std")]
impl Tokio {
    /// A clock whose origin is now.
    pub fn new() -> Self {
        Self {
            origin: tokio::time::Instant::now(),
        }
    }
}

#[cfg(feature = "std")]
impl Default for Tokio {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "std")]
impl Clock for Tokio {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn sleep_ms(&self, ms: u64) -> impl Future<Output = ()> {
        tokio::time::sleep(Duration::from_millis(ms))
    }
}

/// A clock that moves only when the runner sleeps, for the runners' tests.
#[cfg(test)]
pub(crate) mod fake {
    use core::cell::Cell;
    use core::future::Future;

    use super::Clock;

    /// Each sleep advances the reading by exactly what was asked and returns
    /// at once, so a budget expires after budget/poll iterations and no
    /// runtime timer is involved.
    ///
    /// The step happens when the sleep is first polled, not when it is made:
    /// a sleep raced against other work and never polled because the work
    /// won is a timer that never fired, and must not move the clock.
    #[derive(Default)]
    pub struct Stepping {
        now: Cell<u64>,
        pub sleeps: Cell<u32>,
    }

    impl Clock for Stepping {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }

        fn sleep_ms(&self, ms: u64) -> impl Future<Output = ()> {
            core::future::poll_fn(move |_| {
                self.now.set(self.now.get().saturating_add(ms));
                self.sleeps.set(self.sleeps.get() + 1);
                core::task::Poll::Ready(())
            })
        }
    }
}
