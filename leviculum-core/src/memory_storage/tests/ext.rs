use super::*;

/// The cache must be reused: repeated queries must not rescan the table.
#[test]
fn cached_min_is_not_recomputed_on_every_query() {
    let mut s = MemoryStorage::with_caps(TableCaps {
        path_cap: 16,
        ..TableCaps::default()
    });
    assert_eq!(s.earliest_path_expiry(), None);
    let scans = s.path_expiry_cache.scans();
    for _ in 0..100 {
        assert_eq!(s.earliest_path_expiry(), None);
    }
    assert_eq!(
        s.path_expiry_cache.scans(),
        scans,
        "cache was recomputed on repeated queries"
    );
    s.set_path(
        [1u8; TRUNCATED_HASHBYTES],
        PathEntry {
            hops: 1,
            expires_ms: 500,
            interface_index: 0,
            random_blobs: Vec::new(),
            next_hop: None,
            via_peer: None,
        },
    );
    assert_eq!(s.earliest_path_expiry(), Some(500));
    let scans = s.path_expiry_cache.scans();
    for _ in 0..100 {
        assert_eq!(s.earliest_path_expiry(), Some(500));
    }
    assert_eq!(
        s.path_expiry_cache.scans(),
        scans,
        "cache was recomputed on repeated queries"
    );
    // expire_paths computes the minimum in its sweep: following query must not scan anew
    s.expire_paths(600);
    let scans = s.path_expiry_cache.scans();
    assert_eq!(s.earliest_path_expiry(), None);
    assert_eq!(
        s.path_expiry_cache.scans(),
        scans,
        "expire_paths triggered redundant table scan"
    );
}

/// The cached minimum must equal a full scan after any mix of inserts,
/// refreshes (same key, new expiry), evictions at the cap, removals,
/// interface purges and expiry sweeps.
#[test]
fn earliest_path_expiry_cache_matches_full_scan() {
    let mut s = MemoryStorage::with_caps(TableCaps {
        path_cap: 16,
        ..TableCaps::default()
    });
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let scan = |s: &MemoryStorage| s.path_table.values().map(|e| e.expires_ms).min();
    for step in 0..20_000u32 {
        let r = next();
        let key = [(r % 24) as u8; TRUNCATED_HASHBYTES];
        match (r >> 8) % 10 {
            0..=5 => s.set_path(
                key,
                PathEntry {
                    hops: 1,
                    expires_ms: 1_000 + (r >> 16) % 500,
                    interface_index: ((r >> 32) % 3) as usize,
                    random_blobs: Vec::new(),
                    next_hop: None,
                    via_peer: None,
                },
            ),
            6 | 7 => {
                s.remove_path(&key);
            }
            8 => {
                s.remove_paths_for_interface(((r >> 40) % 3) as usize);
            }
            _ => {
                s.expire_paths(1_000 + (r >> 20) % 500);
            }
        }
        if step % 3 == 0 {
            assert_eq!(s.earliest_path_expiry(), scan(&s), "step {step}");
        }
    }
    s.clear_all();
    assert_eq!(s.earliest_path_expiry(), None);
}
