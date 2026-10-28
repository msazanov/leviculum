//! File-backed [`PeerStore`] (Codeberg #384, part 2): the host half of the
//! peer-table persistence the concept paper's §5 assigns — "the host
//! persists its table in a file, as the reference does"
//! (`LXMRouter.exit_handler` writes its peers with `LXMPeer.to_bytes`,
//! `reference/LXMF/LXMF/LXMRouter.py:1367`). The board's implementation is
//! part 3's tagged-record adapter; see
//! [`leviculum_lxmf::peering::PeerRecord`] for what the trait demands of
//! the record log.
//!
//! One msgpack file, rewritten whole on every change: the table is at most
//! `max_peers` (default 20) records of ~100 B, and peer-state churn is
//! announce-cadence slow, so atomic-rewrite is simpler than any journal
//! and always consistent on reopen. Written via the durable temp+rename
//! path for the same reason the message store is: a peering key can cost
//! minutes of mining (§5's cost table), and losing it to a power cut costs
//! that again.
//!
//! # Off the caller's thread, and only on a change (order 1081)
//!
//! `lnpnd` saves a record from its core hooks, under the core lock and a 5 ms
//! budget, and one rewrite with its two `fsync`s measured up to 9.5 ms on the
//! coder host (order 1009). So the table lives in memory, a save or remove
//! that changes it hands the encoded table to a writer thread, and the
//! writer rewrites the file with whatever is newest when it gets to it:
//! changes that arrive while a write runs are coalesced into the next one,
//! and a save that changes nothing writes nothing. Nothing on the wire waits
//! on this file, so nothing waits on the writer; what a power cut can cost
//! is the changes of the last write's duration, which re-form from announces
//! as the reboot case above already describes. Dropping the store waits for
//! the newest table to be written.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use leviculum_lxmf::msgpack;
use leviculum_lxmf::peering::{PeerRecord, PeerStore, PeerTraffic};
use leviculum_lxmf::storage::StorageError;

use crate::durable::{device_fsync, write_durably, Fsync};

/// A peer table in one msgpack file.
pub struct FilePeerStore {
    path: PathBuf,
    records: Vec<PeerRecord>,
    shared: Arc<Shared>,
    writer: Option<JoinHandle<()>>,
}

/// What the store and its writer share.
struct Shared {
    slot: Mutex<Slot>,
    changed: Condvar,
}

#[derive(Default)]
struct Slot {
    /// The newest encoded table not yet taken by the writer.
    newest: Option<Vec<u8>>,
    /// The writer is writing a table it took.
    writing: bool,
    closed: bool,
    /// Rewrites made since the store opened.
    writes: u64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        // A writer that panicked mid-write left the slot as consistent as
        // it was: every field is set under the lock in one step.
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn run_writer(path: PathBuf, shared: Arc<Shared>, fsync: Arc<dyn Fsync>) {
    let temp = path.with_extension("tmp");
    loop {
        let bytes = {
            let mut slot = shared.lock();
            loop {
                if let Some(bytes) = slot.newest.take() {
                    slot.writing = true;
                    break bytes;
                }
                if slot.closed {
                    return;
                }
                slot = shared
                    .changed
                    .wait(slot)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        if write_durably(&temp, &path, &bytes, &*fsync).is_err() {
            tracing::warn!("peer store write to {} failed", path.display());
        }
        let mut slot = shared.lock();
        slot.writing = false;
        slot.writes += 1;
        shared.changed.notify_all();
    }
}

impl FilePeerStore {
    /// Open (or create) the store at `path`. An unreadable or corrupt
    /// file starts empty rather than failing the daemon: the table
    /// re-forms from announces, one bounded re-offer per re-formed peer
    /// (§5's reboot case).
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        Self::open_with(path, device_fsync())
    }

    /// [`Self::open`] over another [`Fsync`], for a test's slow device.
    pub fn open_with<P: AsRef<Path>>(path: P, fsync: Arc<dyn Fsync>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StorageError::Io)?;
        }
        let records = match std::fs::read(&path) {
            Ok(bytes) => decode_records(&bytes).unwrap_or_else(|| {
                tracing::warn!(
                    "peer store at {} did not parse; starting with an empty table",
                    path.display()
                );
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot::default()),
            changed: Condvar::new(),
        });
        let writer = {
            let path = path.clone();
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("peer-store-writer".into())
                .spawn(move || run_writer(path, shared, fsync))
                .map_err(|_| StorageError::Io)?
        };
        Ok(Self {
            path,
            records,
            shared,
            writer: Some(writer),
        })
    }

    /// The file this store writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hand the table to the writer, replacing a newer-than-written table it
    /// has not taken yet: that one is coalesced into this.
    fn persist(&self) {
        let bytes = encode_records(&self.records);
        let mut slot = self.shared.lock();
        slot.newest = Some(bytes);
        self.shared.changed.notify_all();
    }

