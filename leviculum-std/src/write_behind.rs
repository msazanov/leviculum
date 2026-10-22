//! Bounded disk writes outside the node engine thread.
//!
//! Announce processing must not wait for filesystem latency. Public known
//! ratchets and discovery records are therefore serialized by one worker. The
//! bounded sync channel and atomic outstanding-job reservation apply equally to
//! writes and removals. If saturated, an operation fails promptly without
//! changing the read-your-writes view or doing disk I/O on the caller thread.
//! Private ratchet keys do not use this module; they remain synchronous through
//! `atomic_write_secret`. This is a cache, not a durability boundary: queued
//! public ratchet/discovery writes can be lost on process shutdown or power
//! failure and are learned again from later announces.
//!
//! The byte budget counts each queued payload once. A pending write is held
//! twice (in the job and in the read-your-writes mirror), so the worst-case
//! heap use is about twice `MAX_PENDING_BYTES`.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};

const MAX_PENDING: usize = 4096;
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
const MAX_FAILED_WRITES: usize = 4096;

enum Job {
    Write {
        path: PathBuf,
        data: Vec<u8>,
        seq: u64,
    },
    Remove {
        path: PathBuf,
        seq: u64,
    },
    /// Remove a discovery record only if the worker still sees the semantic
    /// version observed by the expiry scan.
    ConditionalRemove {
        path: PathBuf,
        expected: [u8; 32],
        now: f64,
        seq: u64,
    },
    #[cfg(test)]
    Barrier(SyncSender<()>),
    #[cfg(test)]
    Pause {
        entered: SyncSender<()>,
        release: Receiver<()>,
    },
    #[cfg(test)]
    Panic,
}

#[derive(Clone, PartialEq, Eq)]
enum LatestValue {
    Write(Vec<u8>),
    Remove,
    ConditionalRemove([u8; 32]),
}

#[derive(Default)]
struct State {
    /// Last queued state per path, including the in-flight write. It is
    /// removed only when the matching job has settled.
    latest: Mutex<BTreeMap<PathBuf, (u64, LatestValue)>>,
    failed_writes: Mutex<HashSet<PathBuf>>,
    pending: AtomicUsize,
    pending_bytes: AtomicUsize,
    next_seq: AtomicU64,
    worker_failed: AtomicBool,
}

#[derive(Clone)]
struct Writer {
    tx: SyncSender<Job>,
    state: Arc<State>,
    capacity: usize,
    #[cfg(test)]
    enqueue_hook: Arc<Mutex<Option<(SyncSender<()>, Receiver<()>)>>>,
}

