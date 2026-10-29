//! How the home node answers its clients: a pure function of the request,
//! the identity the link was identified with, and the store. The daemon
//! (`super::daemon`) carries the bytes; everything decided is here, where a
//! test reaches it without a network.
//!
//! Membership is checked on every request, as lxmd's control handlers check
//! their allow list on each call (`reference/LXMF/LXMF/LXMRouter.py:838-865`):
//! a device revoked while its link is up is refused on its next request.

use alloc::vec::Vec;

use super::wire::{self, Status};
use crate::store::{Redeemed, Store};

/// The answer to one request on the home destination. `remote` is the
/// identity hash the link was identified with, if it was.
pub fn answer(
    store: &mut Store,
    path: &str,
    data: &[u8],
    remote: Option<[u8; 16]>,
    now_secs: i64,
) -> Vec<u8> {
    let Some(device) = remote else {
        return wire::status_answer(Status::NotIdentified);
    };
    match path {
        super::JOIN_PATH => join(store, data, device, now_secs),
        super::SYNC_PATH => sync(store, data, device),
        _ => wire::status_answer(Status::Malformed),
    }
}

fn join(store: &mut Store, data: &[u8], device: [u8; 16], now_secs: i64) -> Vec<u8> {
    let Ok((token, _name)) = wire::read_join_request(data) else {
        return wire::status_answer(Status::Malformed);
    };
    match store.redeem_token(&token, &device, now_secs) {
        Ok(Redeemed::Admitted { name }) => {
            tracing::info!(
                "HOME admitted device={} name={name:?}",
                crate::address::to_hex(&device)
            );
            wire::status_answer(Status::Ok)
        }
        Ok(Redeemed::Unknown) => {
            tracing::info!(
                "HOME join refused device={} reason=token",
                crate::address::to_hex(&device)
            );
            wire::status_answer(Status::BadToken)
        }
        Err(error) => {
            tracing::warn!("HOME join: {error}");
            wire::status_answer(Status::Failed)
        }
    }
}

