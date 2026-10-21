//! Which executor polls the LoRa task, and what that choice costs the receiver
//! between the end of one frame and the `SetRx` that listens for the next
//! (leviculum#850).
//!
//! # The mechanism
//!
//! Every chip transaction the radio driver issues is an `await` on a DMA
//! transfer, and an `await` that returns `Pending` hands the executor back.
//! On the board's thread-mode executor the task it hands it to is whichever
//! else is runnable, and while the propagation role validates a stamp the
//! main loop always is: a parked grind is due again at once
//! (`leviculum_nrf::pn::GRIND_POLL_MS = 0`), so between two of the radio's
//! transactions the main loop runs one whole settle pass, a 77 ms workblock
//! slice. The transfer itself is done in microseconds; the radio waits a
//! slice for the CPU.
//!
//! The SX1262 driver issues 21 such transactions between `RxDone` and the
//! `SetRx` that re-arms (18 for the readout of a timed window, 3 for the
//! arm). Twenty-one slices of 77 ms are 1617 ms, and the capture of
//! 2026-10-19 (847, feld-pocket, build ca6d3d2f) reads `SX_RX_ARM site=ack
//! dark_ms=1674` inside a `PN_STAMP ms=3854 slices=50` grind. The same
//! multiplication delays `TX done` by the four transactions after `TxDone`.
//!
//! It is the #425 family, a loop away from its `select` leaving the radio
//! dark, but not #425's mechanism surviving the slicing. #425 was the
//! hand-off channel: a loop away for a whole workblock left the LoRa task
//! parked in a full `LORA_INCOMING`. The slicing fixed that, and the model
//! it was sized with (`leviculum_settle_budget::dark_span_us`) prices one
//! absence per frame the task has to hand up. What it did not price is that
//! the task pays one absence per *bus transaction*, hand-off or not.
//!
//! No slice size answers that. A shorter slice divides the 1674 ms, but the
//! loop has other long polls besides the grind: in window 2 of the same
//! capture no stamp was being validated, and the LoRa task still saw part 1
//! of a split frame 41 to 46 ms after it ended. Part 2 followed it back to
//! back, its preamble was over before the receiver listened, and a split
//! frame without its second half is never reassembled.
//!
//! # The placement
//!
//! [`Placement::Preemptive`] runs the LoRa task on an interrupt executor at
//! an application priority above thread mode. A completed transfer then
//! preempts whatever thread-mode task holds the CPU, grind or not, and the
//! 21 transactions cost what the bus costs. [`LORA_TASK`] is the placement
//! the firmware spawns with, and the tests below run the receive path of
//! this crate, [`crate::receive_and_hand_up`], under both placements on a
//! virtual clock.

/// Where the board's LoRa task is polled from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// On the thread-mode executor, beside the main loop. Every `Pending` the
    /// radio returns waits for the poll that is running to end.
    Cooperative,
    /// On an interrupt executor above thread mode. A completed transfer wakes
    /// the radio and preempts the thread-mode task that is running.
    Preemptive,
}