impl Writer {
    fn spawn(capacity: usize) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(capacity);
        let state = Arc::new(State::default());
        let worker_state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("leviculum-writer".into())
            .spawn(move || run(rx, worker_state))?;
        Ok(Self {
            tx,
            state,
            capacity,
            #[cfg(test)]
            enqueue_hook: Arc::new(Mutex::new(None)),
        })
    }

    fn write(&self, path: PathBuf, data: Vec<u8>) -> io::Result<u64> {
        self.enqueue(path, Some(data))
    }

    fn remove(&self, path: PathBuf) -> io::Result<u64> {
        self.enqueue(path, None)
    }

    fn remove_unless_pending_write(&self, path: PathBuf) -> io::Result<bool> {
        let mut latest = self.state.latest.lock().unwrap_or_else(|e| e.into_inner());
        match latest.get(&path).map(|(_, value)| value) {
            Some(LatestValue::Write(_)) => return Ok(false),
            Some(LatestValue::Remove | LatestValue::ConditionalRemove(_)) => return Ok(true),
            None => {}
        }
        if !self.reserve(0) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "write-behind queue is full",
            ));
        }
        let seq = self
            .state
            .next_seq
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let previous = latest.insert(path.clone(), (seq, LatestValue::Remove));
        match self.tx.try_send(Job::Remove {
            path: path.clone(),
            seq,
        }) {
            Ok(()) => Ok(true),
            Err(mpsc::TrySendError::Full(_)) => {
                restore_latest(&mut latest, path, seq, previous);
                self.release(0);
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "write-behind queue is full",
                ))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                restore_latest(&mut latest, path, seq, previous);
                self.release(0);
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "write-behind worker stopped",
                ))
            }
        }
    }

    /// Queue an expiry unlink guarded by the record version observed by the
    /// scan. The disk read and predicate run in this same FIFO worker directly
    /// before unlink, so no queued write can interleave between check and use.
    /// The fixed-size digest participates in both queue and byte reservations.
    fn remove_if_discovery_version(
        &self,
        path: PathBuf,
        expected: [u8; 32],
        now: f64,
    ) -> io::Result<bool> {
        let mut latest = self.state.latest.lock().unwrap_or_else(|e| e.into_inner());
        match latest.get(&path).map(|(_, value)| value) {
            Some(LatestValue::Write(_)) => return Ok(false),
            Some(LatestValue::Remove) => return Ok(true),
            Some(LatestValue::ConditionalRemove(queued)) if *queued == expected => return Ok(true),
            _ => {}
        }

        let bytes = expected.len();
        if !self.reserve(bytes) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "write-behind queue or byte budget is full",
            ));
        }
        let seq = self
            .state
            .next_seq
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let previous = latest.insert(
            path.clone(),
            (seq, LatestValue::ConditionalRemove(expected)),
        );
        match self.tx.try_send(Job::ConditionalRemove {
            path: path.clone(),
            expected,
            now,
            seq,
        }) {
            Ok(()) => Ok(true),
            Err(mpsc::TrySendError::Full(_)) => {
                restore_latest(&mut latest, path, seq, previous);
                self.release(bytes);
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "write-behind queue is full",
                ))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                restore_latest(&mut latest, path, seq, previous);
                self.release(bytes);
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "write-behind worker stopped",
                ))
            }
        }
    }

    /// Give back a reservation that never reached the worker. Saturating: a
    /// dead worker resets the counters, and a racing producer must not wrap them.
    fn release(&self, bytes: usize) {
        let _ = self
            .state
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_sub(1))
            });
        let _ = self
            .state
            .pending_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_sub(bytes))
            });
    }

    fn reserve(&self, bytes: usize) -> bool {
        if bytes > MAX_PENDING_BYTES {
            return false;
        }
        let mut current = self.state.pending.load(Ordering::Acquire);
        loop {
            if current >= self.capacity {
                return false;
            }
            match self.state.pending.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        let mut current_bytes = self.state.pending_bytes.load(Ordering::Acquire);
        loop {
            let Some(next_bytes) = current_bytes.checked_add(bytes) else {
                self.release(0);
                return false;
            };
            if next_bytes > MAX_PENDING_BYTES {
                self.release(0);
                return false;
            }
            match self.state.pending_bytes.compare_exchange_weak(
                current_bytes,
                next_bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current_bytes = observed,
            }
        }
    }

    fn enqueue(&self, path: PathBuf, data: Option<Vec<u8>>) -> io::Result<u64> {
        let bytes = data.as_ref().map_or(0, Vec::len);
        if !self.reserve(bytes) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "write-behind queue or byte budget is full",
            ));
        }

        // Serialize sequence assignment, read-your-writes replacement, and
        // channel enqueue. The worker's settle path takes this same mutex, so
        // same-path disk order is identical to the visible latest order.
        let mut latest = self.state.latest.lock().unwrap_or_else(|e| e.into_inner());
        let seq = self
            .state
            .next_seq
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let latest_value = match &data {
            Some(data) => LatestValue::Write(data.clone()),
            None => LatestValue::Remove,
        };
        let previous = latest.insert(path.clone(), (seq, latest_value));
        if data.is_some() {
            self.state
                .failed_writes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&path);
        }
        let job = match data {
            Some(data) => Job::Write {
                path: path.clone(),
                data,
                seq,
            },
            None => Job::Remove {
                path: path.clone(),
                seq,
            },
        };

        #[cfg(test)]
        if let Some((entered, release)) = self
            .enqueue_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = entered.send(());
            let _ = release.recv();
        }

        match self.tx.try_send(job) {
            Ok(()) => Ok(seq),
            Err(mpsc::TrySendError::Full(_)) => {
                restore_latest(&mut latest, path, seq, previous);
                self.release(bytes);
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "write-behind queue is full",
                ))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                restore_latest(&mut latest, path, seq, previous);
                self.release(bytes);
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "write-behind worker stopped",
                ))
            }
        }
    }

    fn read_latest(&self, path: &Path) -> io::Result<Vec<u8>> {
        if self.state.worker_failed.load(Ordering::Acquire) {
            return Err(worker_unavailable());
        }
        let pending = self
            .state
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .map(|(_, value)| value.clone());
        match pending {
            Some(LatestValue::Write(data)) => Ok(data),
            Some(LatestValue::Remove) => Err(io::ErrorKind::NotFound.into()),
            Some(LatestValue::ConditionalRemove(_)) | None => std::fs::read(path),
        }
    }

    #[cfg(test)]
    fn sync(&self) {
        let (done, wait) = mpsc::sync_channel(0);
        if self.tx.send(Job::Barrier(done)).is_ok() {
            let _ = wait.recv();
        }
    }

    #[cfg(test)]
    fn pause_worker(&self) -> SyncSender<()> {
        let (entered, wait_entered) = mpsc::sync_channel(0);
        let (release, wait_release) = mpsc::sync_channel(0);
        self.tx
            .send(Job::Pause {
                entered,
                release: wait_release,
            })
            .expect("pause worker");
        wait_entered.recv().expect("worker entered pause");
        release
    }

    #[cfg(test)]
    fn pause_after_current_queue(&self) -> (SyncSender<()>, Receiver<()>) {
        let (entered, wait_entered) = mpsc::sync_channel(0);
        let (release, wait_release) = mpsc::sync_channel(0);
        self.tx
            .send(Job::Pause {
                entered,
                release: wait_release,
            })
            .expect("pause worker");
        (release, wait_entered)
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        self.state.pending.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn pending_bytes(&self) -> usize {
        self.state.pending_bytes.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn pause_enqueue_before_send(&self) -> (Receiver<()>, SyncSender<()>) {
        let (entered, wait_entered) = mpsc::sync_channel(0);
        let (release, wait_release) = mpsc::sync_channel(0);
        *self.enqueue_hook.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((entered, wait_release));
        (wait_entered, release)
    }
}

fn restore_latest(
    latest: &mut BTreeMap<PathBuf, (u64, LatestValue)>,
    path: PathBuf,
    seq: u64,
    previous: Option<(u64, LatestValue)>,
) {
    if latest
        .get(&path)
        .is_some_and(|(current, _)| *current == seq)
    {
        match previous {
            Some(previous) => {
                latest.insert(path, previous);
            }
            None => {
                latest.remove(&path);
            }
        }
    }
}

fn run(rx: Receiver<Job>, state: Arc<State>) {
    let worker_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for job in rx {
            let (path, seq, bytes, result, is_write, is_conditional_remove) = match job {
                Job::Write { path, data, seq } => {
                    let bytes = data.len();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        write_now(&path, &data)
                    }))
                    .unwrap_or_else(|_| Err(io::Error::other("write-behind write panicked")));
                    (path, seq, bytes, result, true, false)
                }
                Job::Remove { path, seq } => {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        remove_now(&path)
                    }))
                    .unwrap_or_else(|_| Err(io::Error::other("write-behind remove panicked")));
                    (path, seq, 0, result, false, false)
                }
                Job::ConditionalRemove {
                    path,
                    expected,
                    now,
                    seq,
                } => {
                    let bytes = expected.len();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        remove_discovery_if_version_now(&path, expected, now)
                    }))
                    .unwrap_or_else(|_| {
                        Err(io::Error::other("write-behind conditional remove panicked"))
                    });
                    (path, seq, bytes, result, false, true)
                }
                #[cfg(test)]
                Job::Barrier(done) => {
                    let _ = done.send(());
                    continue;
                }
                #[cfg(test)]
                Job::Pause { entered, release } => {
                    let _ = entered.send(());
                    let _ = release.recv();
                    continue;
                }
                #[cfg(test)]
                Job::Panic => panic!("injected worker panic"),
            };
            if let Err(error) = &result {
                tracing::warn!("write-behind operation failed: {error}");
                if is_write {
                    let mut failures = state
                        .failed_writes
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if failures.contains(&path) || failures.len() < MAX_FAILED_WRITES {
                        failures.insert(path.clone());
                    } else {
                        tracing::warn!(
                            "write-behind failure markers are full; retry signal for this path was not retained"
                        );
                    }
                }
            }
            if is_conditional_remove && result.is_ok() {
                // Conditional expiry jobs may have skipped deletion because
                // a newer record reached disk, or may have failed to inspect
                // its version semantically. Invalidate cache after the
                // predicate/file operation and before clearing this job's
                // latest marker. On I/O error, don't force another immediate
                // cleanup attempt; normal cache revalidation provides retry.
                crate::discovery::note_record_job_settled(&path);
            }
            settled(&state, &path, seq);
            state.pending.fetch_sub(1, Ordering::AcqRel);
            state.pending_bytes.fetch_sub(bytes, Ordering::AcqRel);
        }
    }));
    if worker_result.is_err() {
        state.worker_failed.store(true, Ordering::Release);
        state
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        state
            .failed_writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        state.pending.store(0, Ordering::Release);
        state.pending_bytes.store(0, Ordering::Release);
        tracing::error!("write-behind worker panicked; stale in-memory cache cleared");
    }
}

