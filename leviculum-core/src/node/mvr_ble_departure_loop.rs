//! mvr: after a phone leaves, a relay takes its neighbour's route back
//! through itself and every packet for the phone makes one round trip
//! (leviculum#848).
//!
//! ## The field failure this reproduces
//!
//! Field session 2026-10-19 (boards on 9018725f, captures under
//! `rig-run/ble-drop/`, UTC stamps): the Fairphone (transport id
//! `c4a8083f`, delivery destination D = `0a68951f`) turned Bluetooth off.
//! At 10:45:30 feld-t114 logged `BLE peer lost, 3 paths culled` — the #365
//! cull removed its direct entry for D. t114-boot (`2fe95060`) held D at
//! two hops VIA feld-t114, learned from feld-t114's own rebroadcast; its
//! cull correctly removed nothing (`0 paths culled`), the phone was never
//! its next hop. At 10:45:58 the Pixel asked for D; t114-boot answered
//! from that entry (`ANN_TX occasion=path-response dst=0a68951f hops=2
//! iface=2`), and feld-t114, now without any entry for D, installed it at
//! three hops via t114-boot. From 10:46:02 every opportunistic LXMF packet
//! the Pixel handed feld-t114 went to t114-boot (`PKT_RELAY
//! outcome=forwarded ... iface_out=2`, iface 2 is the boards' BLE
//! interface), t114-boot sent it straight back, and feld-t114 dropped it
//! (`outcome=duplicate hops=3`). Five packets, two transmissions each;
//! the message reached the phone later over the propagation node.
//!
//! ## Reference semantics
//!
//! Python takes the same loop. An AutoInterface peer that stops is
//! detached and handed to `Transport.remove_interface`
//! (AutoInterface.py:385-390, :661-677); the next table cull removes
//! every path whose interface is gone (Transport.py:784-785), and with
//! the entry go its random blobs, the only thing Python's admission uses
//! "to avoid loops in the network" (Transport.py:1790-1797). The path
//! response then meets an unknown destination and is admitted outright
//! (Transport.py:1828-1831). A copy that comes back is dropped by the
//! packet-hash filter (Transport.py:1376-1386), exactly as ours is
//! (`transport.rs`, the dedup gate ahead of routing). Python's own
//! requestor guard (Transport.py:2958-2966) refuses to answer the next
//! hop's REQUEST; it does not stop the next hop from hearing an answer
//! given to somebody else.
//!
//! ## Shape
//!
//! 2 nodes (both the boards' exact storage type), deterministic,
//! sub-second. X (feld-t114) has interface A, where the phone and the
//! Pixel are linked, and interface B, which it shares with N
//! (t114-boot). The phone announces on A; X rebroadcasts on B; N learns
//! D via X. The phone's peer loss on A culls X's entry. The Pixel's path
//! request reaches N, and N's real answer reaches X. Then the Pixel's
//! packet for D arrives at X on A. The Pixel is a packet source only.
//!
//! The first test pins the damping the order asked about and holds
//! today: a relayed copy that comes back from the next hop is not
//! relayed again. The second is the loop itself, red today and
//! `#[ignore]`d until a fix is chosen; it asserts the outcome (no copy
//! toward N), so a receive-side refusal and a responder-side split
//! horizon would both turn it green.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, PATH_REQUEST_GRACE_MS, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::embedded_storage::EmbeddedStorage;
use crate::identity::Identity;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Clock;
use crate::transport::{Action, InterfaceId, TickOutput};

/// The boards' exact shape: `EmbeddedStorage`, transport enabled.
type EmbeddedNode = NodeCore<OsRng, MockClock, EmbeddedStorage>;

fn make_node() -> Box<EmbeddedNode> {
    NodeCoreBuilder::new().enable_transport(true).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        EmbeddedStorage::new(),
    )
}

fn add_iface(node: &mut EmbeddedNode, name: &'static str, id: u8) -> usize {
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new(name, id)));
    node.set_interface_name(idx, String::from(name));
    idx
}

fn pack(packet: &Packet) -> Vec<u8> {
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// The phone: the identity it presents on the BLE link, and its
/// delivery destination.
struct Phone {
    identity_hash: [u8; TRUNCATED_HASHBYTES],
    dest_hash: crate::DestinationHash,
    dest: Destination,
}

fn make_phone() -> Phone {
    let identity = Identity::generate(&mut OsRng);
    let identity_hash = *identity.hash();
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    Phone {
        identity_hash,
        dest_hash,
        dest,
    }
}

impl Phone {
    /// A direct (wire hops 0) announce emitted at `ts`.
    fn direct_announce(&mut self, ts: u64) -> Vec<u8> {
        pack(&self.dest.announce(None, &mut OsRng, ts, ts / 1000).unwrap())
    }
}

/// The Pixel's opportunistic packet for `dest`, handed to the relay
/// `transport_id` (wire hops 0). `tag` keeps two packets distinct.
fn packet_via(transport_id: [u8; TRUNCATED_HASHBYTES], dest: &[u8; 16], tag: u8) -> Vec<u8> {
    pack(&Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type2,
            context_flag: false,
            transport_type: TransportType::Transport,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Data,
        },
        hops: 0,
        transport_id: Some(transport_id),
        destination_hash: *dest,
        context: PacketContext::None,
        data: PacketData::Owned(std::vec![tag; 64]),
    })
}

