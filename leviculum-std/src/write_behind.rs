//! Запись на диск вне рабочего потока движка.
//!
//! Движок узла работает на одном рабочем потоке (`reticulum-node`, см. driver): блокирующий вызов в нём
//! останавливает обработку всех Link, включая ответы на пинги. Сохранение изменившегося ratchet, запись и
//! удаление записей discovery создают или переименовывают файлы. На диске, где журнал ext4 ждёт коммита,
//! такой вызов занимает секунды (fin1, W72: поток стоит в D-состоянии до ~3,9 с на `rename` в `ratchets/`).
//! Поэтому эти операции ставятся в очередь одного фонового потока: порядок сохраняется, значение в памяти
//! уже актуально, на диск попадает с задержкой. Чтение состояния, которое ещё не записано на диск, идёт
//! через [`read_latest`]. При аварийной остановке последние записи могут не попасть на диск: ratchet заново
//! выучится из следующего анонса, запись discovery обновится при следующем анонсе.
//!
//! Фоновый поток живёт до конца процесса. Если создать его не удалось, запись выполняется синхронно.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

/// Предел ожидающих записей. При очень медленном диске очередь не растёт без границы:
/// лишняя запись пропускается с предупреждением (значение в памяти остаётся актуальным).
const MAX_PENDING: usize = 4096;

enum Job {
    /// `seq` - порядковый номер постановки: по нему состояние пути сбрасывается, только если оно не сменилось.
    Write { path: PathBuf, data: Vec<u8>, seq: u64 },
    Remove { path: PathBuf, seq: u64 },
    /// Ответить, когда все задания, поставленные раньше, выполнены (только для `sync` в тестах).
    #[cfg(test)]
    Barrier(Sender<()>),
}

static PENDING: AtomicUsize = AtomicUsize::new(0);
static SEQ: AtomicU64 = AtomicU64::new(0);
static QUEUE: OnceLock<Option<Sender<Job>>> = OnceLock::new();
/// Последнее поставленное состояние каждого пути: `Some(данные)` - запись, `None` - удаление.
/// Пока задание не выполнено, `read_latest` отдаёт это состояние, а не старый файл.
static LATEST: Mutex<BTreeMap<PathBuf, (u64, Option<Vec<u8>>)>> = Mutex::new(BTreeMap::new());

fn latest() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, (u64, Option<Vec<u8>>)>> {
    LATEST.lock().unwrap_or_else(|e| e.into_inner())
}

fn queue() -> Option<&'static Sender<Job>> {
    QUEUE
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            match std::thread::Builder::new().name("leviculum-writer".into()).spawn(move || run(rx)) {
                Ok(_) => Some(tx),
                Err(e) => {
                    tracing::warn!("фоновая запись на диск недоступна, запись синхронная: {e}");
                    None
                }
            }
        })
        .as_ref()
}

fn run(rx: mpsc::Receiver<Job>) {
    for job in rx {
        match job {
            Job::Write { path, data, seq } => {
                PENDING.fetch_sub(1, Relaxed);
                write_now(&path, &data);
                settled(&path, seq);
            }
            Job::Remove { path, seq } => {
                PENDING.fetch_sub(1, Relaxed);
                remove_now(&path);
                settled(&path, seq);
            }
            #[cfg(test)]
            Job::Barrier(done) => {
                let _ = done.send(());
            }
        }
    }
}

/// Задание по пути выполнено: если это последнее поставленное состояние, оно больше не «ожидает».
fn settled(path: &Path, seq: u64) {
    let mut m = latest();
    if m.get(path).is_some_and(|(s, _)| *s == seq) {
        m.remove(path);
    }
}

fn write_now(path: &Path, data: &[u8]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = crate::storage::atomic_write(path, data) {
        tracing::warn!("запись на диск не выполнена: {e}");
    }
}

fn remove_now(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != io::ErrorKind::NotFound {
            tracing::warn!("удаление с диска не выполнено: {e}");
        }
    }
}