fn settled(state: &State, path: &Path, seq: u64) {
    let mut latest = state.latest.lock().unwrap_or_else(|e| e.into_inner());
    if latest.get(path).is_some_and(|(current, _)| *current == seq) {
        latest.remove(path);
    }
}

fn write_now(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::storage::atomic_write(path, data).map_err(|error| io::Error::other(error.to_string()))
}

fn remove_now(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn remove_discovery_if_version_now(path: &Path, expected: [u8; 32], now: f64) -> io::Result<()> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let Some(record) = leviculum_core::discovery::DiscoveredInterfaceRecord::decode_msgpack(&bytes)
    else {
        // An unreadable or forward-incompatible record is never deleted by an
        // expiry job. A later revalidation can decide what to do with it.
        return Ok(());
    };
    if !record.should_remove(now) {
        return Ok(());
    }
    let actual = crate::discovery::record_version(&record);
    if actual != expected {
        return Ok(());
    }
    remove_now(path)
}

fn worker_unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "background disk writer unavailable",
    )
}

static QUEUE: OnceLock<Option<Writer>> = OnceLock::new();

fn queue() -> Option<&'static Writer> {
    QUEUE
        .get_or_init(|| match Writer::spawn(MAX_PENDING) {
            Ok(writer) => Some(writer),
            Err(error) => {
                tracing::warn!("background disk writer unavailable: {error}");
                None
            }
        })
        .as_ref()
}

