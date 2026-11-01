use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use leviculum_core::discovery::DiscoveredInterfaceRecord;
use sha2::{Digest, Sha256};

use super::{record_fingerprint, CachedEntry, GENERATION, WARNED_CORRUPT};

pub(super) fn bump_generation(generations: &mut BTreeMap<PathBuf, u64>, dir: &Path) {
    match generations.get_mut(dir) {
        Some(generation) => *generation = generation.wrapping_add(1),
        None => {
            generations.insert(dir.to_path_buf(), 1);
        }
    }
}

/// Invalidate the in-memory directory cache after a worker has evaluated an
/// expiry predicate. The worker calls this only after releasing write-behind
/// `LATEST`, preserving the one-way lock order.
pub(crate) fn note_record_job_settled(path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let mut generations = GENERATION.lock().unwrap_or_else(|e| e.into_inner());
    bump_generation(&mut generations, dir);
}

/// A fixed-size semantic version for a decoded record. The expiry worker
/// decodes the current file and hashes the same canonical encoding; this is
/// independent of filesystem mtime/inode granularity.
pub(crate) fn record_version(record: &DiscoveredInterfaceRecord) -> [u8; 32] {
    Sha256::digest(record.encode_msgpack()).into()
}

pub(super) fn decode_record_bytes(path: &Path, bytes: &[u8]) -> Option<CachedEntry> {
    if let Some(record) = DiscoveredInterfaceRecord::decode_msgpack(bytes) {
        return Some(CachedEntry::Record(Arc::new(record)));
    }
    let fingerprint = record_fingerprint(bytes);
    let already_warned = WARNED_CORRUPT
        .lock()
        .map(|w| w.get(path) == Some(&fingerprint))
        .unwrap_or(false);
    if !already_warned {
        tracing::warn!("discovery: corrupt record {}, skipping", path.display());
    }
    Some(CachedEntry::Corrupt(fingerprint))
}
