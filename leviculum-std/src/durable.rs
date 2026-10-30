//! The one durable-write path the propagation stores share, and the seam
//! under its `fsync` (order 1081).
//!
//! A durable write is: write a temp file, `fsync` it, rename it into place,
//! `fsync` the directory. After it returns the bytes survive a power cut, and
//! a cut at any earlier point leaves only the temp file, which the stores'
//! reopen ignores. The two `fsync`s are what the write costs: 1.0 to 1.8 ms
//! per message append on the coder host's ext4, 3 to 9 ms on the Odroid that
//! runs the field node (order 1009), which is why neither store runs this
//! path under the core lock any more.
//!
//! [`Fsync`] is the seam a test uses to make the device slow, or to make the
//! writer die between the write and the rename, without a slow device or a
//! killed process.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use leviculum_lxmf::storage::StorageError;

/// Makes a written file, or a directory's entries, durable.
pub trait Fsync: Send + Sync + 'static {
    /// `fsync` this handle: a file's bytes, or a directory's entries.
    fn sync(&self, handle: &File) -> std::io::Result<()>;
}

/// The device's own `fsync`, which every production store uses.
pub struct DeviceFsync;

impl Fsync for DeviceFsync {
    fn sync(&self, handle: &File) -> std::io::Result<()> {
        handle.sync_all()
    }
}

/// The production seam, shared.
pub fn device_fsync() -> Arc<dyn Fsync> {
    Arc::new(DeviceFsync)
}

/// Write `body` to `path` durably, through `temp`.
///
/// The directory `fsync` is best effort, as it always was here: a filesystem
/// that refuses to open a directory for syncing still renamed the file, and
/// failing the write for it would refuse every message on such a host.
pub(crate) fn write_durably(
    temp: &Path,
    path: &Path,
    body: &[u8],
    fsync: &dyn Fsync,
) -> Result<(), StorageError> {
    let mut file = File::create(temp).map_err(|_| StorageError::Io)?;
    file.write_all(body).map_err(|_| StorageError::Io)?;
    fsync.sync(&file).map_err(|_| StorageError::Io)?;
    drop(file);
    std::fs::rename(temp, path).map_err(|_| StorageError::Io)?;
    // The rename itself must survive the power cut, not just the bytes.
    if let Some(dir) = path.parent() {
        if let Ok(dir_handle) = File::open(dir) {
            let _ = fsync.sync(&dir_handle);
        }
    }
    Ok(())
}
