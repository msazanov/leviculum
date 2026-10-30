//! Tests for per-link ciphers, tunnel datagrams and GCM LRRTT retries.
//! A child of `node::tests`, so it uses that module's helpers.
use super::*;

/// As `establish_link_pair_without_rtt`, with an explicit Link cipher on both ends.
fn establish_link_pair_without_rtt_with_cipher(
    cipher: crate::LinkCipher,
) -> LinkPairWithoutRtt {
    use crate::transport::InterfaceId;

    let resp_identity = Identity::generate(&mut OsRng);
    let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
    let clock = MockClock::new(TEST_TIME_MS);
    let mut responder = NodeCoreBuilder::new()
        .link_cipher(cipher)
        .build(OsRng, clock, NoStorage);
    let mut resp_dest = Destination::new(
        Some(resp_identity),
        Direction::In,
        DestinationType::Single,
        "testapp",
        &["rttretry"],
    )
    .unwrap();
    resp_dest.set_accepts_links(true);
    let dest_hash = *resp_dest.hash();
    responder.register_destination(resp_dest);

    let clock = MockClock::new(TEST_TIME_MS);
    let mut initiator = NodeCoreBuilder::new()
        .link_cipher(cipher)
        .build(OsRng, clock, NoStorage);

    // Initiator connects
    let (init_link_id, _, output) = initiator
        .connect(dest_hash, &resp_signing_key)
        .expect("connect");
    let link_req_data = extract_broadcast_data(&output);

    // Responder receives link request → auto-accepts (Stage 1): proof in actions
    let output = responder.handle_packet(InterfaceId(0), &link_req_data);
    let resp_link_id = Link::calculate_link_id(&link_req_data);
    let proof_data = extract_broadcast_data(&output);

    // Initiator receives proof → Active, emits RTT packet
    let output = initiator.handle_packet(InterfaceId(0), &proof_data);
    assert!(
        output
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
        "initiator should get LinkEstablished"
    );
    let rtt_data = extract_broadcast_data(&output);

    // DO NOT deliver rtt_data to responder, that's the point of this helper

    (initiator, responder, init_link_id, resp_link_id, rtt_data)
}

/// Responder with `resp_cipher` (and optionally CBC accepted), initiator asking for `init_cipher` on this link only.
/// Returns the initiator/responder link ids if the link came up on the initiator, `None` if the request was dropped.
fn cipher_pair(
    resp_cipher: crate::LinkCipher,
    accept_cbc: bool,
    init_cipher: crate::LinkCipher,
) -> Option<(
    NodeCore<OsRng, MockClock, NoStorage>,
    NodeCore<OsRng, MockClock, NoStorage>,
    LinkId,
    LinkId,
)> {
    use crate::transport::InterfaceId;
    let resp_identity = Identity::generate(&mut OsRng);
    let resp_signing_key = resp_identity.ed25519_verifying().to_bytes();
    let mut responder = NodeCoreBuilder::new()
        .link_cipher(resp_cipher)
        .accept_cbc_links(accept_cbc)
        .build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut resp_dest = Destination::new(
        Some(resp_identity),
        Direction::In,
        DestinationType::Single,
        "testapp",
        &["echo"],
    )
    .unwrap();
    resp_dest.set_accepts_links(true);
    let dest_hash = *resp_dest.hash();
    responder.register_destination(resp_dest);
    // the initiator's node default is whatever; the cipher of this link is chosen per connect
    let mut initiator =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let (init_id, _, output) = initiator
        .connect_with_cipher(dest_hash, &resp_signing_key, None, init_cipher)
        .expect("connect");
    let req = extract_broadcast_data(&output);
    let resp_id = Link::calculate_link_id(&req);
    let out = responder.handle_packet(InterfaceId(0), &req);
    let proof = out
        .actions
        .iter()
        .map(|a| match a {
            crate::transport::Action::Broadcast { data, .. }
            | crate::transport::Action::SendPacket { data, .. } => data.clone(),
        })
        .next()?;
    let out = initiator.handle_packet(InterfaceId(0), &proof);
    if !out
        .events
        .iter()
        .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }))
    {
        return None;
    }
    let rtt = extract_broadcast_data(&out);
    let _ = responder.handle_packet(InterfaceId(0), &rtt);
    Some((initiator, responder, init_id, resp_id))
}