/// Start/check the async writer before a caller performs any disk fallback.
/// A failed worker is reported; cache-only persistence is deferred rather than
/// synchronously writing on the engine thread.
pub(crate) fn ensure_worker() -> io::Result<()> {
    match queue() {
        Some(writer) if !writer.state.worker_failed.load(Ordering::Acquire) => Ok(()),
        _ => Err(worker_unavailable()),
    }
}

/// Queue an atomic write. `WouldBlock` means the bounded queue is saturated;
/// `BrokenPipe` means the worker could not start or has failed. Neither case
/// falls back to filesystem I/O on the caller thread.
pub(crate) fn write(path: PathBuf, data: Vec<u8>) -> io::Result<()> {
    match queue() {
        Some(writer) => writer.write(path, data).map(|_| ()),
        None => Err(worker_unavailable()),
    }
}

/// Queue an unlink using the same bounded capacity and ordering as writes.
pub(crate) fn remove(path: PathBuf) -> io::Result<()> {
    match queue() {
        Some(writer) => writer.remove(path).map(|_| ()),
        None => Err(worker_unavailable()),
    }
}

/// Remove only if no newer queued write for this path could be erased.
pub(crate) fn remove_unless_pending_write(path: PathBuf) -> io::Result<bool> {
    match queue() {
        Some(writer) => writer.remove_unless_pending_write(path),
        None => Err(worker_unavailable()),
    }
}

