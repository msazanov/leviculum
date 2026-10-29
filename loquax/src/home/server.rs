//! How the home node answers its clients: a pure function of the request,
//! the identity the link was identified with, and the store. The daemon
//! (`super::daemon`) carries the bytes; everything decided is here, where a
//! test reaches it without a network.
//!
//! Membership is checked on every request, as lxmd's control handlers check
//! their allow list on each call (`reference/LXMF/LXMF/LXMRouter.py:838-865`):
//! a device revoked while its link is up is refused on its next request.

use alloc::vec::Vec;

use leviculum_core::Identity;
use leviculum_lxmf::DeliveryMethod;

use super::wire::{self, Status};
use crate::store::{Accepted, Outgoing, Redeemed, Store};

/// What the home node answers with besides the store: the identity that
/// signs what clients send, and the configured limits.
pub struct Home<'a> {
    /// The home identity, with its private key.
    pub identity: &'a Identity,
    /// Its LXMF delivery address, every sent message's source.
    pub address: [u8; 16],
    /// How long an admission token stays good, in seconds.
    pub token_ttl_secs: i64,
    /// The archive's cap in bytes.
    pub cap_bytes: u64,
}

/// One answer: the bytes for the client, and the message a `/send` put in
/// the outbox, when it was a new one, for the daemon to start delivering.
pub struct Answer {
    pub bytes: Vec<u8>,
    pub accepted: Option<Outgoing>,
}

impl Answer {
    fn only(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            accepted: None,
        }
    }
}

/// The answer to one request on the home destination. `remote` is the
/// identity hash the link was identified with, if it was; `now` is the unix
/// time in seconds.
pub fn answer(
    store: &mut Store,
    home: &Home<'_>,
    path: &str,
    data: &[u8],
    remote: Option<[u8; 16]>,
    now: f64,
) -> Answer {
    let Some(device) = remote else {
        return Answer::only(wire::status_answer(Status::NotIdentified));
    };
    let now_secs = now as i64;
    match path {
        super::JOIN_PATH => Answer::only(join(store, home, data, device, now_secs)),
        super::SYNC_PATH => Answer::only(sync(store, data, device)),
        super::SEND_PATH => send(store, home, data, device, now),
        _ => Answer::only(wire::status_answer(Status::Malformed)),
    }
}

/// Whether `device` may ask: an answer to refuse it with, when not.
fn refuse_unless_admitted(store: &Store, device: &[u8; 16], what: &str) -> Option<Vec<u8>> {
    match store.is_admitted(device) {
        Ok(true) => None,
        Ok(false) => {
            tracing::info!(
                "HOME {what} refused device={} reason=not-admitted",
                crate::address::to_hex(device)
            );
            Some(wire::status_answer(Status::NotAdmitted))
        }
        Err(error) => {
            tracing::warn!("HOME {what}: {error}");
            Some(wire::status_answer(Status::Failed))
        }
    }
}

fn send(store: &mut Store, home: &Home<'_>, data: &[u8], device: [u8; 16], now: f64) -> Answer {
    let Ok(submission) = wire::read_send_request(data) else {
        return Answer::only(wire::status_answer(Status::Malformed));
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "send") {
        return Answer::only(refused);
    }
    // A retried submission is answered from the store, before anything is
    // signed: the message it made then is the one it gets.
    match store.outbox_by_client(&submission.client_id) {
        Ok(Some(id)) => {
            tracing::info!(
                "HOME send again device={} message={}",
                crate::address::to_hex(&device),
                crate::address::to_hex(&id)
            );
            return Answer::only(wire::send_answer(&id));
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!("HOME send: {error}");
            return Answer::only(wire::status_answer(Status::Failed));
        }
    }
    let message = match leviculum_lxmf::Message::create(
        submission.destination,
        home.address,
        home.identity,
        now,
        submission.title,
        submission.content,
        submission.fields,
        DeliveryMethod::Direct,
    ) {
        Ok(message) => message,
        Err(error) => {
            tracing::info!("HOME send refused: {error}");
            return Answer::only(wire::status_answer(Status::Malformed));
        }
    };
    let packed = message.pack();
    let at = now as i64;
    match store.outbox_accept(
        &submission.client_id,
        &message.message_id,
        &submission.destination,
        &packed,
        at,
        home.cap_bytes,
    ) {
        Ok(Accepted::New { .. }) => {
            tracing::info!(
                "HOME send accepted device={} message={} to={}",
                crate::address::to_hex(&device),
                crate::address::to_hex(&message.message_id),
                crate::address::to_hex(&submission.destination)
            );
            Answer {
                bytes: wire::send_answer(&message.message_id),
                accepted: Some(Outgoing {
                    id: message.message_id,
                    destination: submission.destination,
                    packed,
                    created_at: at,
                    propagated: false,
                }),
            }
        }
        Ok(Accepted::Again { id }) => Answer::only(wire::send_answer(&id)),
        Err(error) => {
            tracing::warn!("HOME send: {error}");
            Answer::only(wire::status_answer(Status::Failed))
        }
    }
}

