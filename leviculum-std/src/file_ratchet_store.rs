//! File-backed ratchet store (msgpack per file, Python-compatible).
//!
//! Known ratchets: one file per destination in `{storage}/ratchets/`
//! Dest ratchet keys: one file per destination in `{storage}/ratchetkeys/`
//! Filenames are hex-encoded truncated destination hashes.

use std::path::{Path, PathBuf};

use leviculum_core::constants::{RATCHET_SIZE, TRUNCATED_HASHBYTES};
use leviculum_core::ratchet_store::{KnownRatchetEntry, RatchetStore};

use crate::error::Error;
use crate::storage::{atomic_write_secret, hex_decode, hex_encode, note_unreadable_entry};
use crate::write_behind;

pub(crate) const RATCHETS_DIR: &str = "ratchets";
pub(crate) const RATCHETKEYS_DIR: &str = "ratchetkeys";

fn encode_known_ratchet(ratchet_pub: &[u8; RATCHET_SIZE], received_secs: f64) -> Vec<u8> {
    let map = rmpv::Value::Map(vec![
        (
            rmpv::Value::String("ratchet".into()),
            rmpv::Value::Binary(ratchet_pub.to_vec()),
        ),
        (
            rmpv::Value::String("received".into()),
            rmpv::Value::F64(received_secs),
        ),
    ]);
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &map).expect("Vec write cannot fail");
    buf
}

fn decode_known_ratchet(data: &[u8]) -> Option<([u8; RATCHET_SIZE], f64)> {
    let value = rmpv::decode::read_value(&mut &data[..]).ok()?;
    let map = value.as_map()?;

    let mut ratchet: Option<[u8; RATCHET_SIZE]> = None;
    let mut received: Option<f64> = None;

    for (k, v) in map {
        let key_str = k.as_str()?;
        match key_str {
            "ratchet" => {
                let bytes = v.as_slice()?;
                if bytes.len() != RATCHET_SIZE {
                    return None;
                }
                let mut arr = [0u8; RATCHET_SIZE];
                arr.copy_from_slice(bytes);
                ratchet = Some(arr);
            }
            "received" => {
                received = Some(v.as_f64()?);
            }
            _ => {}
        }
    }

    Some((ratchet?, received?))
}

pub(crate) struct FileRatchetStore {
    ratchets_dir: PathBuf,
    ratchetkeys_dir: PathBuf,
    permissions_checked: bool,
}

impl FileRatchetStore {
    pub(crate) fn new(storage_dir: &Path) -> Self {
        Self {
            ratchets_dir: storage_dir.join(RATCHETS_DIR),
            ratchetkeys_dir: storage_dir.join(RATCHETKEYS_DIR),
            permissions_checked: false,
        }
    }

    /// Delete a known ratchet file (for expiry). Not part of the trait.
    /// Удаление уходит в фоновую очередь: на рабочем потоке движка не ждём диск (write_behind.rs).
    pub(crate) fn delete_known_ratchet(
        &self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
    ) -> core::result::Result<(), Error> {
        let hex_name = hex_encode(dest_hash);
        write_behind::remove(self.ratchets_dir.join(&hex_name))
            .map_err(|e| Error::Storage(format!("known ratchet removal queue: {e}")))
    }

    /// Create a missing store directory owner-only. Existing directories are
    /// checked without following a symlink; private `ratchetkeys/` receives a
    /// separate narrow permission migration below.
    fn ensure_dir(dir: &Path) -> std::io::Result<()> {
        match std::fs::symlink_metadata(dir) {
            Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
            Ok(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ratchet store path is not a real directory",
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::storage::create_secret_dir_all(dir)
            }
            Err(error) => Err(error),
        }
    }

    /// Upgrade only our private ratchet-key directory and its recognized key
    /// files. Open the directory without following the final symlink, then use
    /// openat(O_NOFOLLOW) for entries so a symlink or arbitrary filename can
    /// never redirect chmod outside this store.
    fn secure_existing_ratchetkeys(&mut self) -> std::io::Result<()> {
        if self.permissions_checked {
            return Ok(());
        }
        if !self.ratchetkeys_dir.exists() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            secure_existing_private_dir(&self.ratchetkeys_dir)?;
        }
        #[cfg(not(unix))]
        {}
        self.permissions_checked = true;
        Ok(())
    }

    fn write(path: &Path, data: &[u8]) -> core::result::Result<(), Error> {
        atomic_write_secret(path, data)
            .map_err(|e| Error::Storage(format!("{}: {e}", path.display())))
    }
}

