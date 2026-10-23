//! The shared asynchronous packet pump, independent of the BLE backend.
//!
//! Adapters own fragment storage, enqueue deadlines and the handle-specific
//! completion wait. The pump owns every retry/abort decision, so a timeout can
//! re-offer only an unaccepted fragment, never one already queued.

use crate::{Action, Event, NotifyOutcome, PacketTx};

/// Static I/O boundary for one packet's fragments.
///
/// Reset the connection's drain slot before starting a packet. Neither method
/// may turn an enqueue attempt into an acceptance after its future is dropped.
/// Deadlines and controller-specific resource ownership belong to the adapter;
/// no boxed futures or fragment copies are needed by this interface.
#[allow(async_fn_in_trait)]
pub trait FragmentPort {
    /// Offer fragment `index` once.
    ///
    /// Return [`NotifyOutcome::Sent`] only after successful acceptance. A full
    /// queue or an expired enqueue deadline returns [`NotifyOutcome::QueueFull`]
    /// without accepting the fragment, so the pump may safely re-offer it.
    async fn notify(&mut self, index: usize) -> NotifyOutcome;

    /// Await an actual completion on this connection, bounded by
    /// [`crate::DRAIN_WAIT_MS`].
    ///
    /// Return [`Event::Drained`] only for a matching connection completion, or
    /// [`Event::WaitTimedOut`] on expiry. Memory freed by a different connection
    /// is not a drain edge for this one; the timeout re-offer can use that memory.
    async fn wait_drain(&mut self) -> Event;
}

/// Send one packet through its backend, returning its counters and terminal
/// verdict for adapter-owned logging and disconnect policy.
///
/// Every accepted fragment is offered exactly once. Queue-full retries and
/// drain timeouts spend [`PacketTx`]'s existing finite wait budget. An invalid
/// adapter event returns [`Action::Nothing`] rather than silently retrying.
pub async fn pump_fragments<P: FragmentPort>(
    port: &mut P,
    fragment_count: usize,
) -> (PacketTx, Action) {
    let (mut tx, mut action) = PacketTx::start(fragment_count);
    loop {
        action = match action {
            Action::Send { index } => tx.step(Event::Notify(port.notify(index).await)),
            Action::AwaitDrain { .. } => tx.step(port.wait_drain().await),
            terminal => return (tx, terminal),
        };
    }
}

