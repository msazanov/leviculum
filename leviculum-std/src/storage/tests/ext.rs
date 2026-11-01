use super::*;

#[test]
fn test_unchanged_known_ratchet_is_not_rewritten_within_refresh_window() {
    let (mut storage, path) = unique_temp_storage("kr_skip");
    let hash = [0x21; TRUNCATED_HASHBYTES];
    let file_path = path.path().join(RATCHETS_DIR).join(hex_encode(&hash));

    CoreStorage::remember_known_ratchet(&mut storage, hash, [0xaa; 32], 5_000);
    crate::write_behind::sync();
    assert!(file_path.exists());
    // mark the file so a rewrite is detectable
    std::fs::write(&file_path, b"marker").unwrap();

    // same key again, well inside the window: memory is refreshed, disk is not touched
    CoreStorage::remember_known_ratchet(
        &mut storage,
        hash,
        [0xaa; 32],
        5_000 + RATCHET_REFRESH_MS - 1,
    );
    crate::write_behind::sync();
    assert_eq!(
        std::fs::read(&file_path).unwrap(),
        b"marker",
        "unchanged ratchet must not be rewritten"
    );
    assert_eq!(
        CoreStorage::get_known_ratchet(&storage, &hash),
        Some([0xaa; 32])
    );

    // window elapsed: timestamp refresh reaches the disk
    CoreStorage::remember_known_ratchet(
        &mut storage,
        hash,
        [0xaa; 32],
        5_000 + RATCHET_REFRESH_MS,
    );
    crate::write_behind::sync();
    assert_ne!(
        std::fs::read(&file_path).unwrap(),
        b"marker",
        "stale timestamp must be refreshed"
    );

    // a different key is always written at once
    std::fs::write(&file_path, b"marker").unwrap();
    CoreStorage::remember_known_ratchet(
        &mut storage,
        hash,
        [0xbb; 32],
        5_000 + RATCHET_REFRESH_MS + 1,
    );
    crate::write_behind::sync();
    assert_ne!(
        std::fs::read(&file_path).unwrap(),
        b"marker",
        "changed ratchet must be written"
    );
    assert_eq!(
        CoreStorage::get_known_ratchet(&storage, &hash),
        Some([0xbb; 32])
    );

    // after a restart the loaded timestamp counts as already saved
    drop(storage);
    let mut storage2 = Storage::new(&path).unwrap();
    std::fs::write(&file_path, b"marker").unwrap();
    let ts = 5_000 + RATCHET_REFRESH_MS + 1;
    let mono = wallclock_secs_to_mono_ms(
        crate::storage::mono_to_wallclock_secs(ts, storage2.mono_offset_ms),
        storage2.mono_offset_ms,
    );
    CoreStorage::remember_known_ratchet(&mut storage2, hash, [0xbb; 32], mono + 10);
    crate::write_behind::sync();
    assert_eq!(
        std::fs::read(&file_path).unwrap(),
        b"marker",
        "freshly loaded ratchet must not be rewritten at once"
    );
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn changed_ratchet_enqueue_failure_does_not_suppress_the_next_observation() {
    let (mut storage, path) = unique_temp_storage("kr_retry_enqueue");
    let hash = [0x2A; TRUNCATED_HASHBYTES];
    let dir = path.path().join(RATCHETS_DIR);
    CoreStorage::remember_known_ratchet(&mut storage, hash, [0xA1; 32], 1_000);
    crate::write_behind::sync();

    std::fs::remove_file(dir.join(hex_encode(&hash))).unwrap();
    std::fs::remove_dir(&dir).unwrap();
    std::fs::write(&dir, b"not a directory").unwrap();
    CoreStorage::remember_known_ratchet(&mut storage, hash, [0xB2; 32], 10_000);
    std::fs::remove_file(&dir).unwrap();
    std::fs::create_dir(&dir).unwrap();

    // Same key well inside the refresh window: this must retry because the
    // first changed-key enqueue failed and cannot inherit A's saved time.
    CoreStorage::remember_known_ratchet(&mut storage, hash, [0xB2; 32], 70_000);
    crate::write_behind::sync();
    let restored = Storage::new(path.path()).unwrap();
    assert_eq!(
        CoreStorage::get_known_ratchet(&restored, &hash),
        Some([0xB2; 32])
    );
}

/// The enqueue succeeds, then the worker fails (directory not writable):
/// the later observation of the same key must retry instead of inheriting
/// the refresh window of data that never reached disk.
#[cfg(unix)]
#[test]
fn async_known_ratchet_write_failure_is_retried_on_the_next_observation() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // root ignores directory mode bits
    }
    let (mut storage, path) = unique_temp_storage("kr_retry_async");
    let hash = [0x2B; TRUNCATED_HASHBYTES];
    let dir = path.path().join(RATCHETS_DIR);
    CoreStorage::remember_known_ratchet(&mut storage, hash, [0xA1; 32], 1_000);
    crate::write_behind::sync();

    let release = crate::write_behind::pause_global();
    CoreStorage::remember_known_ratchet(&mut storage, hash, [0xB2; 32], 10_000);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    release.send(()).unwrap();
    crate::write_behind::sync();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let on_disk = Storage::new(path.path()).unwrap();
    assert_eq!(
        CoreStorage::get_known_ratchet(&on_disk, &hash),
        Some([0xA1; 32]),
        "the failed worker write left the old key on disk"
    );

    // Same key, well inside the refresh window of the (lost) save.
    CoreStorage::remember_known_ratchet(&mut storage, hash, [0xB2; 32], 70_000);
    crate::write_behind::sync();
    let restored = Storage::new(path.path()).unwrap();
    assert_eq!(
        CoreStorage::get_known_ratchet(&restored, &hash),
        Some([0xB2; 32])
    );
}