#[cfg(unix)]
fn secure_existing_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let before = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !before.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ratchetkeys path is not a real directory",
        ));
    }
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)?;
    let opened = directory.metadata()?;
    let owner = unsafe { libc::geteuid() };
    if !opened.is_dir()
        || before.dev() != opened.dev()
        || before.ino() != opened.ino()
        || opened.uid() != owner
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "ratchetkeys directory is not an owned directory",
        ));
    }

    // Tighten the directory first, so other users cannot race the migration.
    if opened.mode() & 0o777 != 0o700 {
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !hex_decode(&name).is_some_and(|bytes| bytes.len() == TRUNCATED_HASHBYTES) {
            continue;
        }
        match open_private_key_file_at(&directory, &name) {
            Ok(Some(file)) => {
                let metadata = match file.metadata() {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        tracing::warn!("ratchetkeys: cannot inspect key file {name}: {error}");
                        continue;
                    }
                };
                if metadata.nlink() == 1 && metadata.mode() & 0o777 != 0o600 {
                    if let Err(error) = file.set_permissions(std::fs::Permissions::from_mode(0o600)) {
                        tracing::warn!("ratchetkeys: cannot secure key file {name}: {error}");
                    }
                }
            }
            Ok(None) => tracing::warn!("ratchetkeys: refusing unsafe key file {name} (symlink, hardlink, owner, or file type check)"),
            Err(error) => tracing::warn!("ratchetkeys: cannot inspect key file {name}: {error}"),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn open_private_key_file_at(
    directory: &std::fs::File,
    name: &str,
) -> std::io::Result<Option<std::fs::File>> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;

    let name = CString::new(name).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid key filename")
    })?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::ENOENT) | Some(libc::ELOOP) | Some(libc::ENXIO)
        ) {
            return Ok(None);
        }
        return Err(error);
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Ok(None);
    }
    Ok(Some(file))
}

#[cfg(unix)]
fn read_private_key_file(dir: &Path, name: &str) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)?;
    let Some(mut file) = open_private_key_file_at(&directory, name)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