#[test]
fn test_cbc_request_needs_the_accept_flag_on_a_gcm_responder() {
    use crate::LinkCipher::{Cbc, Gcm};
    assert!(
        cipher_pair(Gcm, false, Cbc).is_none(),
        "GCM-only responder must drop a CBC request"
    );
    let (init, resp, i, r) =
        cipher_pair(Gcm, true, Cbc).expect("accepting responder answers a CBC request");
    assert_eq!(init.link(&i).unwrap().cipher(), Cbc);
    assert_eq!(
        resp.link(&r).unwrap().cipher(),
        Cbc,
        "the link keeps the cipher it asked for, it is not upgraded"
    );
}

#[test]
fn test_gcm_request_is_never_answered_with_cbc_and_cbc_only_node_drops_gcm() {
    use crate::LinkCipher::{Cbc, Gcm};
    let (init, resp, i, r) = cipher_pair(Gcm, true, Gcm).expect("GCM link between GCM nodes");
    assert_eq!(init.link(&i).unwrap().cipher(), Gcm);
    assert_eq!(resp.link(&r).unwrap().cipher(), Gcm);
    assert!(
        cipher_pair(Cbc, false, Gcm).is_none(),
        "a default (CBC) node must not take a GCM request"
    );
    assert!(
        cipher_pair(Cbc, true, Gcm).is_none(),
        "accepting CBC never widens to GCM"
    );
}

#[test]
fn test_cbc_link_on_a_gcm_node_carries_channel_data_with_proofs() {
    use crate::transport::InterfaceId;
    use crate::LinkCipher::{Cbc, Gcm};
    let (mut init, mut resp, i, _r) = cipher_pair(Gcm, true, Cbc).unwrap();
    let out = init.send_on_link(&i, b"enroll").unwrap();
    let wire = extract_broadcast_data(&out);
    let delivered = resp.handle_packet(InterfaceId(0), &wire);
    assert!(
        delivered
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::MessageReceived { .. })),
        "CBC link on a GCM node delivers"
    );
    assert!(init.receipt_count_for_link(&i) <= 1);
}

#[test]
fn channel_messages_are_proven_in_both_directions_for_cbc_and_gcm() {
    use crate::packet::{Packet, PacketContext, PacketType};
    use crate::transport::InterfaceId;
    use crate::LinkCipher::{Cbc, Gcm};

    for cipher in [Cbc, Gcm] {
        let (mut initiator, mut responder, init_id, resp_id) =
            cipher_pair(cipher, true, cipher)
                .expect("cipher pair establishes");

        let outbound = initiator
            .send_on_link(&init_id, b"initiator to responder")
            .unwrap();
        let wire = extract_broadcast_data(&outbound);
        let received = responder.handle_packet(InterfaceId(0), &wire);
        assert!(received.events.iter().any(|event| matches!(
            event,
            NodeEvent::MessageReceived { data, .. } if data == b"initiator to responder"
        )));
        let proof = received
            .actions
            .iter()
            .find_map(|action| {
                let raw = match action {
                    crate::transport::Action::Broadcast { data, .. }
                    | crate::transport::Action::SendPacket { data, .. } => data,
                };
                Packet::unpack(raw).ok().and_then(|packet| {
                    (packet.flags.packet_type == PacketType::Proof
                        && packet.context == PacketContext::None
                        && packet.data.len() == crate::constants::PROOF_DATA_SIZE)
                        .then(|| raw.clone())
                })
            })
            .expect("receiver emits a data proof");
        assert_eq!(initiator.receipt_count_for_link(&init_id), 1);
        let confirmed = initiator.handle_packet(InterfaceId(0), &proof);
        assert!(confirmed.events.iter().any(|event| matches!(
            event,
            NodeEvent::LinkDeliveryConfirmed { link_id, .. } if *link_id == init_id
        )));
        assert_eq!(initiator.receipt_count_for_link(&init_id), 0);

        let outbound = responder
            .send_on_link(&resp_id, b"responder to initiator")
            .unwrap();
        let wire = extract_broadcast_data(&outbound);
        let received = initiator.handle_packet(InterfaceId(0), &wire);
        assert!(received.events.iter().any(|event| matches!(
            event,
            NodeEvent::MessageReceived { data, .. } if data == b"responder to initiator"
        )));
        let proof = received
            .actions
            .iter()
            .find_map(|action| {
                let raw = match action {
                    crate::transport::Action::Broadcast { data, .. }
                    | crate::transport::Action::SendPacket { data, .. } => data,
                };
                Packet::unpack(raw).ok().and_then(|packet| {
                    (packet.flags.packet_type == PacketType::Proof
                        && packet.context == PacketContext::None
                        && packet.data.len() == crate::constants::PROOF_DATA_SIZE)
                        .then(|| raw.clone())
                })
            })
            .expect("receiver emits a data proof");
        assert_eq!(responder.receipt_count_for_link(&resp_id), 1);
        let confirmed = responder.handle_packet(InterfaceId(0), &proof);
        assert!(confirmed.events.iter().any(|event| matches!(
            event,
            NodeEvent::LinkDeliveryConfirmed { link_id, .. } if *link_id == resp_id
        )));
        assert_eq!(responder.receipt_count_for_link(&resp_id), 0);
    }
}