    /// Wait until the newest table is written.
    pub fn flush(&self) {
        let mut slot = self.shared.lock();
        while (slot.newest.is_some() || slot.writing) && self.writer.is_some() {
            slot = self
                .shared
                .changed
                .wait(slot)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// How many times the file has been rewritten since the store opened.
    pub fn writes(&self) -> u64 {
        self.shared.lock().writes
    }
}

impl Drop for FilePeerStore {
    /// The newest table is written before the store is gone.
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.changed.notify_all();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

impl PeerStore for FilePeerStore {
    fn save(&mut self, record: &PeerRecord) -> Result<(), StorageError> {
        match self
            .records
            .iter_mut()
            .find(|known| known.destination_hash == record.destination_hash)
        {
            // The same record again: the file already says this.
            Some(existing) if existing == record => return Ok(()),
            Some(existing) => *existing = record.clone(),
            None => self.records.push(record.clone()),
        }
        self.persist();
        Ok(())
    }

    fn remove(&mut self, destination_hash: &[u8; 16]) -> Result<(), StorageError> {
        let before = self.records.len();
        self.records
            .retain(|record| &record.destination_hash != destination_hash);
        if self.records.len() != before {
            self.persist();
        }
        Ok(())
    }

    fn load_all(&self) -> Result<Vec<PeerRecord>, StorageError> {
        Ok(self.records.clone())
    }
}

const RECORD_FIELDS: usize = 14;

/// Field count before the peer's traffic statistics were persisted (557);
/// records of this arity still decode, counters at zero.
const RECORD_FIELDS_NO_TRAFFIC: usize = 13;

/// Field count before the peer's public keys were persisted (#388 pass
/// 3); records of this arity still decode, keys absent, so an upgrade
/// keeps its peer table.
const RECORD_FIELDS_LEGACY: usize = 12;

/// Elements of the traffic array, the order of [`PeerTraffic`]'s fields.
const TRAFFIC_FIELDS: usize = 7;

fn encode_records(records: &[PeerRecord]) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, records.len());
    for record in records {
        msgpack::array(&mut out, RECORD_FIELDS);
        msgpack::bin(&mut out, &record.destination_hash);
        match &record.identity_hash {
            Some(hash) => msgpack::bin(&mut out, hash),
            None => msgpack::nil(&mut out),
        }
        match &record.peering_key {
            Some((key, value)) => {
                msgpack::array(&mut out, 2);
                msgpack::bin(&mut out, key);
                msgpack::uint(&mut out, *value as u64);
            }
            None => msgpack::nil(&mut out),
        }
        msgpack::uint(&mut out, record.transfer_limit_kb);
        msgpack::uint(&mut out, record.sync_limit_kb);
        msgpack::uint(&mut out, record.stamp_cost as u64);
        msgpack::uint(&mut out, record.stamp_cost_flexibility as u64);
        msgpack::uint(&mut out, record.peering_cost as u64);
        msgpack::uint(&mut out, record.peering_timebase);
        msgpack::uint(&mut out, record.last_heard);
        msgpack::uint(&mut out, record.cursor);
        msgpack::bool(&mut out, record.is_static);
        match &record.public_keys {
            Some(keys) => msgpack::bin(&mut out, keys),
            None => msgpack::nil(&mut out),
        }
        let traffic = &record.traffic;
        msgpack::array(&mut out, TRAFFIC_FIELDS);
        msgpack::uint(&mut out, traffic.offered);
        msgpack::uint(&mut out, traffic.outgoing);
        msgpack::uint(&mut out, traffic.incoming);
        msgpack::uint(&mut out, traffic.rx_bytes);
        msgpack::uint(&mut out, traffic.tx_bytes);
        msgpack::uint(&mut out, traffic.last_sync_attempt);
        msgpack::bool(&mut out, traffic.alive);
    }
    out
}

fn decode_records(bytes: &[u8]) -> Option<Vec<PeerRecord>> {
    let mut position = 0;
    let count = msgpack::array_len(bytes, &mut position).ok()?;
    if count > bytes.len() {
        return None;
    }
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let fields = msgpack::array_len(bytes, &mut position).ok()?;
        if ![
            RECORD_FIELDS,
            RECORD_FIELDS_NO_TRAFFIC,
            RECORD_FIELDS_LEGACY,
        ]
        .contains(&fields)
        {
            return None;
        }
        let destination_hash: [u8; 16] = msgpack::read_bin(bytes, &mut position)
            .ok()?
            .try_into()
            .ok()?;
        let identity_hash = if msgpack::peek_kind(bytes, position).ok()? == msgpack::Kind::Nil {
            msgpack::read_nil(bytes, &mut position).ok()?;
            None
        } else {
            Some(
                msgpack::read_bin(bytes, &mut position)
                    .ok()?
                    .try_into()
                    .ok()?,
            )
        };
        let peering_key = if msgpack::peek_kind(bytes, position).ok()? == msgpack::Kind::Nil {
            msgpack::read_nil(bytes, &mut position).ok()?;
            None
        } else {
            if msgpack::array_len(bytes, &mut position).ok()? != 2 {
                return None;
            }
            let key: [u8; 32] = msgpack::read_bin(bytes, &mut position)
                .ok()?
                .try_into()
                .ok()?;
            let value = msgpack::read_uint(bytes, &mut position).ok()?;
            Some((key, value.min(u16::MAX as u64) as u16))
        };
        let transfer_limit_kb = msgpack::read_uint(bytes, &mut position).ok()?;
        let sync_limit_kb = msgpack::read_uint(bytes, &mut position).ok()?;
        let stamp_cost = msgpack::read_uint(bytes, &mut position).ok()? as u8;
        let stamp_cost_flexibility = msgpack::read_uint(bytes, &mut position).ok()? as u8;
        let peering_cost = msgpack::read_uint(bytes, &mut position).ok()? as u8;
        let peering_timebase = msgpack::read_uint(bytes, &mut position).ok()?;
        let last_heard = msgpack::read_uint(bytes, &mut position).ok()?;
        let cursor = msgpack::read_uint(bytes, &mut position).ok()?;
        let is_static = msgpack::read_bool(bytes, &mut position).ok()?;
        let public_keys = if fields == RECORD_FIELDS_LEGACY
            || msgpack::peek_kind(bytes, position).ok()? == msgpack::Kind::Nil
        {
            if fields != RECORD_FIELDS_LEGACY {
                msgpack::read_nil(bytes, &mut position).ok()?;
            }
            None
        } else {
            Some(
                msgpack::read_bin(bytes, &mut position)
                    .ok()?
                    .try_into()
                    .ok()?,
            )
        };
        let traffic = if fields == RECORD_FIELDS {
            if msgpack::array_len(bytes, &mut position).ok()? != TRAFFIC_FIELDS {
                return None;
            }
            PeerTraffic {
                offered: msgpack::read_uint(bytes, &mut position).ok()?,
                outgoing: msgpack::read_uint(bytes, &mut position).ok()?,
                incoming: msgpack::read_uint(bytes, &mut position).ok()?,
                rx_bytes: msgpack::read_uint(bytes, &mut position).ok()?,
                tx_bytes: msgpack::read_uint(bytes, &mut position).ok()?,
                last_sync_attempt: msgpack::read_uint(bytes, &mut position).ok()?,
                alive: msgpack::read_bool(bytes, &mut position).ok()?,
            }
        } else {
            PeerTraffic::default()
        };
        records.push(PeerRecord {
            destination_hash,
            identity_hash,
            public_keys,
            peering_key,
            transfer_limit_kb,
            sync_limit_kb,
            stamp_cost,
            stamp_cost_flexibility,
            peering_cost,
            peering_timebase,
            last_heard,
            cursor,
            is_static,
            traffic,
        });
    }
    Some(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seed: u8) -> PeerRecord {
        PeerRecord {
            destination_hash: [seed; 16],
            identity_hash: seed.is_multiple_of(2).then(|| [seed + 1; 16]),
            public_keys: seed.is_multiple_of(2).then(|| [seed + 2; 64]),
            peering_key: Some(([seed; 32], 18)),
            transfer_limit_kb: 4,
            sync_limit_kb: 32,
            stamp_cost: 16,
            stamp_cost_flexibility: 3,
            peering_cost: 18,
            peering_timebase: 1000 + seed as u64,
            last_heard: 2000,
            cursor: 7,
            is_static: seed == 3,
            traffic: PeerTraffic {
                offered: 10 + seed as u64,
                outgoing: 4,
                incoming: 2,
                rx_bytes: 900,
                tx_bytes: 1800,
                last_sync_attempt: 1500,
                alive: seed == 2,
            },
        }
    }