fn sync(store: &mut Store, data: &[u8], device: [u8; 16]) -> Vec<u8> {
    let Ok(cursor) = wire::read_sync_request(data) else {
        return wire::status_answer(Status::Malformed);
    };
    match store.is_admitted(&device) {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(
                "HOME sync refused device={} reason=not-admitted",
                crate::address::to_hex(&device)
            );
            return wire::status_answer(Status::NotAdmitted);
        }
        Err(error) => {
            tracing::warn!("HOME sync: {error}");
            return wire::status_answer(Status::Failed);
        }
    }
    match store.archive_since(cursor, super::SYNC_PAGE_BYTES) {
        Ok(page) => {
            tracing::info!(
                "HOME sync device={} cursor={cursor} entries={} more={}",
                crate::address::to_hex(&device),
                page.entries.len(),
                page.more
            );
            wire::sync_answer(&page)
        }
        Err(error) => {
            tracing::warn!("HOME sync: {error}");
            wire::status_answer(Status::Failed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::wire::{read_status, read_sync_answer, sync_request, Page};
    use crate::home::{JOIN_PATH, KIND_RECEIVED, SYNC_PATH};

    const HOME: [u8; 16] = [0x11; 16];
    const LAPTOP: [u8; 16] = [0x22; 16];
    const TOKEN: [u8; 16] = [0x33; 16];

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("dir");
        let store = Store::open_in(dir.path(), HOME).expect("store");
        (dir, store)
    }

    fn join(store: &mut Store, device: [u8; 16], token: [u8; 16]) -> Status {
        let answer = answer(
            store,
            JOIN_PATH,
            &wire::join_request(&token, "laptop"),
            Some(device),
            100,
        );
        read_status(&answer).expect("status")
    }

    fn sync(store: &mut Store, device: Option<[u8; 16]>, cursor: u64) -> Result<Page, Status> {
        let answer = answer(store, SYNC_PATH, &sync_request(cursor), device, 100);
        read_sync_answer(&answer).expect("answer")
    }

    fn archive(store: &mut Store, n: u8) -> u64 {
        store
            .archive_append(KIND_RECEIVED, &[n; 32], &[n; 120], 50, u64::MAX)
            .expect("append")
            .expect("new")
    }

    #[test]
    fn a_good_token_admits_once_and_a_reused_one_is_refused() {
        let (_dir, mut store) = store();
        store.issue_token(&TOKEN, "laptop", 90).expect("issue");
        assert_eq!(join(&mut store, LAPTOP, TOKEN), Status::Ok);
        assert_eq!(
            store.devices().expect("devices"),
            [crate::store::Device {
                device: LAPTOP,
                name: "laptop".into(),
                admitted_at: 100,
            }]
        );
        // The same token again, from another device: used up.
        assert_eq!(join(&mut store, [0x44; 16], TOKEN), Status::BadToken);
        assert_eq!(store.devices().expect("devices").len(), 1);
        // A token never issued.
        assert_eq!(join(&mut store, [0x44; 16], [0x55; 16]), Status::BadToken);
    }

    #[test]
    fn an_unidentified_link_gets_nothing() {
        let (_dir, mut store) = store();
        archive(&mut store, 1);
        assert_eq!(sync(&mut store, None, 0), Err(Status::NotIdentified));
        let answer = answer(
            &mut store,
            JOIN_PATH,
            &wire::join_request(&TOKEN, "x"),
            None,
            1,
        );
        assert_eq!(read_status(&answer), Ok(Status::NotIdentified));
    }

    #[test]
    fn a_device_never_admitted_or_revoked_gets_nothing() {
        let (_dir, mut store) = store();
        archive(&mut store, 1);
        assert_eq!(sync(&mut store, Some(LAPTOP), 0), Err(Status::NotAdmitted));
        store.issue_token(&TOKEN, "laptop", 90).expect("issue");
        assert_eq!(join(&mut store, LAPTOP, TOKEN), Status::Ok);
        assert_eq!(
            sync(&mut store, Some(LAPTOP), 0)
                .expect("page")
                .entries
                .len(),
            1
        );
        assert!(matches!(
            store.revoke("laptop").expect("revoke"),
            crate::store::Revoked::One(_)
        ));
        assert_eq!(sync(&mut store, Some(LAPTOP), 0), Err(Status::NotAdmitted));
    }

    #[test]
    fn the_cursor_gives_what_came_after_it_and_fetching_deletes_nothing() {
        let (_dir, mut store) = store();
        store.issue_token(&TOKEN, "laptop", 90).expect("issue");
        assert_eq!(join(&mut store, LAPTOP, TOKEN), Status::Ok);
        let a = archive(&mut store, 1);
        let b = archive(&mut store, 2);
        assert!(b > a);
        let all = sync(&mut store, Some(LAPTOP), 0).expect("page");
        assert_eq!(
            all.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [a, b]
        );
        assert_eq!(all.first, a);
        assert!(!all.more);
        // The same cursor twice is the same answer: idempotent, nothing gone.
        assert_eq!(sync(&mut store, Some(LAPTOP), 0).expect("page"), all);
        let after_a = sync(&mut store, Some(LAPTOP), a).expect("page");
        assert_eq!(
            after_a.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [b]
        );
        assert!(sync(&mut store, Some(LAPTOP), b)
            .expect("page")
            .entries
            .is_empty());
    }

    #[test]
    fn a_message_archived_twice_is_one_entry() {
        let (_dir, mut store) = store();
        archive(&mut store, 1);
        assert_eq!(
            store
                .archive_append(KIND_RECEIVED, &[1; 32], &[1; 120], 60, u64::MAX)
                .expect("append"),
            None
        );
        assert_eq!(
            store
                .archive_since(0, usize::MAX)
                .expect("page")
                .entries
                .len(),
            1
        );
    }

    #[test]
    fn the_cap_evicts_the_oldest_first_and_keeps_the_newest() {
        let (_dir, mut store) = store();
        let cap = 250; // two 120-byte entries fit, three do not
        let seqs: Vec<u64> = (1..=4)
            .map(|n| {
                store
                    .archive_append(KIND_RECEIVED, &[n; 32], &[n; 120], 50, cap)
                    .expect("append")
                    .expect("new")
            })
            .collect();
        let page = store.archive_since(0, usize::MAX).expect("page");
        assert_eq!(
            page.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            seqs[2..]
        );
        assert_eq!(page.first, seqs[2]);
        assert_eq!(store.archive_bytes().expect("bytes"), 240);
        // One entry larger than the cap is kept until the next one comes.
        let big = store
            .archive_append(KIND_RECEIVED, &[9; 32], &[9; 400], 50, cap)
            .expect("append")
            .expect("new");
        let page = store.archive_since(0, usize::MAX).expect("page");
        assert_eq!(
            page.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [big]
        );
        // And the sequence goes on growing past every eviction.
        let next = archive(&mut store, 10);
        assert!(next > big);
    }

    #[test]
    fn a_page_stops_at_its_byte_budget_but_carries_one_entry_at_least() {
        let (_dir, mut store) = store();
        for n in 1..=3 {
            archive(&mut store, n);
        }
        let page = store.archive_since(0, 200).expect("page");
        assert_eq!(page.entries.len(), 1);
        assert!(page.more);
        let page = store.archive_since(0, 240).expect("page");
        assert_eq!(page.entries.len(), 2);
        assert!(page.more);
    }

    #[test]
    fn revoke_takes_a_name_or_a_hash_and_refuses_an_ambiguous_one() {
        let (_dir, mut store) = store();
        store.issue_token(&[1; 16], "phone", 1).expect("issue");
        store.issue_token(&[2; 16], "phone", 1).expect("issue");
        store
            .redeem_token(&[1; 16], &[0xab; 16], 2)
            .expect("redeem");
        store
            .redeem_token(&[2; 16], &[0xcd; 16], 3)
            .expect("redeem");
        assert!(matches!(
            store.revoke("phone").expect("revoke"),
            crate::store::Revoked::Ambiguous(ref both) if both.len() == 2
        ));
        assert_eq!(store.devices().expect("devices").len(), 2);
        assert!(matches!(
            store.revoke("CDCD").expect("revoke"),
            crate::store::Revoked::One(ref d) if d.device == [0xcd; 16]
        ));
        assert_eq!(
            store.revoke("nobody").expect("revoke"),
            crate::store::Revoked::None
        );
        assert_eq!(store.devices().expect("devices").len(), 1);
    }
}
