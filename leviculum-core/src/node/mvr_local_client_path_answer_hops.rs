//! mvr: a shared instance answers a local client's path request for a
//! destination hosted on ANOTHER local client at zero hops (714).
//!
//! ## The defect
//!
//! Two local clients on one shared instance: A hosts a destination (a stock
//! `lxmd`), B wants to link to it (a stock LXMF client). A hands the instance
//! its announce at wire hops 0, flagged as a path response. The instance
//! stores it at 0 (`incoming_hop_count`: the receipt increment, then the
//! local-client decrement, Transport.py:1457 and :1482). B's path request is
//! answered from the cache by case 2a of `handle_path_request`, which put
//! `cached + 1` on the wire: right for a path learned from the network, where
//! the stored count IS the cached byte plus the receipt hop, but one too many
//! for a local client's announce, whose stored count equals the cached byte.
//! B learned the destination at 1 hop. A proves B's link request at 0 hops and
//! the instance forwards the proof unchanged, so a Python client drops it
//! (`packet.hops == link.expected_hops`, Transport.py:2228) and the link times
//! out.
//!
//! The reference answers with the stored path-table count
//! (Transport.py:2956 `packet.hops = path_table[dst][IDX_PT_HOPS]`), which is
//! 0 here; B's receipt over its shared-instance uplink adds and removes one
//! (Transport.py:1457, :1484), so B records 0 hops, matching the proof.
//!
//! ## The scenario
//!
//! ```text
//!   A (hosts D) --IPC-- R (shared instance) --IPC-- B (requests D, links)
//! ```
//!
//! Our Rust link layer does not enforce the reference's `expected_hops`
//! check, so the test states it explicitly: the hop count B holds for D must
//! equal the hop count the forwarded proof carries after B's receipt
//! adjustment (wire + 1 - 1 on a shared-instance uplink).
//!
//! Sans-I/O: no Docker, no Python, sub-second wall clock.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::MTU;
use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::{Packet, PacketContext, PacketType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{Action, InterfaceId, TickOutput};
use crate::DestinationHash;

type InstanceNode = NodeCore<OsRng, MockClock, MemoryStorage>;
type ClientNode = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The shared instance: one network interface and two local-client
/// interfaces. Returns (node, client A's interface, client B's interface).
fn make_instance(enable_transport: bool) -> (InstanceNode, usize, usize) {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node: InstanceNode = NodeCoreBuilder::new()
        .enable_transport(enable_transport)
        .build(OsRng, clock, MemoryStorage::with_defaults());
    let net = node
        .transport
        .register_interface(Box::new(MockInterface::new("net", 0)));
    node.set_interface_name(net, String::from("tcp_server/0.0.0.0:4242"));
    let mut client = |name: &'static str, hw: u8| {
        let idx = node
            .transport
            .register_interface(Box::new(MockInterface::new(name, hw)));
        node.set_interface_name(idx, String::from(name));
        node.set_interface_local_client(idx, true);
        idx
    };
    let a = client("Local[rns/default]/A", 1);
    let b = client("Local[rns/default]/B", 2);
    (node, a, b)
}

/// A client of the shared instance: transport off, its only interface the
/// uplink to the instance.
fn make_client() -> (ClientNode, usize) {
    let clock = MockClock::new(TEST_TIME_MS);
    let mut node: ClientNode =
        NodeCoreBuilder::new().build(OsRng, clock, MemoryStorage::with_defaults());
    let up = node
        .transport
        .register_interface(Box::new(MockInterface::new("uplink", 0)));
    node.set_interface_name(up, String::from("LocalClient[default]"));
    node.transport.set_shared_instance_interface(Some(up));
    (node, up)
}

/// Client A with a link-accepting destination D. Returns A, its uplink, D's
/// hash, D's signing key, and the announce A hands the instance: wire hops 0,
/// context PATH_RESPONSE, as `lxmd` sends it on attaching.
fn make_host() -> (ClientNode, usize, DestinationHash, [u8; 32], Vec<u8>) {
    let (mut node, up) = make_client();
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["localpeer"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();

    let mut announce = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    announce.context = PacketContext::PathResponse;
    let mut buf = [0u8; MTU];
    let len = announce.pack(&mut buf).unwrap();

    node.register_destination(dest);
    (node, up, dest_hash, signing_key, buf[..len].to_vec())
}

/// Every packet this output puts on interface `iface`.
fn reaching(output: &TickOutput, iface: usize) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::SendPacket { iface: i, data, .. } if i.0 == iface => Some(data.clone()),
            Action::Broadcast {
                data,
                exclude_iface,
                exclude_ifaces,
                ..
            } if exclude_iface.map(|e| e.0) != Some(iface)
                && !exclude_ifaces.iter().any(|e| e.0 == iface) =>
            {
                Some(data.clone())
            }
            _ => None,
        })
        .collect()
}

