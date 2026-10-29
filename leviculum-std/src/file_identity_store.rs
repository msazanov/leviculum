//! File-backed identity store for std targets.
//!
//! Stores the identity as raw 64 bytes at `{storage_path}/transport_identity`,
//! compatible with Python Reticulum (`rnsd`, `rnstatus`, etc.).

use leviculum_core::constants::IDENTITY_KEY_SIZE;
use leviculum_core::identity::Identity;
use leviculum_core::identity_store::IdentityStore;
use std::path::{Path, PathBuf};

const IDENTITY_FILE: &str = "transport_identity";

/// File-backed identity store, Python-compatible.
///
/// The identity file is raw 64 bytes (32 X25519 + 32 Ed25519 private keys),
/// the same format Python Reticulum uses. No magic bytes, no checksum./// the filesystem provides existence checking.
pub struct FileIdentityStore {
    path: PathBuf,
}

impl FileIdentityStore {
    /// Create a store that reads/writes `{storage_dir}/transport_identity`.
    pub fn new(storage_dir: &Path) -> Self {
        Self {
            path: storage_dir.join(IDENTITY_FILE),
        }
    }
}

impl IdentityStore for FileIdentityStore {
    type Error = std::io::Error;

    /// Absence of the file is the only "no identity" answer.
    ///
    /// A file that exists but does not parse is a damaged key, not a missing
    /// one. Answering `Ok(None)` there makes the caller mint a fresh identity
    /// and save it over the same path, which destroys the evidence and the
    /// node's address in one step; a hard error keeps the damaged file for the
    /// operator to restore from backup.
    fn load(&mut self) -> Result<Option<Identity>, Self::Error> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.len() == IDENTITY_KEY_SIZE => {
                // An older writer may have left it with the umask's mode.
                crate::storage::narrow_secret_mode(&self.path)?;
                Identity::from_private_key_bytes(&bytes)
                    .map(Some)
                    .map_err(|e| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "{} holds {} bytes that are not a valid identity ({:?}); \
                             refusing to replace it, restore a backup or move the file aside",
                                self.path.display(),
                                IDENTITY_KEY_SIZE,
                                e
                            ),
                        )
                    })
            }
            Ok(bytes) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{} has wrong size: {} (expected {}); \
                     refusing to replace it, restore a backup or move the file aside",
                    self.path.display(),
                    bytes.len(),
                    IDENTITY_KEY_SIZE
                ),
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Durable atomic write, owner-only from creation (L-0039): the data
    /// reaches the disk before the rename and the directory entry after it.
    /// Without both fsyncs a power cut can resurrect the file short or empty,
    /// which is exactly the state `load` refuses to start on. The stack's one
    /// writer for a secret, [`crate::storage::atomic_write_secret`], does all
    /// of it.
    fn save(&mut self, identity: &Identity) -> Result<(), Self::Error> {
        let bytes = identity
            .private_key_bytes()
            .map_err(|e| std::io::Error::other(format!("{:?}", e)))?;
        crate::storage::atomic_write_secret(&self.path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L-0039: the identity is the node's private key, and the shared
    /// instance's RPC authkey is its hash, so the file must be owner-only.
    /// It used to inherit the umask (0644 on Debian).
    #[cfg(unix)]
    #[test]
    fn saved_identity_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = FileIdentityStore::new(dir.path());
        store
            .save(&crate::generate_identity())
            .expect("save identity");

        let path = dir.path().join(IDENTITY_FILE);
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        println!("{}: mode {mode:o}", path.display());
        assert_eq!(mode, 0o600, "transport_identity mode {mode:o}");
    }

    /// 1119: a key an older writer left group- or world-readable is narrowed
    /// to 0600 when it is loaded, and loads as the same key.
    #[cfg(unix)]
    #[test]
    fn a_wide_identity_is_narrowed_at_load() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let identity = crate::generate_identity();
        let path = dir.path().join(IDENTITY_FILE);
        std::fs::write(&path, identity.private_key_bytes().expect("key")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).expect("chmod");

        let loaded = FileIdentityStore::new(dir.path())
            .load()
            .expect("load")
            .expect("an identity");
        assert_eq!(loaded.hash(), identity.hash());
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "transport_identity mode {mode:o}");
    }
}
