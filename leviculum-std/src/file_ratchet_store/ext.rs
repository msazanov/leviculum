use std::path::Path;

use super::FileRatchetStore;
use super::{hex_decode, TRUNCATED_HASHBYTES};

#[cfg(unix)]
pub(super) fn secure_existing_private_dir(dir: &Path) -> std::io::Result<()> {
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
pub(super) fn read_private_key_file(dir: &Path, name: &str) -> std::io::Result<Option<Vec<u8>>> {
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
pub(super) fn secure_existing_private_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn read_private_key_file(dir: &Path, name: &str) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(dir.join(name)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

impl FileRatchetStore {
    /// Upgrade only our private ratchet-key directory and its recognized key
    /// files. Open the directory without following the final symlink, then use
    /// openat(O_NOFOLLOW) for entries so a symlink or arbitrary filename can
    /// never redirect chmod outside this store.
    pub(super) fn secure_existing_ratchetkeys(&mut self) -> std::io::Result<()> {
        if self.permissions_checked {
            return Ok(());
        }
        if !self.ratchetkeys_dir.exists() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            super::secure_existing_private_dir(&self.ratchetkeys_dir)?;
        }
        #[cfg(not(unix))]
        {}
        self.permissions_checked = true;
        Ok(())
    }

    /// Create a missing store directory owner-only. Existing directories are
    /// checked without following a symlink; private `ratchetkeys/` receives a
    /// separate narrow permission migration.
    pub(super) fn ensure_dir(dir: &Path) -> std::io::Result<()> {
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
}