/// Queue a discovery expiry unlink guarded by the scanned record version.
/// `false` means a newer queued write already owns this path; otherwise the
/// bounded worker will compare immediately before deleting.
pub(crate) fn remove_discovery_if_version(
    path: PathBuf,
    expected: [u8; 32],
    now: f64,
) -> io::Result<bool> {
    match queue() {
        Some(writer) => writer.remove_if_discovery_version(path, expected, now),
        None => Err(worker_unavailable()),
    }
}

/// Consume an asynchronous persistence failure so its caller can retry instead
/// of suppressing an update based on a timestamp for data that never reached disk.
pub(crate) fn take_write_failure(path: &Path) -> bool {
    QUEUE.get().and_then(Option::as_ref).is_some_and(|writer| {
        writer
            .state
            .failed_writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(path)
    })
}

/// Read a path through the latest queued state, falling back to disk.
pub(crate) fn read_latest(path: &Path) -> io::Result<Vec<u8>> {
    match QUEUE.get() {
        Some(Some(writer)) => writer.read_latest(path),
        Some(None) => Err(worker_unavailable()),
        None => std::fs::read(path),
    }
}

/// Wait until all jobs queued before this call have settled. Test-only: never
/// call this from the engine thread because worker I/O can block.
#[cfg(test)]
pub(crate) fn sync() {
    if let Some(writer) = queue() {
        writer.sync();
    }
}