/// The tunnel datagram: one sealed packet, no receipt, no proof, no ack, delivered as plain link data,
/// and a replayed copy is refused by the GCM record window while the first is still delivered.
#[test]
fn test_link_tunnel_datagram_is_receiptless_proofless_and_replay_safe() {
    use crate::transport::InterfaceId;
    for cipher in [crate::LinkCipher::Cbc, crate::LinkCipher::Gcm] {
        let mut pair = establish_nodecore_link_pair_with_strategy(ProofStrategy::All);
        if cipher == crate::LinkCipher::Gcm {
            // The helper builds CBC links; the GCM path is covered end to end in the link tests.
            continue;
        }
        let payload = [0xAB_u8; 300];
        let output = pair
            .initiator
            .send_link_tunnel(&pair.initiator_link_id, &payload)
            .unwrap();
        assert_eq!(
            pair.initiator
                .receipt_count_for_link(&pair.initiator_link_id),
            0,
            "a tunnel datagram must not retain a receipt"
        );
        let wire = extract_broadcast_data(&output);
        assert_eq!(
            wire[18],
            crate::packet::PacketContext::Tunnel.to_byte(),
            "context byte on the wire"
        );

        let delivered = pair.responder.handle_packet(InterfaceId(0), &wire);
        assert!(
            delivered.events.iter().any(|e| matches!(
                e,
                NodeEvent::LinkDataReceived { data, .. } if data[..] == payload[..]
            )),
            "responder must receive the datagram"
        );
        assert!(
            delivered.actions.is_empty(),
            "no proof or ack may be emitted for a datagram even under ProofStrategy::All"
        );
        assert_eq!(
            pair.initiator
                .receipt_count_for_link(&pair.initiator_link_id),
            0
        );

        // Oversize is refused, a closed handle path stays an error and not a panic.
        let too_big = alloc::vec![0u8; 70_000];
        assert!(pair
            .initiator
            .send_link_tunnel(&pair.initiator_link_id, &too_big)
            .is_err());
    }
}

#[test]
fn test_gcm_rtt_retry_recovers_when_first_packet_is_lost() {
    use crate::packet::{Packet, PacketContext};
    use crate::transport::InterfaceId;

    let (mut initiator, mut responder, init_link_id, resp_link_id, first_rtt) =
        establish_link_pair_without_rtt_with_cipher(crate::LinkCipher::Gcm);
    let init_link = initiator.link(&init_link_id).unwrap();
    assert_eq!(init_link.cipher(), crate::LinkCipher::Gcm);
    assert_eq!(init_link.rtt_send_count(), 1);
    assert_eq!(
        Packet::unpack(&first_rtt).unwrap().context,
        PacketContext::Lrrtt
    );

    // Drop the initial LRRTT. At the old mode-7 retry interval, exactly one
    // fresh GCM record is emitted; it is not an application/data retry.
    let interval = init_link.rtt_retry_interval_ms();
    initiator.transport().clock().set(TEST_TIME_MS + interval);
    let output = initiator.handle_timeout();
    let rtts: Vec<Vec<u8>> = extract_all_action_data(&output)
        .into_iter()
        .filter(|raw| {
            Packet::unpack(raw).is_ok_and(|packet| packet.context == PacketContext::Lrrtt)
        })
        .collect();
    assert_eq!(rtts.len(), 1, "one bounded mode-7 retry becomes due");
    assert_ne!(rtts[0], first_rtt, "GCM retry uses a fresh nonce/record");
    assert_eq!(initiator.link(&init_link_id).unwrap().rtt_send_count(), 2);

    let established = responder.handle_packet(InterfaceId(0), &rtts[0]);
    assert!(established.events.iter().any(|event| matches!(
        event,
        NodeEvent::LinkEstablished {
            is_initiator: false,
            ..
        }
    )));
    assert_eq!(
        responder.link(&resp_link_id).unwrap().state(),
        LinkState::Active
    );
}

