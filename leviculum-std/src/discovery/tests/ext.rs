use super::*;

#[test]
fn expiry_does_not_remove_a_newer_queued_announce_for_the_same_record() {
    use leviculum_core::discovery::THRESHOLD_REMOVE;
    let td = tempfile::tempdir().unwrap();
    let di = rnode_di("Reannounced", 0x31);
    persist_discovered(td.path(), &di, 1, 1_000.0).unwrap();
    crate::write_behind::sync();
    assert_eq!(
        list_discovered_interfaces_no_sync(td.path(), 1_000.0).len(),
        1
    );

    let release = crate::write_behind::pause_global();
    let fresh_at = 1_000.0 + THRESHOLD_REMOVE + 20.0;
    persist_discovered(td.path(), &di, 1, fresh_at).unwrap();
    let list = list_discovered_interfaces_no_sync(td.path(), fresh_at);
    assert_eq!(
        list.len(),
        1,
        "the fresh queued re-announce remains visible"
    );
    assert_eq!(list[0].last_heard, fresh_at);

    release.send(()).unwrap();
    crate::write_behind::sync();
    let path = discovery_dir(td.path()).join(crate::storage::hex_encode(&di.discovery_hash));
    let record = DiscoveredInterfaceRecord::decode_msgpack(&std::fs::read(path).unwrap())
        .expect("fresh record is kept on disk");
    assert_eq!(record.last_heard, fresh_at);
}

#[test]
fn expiry_predicate_preserves_a_write_that_settled_after_scan() {
    use leviculum_core::discovery::THRESHOLD_REMOVE;
    let td = tempfile::tempdir().unwrap();
    let di = rnode_di("Settled between scan and unlink", 0x32);
    persist_discovered(td.path(), &di, 1, 1_000.0).unwrap();
    crate::write_behind::sync();
    let dir = discovery_dir(td.path());
    let path = dir.join(crate::storage::hex_encode(&di.discovery_hash));
    let scanned_generation = generation_of(&dir);

    // Exercise the real scan/expiry methods directly. The public listing
    // wrapper has a cfg(test) sync at entry, which would erase the exact
    // interleaving this regression needs to preserve.
    let mut cache = DirCache::new();
    let dir_mtime = std::fs::metadata(&dir).unwrap().modified().ok();
    cache.revalidate(&dir, dir_mtime, scanned_generation);
    assert!(cache.files.contains_key(path.file_name().unwrap()));

    let fresh_at = 1_000.0 + THRESHOLD_REMOVE + 20.0;
    persist_discovered(td.path(), &di, 1, fresh_at).unwrap();
    // This FIFO barrier places a settled atomic write strictly between the
    // stale scan and expiry assembly; it is not the listing wrapper's
    // pre-scan sync and cannot hide the race.
    crate::write_behind::sync();

    cache.assemble(&dir, fresh_at, true);
    assert!(cache.sorted.is_empty(), "stale scan is not served");
    // Drain a buggy stale Remove before observing the filesystem. With the
    // former unconditional expiry path this deletes the fresh file.
    crate::write_behind::sync();
    let record = DiscoveredInterfaceRecord::decode_msgpack(&std::fs::read(path).unwrap())
        .expect("fresh record remains on disk");
    assert_eq!(record.last_heard, fresh_at);

    let fresh_generation = generation_of(&dir);
    let dir_mtime = std::fs::metadata(&dir).unwrap().modified().ok();
    cache.revalidate(&dir, dir_mtime, fresh_generation);
    cache.assemble(&dir, fresh_at, true);
    assert_eq!(cache.sorted.len(), 1);
    assert_eq!(cache.sorted[0].last_heard, fresh_at);
}

#[test]
fn expiry_does_not_remove_write_enqueued_before_snapshot_and_settled_during_scan() {
    use leviculum_core::discovery::THRESHOLD_REMOVE;
    let td = tempfile::tempdir().unwrap();
    let di = rnode_di("Enqueued before snapshot", 0x33);
    persist_discovered(td.path(), &di, 1, 1_000.0).unwrap();
    crate::write_behind::sync();

    let dir = discovery_dir(td.path());
    let path = dir.join(crate::storage::hex_encode(&di.discovery_hash));
    let target_name = path.file_name().unwrap().to_os_string();
    let release = crate::write_behind::pause_global();
    let fresh_at = 1_000.0 + THRESHOLD_REMOVE + 20.0;

    // The write and its generation bump happen before the listing takes
    // its snapshot, but the paused worker leaves the old bytes on disk.
    persist_discovered(td.path(), &di, 1, fresh_at).unwrap();
    let snapshot_generation = generation_of(&dir);
    let scan_dir_mtime = std::fs::metadata(&dir).unwrap().modified().ok();

    let mut cache = DirCache::new();
    let mut release = Some(release);
    cache.revalidate_with_hook(&dir, scan_dir_mtime, snapshot_generation, |name| {
        if name == target_name {
            // The real revalidation has already decoded the stale
            // file. Complete the earlier fresh job while the scan is
            // still active, then allow it to assemble the stale view.
            release
                .take()
                .expect("the target record is visited once")
                .send(())
                .unwrap();
            crate::write_behind::sync();
        }
    });
    assert!(release.is_none(), "the target was read during revalidation");

    cache.assemble(&dir, fresh_at, true);
    crate::write_behind::sync();
    let record = DiscoveredInterfaceRecord::decode_msgpack(&std::fs::read(&path).unwrap())
        .expect("expiry must preserve the completed fresh write");
    assert_eq!(record.last_heard, fresh_at);
}

#[test]
fn announce_queued_after_conditional_expiry_survives_fifo_remove() {
    use leviculum_core::discovery::THRESHOLD_REMOVE;
    let td = tempfile::tempdir().unwrap();
    let di = rnode_di("Announced after expiry job", 0x34);
    persist_discovered(td.path(), &di, 1, 1_000.0).unwrap();
    crate::write_behind::sync();

    let dir = discovery_dir(td.path());
    let path = dir.join(crate::storage::hex_encode(&di.discovery_hash));
    let scan_generation = generation_of(&dir);
    let mut cache = DirCache::new();
    let dir_mtime = std::fs::metadata(&dir).unwrap().modified().ok();
    cache.revalidate(&dir, dir_mtime, scan_generation);

    // Hold the single worker. The conditional Remove enters first; the
    // fresh announce is enqueued strictly after it and must be final.
    let release = crate::write_behind::pause_global();
    let fresh_at = 1_000.0 + THRESHOLD_REMOVE + 20.0;
    cache.assemble(&dir, fresh_at, true);
    persist_discovered(td.path(), &di, 1, fresh_at).unwrap();

    release.send(()).unwrap();
    crate::write_behind::sync();
    let record = DiscoveredInterfaceRecord::decode_msgpack(&std::fs::read(path).unwrap())
        .expect("FIFO write after conditional expiry must survive");
    assert_eq!(record.last_heard, fresh_at);
    assert_eq!(record.heard_count, 1);
}