#[cfg(test)]
mod tests {
    use core::cell::{Cell, RefCell};
    use core::future::{poll_fn, Future};
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};
    use std::vec::Vec;

    use super::*;
    use crate::{abort_disconnects, AbortReason, DrainRouter, DrainSlot, LinkRole, DRAIN_WAIT_MS};

    #[derive(Default)]
    struct Clock(Cell<u64>);

    impl Clock {
        fn advance(&self, duration_ms: u64) {
            self.0.set(self.0.get() + duration_ms);
        }
    }

    struct Credits {
        capacity: usize,
        in_flight: RefCell<Vec<u16>>,
        accepted: RefCell<Vec<(u16, usize)>>,
        attempts: RefCell<Vec<(u16, usize)>>,
    }

    impl Credits {
        fn new(capacity: usize) -> Self {
            Self {
                capacity,
                in_flight: RefCell::new(Vec::new()),
                accepted: RefCell::new(Vec::new()),
                attempts: RefCell::new(Vec::new()),
            }
        }

        fn enqueue(&self, handle: u16, index: usize) -> NotifyOutcome {
            self.attempts.borrow_mut().push((handle, index));
            let mut in_flight = self.in_flight.borrow_mut();
            if in_flight.len() == self.capacity {
                return NotifyOutcome::QueueFull;
            }
            in_flight.push(handle);
            self.accepted.borrow_mut().push((handle, index));
            NotifyOutcome::Sent
        }

        fn complete<const N: usize>(&self, router: &DrainRouter<N>, handle: u16) {
            let mut in_flight = self.in_flight.borrow_mut();
            let index = in_flight.iter().position(|h| *h == handle).unwrap();
            in_flight.remove(index);
            assert!(router.drained(handle));
        }
    }

    struct CreditPort<'a> {
        handle: u16,
        credits: &'a Credits,
        drain: &'a DrainSlot,
        clock: &'a Clock,
    }

    impl FragmentPort for CreditPort<'_> {
        async fn notify(&mut self, index: usize) -> NotifyOutcome {
            self.credits.enqueue(self.handle, index)
        }

        async fn wait_drain(&mut self) -> Event {
            let deadline = self.clock.0.get() + DRAIN_WAIT_MS;
            let mut drain = pin!(self.drain.wait());
            poll_fn(|cx| {
                if drain.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Event::Drained)
                } else if self.clock.0.get() >= deadline {
                    Poll::Ready(Event::WaitTimedOut)
                } else {
                    Poll::Pending
                }
            })
            .await
        }
    }

    fn poll_once<F: Future>(future: core::pin::Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn oom_retries_only_after_a_real_matching_completion() {
        let router: DrainRouter<2> = DrainRouter::new();
        let drain = router.claim(17).unwrap();
        router.claim(23).unwrap();
        let credits = Credits::new(1);
        let clock = Clock::default();
        let mut port = CreditPort {
            handle: 17,
            credits: &credits,
            drain,
            clock: &clock,
        };
        drain.reset();
        let mut pump = pin!(pump_fragments(&mut port, 2));

        assert!(poll_once(pump.as_mut()).is_pending());
        assert_eq!(*credits.accepted.borrow(), [(17, 0)]);
        assert_eq!(*credits.attempts.borrow(), [(17, 0), (17, 1)]);

        assert!(router.drained(23));
        assert!(poll_once(pump.as_mut()).is_pending());
        assert_eq!(*credits.attempts.borrow(), [(17, 0), (17, 1)]);

        credits.complete(&router, 17);
        let Poll::Ready((tx, action)) = poll_once(pump.as_mut()) else {
            panic!("matching completion must re-offer the refused fragment");
        };
        assert_eq!(action, Action::Done);
        assert_eq!(*credits.accepted.borrow(), [(17, 0), (17, 1)]);
        assert_eq!(*credits.attempts.borrow(), [(17, 0), (17, 1), (17, 1)]);
        assert_eq!(tx.fragments_sent(), 2);
        assert_eq!(tx.drain_waits(), 1);
        assert!(!tx.torn());
    }

    #[test]
    fn no_completion_exhausts_a_bounded_budget_and_disconnects_the_torn_stream() {
        let router: DrainRouter<1> = DrainRouter::new();
        let drain = router.claim(17).unwrap();
        let credits = Credits::new(1);
        let clock = Clock::default();
        let mut port = CreditPort {
            handle: 17,
            credits: &credits,
            drain,
            clock: &clock,
        };
        let mut pump = pin!(pump_fragments(&mut port, 2));
        assert!(poll_once(pump.as_mut()).is_pending());

        for wait in 1..=crate::drain_wait_budget(2) {
            clock.advance(DRAIN_WAIT_MS);
            match poll_once(pump.as_mut()) {
                Poll::Pending => assert!(wait < crate::drain_wait_budget(2)),
                Poll::Ready((tx, action)) => {
                    assert_eq!(
                        action,
                        Action::Abort {
                            index: 1,
                            reason: AbortReason::Stalled
                        }
                    );
                    assert_eq!(tx.drain_waits(), tx.budget());
                    assert_eq!(clock.0.get(), u64::from(tx.budget()) * DRAIN_WAIT_MS);
                    assert_eq!(*credits.accepted.borrow(), [(17, 0)]);
                    assert_eq!(credits.attempts.borrow().len(), tx.budget() as usize + 1);
                    assert!(tx.torn());
                    assert!(abort_disconnects(LinkRole::Peripheral, tx.torn(), false));
                    assert!(abort_disconnects(LinkRole::Central, tx.torn(), false));
                    return;
                }
            }
        }
        panic!("the pump must terminate at its drain-wait budget");
    }

    #[test]
    fn another_connections_global_credit_is_used_only_on_timeout_reoffer() {
        let router: DrainRouter<2> = DrainRouter::new();
        let drain = router.claim(17).unwrap();
        router.claim(23).unwrap();
        let credits = Credits::new(1);
        assert_eq!(credits.enqueue(23, 0), NotifyOutcome::Sent);
        let clock = Clock::default();
        let mut port = CreditPort {
            handle: 17,
            credits: &credits,
            drain,
            clock: &clock,
        };
        let mut pump = pin!(pump_fragments(&mut port, 1));
        assert!(poll_once(pump.as_mut()).is_pending());

        credits.complete(&router, 23);
        assert!(poll_once(pump.as_mut()).is_pending());
        assert_eq!(*credits.accepted.borrow(), [(23, 0)]);
        clock.advance(DRAIN_WAIT_MS - 1);
        assert!(poll_once(pump.as_mut()).is_pending());
        clock.advance(1);
        let Poll::Ready((tx, action)) = poll_once(pump.as_mut()) else {
            panic!("a timeout must re-offer against the shared pool");
        };
        assert_eq!(action, Action::Done);
        assert_eq!(*credits.accepted.borrow(), [(23, 0), (17, 0)]);
        assert_eq!(tx.drain_waits(), 1);
        assert_eq!(tx.fragments_sent(), 1);
        assert!(!tx.torn());
    }

    #[test]
    fn zero_fragments_never_touches_the_backend() {
        let router: DrainRouter<1> = DrainRouter::new();
        let drain = router.claim(17).unwrap();
        let credits = Credits::new(0);
        let clock = Clock::default();
        let mut port = CreditPort {
            handle: 17,
            credits: &credits,
            drain,
            clock: &clock,
        };
        let mut pump = pin!(pump_fragments(&mut port, 0));
        let Poll::Ready((tx, action)) = poll_once(pump.as_mut()) else {
            panic!("empty packet must complete synchronously");
        };
        assert_eq!(action, Action::Done);
        assert_eq!(tx.fragments_sent(), 0);
        assert_eq!(tx.drain_waits(), 0);
        assert!(credits.attempts.borrow().is_empty());
    }
}