#[cfg(test)]
pub(crate) fn pause_global() -> SyncSender<()> {
    queue().expect("test writer thread").pause_worker()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn writer(capacity: usize) -> (Writer, tempfile::TempDir) {
        (
            Writer::spawn(capacity).expect("writer thread"),
            tempfile::tempdir().unwrap(),
        )
    }

    #[test]
    fn writes_and_removes_share_bounded_capacity_and_keep_latest_on_rejection() {
        let (writer, dir) = writer(2);
        let release = writer.pause_worker();
        let path = dir.path().join("ratchet");
        let other = dir.path().join("discovery");

        writer.write(path.clone(), b"first".to_vec()).unwrap();
        writer.remove(other.clone()).unwrap();
        assert_eq!(
            writer
                .write(path.clone(), b"rejected".to_vec())
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            writer.remove(path.clone()).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(writer.read_latest(&path).unwrap(), b"first");
        assert_eq!(
            writer.read_latest(&other).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );

        release.send(()).expect("release worker");
        writer.sync();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        assert!(!other.exists());
    }

    #[test]
    fn remove_then_newer_write_leaves_latest_write_on_disk() {
        let (writer, dir) = writer(4);
        let release = writer.pause_worker();
        let path = dir.path().join("same-path");

        writer.remove(path.clone()).unwrap();
        writer.write(path.clone(), b"new".to_vec()).unwrap();
        assert_eq!(writer.read_latest(&path).unwrap(), b"new");
        release.send(()).expect("release worker");
        writer.sync();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn conditional_expiry_is_coalesced_bounded_and_releases_on_io_error() {
        let (writer, dir) = writer(1);
        let release = writer.pause_worker();
        let path = dir.path().join("directory-is-not-a-record");
        std::fs::create_dir(&path).unwrap();
        let expected = [0x5a; 32];

        assert!(writer
            .remove_if_discovery_version(path.clone(), expected, 10_000.0)
            .unwrap());
        // Same path/version coalesces to one bounded queue slot.
        assert!(writer
            .remove_if_discovery_version(path.clone(), expected, 10_000.0)
            .unwrap());
        assert_eq!(writer.pending(), 1);
        assert_eq!(writer.pending_bytes(), expected.len());
        assert_eq!(
            writer
                .write(dir.path().join("over-cap"), b"x".to_vec())
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(writer.pending(), 1);
        assert_eq!(writer.pending_bytes(), expected.len());

        release.send(()).unwrap();
        writer.sync();
        assert!(path.is_dir(), "read I/O error must never unlink the path");
        assert_eq!(writer.pending(), 0);
        assert_eq!(writer.pending_bytes(), 0);
    }

    #[test]
    fn concurrent_same_path_writes_and_removes_match_linearized_latest() {
        let (writer, dir) = writer(256);
        let path = dir.path().join("contended");
        let expected = Arc::new(Mutex::new((0u64, None::<Vec<u8>>)));
        let mut threads = Vec::new();

        for producer in 0..8u8 {
            let writer = writer.clone();
            let path = path.clone();
            let expected = Arc::clone(&expected);
            threads.push(std::thread::spawn(move || {
                for index in 0..16u8 {
                    let data = if (producer + index) % 3 == 0 {
                        None
                    } else {
                        Some(vec![producer, index])
                    };
                    let seq = writer.enqueue(path.clone(), data.clone()).unwrap();
                    let mut last = expected.lock().unwrap();
                    if seq > last.0 {
                        *last = (seq, data);
                    }
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }

        let last = expected.lock().unwrap().1.clone();
        match &last {
            Some(data) => assert_eq!(writer.read_latest(&path).unwrap(), *data),
            None => assert_eq!(
                writer.read_latest(&path).unwrap_err().kind(),
                io::ErrorKind::NotFound
            ),
        }
        writer.sync();
        match last {
            Some(data) => assert_eq!(std::fs::read(&path).unwrap(), data),
            None => assert!(!path.exists()),
        }
    }

    #[test]
    fn read_latest_sees_queued_state_and_completed_state() {
        let (writer, dir) = writer(2);
        let release = writer.pause_worker();
        let path = dir.path().join("queued");
        writer.write(path.clone(), b"new".to_vec()).unwrap();
        assert_eq!(writer.read_latest(&path).unwrap(), b"new");
        release.send(()).expect("release worker");
        writer.sync();
        assert_eq!(writer.read_latest(&path).unwrap(), b"new");
    }

    #[test]
    fn failed_disk_write_releases_capacity_and_a_later_write_can_retry() {
        let (writer, dir) = writer(2);
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"block").unwrap();
        let path = blocker.join("record");

        // Enqueue succeeds asynchronously; the worker reports the filesystem
        // failure and releases the latest mirror/capacity instead of claiming
        // a durable write.
        writer
            .write(path.clone(), b"first attempt".to_vec())
            .unwrap();
        writer.sync();
        assert!(matches!(
            writer.read_latest(&path).unwrap_err().kind(),
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
        ));

        std::fs::remove_file(blocker).unwrap();
        writer.write(path.clone(), b"retry".to_vec()).unwrap();
        assert_eq!(writer.read_latest(&path).unwrap(), b"retry");
        writer.sync();
        assert_eq!(std::fs::read(&path).unwrap(), b"retry");
        assert_eq!(writer.pending(), 0);
        assert_eq!(writer.pending_bytes(), 0);
    }

    #[test]
    fn byte_budget_rejects_saturation_and_releases_after_success() {
        let (writer, dir) = writer(8);
        let release = writer.pause_worker();
        let path = dir.path().join("large");
        writer
            .write(path.clone(), vec![7; MAX_PENDING_BYTES - 8])
            .unwrap();
        assert_eq!(writer.pending_bytes(), MAX_PENDING_BYTES - 8);
        assert_eq!(
            writer
                .write(dir.path().join("too-large"), vec![8; 16])
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(writer.pending(), 1, "rejected job releases its slot");
        assert_eq!(writer.pending_bytes(), MAX_PENDING_BYTES - 8);
        release.send(()).unwrap();
        writer.sync();
        assert_eq!(writer.pending(), 0);
        assert_eq!(writer.pending_bytes(), 0);
        writer
            .write(dir.path().join("after"), b"released".to_vec())
            .unwrap();
        writer.sync();
        assert_eq!(writer.pending(), 0);
        assert_eq!(writer.pending_bytes(), 0);
    }

    #[test]
    fn failed_write_releases_byte_budget_even_when_disk_operation_errors() {
        let (writer, dir) = writer(2);
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"block").unwrap();
        writer.write(blocker.join("record"), vec![1; 4096]).unwrap();
        writer.sync();
        assert_eq!(writer.pending(), 0);
        assert_eq!(writer.pending_bytes(), 0);
    }

    #[test]
    fn newer_write_survives_settlement_of_an_older_remove() {
        let (writer, dir) = writer(8);
        let first_release = writer.pause_worker();
        let path = dir.path().join("same-path");
        writer.remove(path.clone()).unwrap();
        let (second_release, second_entered) = writer.pause_after_current_queue();
        writer.write(path.clone(), b"newer".to_vec()).unwrap();
        first_release.send(()).unwrap();
        second_entered.recv().unwrap();
        assert_eq!(writer.read_latest(&path).unwrap(), b"newer");
        second_release.send(()).unwrap();
        writer.sync();
        assert_eq!(std::fs::read(&path).unwrap(), b"newer");
        assert_eq!(writer.pending(), 0);
    }

    #[test]
    fn enqueue_sequence_and_channel_send_are_serialized_under_latest_lock() {
        let (writer, dir) = writer(8);
        let path = dir.path().join("serialized");
        let (entered, release) = writer.pause_enqueue_before_send();
        let first_writer = writer.clone();
        let first_path = path.clone();
        let first = std::thread::spawn(move || first_writer.write(first_path, b"first".to_vec()));
        entered.recv().unwrap();

        let (done, wait_done) = mpsc::sync_channel(1);
        let second_writer = writer.clone();
        let second_path = path.clone();
        let second = std::thread::spawn(move || {
            let result = second_writer.write(second_path, b"second".to_vec());
            let _ = done.send(());
            result
        });
        assert!(
            wait_done
                .recv_timeout(std::time::Duration::from_millis(30))
                .is_err(),
            "another enqueue cannot pass sequence/latest/send while the first is paused"
        );
        release.send(()).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        writer.sync();
        assert_eq!(std::fs::read(path).unwrap(), b"second");
        assert_eq!(writer.pending(), 0);
        assert_eq!(writer.pending_bytes(), 0);
    }

    #[test]
    fn conditional_remove_does_not_queue_behind_a_newer_write() {
        let (writer, dir) = writer(4);
        let release = writer.pause_worker();
        let path = dir.path().join("record");
        writer.write(path.clone(), b"fresh".to_vec()).unwrap();
        assert!(!writer.remove_unless_pending_write(path.clone()).unwrap());
        assert_eq!(writer.read_latest(&path).unwrap(), b"fresh");
        release.send(()).unwrap();
        writer.sync();
        assert_eq!(std::fs::read(path).unwrap(), b"fresh");
        assert_eq!(writer.pending(), 0);
    }

    #[test]
    fn unexpected_worker_panic_clears_stale_mirror_and_accounting() {
        let (writer, dir) = writer(4);
        let release = writer.pause_worker();
        let path = dir.path().join("queued-before-worker-panic");
        writer
            .tx
            .try_send(Job::Panic)
            .expect("inject panic after pause");
        writer
            .write(path.clone(), b"not persisted".to_vec())
            .unwrap();
        assert_eq!(writer.read_latest(&path).unwrap(), b"not persisted");
        release.send(()).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while writer.pending() != 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(writer.pending(), 0);
        assert_eq!(writer.pending_bytes(), 0);
        assert_eq!(
            writer.read_latest(&path).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe,
            "worker panic fails closed without a synchronous disk read"
        );
        assert_eq!(
            writer.write(path, b"retry".to_vec()).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
