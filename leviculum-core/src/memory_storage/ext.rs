use core::cell::Cell;

/// Cached `min(expires_ms)` over `path_table`, for `earliest_path_expiry`.
///
/// `Transport::next_deadline` runs after every inbound packet, and a scan
/// of a public-mesh-sized table (tens of thousands of paths) per packet
/// was the hottest function on relay nodes (profiled at 12-20% of
/// samples even when idle). `None` = unknown, recomputed lazily on the
/// next query; `Some(None)` = table empty. Inserts keep it exact in O(1);
/// it is dropped only when the entry holding the minimum leaves (refresh,
/// eviction, removal) or on bulk `retain`/`clear`.
#[derive(Default)]
pub(super) struct PathExpiryCache {
    min: Cell<Option<Option<u64>>>,
    #[cfg(test)]
    scans: Cell<u64>,
}

impl PathExpiryCache {
    pub(super) fn new() -> Self {
        Self {
            min: Cell::new(Some(None)),
            #[cfg(test)]
            scans: Cell::new(0),
        }
    }

    pub(super) fn reset(&self) {
        self.min.set(Some(None));
    }

    pub(super) fn on_insert(&self, old_exp: Option<u64>, new_exp: u64, evicted_exp: Option<u64>) {
        if let Some(cur) = self.min.get() {
            let lost_min = old_exp.is_some_and(|o| Some(o) == cur)
                || evicted_exp.is_some_and(|e| Some(e) == cur);
            self.min.set(if lost_min {
                None
            } else {
                Some(Some(cur.map_or(new_exp, |c| c.min(new_exp))))
            });
        }
    }

    pub(super) fn on_remove(&self, removed_exp: u64) {
        if let Some(cur) = self.min.get() {
            if Some(removed_exp) == cur {
                self.min.set(None);
            }
        }
    }

    pub(super) fn set_min(&self, min: Option<u64>) {
        self.min.set(Some(min));
    }

    pub(super) fn get_or_compute(&self, compute: impl FnOnce() -> Option<u64>) -> Option<u64> {
        if let Some(v) = self.min.get() {
            return v;
        }
        let v = compute();
        self.min.set(Some(v));
        #[cfg(test)]
        self.scans.set(self.scans.get() + 1);
        v
    }

    #[cfg(test)]
    pub(super) fn scans(&self) -> u64 {
        self.scans.get()
    }
}
