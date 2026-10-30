//! Unframed packet bytes accepted by an interface, not delivery receipts.
//!
//! Receive ownership and successful transmit acceptance each count once. Reading
//! a snapshot never changes the last completed speed window.

/// Shortest window a speed may be derived from, in milliseconds.
///
/// A sample taken a few milliseconds after the last one divides a byte count
/// by near-zero and reports a speed the medium never reached. Below this the
/// previous window's figure stands, which is what Python's own decaying
/// speed estimate does between updates.
pub const MIN_WINDOW_MS: u64 = 1_000;

/// Live byte totals and the last sampled speeds in bytes per second.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InterfaceTraffic {
    pub rxb: u64,
    pub txb: u64,
    pub rxs: f64,
    pub txs: f64,
}

/// Allocation-free arithmetic; synchronization belongs to the carrier owner.
#[derive(Clone, Copy, Debug)]
pub struct InterfaceCounters {
    traffic: InterfaceTraffic,
    previous_rx: u64,
    previous_tx: u64,
    at_ms: u64,
}

impl InterfaceCounters {
    pub const fn new() -> Self {
        Self {
            traffic: InterfaceTraffic {
                rxb: 0,
                txb: 0,
                rxs: 0.0,
                txs: 0.0,
            },
            previous_rx: 0,
            previous_tx: 0,
            at_ms: 0,
        }
    }

    pub fn note_rx(&mut self, bytes: usize) {
        self.traffic.rxb = self.traffic.rxb.saturating_add(bytes as u64);
    }

    pub fn note_tx(&mut self, bytes: usize) {
        self.traffic.txb = self.traffic.txb.saturating_add(bytes as u64);
    }

    /// Close a window only after at least [`MIN_WINDOW_MS`], including after a clock
    /// regression. A rejected sample leaves both baselines and speeds unchanged.
    pub fn sample(&mut self, now_ms: u64) {
        let elapsed = now_ms.saturating_sub(self.at_ms);
        if elapsed < MIN_WINDOW_MS {
            return;
        }
        let seconds = elapsed as f64 / 1_000.0;
        self.traffic.rxs = self.traffic.rxb.saturating_sub(self.previous_rx) as f64 / seconds;
        self.traffic.txs = self.traffic.txb.saturating_sub(self.previous_tx) as f64 / seconds;
        self.previous_rx = self.traffic.rxb;
        self.previous_tx = self.traffic.txb;
        self.at_ms = now_ms;
    }

    pub const fn snapshot(&self) -> InterfaceTraffic {
        self.traffic
    }
}

impl Default for InterfaceCounters {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_and_regressed_samples_preserve_window_and_live_reads() {
        let mut counters = InterfaceCounters::new();
        counters.note_rx(1_000);
        counters.note_tx(400);
        counters.sample(999);
        assert_eq!(
            counters.snapshot(),
            InterfaceTraffic {
                rxb: 1_000,
                txb: 400,
                rxs: 0.0,
                txs: 0.0
            }
        );
        counters.sample(1_000);
        assert_eq!(counters.snapshot().rxs, 1_000.0);
        assert_eq!(counters.snapshot().txs, 400.0);
        counters.note_rx(300);
        counters.note_tx(600);
        counters.sample(0);
        counters.sample(1_999);
        assert_eq!(
            counters.snapshot(),
            InterfaceTraffic {
                rxb: 1_300,
                txb: 1_000,
                rxs: 1_000.0,
                txs: 400.0
            }
        );
        counters.sample(2_500);
        assert_eq!(counters.snapshot().rxs, 200.0);
        assert_eq!(counters.snapshot().txs, 400.0);
        counters.sample(32_500);
        assert_eq!(counters.snapshot().rxs, 0.0);
        assert_eq!(counters.snapshot().txs, 0.0);
    }

    #[test]
    fn totals_saturate_and_windows_count_only_representable_increments() {
        let mut counters = InterfaceCounters::new();
        counters.traffic.rxb = u64::MAX - 3;
        counters.traffic.txb = u64::MAX - 7;
        counters.sample(1_000);
        counters.note_rx(10);
        counters.note_tx(10);
        counters.sample(2_000);
        assert_eq!(
            counters.snapshot(),
            InterfaceTraffic {
                rxb: u64::MAX,
                txb: u64::MAX,
                rxs: 3.0,
                txs: 7.0
            }
        );
        counters.note_rx(usize::MAX);
        counters.note_tx(usize::MAX);
        counters.sample(3_000);
        assert_eq!(
            counters.snapshot(),
            InterfaceTraffic {
                rxb: u64::MAX,
                txb: u64::MAX,
                rxs: 0.0,
                txs: 0.0
            }
        );
    }
}
