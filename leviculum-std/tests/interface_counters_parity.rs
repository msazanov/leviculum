//! The firmware's byte counters move onto `leviculum_core::interface_counters`
//! (953, Codeberg #470); the move claims the arithmetic is identical. This
//! test holds that claim while both exist: `old` is the firmware's counter
//! code copied verbatim from `leviculum-nrf/src/iface_bytes.rs` (the copy is
//! checked against that file, so it cannot drift from what the board runs),
//! and the same sample series goes through both. Totals and speeds must agree
//! bit for bit after every step. It goes with the old code when the firmware
//! moves onto core.

mod old {
    #![allow(dead_code)]

    use core::cell::Cell;

    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::blocking_mutex::Mutex;

    // --- verbatim from leviculum-nrf/src/iface_bytes.rs ---
    /// Shortest window a speed may be derived from, in milliseconds.
    ///
    /// A sample taken a few milliseconds after the last one divides a byte count
    /// by near-zero and reports a speed the medium never reached. Below this the
    /// previous window's figure stands, which is what Python's own decaying
    /// speed estimate does between updates.
    const MIN_WINDOW_MS: u64 = 1_000;

    /// Lifetime byte totals of one interface.
    #[derive(Clone, Copy)]
    struct Bytes {
        rx: u64,
        tx: u64,
    }

    /// The last speed window: where the totals stood when it opened, and the
    /// bytes/s it produced when it closed.
    #[derive(Clone, Copy)]
    struct Window {
        rx: u64,
        tx: u64,
        at_ms: u64,
        rxs: f64,
        txs: f64,
    }

    /// One interface's counters.
    pub struct Counters {
        live: Mutex<CriticalSectionRawMutex, Cell<Bytes>>,
        window: Mutex<CriticalSectionRawMutex, Cell<Window>>,
    }

    impl Counters {
        const fn new() -> Self {
            Self {
                live: Mutex::new(Cell::new(Bytes { rx: 0, tx: 0 })),
                window: Mutex::new(Cell::new(Window {
                    rx: 0,
                    tx: 0,
                    at_ms: 0,
                    rxs: 0.0,
                    txs: 0.0,
                })),
            }
        }

        fn add_rx(&self, bytes: usize) {
            self.live.lock(|c| {
                let mut v = c.get();
                v.rx = v.rx.saturating_add(bytes as u64);
                c.set(v);
            });
        }

        fn add_tx(&self, bytes: usize) {
            self.live.lock(|c| {
                let mut v = c.get();
                v.tx = v.tx.saturating_add(bytes as u64);
                c.set(v);
            });
        }

        /// Lifetime `(rxb, txb)` since boot.
        pub fn totals(&self) -> (u64, u64) {
            self.live.lock(|c| {
                let v = c.get();
                (v.rx, v.tx)
            })
        }

        /// The last closed window's `(rxs, txs)` in bytes per second.
        pub fn speeds(&self) -> (f64, f64) {
            self.window.lock(|c| {
                let w = c.get();
                (w.rxs, w.txs)
            })
        }

        /// Close the current speed window at `now_ms` and open the next one.
        ///
        /// Called from the one periodic emitter, so the window a `/status` answer
        /// reports is the same window the `[TRANSPORT]` line printed.
        fn sample(&self, now_ms: u64) {
            let (rx, tx) = self.totals();
            self.window.lock(|c| {
                let prev = c.get();
                let elapsed = now_ms.saturating_sub(prev.at_ms);
                if elapsed < MIN_WINDOW_MS {
                    return;
                }
                let secs = elapsed as f64 / 1000.0;
                c.set(Window {
                    rx,
                    tx,
                    at_ms: now_ms,
                    rxs: rx.saturating_sub(prev.rx) as f64 / secs,
                    txs: tx.saturating_sub(prev.tx) as f64 / secs,
                });
            });
        }
    }

    // --- end verbatim ---

    pub fn new() -> Counters {
        Counters::new()
    }

    pub fn note_rx(c: &Counters, bytes: usize) {
        c.add_rx(bytes);
    }