fn join(
    store: &mut Store,
    home: &Home<'_>,
    data: &[u8],
    device: [u8; 16],
    now_secs: i64,
) -> Vec<u8> {
    let Ok((token, _name)) = wire::read_join_request(data) else {
        return wire::status_answer(Status::Malformed);
    };
    match store.redeem_token(&token, &device, now_secs, home.token_ttl_secs) {
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
        Ok(Redeemed::Expired { name }) => {
            tracing::info!(
                "HOME join refused device={} reason=expired name={name:?}",
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
    if let Some(refused) = refuse_unless_admitted(store, &device, "sync") {
        return refused;
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
    use crate::home::outgoing::Change;
    use crate::home::wire::{read_status, read_sync_answer, sync_request, Page};
    use crate::home::{JOIN_PATH, KIND_RECEIVED, SEND_PATH, SYNC_PATH};

    const HOME: [u8; 16] = [0x11; 16];
    const LAPTOP: [u8; 16] = [0x22; 16];
    const TOKEN: [u8; 16] = [0x33; 16];

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("dir");
        let store = Store::open_in(dir.path(), HOME).expect("store");
        (dir, store)
    }

    const DAY: i64 = 24 * 3600;

    /// The home node's side, with a fresh identity each test.
    fn home() -> Home<'static> {
        let identity: &'static Identity = alloc::boxed::Box::leak(alloc::boxed::Box::new(
            Identity::generate(&mut rand_core::OsRng),
        ));
        Home {
            identity,
            address: HOME,
            token_ttl_secs: DAY,
            cap_bytes: u64::MAX,
        }
    }

    fn join_at(store: &mut Store, device: [u8; 16], token: [u8; 16], now: f64) -> Status {
        let answer = answer(
            store,
            &home(),
            JOIN_PATH,
            &wire::join_request(&token, "laptop"),
            Some(device),
            now,
        );
        read_status(&answer.bytes).expect("status")
    }

    fn join(store: &mut Store, device: [u8; 16], token: [u8; 16]) -> Status {
        join_at(store, device, token, 100.0)
    }

    fn sync(store: &mut Store, device: Option<[u8; 16]>, cursor: u64) -> Result<Page, Status> {
        let answer = answer(
            store,
            &home(),
            SYNC_PATH,
            &sync_request(cursor),
            device,
            100.0,
        );
        read_sync_answer(&answer.bytes).expect("answer")
    }

    fn submission(client_id: u8) -> wire::Submission {
        wire::Submission {
            client_id: [client_id; 16],
            destination: [0x66; 16],
            title: Vec::new(),
            content: b"hallo".to_vec(),
            fields: Vec::new(),
        }
    }

    fn send(
        store: &mut Store,
        home: &Home<'_>,
        device: [u8; 16],
        submission: &wire::Submission,
        now: f64,
    ) -> (Result<[u8; 32], Status>, Option<Outgoing>) {
        let answer = answer(
            store,
            home,
            SEND_PATH,
            &wire::send_request(submission),
            Some(device),
            now,
        );
        (
            wire::read_send_answer(&answer.bytes).expect("answer"),
            answer.accepted,
        )
    }

    fn admitted(store: &mut Store) {
        store.issue_token(&TOKEN, "laptop", 90).expect("issue");
        assert_eq!(join(store, LAPTOP, TOKEN), Status::Ok);
    }

    #[test]
    fn a_send_is_idempotent_by_the_clients_id() {
        let (_dir, mut store) = store();
        admitted(&mut store);
        let home = home();
        let (first, accepted) = send(&mut store, &home, LAPTOP, &submission(1), 200.5);
        let id = first.expect("accepted");
        let accepted = accepted.expect("a new outbox entry");
        assert_eq!(accepted.id, id);
        assert_eq!(accepted.destination, [0x66; 16]);
        // The packed message is signed by the home identity, from its address.
        let message = leviculum_lxmf::Message::unpack(
            &accepted.packed,
            None,
            Some(home.identity),
            DeliveryMethod::Direct,
        )
        .expect("unpack");
        assert_eq!(message.message_id, id);
        assert_eq!(message.source_hash, HOME);
        assert_eq!(message.verification, leviculum_lxmf::Verification::Valid);
        assert_eq!(message.content, b"hallo");
        // The same client id again, later: the same message, nothing new.
        let (again, accepted) = send(&mut store, &home, LAPTOP, &submission(1), 260.0);
        assert_eq!(again, Ok(id));
        assert!(accepted.is_none());
        // Another id is another message.
        let (other, accepted) = send(&mut store, &home, LAPTOP, &submission(2), 260.0);
        assert_ne!(other.expect("accepted"), id);
        assert!(accepted.is_some());
        // The outbox holds both, and the archive has one sent entry each.
        assert_eq!(store.outbox_open().expect("open").len(), 2);
        let page = store.archive_since(0, usize::MAX).expect("page");
        assert_eq!(
            page.entries.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [crate::home::KIND_SENT, crate::home::KIND_SENT]
        );
        assert_eq!(page.entries[0].packed, message.pack());
    }

    #[test]
    fn a_send_from_a_device_not_admitted_is_refused() {
        let (_dir, mut store) = store();
        let home = home();
        let (refused, accepted) = send(&mut store, &home, LAPTOP, &submission(1), 200.0);
        assert_eq!(refused, Err(Status::NotAdmitted));
        assert!(accepted.is_none());
        assert!(store.outbox_open().expect("open").is_empty());
        let answer = answer(
            &mut store,
            &home,
            SEND_PATH,
            &wire::send_request(&submission(1)),
            None,
            1.0,
        );
        assert_eq!(read_status(&answer.bytes), Ok(Status::NotIdentified));
    }

    #[test]
    fn the_outbox_and_its_states_survive_a_restart() {
        let dir = tempfile::tempdir().expect("dir");
        let home = home();
        let id = {
            let mut store = Store::open_in(dir.path(), HOME).expect("store");
            admitted(&mut store);
            let (one, _) = send(&mut store, &home, LAPTOP, &submission(1), 200.0);
            let (two, _) = send(&mut store, &home, LAPTOP, &submission(2), 201.0);
            let (one, two) = (one.expect("one"), two.expect("two"));
            store
                .outbox_record(&one, Change::Propagated, 300, u64::MAX)
                .expect("record");
            store
                .outbox_record(&two, Change::Delivered, 301, u64::MAX)
                .expect("record");
            one
        };
        let mut store = Store::open_in(dir.path(), HOME).expect("reopen");
        let open = store.outbox_open().expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id);
        assert!(open[0].propagated);
        assert_eq!(open[0].created_at, 200);
        // The retried submission still finds it after the restart.
        let (again, accepted) = send(&mut store, &home, LAPTOP, &submission(1), 400.0);
        assert_eq!(again, Ok(id));
        assert!(accepted.is_none());
        let kinds: Vec<u8> = store
            .archive_since(0, usize::MAX)
            .expect("page")
            .entries
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                crate::home::KIND_SENT,
                crate::home::KIND_SENT,
                crate::home::KIND_PROPAGATED,
                crate::home::KIND_DELIVERED
            ]
        );
        store
            .outbox_record(&id, Change::Ended { propagated: true }, 500, u64::MAX)
            .expect("record");
        assert!(store.outbox_open().expect("open").is_empty());
    }

    #[test]
    fn an_admission_token_expires() {
        let (_dir, mut store) = store();
        store.issue_token(&TOKEN, "laptop", 0).expect("issue");
        store.issue_token(&[0x44; 16], "desk", 0).expect("issue");
        store.issue_token(&[0x45; 16], "late", DAY).expect("issue");
        let open = store.open_tokens(DAY - 1, DAY).expect("tokens");
        assert_eq!(
            open.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["desk", "laptop", "late"]
        );
        // A second before the day is up it still admits.
        assert_eq!(
            join_at(&mut store, LAPTOP, TOKEN, (DAY - 1) as f64),
            Status::Ok
        );
        // At the day it is refused, and gone.
        assert_eq!(
            join_at(&mut store, [0x55; 16], [0x44; 16], DAY as f64),
            Status::BadToken
        );
        let open = store.open_tokens(DAY, DAY).expect("tokens");
        assert_eq!(
            open.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["late"]
        );
        assert_eq!(open[0].token, [0x45; 16]);
        assert_eq!(store.open_tokens(2 * DAY, DAY).expect("tokens"), []);
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
            &home(),
            JOIN_PATH,
            &wire::join_request(&TOKEN, "x"),
            None,
            1.0,
        );
        assert_eq!(read_status(&answer.bytes), Ok(Status::NotIdentified));
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
            .redeem_token(&[1; 16], &[0xab; 16], 2, DAY)
            .expect("redeem");
        store
            .redeem_token(&[2; 16], &[0xcd; 16], 3, DAY)
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