/// Поставить атомарную запись файла в очередь. Не ждёт диск, кроме случая без фонового потока.
pub(crate) fn write(path: PathBuf, data: Vec<u8>) {
    let Some(tx) = queue() else {
        write_now(&path, &data);
        return;
    };
    if PENDING.load(Relaxed) >= MAX_PENDING {
        tracing::warn!("очередь записи на диск полна, запись пропущена");
        return;
    }
    let seq = SEQ.fetch_add(1, Relaxed);
    latest().insert(path.clone(), (seq, Some(data.clone())));
    PENDING.fetch_add(1, Relaxed);
    if let Err(mpsc::SendError(job)) = tx.send(Job::Write { path, data, seq }) {
        // Поток уже не работает: записываем сами, значение не теряем.
        PENDING.fetch_sub(1, Relaxed);
        if let Job::Write { path, data, seq } = job {
            write_now(&path, &data);
            settled(&path, seq);
        }
    }
}

/// Поставить удаление файла в очередь (после ранее поставленных записей того же пути). Отсутствие файла - не ошибка.
pub(crate) fn remove(path: PathBuf) {
    let Some(tx) = queue() else {
        remove_now(&path);
        return;
    };
    let seq = SEQ.fetch_add(1, Relaxed);
    latest().insert(path.clone(), (seq, None));
    PENDING.fetch_add(1, Relaxed);
    if let Err(mpsc::SendError(job)) = tx.send(Job::Remove { path, seq }) {
        PENDING.fetch_sub(1, Relaxed);
        if let Job::Remove { path, seq } = job {
            remove_now(&path);
            settled(&path, seq);
        }
    }
}

/// Прочитать файл с учётом очереди: если по пути есть невыполненная запись, вернуть её данные;
/// если удаление - `NotFound`; иначе прочитать диск.
pub(crate) fn read_latest(path: &Path) -> io::Result<Vec<u8>> {
    let pending = latest().get(path).map(|(_, data)| data.clone());
    match pending {
        Some(Some(data)) => Ok(data),
        Some(None) => Err(io::ErrorKind::NotFound.into()),
        None => std::fs::read(path),
    }
}

/// Дождаться выполнения всех записей и удалений, поставленных до вызова. Только для тестов:
/// на рабочем потоке движка такой вызов снова блокирует его на диске.
#[cfg(test)]
pub(crate) fn sync() {
    let Some(tx) = queue() else { return };
    let (done, wait) = mpsc::channel();
    if tx.send(Job::Barrier(done)).is_ok() {
        let _ = wait.recv();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_writes_land_in_order_and_last_value_wins() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("ratchet");
        write(p.clone(), b"first".to_vec());
        write(p.clone(), b"second".to_vec());
        sync();
        assert_eq!(std::fs::read(&p).unwrap(), b"second");
        assert!(!p.with_extension("tmp").exists(), "temp file must be renamed away");
    }

    #[test]
    fn remove_after_write_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gone");
        write(p.clone(), b"value".to_vec());
        remove(p.clone());
        sync();
        assert!(!p.exists());
    }

    #[test]
    fn removing_a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        remove(dir.path().join("never-written"));
        sync();
    }

    #[test]
    fn read_latest_sees_queued_state_before_it_reaches_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("queued");
        write(p.clone(), b"new".to_vec());
        // Состояние либо ещё в очереди (тогда данные из памяти), либо уже на диске: в обоих случаях "new".
        assert_eq!(read_latest(&p).unwrap(), b"new");
        sync();
        assert_eq!(read_latest(&p).unwrap(), b"new");
    }

    #[test]
    fn read_latest_reports_a_queued_removal_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doomed");
        write(p.clone(), b"old".to_vec());
        sync();
        remove(p.clone());
        assert_eq!(read_latest(&p).unwrap_err().kind(), io::ErrorKind::NotFound);
        sync();
        assert!(!p.exists());
    }
}