/// Every packet in this output with its send target (`None` = broadcast).
fn wire_out(out: &TickOutput) -> Vec<(Option<InterfaceId>, Vec<u8>)> {
    out.actions
        .iter()
        .map(|action| match action {
            Action::SendPacket { iface, data, .. } => (Some(*iface), data.clone()),
            Action::Broadcast { data, .. } => (None, data.clone()),
        })
        .collect()
}

/// The packets of `kind` naming `dest` in these outputs.
fn of_kind(outs: &[TickOutput], kind: PacketType, dest: &[u8; 16]) -> Vec<Vec<u8>> {
    outs.iter()
        .flat_map(wire_out)
        .map(|(_, data)| data)
        .filter(|data| {
            Packet::unpack(data)
                .map(|p| p.flags.packet_type == kind && &p.destination_hash == dest)
                .unwrap_or(false)
        })
        .collect()
}

/// Advance `node`'s clock in steps and collect what its scheduler emits:
/// rebroadcasts and grace-window path answers.
fn run_scheduler(node: &mut EmbeddedNode, step_ms: u64, steps: usize) -> Vec<TickOutput> {
    let mut outs = Vec::new();
    for _ in 0..steps {
        let next = node.transport().clock().now_ms() + step_ms;
        node.transport().clock().set(next);
        outs.push(node.handle_timeout());
    }
    outs
}

/// Hand every packet to `node` on `iface`, collecting the outputs.
fn feed(node: &mut EmbeddedNode, iface: usize, packets: &[Vec<u8>]) -> Vec<TickOutput> {
    packets
        .iter()
        .map(|raw| node.handle_packet(InterfaceId(iface), raw))
        .collect()
}