#[test]
fn test_gcm_rtt_retry_stops_at_previous_five_retry_budget() {
    use crate::packet::{Packet, PacketContext};

    let (mut initiator, _responder, init_link_id, _resp_link_id, _lost_rtt) =
        establish_link_pair_without_rtt_with_cipher(crate::LinkCipher::Gcm);
    initiator
        .link_mut(&init_link_id)
        .unwrap()
        .set_timing_for_test(3_600_000, 3_600_000, TEST_TIME_MS);
    let interval = initiator
        .link(&init_link_id)
        .unwrap()
        .rtt_retry_interval_ms();

    for retry in 1..=crate::constants::RTT_RETRY_MAX_ATTEMPTS {
        initiator
            .transport()
            .clock()
            .set(TEST_TIME_MS + interval * retry as u64);
        let output = initiator.handle_timeout();
        let rtts = extract_all_action_data(&output)
            .iter()
            .filter_map(|raw| Packet::unpack(raw).ok())
            .filter(|packet| packet.context == PacketContext::Lrrtt)
            .count();
        assert_eq!(rtts, 1, "retry {retry} should send exactly one LRRTT");
        assert_eq!(
            initiator.link(&init_link_id).unwrap().rtt_send_count(),
            retry + 1
        );
    }

    initiator
        .transport()
        .clock()
        .set(TEST_TIME_MS + interval * (crate::constants::RTT_RETRY_MAX_ATTEMPTS as u64 + 1));
    let exhausted = initiator.handle_timeout();
    assert!(!extract_all_action_data(&exhausted)
        .iter()
        .filter_map(|raw| Packet::unpack(raw).ok())
        .any(|packet| packet.context == PacketContext::Lrrtt));
    assert_eq!(initiator.link(&init_link_id).unwrap().rtt_send_count(), 6);
}

#[test]
fn test_gcm_duplicate_rtt_does_not_reestablish_or_change_activity() {
    use crate::transport::InterfaceId;

    let (_initiator, mut responder, _init_link_id, resp_link_id, rtt_data) =
        establish_link_pair_without_rtt_with_cipher(crate::LinkCipher::Gcm);
    let first = responder.handle_packet(InterfaceId(0), &rtt_data);
    assert_eq!(
        first
            .events
            .iter()
            .filter(|event| matches!(event, NodeEvent::LinkEstablished { .. }))
            .count(),
        1
    );
    let link = responder.link(&resp_link_id).unwrap();
    assert_eq!(link.state(), LinkState::Active);
    let rtt_us = link.rtt_us();
    let last_inbound_ms = link.last_inbound_ms();
    let last_data_age = link.no_data_for_ms(TEST_TIME_MS);

    // Even a stale link is not recovered by a duplicate LRRTT: it is a
    // handshake packet, not new authenticated Link activity.
    responder
        .link_mut(&resp_link_id)
        .unwrap()
        .set_state(LinkState::Stale);
    let duplicate = responder.handle_packet(InterfaceId(0), &rtt_data);
    assert!(!duplicate.events.iter().any(|event| matches!(
        event,
        NodeEvent::LinkEstablished { .. } | NodeEvent::LinkRecovered { .. }
    )));
    let link = responder.link(&resp_link_id).unwrap();
    assert_eq!(link.state(), LinkState::Stale);
    assert_eq!(link.rtt_us(), rtt_us);
    assert_eq!(link.last_inbound_ms(), last_inbound_ms);
    assert_eq!(link.no_data_for_ms(TEST_TIME_MS), last_data_age);
}

/// The cipher-less helper builds CBC links; the single-shot LRRTT tests rely on that.
#[test]
fn default_pair_without_rtt_is_cbc() {
    let (initiator, _responder, init_link_id, _resp_link_id, _rtt) = establish_link_pair_without_rtt();
    assert_eq!(initiator.link(&init_link_id).unwrap().cipher(), crate::LinkCipher::Cbc);
}