    pub fn note_tx(c: &Counters, bytes: usize) {
        c.add_tx(bytes);
    }

    pub fn sample(c: &Counters, now_ms: u64) {
        c.sample(now_ms);
    }
}

use leviculum_core::interface_counters::InterfaceCounters;

/// The block between the two markers above, as it stands in the firmware.
const FIRMWARE: &str = include_str!("../../leviculum-nrf/src/iface_bytes.rs");
const THIS: &str = include_str!("interface_counters_parity.rs");

#[test]
fn the_copied_counter_code_is_the_firmwares() {
    let start = THIS
        .find("    // --- verbatim from leviculum-nrf/src/iface_bytes.rs ---\n")
        .expect("start marker");
    let start = THIS[start..]
        .find('\n')
        .map(|i| start + i + 1)
        .expect("line");
    let end = THIS
        .find("    // --- end verbatim ---")
        .expect("end marker");
    let copied: String = THIS[start..end]
        .lines()
        .map(|line| line.strip_prefix("    ").unwrap_or(line))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(
        FIRMWARE.contains(&copied),
        "the copy no longer matches leviculum-nrf/src/iface_bytes.rs"
    );
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Rx(usize),
    Tx(usize),
    Sample(u64),
}

/// A fixed-seed series: bursts of both directions, windows under, at and
/// over one second, clock regressions, idle windows, and totals driven into
/// saturation.
fn series() -> Vec<Step> {
    let mut steps = Vec::new();
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut now: u64 = 0;
    for _ in 0..20_000 {
        let r = next();
        let step = match r % 8 {
            0..=2 => Step::Rx((next() % 600) as usize),
            3..=4 => Step::Tx((next() % 600) as usize),
            5 => {
                now += next() % 1_500;
                Step::Sample(now)
            }
            6 => {
                // exactly the boundary, or one under it
                now += 999 + next() % 2;
                Step::Sample(now)
            }
            _ => {
                // a clock regression, or a long idle gap
                if next() % 2 == 0 {
                    now = now.saturating_sub(next() % 5_000);
                } else {
                    now += 30_000 + next() % 60_000;
                }
                Step::Sample(now)
            }
        };
        steps.push(step);
    }
    // Saturation: both totals pinned at u64::MAX, then windows over them.
    for _ in 0..3 {
        steps.push(Step::Rx(usize::MAX));
        steps.push(Step::Tx(usize::MAX));
        now += 1_000;
        steps.push(Step::Sample(now));
    }
    steps.push(Step::Rx(10));
    now += 2_500;
    steps.push(Step::Sample(now));
    steps
}

#[test]
fn old_and_new_counters_agree_on_every_step_of_one_series() {
    let old = old::new();
    let mut new = InterfaceCounters::new();
    let mut windows_closed = 0usize;
    let mut last_speeds = (0.0f64, 0.0f64);
    for (i, step) in series().into_iter().enumerate() {
        match step {
            Step::Rx(n) => {
                old::note_rx(&old, n);
                new.note_rx(n);
            }
            Step::Tx(n) => {
                old::note_tx(&old, n);
                new.note_tx(n);
            }
            Step::Sample(t) => {
                old::sample(&old, t);
                new.sample(t);
            }
        }
        let (rxb, txb) = old.totals();
        let (rxs, txs) = old.speeds();
        let snap = new.snapshot();
        assert_eq!(
            (snap.rxb, snap.txb),
            (rxb, txb),
            "totals at step {i} {step:?}"
        );
        assert_eq!(
            (snap.rxs.to_bits(), snap.txs.to_bits()),
            (rxs.to_bits(), txs.to_bits()),
            "speeds at step {i} {step:?}: new ({}, {}) old ({rxs}, {txs})",
            snap.rxs,
            snap.txs
        );
        if (rxs, txs) != last_speeds {
            windows_closed += 1;
            last_speeds = (rxs, txs);
        }
    }
    // The series must exercise the window, not just the totals.
    assert!(
        windows_closed > 1_000,
        "only {windows_closed} speed changes"
    );
    assert_eq!(old.totals(), (u64::MAX, u64::MAX));
}
