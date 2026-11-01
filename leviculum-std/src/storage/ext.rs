use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use leviculum_core::constants::TRUNCATED_HASHBYTES;

use crate::file_ratchet_store::RATCHETS_DIR;
use crate::storage::hex_encode;

/// Minimum interval between disk rewrites of an unchanged known ratchet (the
/// expiry is 30 days; this only bounds how stale the on-disk timestamp gets).
pub(crate) const RATCHET_REFRESH_MS: u64 = 3_600_000;

#[derive(Default)]
pub(super) struct RatchetWriteFilter {
    // When each known ratchet was last written to disk (monotonic ms). An
    // announce re-delivers the same ratchet every few minutes only to refresh
    // its expiry; rewriting the file each time costs a synchronous
    // write+rename on the transport thread, which stalls on a slow disk.
    saved_ms: HashMap<[u8; TRUNCATED_HASHBYTES], u64>,
}

impl RatchetWriteFilter {
    pub(super) fn new() -> Self {
        Self {
            saved_ms: HashMap::new(),
        }
    }

    pub(super) fn record_loaded(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], mono_ms: u64) {
        self.saved_ms.insert(dest_hash, mono_ms);
    }

    pub(super) fn should_write(
        &mut self,
        base_path: &Path,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
        unchanged: bool,
        received_at_ms: u64,
    ) -> bool {
        let path = base_path.join(RATCHETS_DIR).join(hex_encode(dest_hash));
        if crate::write_behind::take_write_failure(&path) {
            self.saved_ms.remove(dest_hash);
        }
        if unchanged {
            if let Some(saved) = self.saved_ms.get(dest_hash) {
                if received_at_ms.saturating_sub(*saved) < RATCHET_REFRESH_MS {
                    return false;
                }
            }
        }
        true
    }

    pub(super) fn record_saved(&mut self, dest_hash: [u8; TRUNCATED_HASHBYTES], received_at_ms: u64) {
        self.saved_ms.insert(dest_hash, received_at_ms);
    }

    pub(super) fn record_failed(&mut self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) {
        self.saved_ms.remove(dest_hash);
    }

    pub(super) fn record_expired(&mut self, dest_hash: &[u8; TRUNCATED_HASHBYTES]) {
        self.saved_ms.remove(dest_hash);
    }

    pub(super) fn len(&self) -> usize {
        self.saved_ms.len()
    }

    pub(super) fn append_diagnostic_dump(&self, s: &mut String, total: &mut u64) {
        let n = self.saved_ms.len();
        let raw = (n * (TRUNCATED_HASHBYTES + std::mem::size_of::<u64>())) as u64;
        let est = raw * 3 / 2;
        *total += est;
        let _ = writeln!(
            s,
            "ratchet_saved_ms: {} entries, raw {} bytes, estimated {} bytes (HashMap 1.5x)",
            n, raw, est
        );
    }
}
