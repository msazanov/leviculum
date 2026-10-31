//! Tests for hash-free tunnel datagrams.
use super::*;

/// A link-addressed Data packet with the given context, wire hops 0.
fn link_packet_with_context(
    link_id: [u8; TRUNCATED_HASHBYTES],
    context: PacketContext,
) -> Vec<u8> {
    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: true,
            transport_type: TransportType::Broadcast,
            dest_type: crate::destination::DestinationType::Link,
            packet_type: PacketType::Data,
        },
        hops: 0,
        transport_id: None,
        destination_hash: link_id,
        context,
        data: PacketData::Owned(alloc::vec![0x5A; 200]),
    };
    let mut buf = [0u8; 500];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

fn relay_with_link(link_id: [u8; TRUNCATED_HASHBYTES]) -> Transport<MockClock, MemoryStorage> {
    let mut transport = make_transport_enabled();
    let _ = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
    let _ = transport.register_interface(Box::new(MockInterface::new("if1", 2)));
    let now = transport.clock.now_ms();
    transport.storage_mut().set_link_entry(
        link_id,
        LinkEntry {
            timestamp_ms: now,
            next_hop_interface_index: 1,
            remaining_hops: 1,
            received_interface_index: 0,
            hops: 1,
            validated: true,
            proof_timeout_ms: now + 900_000,
            destination_hash: [0xBB; TRUNCATED_HASHBYTES],
            peer_signing_key: None,
            received_peer: None,
            next_hop_peer: None,
        },
    );
    transport
}

fn truncated_packet_hash_of(raw: &[u8]) -> [u8; TRUNCATED_HASHBYTES] {
    let mut out = [0u8; TRUNCATED_HASHBYTES];
    out.copy_from_slice(&packet_hash(raw)[..TRUNCATED_HASHBYTES]);
    out
}

/// The bulk-link hot path: a relay forwards tunnel datagrams without
/// hashing the payload, leaves no table entry keyed by a hash, and a
/// byte-identical retransmission is forwarded again, as before.
#[test]
fn test_tunnel_data_relay_forwards_without_hashing() {
    let _journey_off = journey_override::JourneyOff::new();
    let link_id = [0xC1; TRUNCATED_HASHBYTES];
    let mut transport = relay_with_link(link_id);
    let raw = link_packet_with_context(link_id, PacketContext::Tunnel);

    for round in 0..2 {
        transport.process_incoming(0, &raw).unwrap();
        let sent = transport
            .drain_actions()
            .iter()
            .filter(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == 1))
            .count();
        assert_eq!(sent, 1, "round {round}: tunnel datagram must be forwarded");
    }
    assert_eq!(transport.storage().packet_hash_count(), 0, "no hash may be inserted");
    assert!(
        transport.storage().get_reverse(&truncated_packet_hash_of(&raw)).is_none(),
        "no reverse entry: tunnel datagrams are never proven"
    );
    assert_eq!(transport.stats().packets_forwarded, 2);
}

/// The neighbouring classes must keep their hash bookkeeping exactly.
#[test]
fn test_non_tunnel_link_data_relay_still_hashes() {
    let _journey_off = journey_override::JourneyOff::new();
    for context in [PacketContext::Channel, PacketContext::Resource, PacketContext::Keepalive, PacketContext::None] {
        let link_id = [0xC2; TRUNCATED_HASHBYTES];
        let mut transport = relay_with_link(link_id);
        let raw = link_packet_with_context(link_id, context);
        transport.process_incoming(0, &raw).unwrap();
        assert_eq!(transport.stats().packets_forwarded, 1, "{context:?} forwarded");
        assert_eq!(transport.storage().packet_hash_count(), 1, "{context:?} hash cached");
        assert!(
            transport.storage().get_reverse(&truncated_packet_hash_of(&raw)).is_some(),
            "{context:?} keeps its reverse entry"
        );
    }
}

/// Local delivery of a tunnel datagram carries no hash and requests no
/// proof from the destination layer; any other link context still does.
#[test]
fn test_tunnel_data_local_delivery_carries_no_hash() {
    let _journey_off = journey_override::JourneyOff::new();
    let link_id = [0xC3; TRUNCATED_HASHBYTES];
    for (context, hash_free) in [
        (PacketContext::Tunnel, true),
        (PacketContext::Channel, false),
        (PacketContext::Resource, false),
        (PacketContext::None, false),
    ] {
        let mut transport = make_transport_enabled();
        let _ = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
        transport.register_destination(link_id);
        let raw = link_packet_with_context(link_id, context);
        transport.process_incoming(0, &raw).unwrap();
        let events: Vec<_> = transport.drain_events().collect();
        let raw_hash = events.iter().find_map(|e| match e {
            TransportEvent::PacketReceived { raw_hash, .. } => Some(*raw_hash),
            _ => None,
        });
        let proof_requested =
            events.iter().any(|e| matches!(e, TransportEvent::ProofRequested { .. }));
        if hash_free {
            assert_eq!(raw_hash, Some(None), "{context:?}: delivered without hash");
            assert!(!proof_requested, "{context:?}: no ProofRequested");
            assert_eq!(transport.storage().packet_hash_count(), 0);
        } else {
            assert_eq!(raw_hash, Some(Some(packet_hash(&raw))), "{context:?}: hash kept");
            assert!(proof_requested, "{context:?}: ProofRequested kept");
        }
    }
}

/// A tunnel datagram for a link nobody here holds is handed up unhashed
/// (the node layer drops it as `link-data-no-link`), not cached.
#[test]
fn test_tunnel_data_for_unknown_link_is_handed_up_unhashed() {
    let _journey_off = journey_override::JourneyOff::new();
    // A non-transport node: link-addressed data it neither relays nor owns.
    let mut transport = make_transport_enabled();
    transport.config.enable_transport = false;
    let _ = transport.register_interface(Box::new(MockInterface::new("if0", 1)));
    let raw = link_packet_with_context([0xC4; TRUNCATED_HASHBYTES], PacketContext::Tunnel);
    transport.process_incoming(0, &raw).unwrap();
    let events: Vec<_> = transport.drain_events().collect();
    assert!(events.iter().any(|e| matches!(
        e,
        TransportEvent::PacketReceived { raw_hash: None, .. }
    )));
    assert_eq!(transport.storage().packet_hash_count(), 0);
}

/// With the journey target enabled the hash exists anyway (the PKT_RX
/// correlator needs it), so it is used exactly as before: still no extra
/// hashing, and nothing downstream sees a placeholder.
#[test]
fn test_tunnel_data_keeps_its_hash_while_the_journey_target_is_on() {
    // No JourneyOff here: the process-global capture subscriber (or the
    // real one in production) is what turns the target on.
    crate::test_log_capture::with_captured_logs(|| {
        let link_id = [0xC5; TRUNCATED_HASHBYTES];
        let mut transport = relay_with_link(link_id);
        assert!(transport.pkt_journey_enabled(), "capture subscriber must enable it");
        let raw = link_packet_with_context(link_id, PacketContext::Tunnel);
        transport.process_incoming(0, &raw).unwrap();
        assert_eq!(transport.stats().packets_forwarded, 1);
        assert_eq!(transport.storage().packet_hash_count(), 1);
        assert!(transport.storage().get_reverse(&truncated_packet_hash_of(&raw)).is_some());
    });
}
