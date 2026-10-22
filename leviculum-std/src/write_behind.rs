//! Bounded disk writes outside the node engine thread.
//!
//! Announce processing must not wait for filesystem latency. Public known
//! ratchets and discovery records are therefore serialized by one worker. The
//! bounded sync channel and atomic outstanding-job reservation apply equally to
//! writes and removals. If saturated, an operation fails promptly without
//! changing the read-your-writes view or doing disk I/O on the caller thread.
//! Private ratchet keys do not use this module; they remain synchronous through
//! `atomic_write_secret`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};

const MAX_PENDING: usize = 4096;

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
    #[cfg(test)]
    Barrier(SyncSender<()>),
    #[cfg(test)]
    Pause {
        entered: SyncSender<()>,
        release: Receiver<()>,
    },
}

#[derive(Default)]
struct State {
    /// Last queued state per path, including the in-flight write. It is
    /// removed only when the matching job has settled.
    latest: Mutex<BTreeMap<PathBuf, (u64, Option<Vec<u8>>)>>,
    pending: AtomicUsize,
    next_seq: AtomicU64,
}

#[derive(Clone)]
struct Writer {
    tx: SyncSender<Job>,
    state: Arc<State>,
    capacity: usize,
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
        })
    }

    fn write(&self, path: PathBuf, data: Vec<u8>) -> io::Result<u64> {
        self.enqueue(path, Some(data))
    }

    fn remove(&self, path: PathBuf) -> io::Result<u64> {
        self.enqueue(path, None)
    }

    fn reserve(&self) -> bool {
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
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn enqueue(&self, path: PathBuf, data: Option<Vec<u8>>) -> io::Result<u64> {
        if !self.reserve() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "write-behind queue is full",
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
        let previous = latest.insert(path.clone(), (seq, data.clone()));
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

        match self.tx.try_send(job) {
            Ok(()) => Ok(seq),
            Err(mpsc::TrySendError::Full(_)) => {
                restore_latest(&mut latest, path, seq, previous);
                self.state.pending.fetch_sub(1, Ordering::AcqRel);
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "write-behind queue is full",
                ))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                restore_latest(&mut latest, path, seq, previous);
                self.state.pending.fetch_sub(1, Ordering::AcqRel);
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "write-behind worker stopped",
                ))
            }
        }
    }

    fn read_latest(&self, path: &Path) -> io::Result<Vec<u8>> {
        let pending = self
            .state
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .map(|(_, data)| data.clone());
        match pending {
            Some(Some(data)) => Ok(data),
            Some(None) => Err(io::ErrorKind::NotFound.into()),
            None => std::fs::read(path),
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
}

fn restore_latest(
    latest: &mut BTreeMap<PathBuf, (u64, Option<Vec<u8>>)>,
    path: PathBuf,
    seq: u64,
    previous: Option<(u64, Option<Vec<u8>>)>,
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
    for job in rx {
        let (path, seq, result) = match job {
            Job::Write { path, data, seq } => {
                let result = write_now(&path, &data);
                (path, seq, result)
            }
            Job::Remove { path, seq } => {
                let result = remove_now(&path);
                (path, seq, result)
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
        };
        if let Err(error) = result {
            tracing::warn!("write-behind operation failed: {error}");
        }
        settled(&state, &path, seq);
        state.pending.fetch_sub(1, Ordering::AcqRel);
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

static QUEUE: OnceLock<Option<Writer>> = OnceLock::new();

fn queue() -> Option<&'static Writer> {
    QUEUE
        .get_or_init(|| match Writer::spawn(MAX_PENDING) {
            Ok(writer) => Some(writer),
            Err(error) => {
                tracing::warn!(
                    "background disk writer unavailable; using synchronous writes: {error}"
                );
                None
            }
        })
        .as_ref()
}

/// Queue an atomic write. `WouldBlock` means the bounded queue is saturated;
/// the state was not committed and the caller may retry on a later update.
/// Synchronous fallback is used only when the worker thread could not start.
pub(crate) fn write(path: PathBuf, data: Vec<u8>) -> io::Result<()> {
    match queue() {
        Some(writer) => writer.write(path, data).map(|_| ()),
        None => write_now(&path, &data),
    }
}

/// Queue an unlink using the same bounded capacity and ordering as writes.
pub(crate) fn remove(path: PathBuf) -> io::Result<()> {
    match queue() {
        Some(writer) => writer.remove(path).map(|_| ()),
        None => remove_now(&path),
    }
}

/// Read a path through the latest queued state, falling back to disk.
pub(crate) fn read_latest(path: &Path) -> io::Result<Vec<u8>> {
    match QUEUE.get().and_then(Option::as_ref) {
        Some(writer) => writer.read_latest(path),
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
    }
}
