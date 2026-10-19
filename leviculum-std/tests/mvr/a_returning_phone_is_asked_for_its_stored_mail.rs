//! mvr: a propagation node holding mail for a phone delivers it when the
//! phone's link comes back up, not only when the phone announces
//! (leviculum#849).
//!
//! **The named failure mode:** field session 2026-10-19. The Fairphone had a
//! message waiting in feld-t114's store and reconnected over BLE; every board
//! logged `BLE: connected` and the peer id `c4a8083f` within two seconds,
//! and nothing was delivered until the phone announced by hand. The board's
//! push is triggered only by an announce or a found path for the recipient
//! (`trigger_active` in `leviculum-nrf/src/pn.rs`), and at link-up it holds
//! neither: the #365 cull dropped the phone's path entry when the phone left,
//! and the handshake names the phone's TRANSPORT identity, while the store is
//! keyed by the delivery destination of its MESSAGE identity, a different
//! keypair on Columba.
//!
//! Two `NodeCore`s here, the board (transport on) and the phone, one
//! multi-peer interface between them. The board learns the phone's delivery
//! destination from an announce on the peer link, stores a message for it,
//! loses the peer, and sees it come back. What the board does at peer-up is
//! what `on_ble_peer_up` in the board firmware does: the core's
//! `handle_interface_peer_up` and the propagation role's
//! [`solicit_on_peer_up`]. The push is the firmware's `trigger_active`
//! reduced to its decision ([`ActiveCandidate`]) and its send
//! (`send_raw_single_packet`). Green: a path request for the stored
//! destination leaves on the returning peer's link, the phone answers it as
//! a path response, and the stored ciphertext reaches the phone, without any
//! announce the phone sent on its own.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use leviculum_core::constants::PATH_REQUEST_MIN_INTERVAL_MS;
use leviculum_core::transport::TickOutput;
use leviculum_core::{
    Action, Clock, Destination, DestinationHash, DestinationType, Direction, Identity, InterfaceId,
    MemoryStorage, NodeCore, NodeCoreBuilder, NodeEvent, ProofStrategy,
};
use leviculum_lxmf::active_delivery::{
    solicit_on_peer_up, ActiveCandidate, PeerUpSolicit, SolicitPacer,
};
use leviculum_lxmf::propagation_store::StoredMessage;
use rand_core::OsRng;

/// One clock for both nodes, advanced by hand.
#[derive(Clone)]
struct SharedClock(Arc<AtomicU64>);

impl SharedClock {
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for SharedClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn wall_unix_secs(&self) -> Option<u64> {
        Some(self.now_ms() / 1000)
    }
}

type TestNode = NodeCore<OsRng, SharedClock, MemoryStorage>;

/// The board's one BLE interface.
const BLE: usize = 0;

/// The phone's transport identity as the handshake names it: unrelated to
/// the delivery destination, as on Columba.
const PHONE_LINK: [u8; 16] = [0xC4; 16];

/// A second phone in the room that owns nothing the store holds.
const OTHER_LINK: [u8; 16] = [0x5A; 16];

struct Room {
    clock: SharedClock,
    board: TestNode,
    phone: TestNode,
    delivery: DestinationHash,
    store: Vec<StoredMessage>,
    ciphertext: Vec<u8>,
    plaintext: Vec<u8>,
    pacer: SolicitPacer,
}

fn frames(output: &TickOutput) -> Vec<(Vec<u8>, Option<[u8; 16]>)> {
    output
        .actions
        .iter()
        .map(|action| match action {
            Action::SendPacket { data, peer, .. } => (data.clone(), *peer),
            Action::Broadcast { data, .. } => (data.clone(), None),
        })
        .collect()
}

/// Every announce for `destination` the board reports in `output`, as
/// `is_path_response` flags.
fn announces_for(output: &TickOutput, destination: &DestinationHash) -> Vec<bool> {
    output
        .events
        .iter()
        .filter_map(|event| match event {
            NodeEvent::AnnounceReceived { announce, .. }
                if announce.destination_hash() == destination =>
            {
                Some(announce.is_path_response())
            }
            _ => None,
        })
        .collect()
}