/// The placement `leviculum_nrf::lora::spawn` gives the LoRa task.
///
/// The firmware matches on this constant rather than keeping its own
/// choice, so the placement the board runs is the placement the tests below
/// hold to the preamble bound.
pub const LORA_TASK: Placement = Placement::Preemptive;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{receive_and_hand_up, FrameSink, RxArmState, RxEvent, RxLatch, RxPort};
    use crate::{RxWindowProbe, StandingWindow};
    use core::cell::Cell;
    use core::future::Future;
    use core::pin::{pin, Pin};
    use core::task::{Context, Poll, Waker};

    /// Bus transactions the SX1262 driver issues from the `RxDone` edge to
    /// the end of the readout of a timed window (`leviculum-nrf/src/sx1262.rs`,
    /// `await_rx` and `settle_rx`): `GetIrqStatus` 3 (one per SPI operation:
    /// opcode, status, response), `ClearIrqStatus` 1, the §15.3 timeout
    /// workaround 5 (write, read 3, write), `GetRxBufferStatus` 3,
    /// `ReadBuffer` 3, `GetPacketStatus` 3.
    const READOUT: u32 = 18;
    /// Bus transactions of `arm_rx` on a chip that has left RX by itself:
    /// `SetDioIrqParams`, `ClearIrqStatus`, `SetRx`. The standby at its head
    /// is a no-op there and touches no bus.
    const ARM: u32 = 3;
    /// The status read `take_latched_frame` takes before it waits.
    const STATUS: u32 = 3;
    /// Bus transactions between `TxDone` and the `[LORA] TX done` line
    /// (`on_air`): `GetIrqStatus` 3, `ClearIrqStatus` 1.
    const TX_DONE: u32 = 4;

    /// One SPI transaction at the board's 4 MHz bus, generously rounded up:
    /// the longest is the 255-byte buffer read, 0.5 ms.
    const BUS_US: u64 = 100;
    /// One settle pass of the capture's grind: `PN_STAMP ms=3854 slices=50`.
    const SLICE_US: u64 = 3_854_000 / 50;
    /// The capture's own number for the dark span inside that grind.
    const MEASURED_DARK_US: u64 = 1_674_000;
    /// The first-poll latency window 2 measured with no grind running.
    const WINDOW_2_LATE_US: u64 = 41_000;

    /// How long the next frame's preamble is on the air at the PHY the
    /// boards run by default (SF8/BW125/CR4:5, `RadioConfig::eu_medium`),
    /// with the preamble the firmware derives for it. A receiver armed later
    /// than this after the previous frame ended cannot catch a frame sent
    /// back to back, which is how the second part of a split frame is sent.
    fn preamble_us() -> u64 {
        let (sf, cr, bw) = (8u8, 5u8, 125_000u64);
        let symbols = leviculum_core::rnode::derive_preamble_symbols(sf, cr, bw as u32);
        u64::from(symbols) * (1u64 << sf) * 1_000_000 / bw
    }

    /// The virtual clock and the one hardware event the radio waits on.
    struct Bench {
        now_us: Cell<u64>,
        /// When the radio's outstanding hardware event completes. `None` is a
        /// radio that has not yet been polled or has just been woken.
        due_us: Cell<Option<u64>>,
        /// Instant the frame on the air ends: the `RxDone` edge.
        frame_end_us: u64,
        /// Instant the re-arming `SetRx` completed.
        rearmed_us: Cell<Option<u64>>,
    }

    impl Bench {
        fn new(frame_end_us: u64) -> Self {
            Self {
                now_us: Cell::new(0),
                due_us: Cell::new(None),
                frame_end_us,
                rearmed_us: Cell::new(None),
            }
        }

        fn runnable(&self) -> bool {
            self.due_us.get().is_none_or(|due| due <= self.now_us.get())
        }
    }

    /// Pending until the bench clock reaches `at`: a DMA transfer, or the
    /// DIO1 edge at the end of a frame.
    struct Until<'a> {
        bench: &'a Bench,
        at: u64,
    }

    impl Future for Until<'_> {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
            if self.bench.now_us.get() >= self.at {
                self.bench.due_us.set(None);
                Poll::Ready(())
            } else {
                self.bench.due_us.set(Some(self.at));
                Poll::Pending
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Window(u32);

    /// The ack window the capture's dark span ended in (`timeout_ms=1188`):
    /// timed, so its readout carries the §15.3 workaround.
    const ACK: Window = Window(1188);

    /// An SX1262 whose every transaction is one DMA transfer on the bench.
    struct BusPort<'a> {
        bench: &'a Bench,
        state: RxArmState<Window>,
        arms: u32,
    }

    impl BusPort<'_> {
        async fn bus(&self, transactions: u32) {
            for _ in 0..transactions {
                let at = self.bench.now_us.get() + BUS_US;
                Until {
                    bench: self.bench,
                    at,
                }
                .await;
            }
        }
    }

    impl RxPort for BusPort<'_> {
        type Window = Window;
        type Meta = ();
        type Error = ();

        async fn arm(&mut self, window: Window) -> Result<(), ()> {
            self.state.arming(window);
            self.bus(ARM).await;
            self.arms += 1;
            if self.arms == 2 {
                self.bench.rearmed_us.set(Some(self.bench.now_us.get()));
            }
            Ok(())
        }

        async fn await_frame(&mut self, buf: &mut [u8]) -> Result<(u8, ()), ()> {
            Until {
                bench: self.bench,
                at: self.bench.frame_end_us,
            }
            .await;
            self.state.chip_left_rx();
            self.bus(READOUT).await;
            buf[0] = 0x2c;
            Ok((1, ()))
        }

        async fn disarm(&mut self) -> Result<(), ()> {
            self.state.disarmed();
            Ok(())
        }
    }

    impl RxWindowProbe for BusPort<'_> {
        fn standing_window(&self) -> Option<StandingWindow<Window>> {
            self.state.window().map(|w| StandingWindow {
                window: *w,
                stood_ms: 0,
            })
        }

        async fn latched(&mut self) -> Result<RxLatch, ()> {
            self.bus(STATUS).await;
            Ok(RxLatch::CLEAR)
        }

        async fn take_latched_frame(&mut self, _buf: &mut [u8]) -> Result<Option<(u8, ())>, ()> {
            self.bus(STATUS).await;
            Ok(None)
        }

        fn window_ends_itself(&self, window: &Window) -> bool {
            window.0 != 0
        }

        fn defer_ms(&self, _latch: &RxLatch) -> Option<u64> {
            None
        }

        fn false_preamble_ms(&self) -> Option<u64> {
            None
        }

        fn latch_settle_ms(&self) -> Option<u64> {
            None
        }

        async fn wait_for_frame(&mut self, _wait_ms: u64) -> u64 {
            0
        }

        fn report(&mut self, _event: RxEvent<'_, ()>) {}
    }

    /// The hand-off is not what this measures: it completes at once.
    struct Upward;

    impl FrameSink for Upward {
        type Meta = ();
        async fn deliver(&mut self, _bytes: &[u8], _meta: &()) {}
    }

    /// What else the executor has to run while the radio works.
    #[derive(Clone, Copy)]
    enum Foreign {
        /// A stamp grind: one settle pass after another, each due at once.
        Grind { slice_us: u64 },
        /// One long poll of some other task, `[start_us, end_us)`, and
        /// nothing else.
        OnePoll { start_us: u64, end_us: u64 },
    }

    impl Foreign {
        /// The foreign poll a cooperative executor enters at `now_us`, given
        /// that the radio is not runnable before `due_us`: when it ends.
        fn next_poll_end(&mut self, now_us: u64, due_us: u64) -> Option<u64> {
            match *self {
                Foreign::Grind { slice_us } => Some(now_us + slice_us),
                Foreign::OnePoll { start_us, end_us } if start_us < due_us && end_us > now_us => {
                    *self = Foreign::OnePoll {
                        start_us: 0,
                        end_us: 0,
                    };
                    Some(end_us)
                }
                Foreign::OnePoll { .. } => None,
            }
        }
    }

    /// Run a future that drives the radio on the bench, under `placement`,
    /// beside `foreign`, until it completes.
    fn run<F: Future>(bench: &Bench, placement: Placement, mut foreign: Foreign, radio: F) {
        let mut radio = pin!(radio);
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..1_000_000 {
            if bench.runnable() {
                if radio.as_mut().poll(&mut cx).is_ready() {
                    return;
                }
                continue;
            }
            let now = bench.now_us.get();
            let due = bench.due_us.get().unwrap_or(now);
            let next = match placement {
                // The thread executor runs one poll per runnable task per
                // round, and a poll it has entered runs to its end.
                Placement::Cooperative => foreign.next_poll_end(now, due).unwrap_or(due),
                // The completion preempts whatever ran in the meantime.
                Placement::Preemptive => due,
            };
            bench.now_us.set(next.max(due).max(now));
        }
        panic!("the radio never finished on the bench");
    }

    /// From the frame's end to the re-arm, through this crate's receive path.
    fn rearm_after_frame(placement: Placement, foreign: Foreign) -> u64 {
        // Long after the initial arm, which pays the same toll and must not
        // still be running when the frame ends.
        let bench = Bench::new(5_000_000);
        let mut port = BusPort {
            bench: &bench,
            state: RxArmState::new(),
            arms: 0,
        };
        let mut buf = [0u8; 8];
        let mut sink = Upward;
        run(
            &bench,
            placement,
            foreign,
            receive_and_hand_up(&mut port, &mut buf, ACK, &mut sink),
        );
        let rearmed = bench.rearmed_us.get().expect("the cycle re-arms");
        rearmed - bench.frame_end_us
    }

    /// From `TxDone` to the `[LORA] TX done` line: the edge, then the
    /// driver's four transactions.
    fn tx_done_after_frame(placement: Placement, foreign: Foreign) -> u64 {
        let bench = Bench::new(5_000_000);
        let port = BusPort {
            bench: &bench,
            state: RxArmState::new(),
            arms: 0,
        };
        let done = Cell::new(0u64);
        run(&bench, placement, foreign, async {
            Until {
                bench: &bench,
                at: bench.frame_end_us,
            }
            .await;
            port.bus(TX_DONE).await;
            done.set(bench.now_us.get());
        });
        done.get() - bench.frame_end_us
    }

    const GRIND: Foreign = Foreign::Grind { slice_us: SLICE_US };

    /// The mechanism, and the capture it explains. Cooperatively, every one
    /// of the 21 transactions between `RxDone` and `SetRx` waits a whole
    /// slice, plus the part of a slice the edge itself fell into: 21 to 22
    /// slices, and the capture's 1674 ms is inside that.
    #[test]
    fn a_cooperative_radio_waits_one_slice_per_transaction() {
        let dark = rearm_after_frame(Placement::Cooperative, GRIND);
        let transactions = u64::from(READOUT + ARM);
        assert!(
            (transactions * SLICE_US..=(transactions + 1) * SLICE_US).contains(&dark),
            "dark={dark} us"
        );
        assert!(
            (transactions * SLICE_US..=(transactions + 1) * SLICE_US).contains(&MEASURED_DARK_US),
            "the model must explain the capture's {MEASURED_DARK_US} us"
        );
    }

    /// The control: the same receive path, preempting the grind, pays the
    /// bus and nothing else.
    #[test]
    fn a_preemptive_radio_pays_only_the_bus() {
        let dark = rearm_after_frame(Placement::Preemptive, GRIND);
        assert_eq!(dark, u64::from(READOUT + ARM) * BUS_US);
    }

    /// **The bound, on the placement the firmware runs.** While a grind
    /// runs, the receiver listens again before a frame sent back to back
    /// would have finished its preamble.
    #[test]
    fn the_firmwares_radio_rearms_inside_the_next_preamble_during_a_grind() {
        let dark = rearm_after_frame(LORA_TASK, GRIND);
        assert!(
            dark < preamble_us(),
            "re-armed {dark} us after the frame, the next preamble lasts {} us",
            preamble_us()
        );
    }

    /// Window 2 of the capture: no grind, one foreign poll that covers the
    /// end of the frame by 41 ms. On the firmware's placement that poll does
    /// not delay the re-arm, so part 2 of a split frame finds the receiver
    /// listening.
    #[test]
    fn the_firmwares_radio_rearms_inside_the_next_preamble_behind_one_long_poll() {
        let one_poll = Foreign::OnePoll {
            start_us: 5_000_000 - 5_000,
            end_us: 5_000_000 + WINDOW_2_LATE_US,
        };
        let cooperative = rearm_after_frame(Placement::Cooperative, one_poll);
        assert!(
            cooperative >= WINDOW_2_LATE_US,
            "the control must reproduce the late re-arm, got {cooperative} us"
        );
        assert!(
            cooperative >= preamble_us(),
            "{cooperative} us must miss a {} us preamble, as window 2 did",
            preamble_us()
        );
        let dark = rearm_after_frame(LORA_TASK, one_poll);
        assert!(
            dark < preamble_us(),
            "re-armed {dark} us after the frame, the next preamble lasts {} us",
            preamble_us()
        );
    }

    /// `TX done` behind a grind: four transactions, four slices and the part
    /// the edge fell into, where the capture read about 500 ms between the
    /// frame leaving the air and the line. On the firmware's placement it
    /// costs the bus.
    #[test]
    fn the_firmwares_radio_reports_tx_done_without_waiting_for_the_grind() {
        let cooperative = tx_done_after_frame(Placement::Cooperative, GRIND);
        assert!(
            cooperative >= u64::from(TX_DONE) * SLICE_US,
            "cooperative={cooperative} us"
        );
        let done = tx_done_after_frame(LORA_TASK, GRIND);
        assert_eq!(done, u64::from(TX_DONE) * BUS_US);
    }

    /// The bound is a real number at this PHY and not a vacuous one: the
    /// cooperative figure misses it by more than an order of magnitude, the
    /// bus fits it with room.
    #[test]
    fn the_preamble_bound_separates_the_two_placements() {
        let preamble = preamble_us();
        assert!(preamble > u64::from(READOUT + ARM) * BUS_US * 4);
        assert!(preamble * 10 < u64::from(READOUT + ARM) * SLICE_US);
    }
}