/// The field chain up to the Pixel's first packet: X learns D direct on
/// A, rebroadcasts it to N on B, loses the phone and culls, and hears
/// N's answer to the Pixel's path request. Returns X, X's interfaces
/// (A, B), N, N's interface, and the phone.
fn field_state() -> (
    Box<EmbeddedNode>,
    usize,
    usize,
    Box<EmbeddedNode>,
    usize,
    Phone,
) {
    let mut phone = make_phone();
    let dest = *phone.dest_hash.as_bytes();

    let mut x = make_node();
    let a = add_iface(&mut x, "ble_phone", 0);
    let b = add_iface(&mut x, "ble_boards", 1);
    let x_id = *x.identity().hash();

    let mut n = make_node();
    let nb = add_iface(&mut n, "ble_boards", 1);

    // The phone announces on A: X holds D direct.
    let _ = x.handle_packet(InterfaceId(a), &phone.direct_announce(TEST_TIME_MS));
    let entry = x
        .get_path_clone(phone.dest_hash.as_bytes())
        .expect("X holds D");
    assert_eq!((entry.hops, entry.next_hop), (1, None), "X holds D direct");

    // X's rebroadcast reaches N on B: N holds D via X, two hops.
    let rebroadcast = of_kind(
        &run_scheduler(&mut x, 1_000, 10),
        PacketType::Announce,
        &dest,
    );
    assert!(
        !rebroadcast.is_empty(),
        "X rebroadcasts the phone's announce"
    );
    let _ = feed(&mut n, nb, &rebroadcast);
    let entry = n
        .get_path_clone(phone.dest_hash.as_bytes())
        .expect("N holds D");
    assert_eq!(
        (entry.hops, entry.next_hop),
        (2, Some(x_id)),
        "N holds D via X, as t114-boot held it via feld-t114"
    );
    // N's own transit rebroadcast is setup traffic; let it go.
    let _ = run_scheduler(&mut n, 1_000, 10);

    // The phone turns Bluetooth off: the #365 cull fires on X.
    let _ = x.handle_interface_peer_lost(InterfaceId(a), phone.identity_hash);
    assert!(
        !x.has_path(&phone.dest_hash),
        "the cull must fire first; without it this is #365, not #848"
    );

    // The Pixel asks for D and N hears it. N's next hop is X, not the
    // Pixel, so the requestor guard lets N answer, on B, where X is.
    let mut pixel = make_node();
    let _ = add_iface(&mut pixel, "ble_boards", 1);
    let pr_hash = *pixel.transport().path_request_hash();
    let request: Vec<Vec<u8>> = wire_out(&pixel.request_path(&phone.dest_hash))
        .into_iter()
        .map(|(_, data)| data)
        .filter(|data| {
            Packet::unpack(data)
                .map(|p| p.destination_hash == pr_hash)
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(request.len(), 1, "the Pixel emits one path request");
    let mut n_outs = feed(&mut n, nb, &request);
    n_outs.extend(run_scheduler(&mut n, PATH_REQUEST_GRACE_MS + 1_000, 3));
    let answer = of_kind(&n_outs, PacketType::Announce, &dest);
    assert!(
        !answer.is_empty(),
        "N answers the Pixel from its entry via X"
    );
    let _ = feed(&mut x, b, &answer);

    (x, a, b, n, nb, phone)
}

/// The damping the order asked to check: a packet X relayed that comes
/// back from the next hop is dropped by the dedup gate, not relayed a
/// second time. Built from the loop state directly (X via N, N via X)
/// so it holds whatever fix closes #848.
#[test]
fn a_relayed_packet_that_comes_back_is_not_relayed_again() {
    let mut phone = make_phone();
    let dest = *phone.dest_hash.as_bytes();

    let mut x = make_node();
    let a = add_iface(&mut x, "ble_phone", 0);
    let b = add_iface(&mut x, "ble_boards", 1);
    let x_id = *x.identity().hash();
    let mut n = make_node();
    let nb = add_iface(&mut n, "ble_boards", 1);
    let n_id = *n.identity().hash();

    // The loop state: each holds D via the other.
    let raw = phone.direct_announce(TEST_TIME_MS);
    let relayed = |via: [u8; TRUNCATED_HASHBYTES], hops: u8| {
        let mut p = Packet::unpack(&raw).unwrap();
        p.flags.header_type = HeaderType::Type2;
        p.flags.transport_type = TransportType::Transport;
        p.hops = hops;
        p.transport_id = Some(via);
        pack(&p)
    };
    let _ = x.handle_packet(InterfaceId(b), &relayed(n_id, 2));
    let _ = n.handle_packet(InterfaceId(nb), &relayed(x_id, 1));
    assert_eq!(
        x.get_path_clone(phone.dest_hash.as_bytes())
            .unwrap()
            .next_hop,
        Some(n_id)
    );
    assert_eq!(
        n.get_path_clone(phone.dest_hash.as_bytes())
            .unwrap()
            .next_hop,
        Some(x_id)
    );

    // The Pixel's packet: X relays it toward N, N sends it back.
    let x_out = x.handle_packet(InterfaceId(a), &packet_via(x_id, &dest, 0x51));
    let to_n = of_kind(&[x_out], PacketType::Data, &dest);
    assert_eq!(to_n.len(), 1, "X relays the packet toward N");
    let back = of_kind(&feed(&mut n, nb, &to_n), PacketType::Data, &dest);
    assert_eq!(back.len(), 1, "N relays it straight back to X");

    let again = of_kind(&feed(&mut x, b, &back), PacketType::Data, &dest);
    assert!(
        again.is_empty(),
        "the returning copy is a duplicate and must not be relayed again"
    );
    assert_eq!(x.transport().stats().drops_duplicate(), 1);
}

/// THE #848 loop, red today: after the cull X admits N's answer, a route
/// through itself, and hands the Pixel's packet to N, which returns it.
/// Green means no copy of the packet goes toward N.
#[test]
#[ignore = "#848: red until a fix is chosen; the report names the candidates"]
fn a_culled_relay_does_not_send_the_phones_packets_round_its_neighbour() {
    let (mut x, a, _b, _n, _nb, phone) = field_state();
    let dest = *phone.dest_hash.as_bytes();
    let x_id = *x.identity().hash();

    let x_out = x.handle_packet(InterfaceId(a), &packet_via(x_id, &dest, 0x52));
    let relayed = of_kind(&[x_out], PacketType::Data, &dest);
    assert!(
        relayed.is_empty(),
        "X relayed the phone's packet toward N, whose only route to the \
         phone is X itself (entry now {:?})",
        x.get_path_clone(phone.dest_hash.as_bytes()).map(|e| (
            e.hops,
            e.interface_index,
            e.next_hop
        ))
    );
}

/// The red test's setup is the field's: pinned green so the ignored test
/// cannot go green by losing its own preconditions. X re-learns D at
/// three hops via N on B.
#[test]
fn the_field_chain_puts_x_back_on_a_route_through_n() {
    let (x, _a, b, n, _nb, phone) = field_state();
    let n_id = *n.identity().hash();
    let entry = x.get_path_clone(phone.dest_hash.as_bytes());
    let state = entry.map(|e| (e.hops, e.interface_index, e.next_hop));
    // Today: admitted. A receive-side fix leaves X without the entry; a
    // responder-side fix never lets N's answer reach X. Both are the fix,
    // so this pin accepts either and refuses only a different route.
    assert!(
        state.is_none() || state == Some((3, b, Some(n_id))),
        "unexpected route for D on X: {:?}",
        state
    );
}