/// A path request (PLAIN destination, the well-known path-request hash)
/// naming `destination` in its first 16 data bytes.
fn is_path_request_for(frame: &[u8], destination: &DestinationHash) -> bool {
    let pr_dest = leviculum_core::crypto::truncated_hash(&Destination::compute_name_hash(
        "rnstransport",
        &["path", "request"],
    ));
    // Header type 1: flags, hops, dest(16), context, data.
    frame.len() >= 19 + 16 && frame[2..18] == pr_dest && frame[19..35] == destination.as_bytes()[..]
}

fn room() -> Room {
    let clock = SharedClock(Arc::new(AtomicU64::new(1_700_000_000_000)));
    let mut board = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        clock.clone(),
        MemoryStorage::with_defaults(),
    );
    board.set_interface_name(BLE, String::from("ble"));
    let mut phone =
        NodeCoreBuilder::new().build(OsRng, clock.clone(), MemoryStorage::with_defaults());
    phone.set_interface_name(0, String::from("ble"));

    // The phone's message identity and its LXMF delivery destination,
    // proving what it receives as an LXMF client does.
    let message_identity = Identity::generate(&mut OsRng);
    let mut destination = Destination::new(
        Some(message_identity),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .expect("delivery destination");
    destination.set_proof_strategy(ProofStrategy::All);
    let delivery = *destination.hash();
    phone.register_destination(destination);

    // The earlier visit: the phone announced on its link, the board learned
    // its identity and a direct path through that peer.
    let announce = phone
        .announce_destination(&delivery, None)
        .expect("the phone announces");
    for (frame, _) in frames(&announce) {
        let _ = board.handle_packet_from_peer(InterfaceId(BLE), PHONE_LINK, &frame);
    }
    assert!(
        board.has_path(&delivery),
        "premise: the board heard the phone"
    );

    // A message stored for it: the ciphertext the originator produced.
    let plaintext = b"stored for the phone while it was away".to_vec();
    let ciphertext = board
        .encrypt_for_destination(&delivery, &plaintext)
        .expect("the board knows the phone's identity");
    let store = vec![StoredMessage {
        transient_id: [0x11; 32],
        destination_hash: *delivery.as_bytes(),
        size: ciphertext.len() as u32,
        received_at: 1_700_000_000,
        stamp_value: 13,
        sequence: 1,
    }];

    // The phone walks away: the #365 cull drops its path.
    let _ = board.handle_interface_peer_lost(InterfaceId(BLE), PHONE_LINK);
    assert!(
        !board.has_path(&delivery),
        "premise: the cull left no path to trigger a push"
    );

    Room {
        clock,
        board,
        phone,
        delivery,
        store,
        ciphertext,
        plaintext,
        pacer: SolicitPacer::new(),
    }
}

impl Room {
    /// What the board does when a BLE peer finishes its handshake.
    fn peer_up(&mut self, link: [u8; 16]) -> (TickOutput, Vec<[u8; 16]>) {
        let mut out = self.board.handle_interface_peer_up(InterfaceId(BLE), link);
        let mut pick = PeerUpSolicit::new(&[]);
        for meta in &self.store {
            pick.consider(meta);
        }
        let (solicit, asked) = solicit_on_peer_up(
            &mut self.board,
            &mut self.pacer,
            BLE,
            link,
            &pick.destinations(),
        );
        out.merge(solicit);
        (out, asked)
    }

    /// Hand `frame` to the phone and collect what it answers, letting its
    /// timers run for a few seconds in case the answer is scheduled.
    fn phone_answers(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        let mut answer: Vec<Vec<u8>> = frames(&self.phone.handle_packet(InterfaceId(0), frame))
            .into_iter()
            .map(|(data, _)| data)
            .collect();
        for _ in 0..50 {
            self.clock.advance(100);
            answer.extend(
                frames(&self.phone.handle_timeout())
                    .into_iter()
                    .map(|(data, _)| data),
            );
        }
        answer
    }

