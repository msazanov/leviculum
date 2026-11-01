use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use leviculum_core::discovery::DiscoveredInterfaceRecord;
use sha2::{Digest, Sha256};

use super::{record_fingerprint, CachedEntry, CachedFile, GENERATION, WARNED_CORRUPT};

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

/// Clean up expired records using the ordered write-behind remove queue with
/// version predicate checks to prevent unlinking newer records.
pub(super) fn expire_records(
    files: &mut BTreeMap<OsString, CachedFile>,
    dir: &Path,
    now: f64,
) -> bool {
    let expired: Vec<OsString> = files
        .iter()
        .filter(|(_, file)| {
            matches!(&file.entry, CachedEntry::Record(record) if record.should_remove(now))
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in &expired {
        let Some(expected_version) = files.get(name).and_then(|file| match &file.entry {
            CachedEntry::Record(record) => Some(record_version(record)),
            CachedEntry::Corrupt(_) => None,
        }) else {
            continue;
        };
        // The worker compares the current on-disk record immediately
        // before unlink. A queued write before this job is therefore
        // observed even if it settled after this scan's cache snapshot.
        match crate::write_behind::remove_discovery_if_version(
            dir.join(name),
            expected_version,
            now,
        ) {
            Ok(false) => {
                // A newer announce is already queued for this filename.
                // Refresh the listing from its read-your-writes bytes so
                // the stale on-disk record is neither deleted nor served.
                let path = dir.join(name);
                if let Ok(bytes) = crate::write_behind::read_latest(&path) {
                    if let Some(entry) = decode_record_bytes(&path, &bytes) {
                        if let Some(file) = files.get_mut(name) {
                            file.entry = entry;
                        }
                    }
                }
            }
            Ok(true) => {
                // Hide the expired scan result now. After the worker has
                // compared and settled this job it bumps the directory
                // generation, so the next listing re-reads disk even if
                // deletion was skipped for a newer version.
                files.remove(name);
            }
            Err(error) => {
                tracing::warn!("discovery record removal was not queued: {error}");
                files.remove(name);
            }
        }
    }
    !expired.is_empty()
}