#[cfg(not(unix))]
fn secure_existing_private_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn read_private_key_file(dir: &Path, name: &str) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(dir.join(name)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

impl RatchetStore for FileRatchetStore {
    type Error = Error;

    fn load_known_ratchets(
        &mut self,
    ) -> core::result::Result<Vec<([u8; TRUNCATED_HASHBYTES], KnownRatchetEntry)>, Error> {
        let dir = match std::fs::read_dir(&self.ratchets_dir) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Storage(format!("ratchets dir: {e}"))),
        };

        let mut entries = Vec::new();
        for entry in dir.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => {
                    note_unreadable_entry(&self.ratchets_dir, &entry.file_name());
                    continue;
                }
            };
            // Skip temp files
            if name.ends_with(".tmp") || name.ends_with(".out") {
                continue;
            }

            let hash_bytes = match hex_decode(&name) {
                Some(b) if b.len() == TRUNCATED_HASHBYTES => {
                    let mut arr = [0u8; TRUNCATED_HASHBYTES];
                    arr.copy_from_slice(&b);
                    arr
                }
                _ => {
                    note_unreadable_entry(&self.ratchets_dir, &entry.file_name());
                    continue;
                }
            };

            let data = match std::fs::read(entry.path()) {
                Ok(d) => d,
                Err(_) => continue,
            };

            match decode_known_ratchet(&data) {
                Some((ratchet, received_secs)) => {
                    entries.push((
                        hash_bytes,
                        KnownRatchetEntry {
                            ratchet,
                            received_at_secs: received_secs,
                        },
                    ));
                }
                None => {
                    // Corrupted file, delete it
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        Ok(entries)
    }

    fn save_known_ratchet(
        &mut self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
        entry: &KnownRatchetEntry,
    ) -> core::result::Result<(), Error> {
        Self::ensure_dir(&self.ratchets_dir)
            .map_err(|e| Error::Storage(format!("ratchets dir: {e}")))?;
        let hex_name = hex_encode(dest_hash);
        let data = encode_known_ratchet(&entry.ratchet, entry.received_at_secs);
        // Изменившийся ratchet приходит из анонсов на рабочем потоке движка: запись - в фоновом потоке,
        // иначе создание файла ждёт журнал диска и останавливает все Link (write_behind.rs).
        write_behind::write(self.ratchets_dir.join(&hex_name), data)
            .map_err(|e| Error::Storage(format!("known ratchet write queue: {e}")))
    }

    fn load_dest_ratchet_keys(
        &mut self,
    ) -> core::result::Result<Vec<([u8; TRUNCATED_HASHBYTES], Vec<u8>)>, Error> {
        self.secure_existing_ratchetkeys()
            .map_err(|e| Error::Storage(format!("ratchetkeys permissions: {e}")))?;
        let dir = match std::fs::read_dir(&self.ratchetkeys_dir) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Storage(format!("ratchetkeys dir: {e}"))),
        };

        let mut entries = Vec::new();
        for entry in dir.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => {
                    note_unreadable_entry(&self.ratchetkeys_dir, &entry.file_name());
                    continue;
                }
            };
            if name.ends_with(".tmp") {
                continue;
            }

            let hash_bytes = match hex_decode(&name) {
                Some(b) if b.len() == TRUNCATED_HASHBYTES => {
                    let mut arr = [0u8; TRUNCATED_HASHBYTES];
                    arr.copy_from_slice(&b);
                    arr
                }
                _ => {
                    note_unreadable_entry(&self.ratchetkeys_dir, &entry.file_name());
                    continue;
                }
            };

            let data = match read_private_key_file(&self.ratchetkeys_dir, &name) {
                Ok(Some(d)) if !d.is_empty() => d,
                Ok(Some(_)) => {
                    let _ = std::fs::remove_file(entry.path());
                    continue;
                }
                Ok(None) => {
                    tracing::warn!("ratchetkeys: refusing unsafe key file {name} (symlink, hardlink, owner, or file type check)");
                    continue;
                }
                Err(error) => {
                    tracing::warn!("ratchetkeys: cannot read key file {name}: {error}");
                    continue;
                }
            };

            entries.push((hash_bytes, data));
        }

        Ok(entries)
    }

    fn save_dest_ratchet_keys(
        &mut self,
        dest_hash: &[u8; TRUNCATED_HASHBYTES],
        serialized: &[u8],
    ) -> core::result::Result<(), Error> {
        Self::ensure_dir(&self.ratchetkeys_dir)
            .map_err(|e| Error::Storage(format!("ratchetkeys dir: {e}")))?;
        self.secure_existing_ratchetkeys()
            .map_err(|e| Error::Storage(format!("ratchetkeys permissions: {e}")))?;
        let hex_name = hex_encode(dest_hash);
        #[cfg(unix)]
        if self.ratchetkeys_dir.join(&hex_name).exists()
            && !read_private_key_file(&self.ratchetkeys_dir, &hex_name)
                .map_err(|e| Error::Storage(format!("ratchetkeys key preflight {hex_name}: {e}")))?
                .is_some()
        {
            return Err(Error::Storage(format!(
                "ratchetkeys key preflight {hex_name}: refusing to replace unsafe existing key"
            )));
        }
        Self::write(&self.ratchetkeys_dir.join(&hex_name), serialized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codeberg #336: both ratchet directories are enumerated on every
    /// restore, and both used to panic inside the hex decode on a name the
    /// store did not write — a four-byte character has an even length, so
    /// the length check let it through and the slice landed inside it.
    /// Skip it, keep the entries that are ours, and stay up.
    #[test]
    fn a_foreign_filename_is_skipped_rather_than_fatal() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FileRatchetStore::new(dir.path());
        FileRatchetStore::ensure_dir(&store.ratchets_dir).unwrap();
        FileRatchetStore::ensure_dir(&store.ratchetkeys_dir).unwrap();

        let dest = [0xA7u8; TRUNCATED_HASHBYTES];
        store
            .save_known_ratchet(
                &dest,
                &KnownRatchetEntry {
                    ratchet: [0x42; RATCHET_SIZE],
                    received_at_secs: 1_700_000_000.0,
                },
            )
            .expect("save ratchet");
        store
            .save_dest_ratchet_keys(&dest, b"private ratchet key")
            .expect("save ratchet key");
        crate::write_behind::sync();

        for target in [&store.ratchets_dir, &store.ratchetkeys_dir] {
            std::fs::write(target.join("\u{1F600}"), b"not ours").expect("stray file");
            let raw = std::ffi::OsStr::from_bytes(b"\xff\xfe").to_owned();
            std::fs::write(target.join(raw), b"not ours either").expect("stray file");
        }

        crate::write_behind::sync();
        let known = store.load_known_ratchets().expect("load ratchets");
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].0, dest);
        assert_eq!(known[0].1.ratchet, [0x42; RATCHET_SIZE]);

        let keys = store.load_dest_ratchet_keys().expect("load ratchet keys");
        assert_eq!(keys, vec![(dest, b"private ratchet key".to_vec())]);
    }

    /// L-0039: a ratchet key is a private key, so its file and the
    /// directories the store creates for it are owner-only; they used to
    /// inherit the umask (0644 files, 0755 directories on Debian).
    #[cfg(unix)]
    #[test]
    fn ratchet_files_and_dirs_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FileRatchetStore::new(dir.path());
        let dest = [0x5Cu8; TRUNCATED_HASHBYTES];
        store
            .save_known_ratchet(
                &dest,
                &KnownRatchetEntry {
                    ratchet: [0x42; RATCHET_SIZE],
                    received_at_secs: 1_700_000_000.0,
                },
            )
            .expect("save ratchet");
        store
            .save_dest_ratchet_keys(&dest, b"private ratchet key")
            .expect("save ratchet key");

        let name = hex_encode(&dest);
        let mode = |p: &Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
        for (path, want) in [
            (store.ratchetkeys_dir.join(&name), 0o600),
            (store.ratchets_dir.clone(), 0o700),
            (store.ratchetkeys_dir.clone(), 0o700),
        ] {
            let got = mode(&path);
            println!("{}: mode {got:o}", path.display());
            assert_eq!(got, want, "{} mode {got:o}", path.display());
        }
    }

    /// Upgrade only owned hash-named key files and the real ratchetkeys
    /// directory. Foreign filenames and symlinks must not be chmodded or read.
    #[cfg(unix)]
    #[test]
    fn existing_private_store_permissions_are_migrated_without_following_links() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(RATCHETKEYS_DIR);
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let owned_hash = [0x31; TRUNCATED_HASHBYTES];
        let owned_key = dir.join(hex_encode(&owned_hash));
        std::fs::write(&owned_key, b"owned private key").unwrap();
        std::fs::set_permissions(&owned_key, std::fs::Permissions::from_mode(0o644)).unwrap();

        let foreign_name = dir.join("operator-note");
        std::fs::write(&foreign_name, b"not a ratchet key").unwrap();
        std::fs::set_permissions(&foreign_name, std::fs::Permissions::from_mode(0o644)).unwrap();

        let victim = root.path().join("outside-private-file");
        std::fs::write(&victim, b"leave this alone").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
        let linked_hash = [0x32; TRUNCATED_HASHBYTES];
        symlink(&victim, dir.join(hex_encode(&linked_hash))).unwrap();
        let hardlinked_hash = [0x33; TRUNCATED_HASHBYTES];
        std::fs::hard_link(&victim, dir.join(hex_encode(&hardlinked_hash))).unwrap();

        let mut store = FileRatchetStore::new(root.path());
        let keys = store.load_dest_ratchet_keys().unwrap();
        assert_eq!(keys, vec![(owned_hash, b"owned private key".to_vec())]);
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&owned_key), 0o600);
        assert_eq!(mode(&foreign_name), 0o644);
        assert_eq!(mode(&victim), 0o644);
        assert!(
            std::fs::symlink_metadata(dir.join(hex_encode(&linked_hash)))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(dir.join(hex_encode(&hardlinked_hash)).exists());
        assert!(
            store
                .save_dest_ratchet_keys(&hardlinked_hash, b"replacement")
                .is_err(),
            "an unsafe existing private key must not be silently replaced"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_permissions_migrate_once_and_leave_already_secure_key_ctime_alone() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(RATCHETKEYS_DIR);
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = [0x41; TRUNCATED_HASHBYTES];
        let first_path = dir.join(hex_encode(&first));
        std::fs::write(&first_path, b"private key").unwrap();
        std::fs::set_permissions(&first_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut store = FileRatchetStore::new(root.path());
        store.load_dest_ratchet_keys().unwrap();
        let ctime = std::fs::metadata(&first_path).unwrap().ctime_nsec();
        store
            .save_dest_ratchet_keys(&[0x42; TRUNCATED_HASHBYTES], b"another key")
            .unwrap();
        assert_eq!(
            std::fs::metadata(&first_path).unwrap().ctime_nsec(),
            ctime,
            "saving another destination must not re-chmod every existing key"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_store_directory_symlink_is_rejected_without_chmodding_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let victim = target.path().join(hex_encode(&[0xA4; TRUNCATED_HASHBYTES]));
        std::fs::write(&victim, b"not our file").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(target.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink(target.path(), root.path().join(RATCHETKEYS_DIR)).unwrap();

        let mut store = FileRatchetStore::new(root.path());
        assert!(store.load_dest_ratchet_keys().is_err());
        assert_eq!(
            std::fs::metadata(target.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}