    /// The firmware's `trigger_active`: on an announce for a destination
    /// with stored mail, push the oldest pushable message.
    fn push_if_triggered(&mut self, output: &TickOutput) -> Option<TickOutput> {
        if announces_for(output, &self.delivery).is_empty() {
            return None;
        }
        let mut candidate = ActiveCandidate::new(*self.delivery.as_bytes(), &[]);
        for meta in &self.store {
            candidate.consider(meta);
        }
        candidate.best()?;
        let (_, sent) = self
            .board
            .send_raw_single_packet(&self.delivery, &self.ciphertext)
            .expect("the answer installed a path");
        Some(sent)
    }
}

#[test]
fn the_returning_phone_is_asked_and_its_mail_follows() {
    let mut room = room();

    let (up, asked) = room.peer_up(PHONE_LINK);
    let requests: Vec<(Vec<u8>, Option<[u8; 16]>)> = frames(&up)
        .into_iter()
        .filter(|(frame, _)| is_path_request_for(frame, &room.delivery))
        .collect();
    assert_eq!(
        requests.len(),
        1,
        "one path request for the stored destination leaves at peer-up"
    );
    assert_eq!(
        requests[0].1,
        Some(PHONE_LINK),
        "the request goes on the returning peer's link alone"
    );
    assert_eq!(asked, vec![*room.delivery.as_bytes()]);
    // The cost per stored destination and link-up: a Type1 header (flags,
    // hops, 16-byte destination, context) and the transport form of the
    // request data (destination, our transport id, tag: 48 bytes).
    assert_eq!(requests[0].0.len(), 2 + 16 + 1 + 48);

    // The phone answers for its own destination, as a path response.
    let answer = room.phone_answers(&requests[0].0);
    assert!(!answer.is_empty(), "the phone answers the path request");
    let mut pushed = None;
    for frame in &answer {
        let heard = room
            .board
            .handle_packet_from_peer(InterfaceId(BLE), PHONE_LINK, frame);
        let kinds = announces_for(&heard, &room.delivery);
        assert!(
            kinds.iter().all(|path_response| *path_response),
            "no unsolicited announce from the phone: every one is a path response"
        );
        if let Some(sent) = room.push_if_triggered(&heard) {
            pushed = Some(sent);
        }
    }
    let pushed = pushed.expect("the answer triggers the push");

    // The stored message reaches the phone.
    let mut received = Vec::new();
    for (frame, _) in frames(&pushed) {
        for event in room.phone.handle_packet(InterfaceId(0), &frame).events {
            if let NodeEvent::PacketReceived {
                destination, data, ..
            } = event
            {
                assert_eq!(destination, room.delivery);
                received.push(data);
            }
        }
    }
    assert_eq!(received, vec![room.plaintext.clone()]);
}

/// A link that flaps asks once per interval, and a second phone linking
/// inside that interval is still asked: the pacing is per destination and
/// per peer link.
#[test]
fn a_flapping_link_asks_once_per_interval_and_a_second_phone_is_still_asked() {
    let mut room = room();

    let (_, first) = room.peer_up(PHONE_LINK);
    assert_eq!(first.len(), 1);
    for _ in 0..5 {
        room.clock.advance(2_000);
        let _ = room
            .board
            .handle_interface_peer_lost(InterfaceId(BLE), PHONE_LINK);
        let (up, asked) = room.peer_up(PHONE_LINK);
        assert!(
            asked.is_empty(),
            "a relink inside the interval asks nothing"
        );
        assert!(
            !frames(&up)
                .iter()
                .any(|(frame, _)| is_path_request_for(frame, &room.delivery)),
            "no request on the wire either"
        );
    }

    let (up, asked) = room.peer_up(OTHER_LINK);
    assert_eq!(asked, vec![*room.delivery.as_bytes()]);
    assert!(frames(&up).iter().any(
        |(frame, peer)| is_path_request_for(frame, &room.delivery) && *peer == Some(OTHER_LINK)
    ));

    room.clock.advance(PATH_REQUEST_MIN_INTERVAL_MS);
    let (_, again) = room.peer_up(PHONE_LINK);
    assert_eq!(again.len(), 1, "past the interval the phone is asked again");
}