/// The single packet of `packet_type` this output puts on `iface`.
fn one_reaching(output: &TickOutput, iface: usize, packet_type: PacketType) -> Vec<u8> {
    let mut hits: Vec<Vec<u8>> = reaching(output, iface)
        .into_iter()
        .filter(|raw| {
            Packet::unpack(raw)
                .map(|p| p.flags.packet_type == packet_type)
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one {packet_type:?} on interface {iface}, got {}",
        hits.len()
    );
    hits.pop().unwrap()
}

fn all_packets(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .collect()
}

/// A registers D on the instance, B asks for the path, then links. Asserts
/// the answer's hop count, what B records, and that B's view matches the
/// proof's hop count end to end.
fn run(enable_transport: bool) {
    let (mut instance, a_iface, b_iface) = make_instance(enable_transport);
    let (mut host, host_up, dest, signing_key, announce) = make_host();
    let (mut requester, req_up) = make_client();

    // A attaches and announces D as a path response at 0 hops.
    let _ = instance.handle_packet(InterfaceId(a_iface), &announce);
    let now = instance.transport().clock().now_ms();
    instance.transport().clock().set(now + 5_000);
    let _ = instance.handle_timeout();
    assert_eq!(
        instance.hops_to(&dest),
        Some(0),
        "the instance stores a local client's 0-hop announce at 0 \
         (Transport.py:1457 then :1482)"
    );

    // B asks for the path; case 2a answers from the cache at once.
    let request_out = requester.request_path(&dest);
    let request = all_packets(&request_out);
    assert_eq!(request.len(), 1, "B emits one path request");
    let out = instance.handle_packet(InterfaceId(b_iface), &request[0]);
    let answer = one_reaching(&out, b_iface, PacketType::Announce);
    let answer_hops = Packet::unpack(&answer).unwrap().hops;
    assert_eq!(
        answer_hops, 0,
        "the answer must carry the stored count 0 (Transport.py:2956), \
         not the cached byte plus one"
    );

    let _ = requester.handle_packet(InterfaceId(req_up), &answer);
    let b_hops = requester.hops_to(&dest);
    assert_eq!(
        b_hops,
        Some(0),
        "B must learn D at 0 hops, as it does on an rnsd shared instance"
    );

    // B links to D through the instance; A proves; the instance relays the
    // proof back to B.
    let (_link, _routed, out) = requester.connect(dest, &signing_key).expect("connect");
    let link_request = all_packets(&out);
    assert_eq!(link_request.len(), 1, "B emits one link request");
    let out = instance.handle_packet(InterfaceId(b_iface), &link_request[0]);
    let forwarded = one_reaching(&out, a_iface, PacketType::LinkRequest);
    let out = host.handle_packet(InterfaceId(host_up), &forwarded);
    let proof = all_packets(&out);
    assert_eq!(proof.len(), 1, "A emits one link proof");
    let out = instance.handle_packet(InterfaceId(a_iface), &proof[0]);
    let relayed = one_reaching(&out, b_iface, PacketType::Proof);

    // The reference's check at the initiator (Transport.py:2228): the
    // proof's hop count after receipt (wire + 1 - 1 over a shared-instance
    // uplink, :1457 and :1484) must equal the path's hop count.
    let proof_hops = Packet::unpack(&relayed).unwrap().hops;
    assert_eq!(
        Some(proof_hops),
        b_hops,
        "a Python client drops a proof whose hop count differs from its \
         path's (Transport.py:2228)"
    );

    let out = requester.handle_packet(InterfaceId(req_up), &relayed);
    assert!(
        out.events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
        "B's link to D must establish"
    );
}

/// lnsd runs as a transport instance in the 712 cell.
#[test]
fn transport_instance_answers_a_co_attached_destination_at_zero_hops() {
    run(true);
}

/// A shared instance without transport answers a local client the same way
/// (Transport.py:2943, `transport_enabled() or is_from_local_client`).
#[test]
fn non_transport_instance_answers_a_co_attached_destination_at_zero_hops() {
    run(false);
}
