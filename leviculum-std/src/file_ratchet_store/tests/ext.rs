use super::*;

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

/// One key file the owner cannot read (mode 000) used to abort the
/// migration, and with it the load and every later save of all private
/// ratchet keys. Per-file errors are isolated; only directory-level errors
/// stay fatal.
#[cfg(unix)]
#[test]
fn one_unreadable_key_file_does_not_block_the_other_private_keys() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // root ignores file mode bits
    }

    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(RATCHETKEYS_DIR);
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let bad = [0x51; TRUNCATED_HASHBYTES];
    let good = [0x52; TRUNCATED_HASHBYTES];
    for (hash, mode) in [(bad, 0o000), (good, 0o644)] {
        let path = dir.join(hex_encode(&hash));
        std::fs::write(&path, b"private key").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    let mut store = FileRatchetStore::new(root.path());
    let loaded = store
        .load_dest_ratchet_keys()
        .expect("a single bad key file is not a directory-level failure");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].0, good);
    let mode = |hash: &[u8; TRUNCATED_HASHBYTES]| {
        std::fs::metadata(dir.join(hex_encode(hash)))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&good), 0o600, "the readable key is still migrated");
    assert_eq!(mode(&bad), 0o000, "the unreadable key is left alone");

    store
        .save_dest_ratchet_keys(&[0x53; TRUNCATED_HASHBYTES], b"new key")
        .expect("saving another destination is not blocked");
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