    #[test]
    fn records_survive_a_reopen_and_upsert_replaces() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("peers");
        let mut store = FilePeerStore::open(&path).expect("open");
        store.save(&record(2)).unwrap();
        store.save(&record(3)).unwrap();
        let mut changed = record(2);
        changed.cursor = 99;
        store.save(&changed).unwrap();
        drop(store);

        let store = FilePeerStore::open(&path).expect("reopen");
        let mut records = store.load_all().unwrap();
        records.sort_by_key(|record| record.destination_hash);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].cursor, 99);
        assert_eq!(records[0].peering_key, Some(([2; 32], 18)));
        assert!(records[1].is_static);
        // The counters rotation ranks by survive the restart, as the
        // reference's do (`LXMPeer.to_bytes`, `LXMPeer.py:154-159`).
        assert_eq!(records[0].traffic, record(2).traffic);
        assert_eq!(records[1].traffic, record(3).traffic);
    }

    /// A file written before the traffic statistics were persisted still
    /// loads, counters at zero: an upgrade keeps its peer table.
    #[test]
    fn a_record_without_traffic_decodes_with_zero_counters() {
        let zeroed = PeerRecord {
            traffic: PeerTraffic::default(),
            ..record(2)
        };
        let mut bytes = encode_records(&[zeroed]);
        // The traffic array is the record's last element: its header, six
        // zero uints and a bool, one byte each.
        bytes.truncate(bytes.len() - (1 + TRAFFIC_FIELDS));
        // Outer fixarray of 1, then the record's fixarray header.
        assert_eq!(bytes[1], 0x90 | RECORD_FIELDS as u8);
        bytes[1] = 0x90 | RECORD_FIELDS_NO_TRAFFIC as u8;

        let records = decode_records(&bytes).expect("a 13-field record decodes");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].traffic, PeerTraffic::default());
        assert_eq!(records[0].cursor, 7);
    }

    #[test]
    fn remove_persists_and_a_corrupt_file_starts_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("peers");
        let mut store = FilePeerStore::open(&path).expect("open");
        store.save(&record(2)).unwrap();
        store.remove(&[2; 16]).unwrap();
        drop(store);
        let store = FilePeerStore::open(&path).expect("reopen");
        assert!(store.load_all().unwrap().is_empty());

        std::fs::write(&path, b"not msgpack at all").unwrap();
        let store = FilePeerStore::open(&path).expect("open corrupt");
        assert!(store.load_all().unwrap().is_empty());
    }

    /// Order 1081: a change writes the file once, a save that changes
    /// nothing does not write it, and a restart reads back the same table.
    #[test]
    fn writes_follow_changes_and_an_unchanged_save_writes_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("peers");
        let mut store = FilePeerStore::open(&path).expect("open");
        for seed in 2..7 {
            store.save(&record(seed)).unwrap();
            store.flush();
        }
        assert_eq!(store.writes(), 5, "five changes, one write each");
        store.save(&record(2)).unwrap();
        store.remove(&[99; 16]).unwrap();
        store.flush();
        assert_eq!(
            store.writes(),
            5,
            "nothing changed, and the file was written"
        );
        store.remove(&[3; 16]).unwrap();
        store.flush();
        assert_eq!(store.writes(), 6);
        let before = store.load_all().unwrap();
        drop(store);

        let store = FilePeerStore::open(&path).expect("reopen");
        assert_eq!(store.load_all().unwrap(), before);
        assert_eq!(store.writes(), 0, "opening writes nothing");
    }

    /// Order 1081: a save costs its caller no `fsync`, and the changes that
    /// arrive while a write runs are coalesced into one write of the newest
    /// table. The device is held for the whole burst, so the count does not
    /// depend on how the threads are scheduled: the writer has taken the
    /// first table or not, and everything after it is one write.
    #[test]
    fn a_burst_during_a_write_is_coalesced_and_costs_the_caller_no_fsync() {
        struct Held {
            gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
            threads: std::sync::Mutex<Vec<std::thread::ThreadId>>,
        }
        impl Fsync for Held {
            fn sync(&self, handle: &std::fs::File) -> std::io::Result<()> {
                if let Ok(gate) = self.gate.lock() {
                    let _ = gate.recv();
                }
                if let Ok(mut threads) = self.threads.lock() {
                    threads.push(std::thread::current().id());
                }
                handle.sync_all()
            }
        }
        let (release, gate) = std::sync::mpsc::channel();
        let held = Arc::new(Held {
            gate: std::sync::Mutex::new(gate),
            threads: std::sync::Mutex::new(Vec::new()),
        });
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("peers");
        let mut store =
            FilePeerStore::open_with(&path, Arc::clone(&held) as Arc<dyn Fsync>).expect("open");
        for cursor in 0..50 {
            let mut changed = record(2);
            changed.cursor = cursor;
            // Returns although the device holds every fsync: the save does
            // not wait for one.
            store.save(&changed).unwrap();
        }
        drop(release);
        store.flush();
        let writes = store.writes();
        assert!(
            (1..=2).contains(&writes),
            "50 changes during one held write made {writes} writes"
        );
        let caller = std::thread::current().id();
        let threads = held.threads.lock().map(|t| t.clone()).unwrap_or_default();
        assert!(
            !threads.is_empty() && threads.iter().all(|thread| *thread != caller),
            "an fsync ran on the caller's thread: {threads:?}"
        );
        drop(store);
        let store = FilePeerStore::open(&path).expect("reopen");
        let records = store.load_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].cursor, 49,
            "the newest table was not the one written"
        );
    }
}
