//! Interop cell for leviculum#849: a propagation node asks a peer that just
//! linked for the destinations it holds mail for, and the REFERENCE answers.
//!
//! The board side is a sans-I/O `NodeCore` with transport on, holding mail
//! for an `lxmf.delivery` destination that a real Python `rnsd` owns. The
//! node knows the destination's identity but has no path, the state the
//! #365 cull leaves after a phone walks away. At peer-up it runs what the
//! board firmware's `on_ble_peer_up` runs ([`solicit_on_peer_up`]) and puts
//! the request on the TCP link to the daemon. Green: the daemon answers with
//! an announce for its destination marked as a path response
//! (`Transport.path_request`, the local destination arm,
//! `reference/Reticulum/RNS/Transport.py:2938-2940`), the announce triggers
//! the push, and the daemon receives the stored plaintext. The daemon never
//! announces on its own in this cell, so the only announce is the answer.

use std::time::Duration;

use rand_core::OsRng;
use tokio::io::AsyncReadExt;
use tokio::time::timeout;

use leviculum_core::packet::{Packet, PacketContext, PacketType};
use leviculum_core::{
    Action, Identity, InterfaceId, MemoryStorage, NodeCoreBuilder, NodeEvent, TickOutput,
};
use leviculum_lxmf::active_delivery::{solicit_on_peer_up, PeerUpSolicit, SolicitPacer};
use leviculum_lxmf::propagation_store::StoredMessage;
use leviculum_std::interfaces::hdlc::{DeframeResult, Deframer};

use crate::common::{connect_to_daemon, parse_dest_hash, send_framed, TestClock};
use crate::harness::TestDaemon;

/// The daemon's link as the board's interface would name its peer.
const DAEMON_LINK: [u8; 16] = [0xD0; 16];

fn frames(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|action| match action {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data.clone(),
        })
        .collect()
}

#[tokio::test]
async fn the_reference_answers_the_peer_up_request_and_the_mail_follows() {
    let daemon = TestDaemon::start().await.expect("start rnsd");
    let info = daemon
        .register_destination("lxmf", &["delivery"])
        .await
        .expect("register lxmf.delivery on the daemon");
    let delivery = parse_dest_hash(&info.hash);
    let public_key = hex::decode(&info.public_key).expect("public key hex");

    let mut board = NodeCoreBuilder::new().enable_transport(true).build(
        OsRng,
        TestClock,
        MemoryStorage::with_defaults(),
    );
    board.set_interface_name(0, String::from("link"));
    board.remember_identity(
        delivery,
        Identity::from_public_key_bytes(&public_key).expect("daemon identity"),
    );
    assert!(!board.has_path(&delivery), "premise: no path at peer-up");

    let plaintext = b"stored on the board for the daemon".to_vec();
    let ciphertext = board
        .encrypt_for_destination(&delivery, &plaintext)
        .expect("encrypt for the daemon's destination");
    let store = [StoredMessage {
        transient_id: [0x22; 32],
        destination_hash: *delivery.as_bytes(),
        size: ciphertext.len() as u32,
        received_at: 0,
        stamp_value: 13,
        sequence: 1,
    }];

    let mut stream = connect_to_daemon(&daemon).await;

    // Peer-up: the board asks the new link about its stored destinations.
    let mut pick = PeerUpSolicit::new(&[]);
    for meta in &store {
        pick.consider(meta);
    }
    let mut pacer = SolicitPacer::new();
    let (out, asked) =
        solicit_on_peer_up(&mut board, &mut pacer, 0, DAEMON_LINK, &pick.destinations());
    assert_eq!(asked, vec![*delivery.as_bytes()]);
    let requests = frames(&out);
    assert_eq!(requests.len(), 1, "one path request at peer-up");
    for frame in &requests {
        send_framed(&mut stream, frame).await;
    }

    // The reference answers; the answer triggers the push.
    let mut deframer = Deframer::new();
    let mut buffer = [0u8; 4096];
    let mut answer = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while answer.is_none() && std::time::Instant::now() < deadline {
        let Ok(Ok(n)) = timeout(Duration::from_millis(200), stream.read(&mut buffer)).await else {
            continue;
        };
        assert!(n > 0, "the daemon closed the link");
        for result in deframer.process(&buffer[..n]) {
            let DeframeResult::Frame(data) = result else {
                continue;
            };
            let Ok(packet) = Packet::unpack(&data) else {
                continue;
            };
            if packet.flags.packet_type == PacketType::Announce
                && packet.destination_hash == *delivery.as_bytes()
            {
                assert_eq!(
                    packet.context,
                    PacketContext::PathResponse,
                    "the only announce is the answer to the request"
                );
                answer = Some(data.clone());
            }
        }
    }
    let answer = answer.expect("the reference answers the path request for its destination");

    let heard = board.handle_packet_from_peer(InterfaceId(0), DAEMON_LINK, &answer);
    assert!(
        heard.events.iter().any(|event| matches!(
            event,
            NodeEvent::AnnounceReceived { announce, .. }
                if announce.destination_hash() == &delivery
        )),
        "the answer reaches the board as an announce, the push trigger"
    );
    let (_, push) = board
        .send_raw_single_packet(&delivery, &ciphertext)
        .expect("the answer installed a path");
    for frame in frames(&push) {
        send_framed(&mut stream, &frame).await;
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let received = daemon
            .get_received_single_packets()
            .await
            .expect("query the daemon");
        if received.iter().any(|packet| packet.data == plaintext) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon never received the stored message: {received:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
