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
//! The first test pins the damping: a relayed copy that comes back from
//! the next hop is not relayed again. The second is the loop itself,
//! closed by #852: the returning copy proves N routes D through X, so X
//! drops the entry and refuses N's replay of the same emission. The
//! last is the valid route a tombstone would have killed (X, N, Y,
//! phone), which must survive because no packet comes back.
//!
//! The last three tests are #854, which keeps the loop from being offered
//! at all on per-peer links. N answers the Pixel on every BLE link but
//! X's, because its own route to D runs over X's link, and N's transit
//! rebroadcast is withheld from X's link the same way. On LoRa, one shared
//! medium, X hears the answer as before, and #852 is what closes the loop
//! there. An interface that cannot address a subset of its links (lnsd's
//! BLE) also answers as before, because withholding there would withhold
//! from everyone.

extern crate std;

use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;
use std::string::String;
use std::vec::Vec;

use alloc::collections::BTreeMap;

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
use crate::traits::{Clock, Interface, InterfaceError};
use crate::transport::{dispatch_actions, Action, InterfaceId, TickOutput};

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

/// THE #848 loop, closed by #852. After the cull X admits N's answer, a
/// route through itself, and hands the Pixel's first packet to N, which
/// returns it addressed to X. That return is the proof: X drops the
/// circular entry, refuses N's next answer carrying the same emission, and
/// the Pixel's second packet no longer goes round N. The first packet is
/// the price of the proof; X cannot tell the route from a valid one before
/// (the counterexample test below is the valid one).
#[test]
fn a_culled_relay_does_not_send_the_phones_packets_round_its_neighbour() {
    let (mut x, a, b, mut n, nb, phone) = field_state();
    let dest = *phone.dest_hash.as_bytes();
    let x_id = *x.identity().hash();

    // The first packet goes round once, as in the field.
    let x_out = x.handle_packet(InterfaceId(a), &packet_via(x_id, &dest, 0x52));
    let to_n = of_kind(&[x_out], PacketType::Data, &dest);
    assert_eq!(to_n.len(), 1, "X relays the first packet toward N");
    let back = of_kind(&feed(&mut n, nb, &to_n), PacketType::Data, &dest);
    assert_eq!(back.len(), 1, "N relays it straight back to X");
    let again = of_kind(&feed(&mut x, b, &back), PacketType::Data, &dest);
    assert!(again.is_empty(), "the returning copy is not relayed again");
    assert!(
        !x.has_path(&phone.dest_hash),
        "the return proves N routes D through X; X must drop the entry (now {:?})",
        x.get_path_clone(phone.dest_hash.as_bytes()).map(|e| (
            e.hops,
            e.interface_index,
            e.next_hop
        ))
    );

    // The Pixel asks again and N answers again from the same emission:
    // X must not take it back.
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
    let mut n_outs = feed(&mut n, nb, &request);
    n_outs.extend(run_scheduler(&mut n, PATH_REQUEST_GRACE_MS + 1_000, 3));
    let answer = of_kind(&n_outs, PacketType::Announce, &dest);
    assert!(!answer.is_empty(), "N answers the Pixel again");
    let _ = feed(&mut x, b, &answer);
    assert!(
        !x.has_path(&phone.dest_hash),
        "N's repeated answer replays the proven loop and must be refused"
    );

    // So the Pixel's next packet does not go round N.
    let x_out = x.handle_packet(InterfaceId(a), &packet_via(x_id, &dest, 0x53));
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

    // The refusal is for that emission only: the phone's next announce,
    // here back on A, installs as usual.
    let mut phone = phone;
    let later = TEST_TIME_MS + 600_000;
    x.transport().clock().set(later);
    let _ = x.handle_packet(InterfaceId(a), &phone.direct_announce(later));
    let entry = x.get_path_clone(phone.dest_hash.as_bytes());
    assert_eq!(
        entry.map(|e| (e.hops, e.interface_index, e.next_hop)),
        Some((1, a, None)),
        "a new emission ends the refusal"
    );
}

/// The field's own shape: the phone, the Pixel and N all sit on X's ONE
/// BLE interface, as on the boards (iface 2), so the interfaces cannot
/// tell the return from the Pixel retrying the same bytes. The peer links
/// can: a retry comes from the peer the original came from, the return
/// from the peer the route runs to. Without peer links on one interface
/// (a shared LoRa channel) nothing is concluded, and the route stays.
#[test]
fn on_one_multi_peer_interface_the_peer_links_tell_a_return_from_a_retry() {
    let mut phone = make_phone();
    let dest = *phone.dest_hash.as_bytes();
    let pixel_peer = *Identity::generate(&mut OsRng).hash();

    let mut x = make_node();
    let ble = add_iface(&mut x, "ble", 2);
    let x_id = *x.identity().hash();
    let mut n = make_node();
    let nb = add_iface(&mut n, "ble", 2);
    let n_id = *n.identity().hash();

    // The loop state, with X's entry learned through N's peer link.
    let raw = phone.direct_announce(TEST_TIME_MS);
    let relayed = |via: [u8; TRUNCATED_HASHBYTES], hops: u8| {
        let mut p = Packet::unpack(&raw).unwrap();
        p.flags.header_type = HeaderType::Type2;
        p.flags.transport_type = TransportType::Transport;
        p.hops = hops;
        p.transport_id = Some(via);
        pack(&p)
    };
    let _ = x.handle_packet_from_peer(InterfaceId(ble), n_id, &relayed(n_id, 2));
    let _ = n.handle_packet(InterfaceId(nb), &relayed(x_id, 1));
    let route = |x: &EmbeddedNode| {
        x.get_path_clone(phone.dest_hash.as_bytes())
            .map(|e| (e.interface_index, e.next_hop, e.via_peer))
    };
    assert_eq!(route(&x), Some((ble, Some(n_id), Some(n_id))));

    // The Pixel sends, X relays toward N on the same interface, and the
    // Pixel retries the same bytes: a duplicate addressed to X on the
    // relay's outbound interface, but from the original's peer.
    let packet = packet_via(x_id, &dest, 0x56);
    let to_n = of_kind(
        &[x.handle_packet_from_peer(InterfaceId(ble), pixel_peer, &packet)],
        PacketType::Data,
        &dest,
    );
    assert_eq!(to_n.len(), 1, "X relays the packet toward N");
    let _ = x.handle_packet_from_peer(InterfaceId(ble), pixel_peer, &packet);
    assert_eq!(x.transport().stats().drops_duplicate(), 1);
    assert_eq!(
        route(&x),
        Some((ble, Some(n_id), Some(n_id))),
        "a retry from the original's peer proves nothing"
    );

    // Without peer links the same retry is indistinguishable from a
    // return, so it proves nothing either.
    let _ = x.handle_packet(InterfaceId(ble), &packet);
    assert_eq!(x.transport().stats().drops_duplicate(), 2);
    assert_eq!(
        route(&x),
        Some((ble, Some(n_id), Some(n_id))),
        "on one interface without peer links nothing is concluded"
    );

    // N sends it back through its own link: that is the proof.
    let back = of_kind(&feed(&mut n, nb, &to_n), PacketType::Data, &dest);
    assert_eq!(back.len(), 1, "N relays it straight back to X");
    let _ = x.handle_packet_from_peer(InterfaceId(ble), n_id, &back[0]);
    assert_eq!(x.transport().stats().drops_duplicate(), 3);
    assert_eq!(
        route(&x),
        None,
        "the copy N returned proves the loop; X must drop the entry"
    );
}

/// The counterexample 848 named against a tombstone: the phone is linked
/// to X and Y, N learned D from Y, and the phone leaves X only. X culls and
/// takes N's answer, X, N, Y, phone, which is a valid route: N hands the
/// packet on to Y, not back to X. On the shared medium X overhears that
/// copy, but it names Y as its next hop, not X, so it proves nothing and
/// the route must survive.
#[test]
fn a_route_through_a_neighbour_that_does_not_return_the_packet_survives() {
    let mut phone = make_phone();
    let dest = *phone.dest_hash.as_bytes();

    let mut x = make_node();
    let a = add_iface(&mut x, "ble_phone", 0);
    let b = add_iface(&mut x, "ble_boards", 1);
    let x_id = *x.identity().hash();
    let mut n = make_node();
    let nb = add_iface(&mut n, "ble_boards", 1);
    let n_id = *n.identity().hash();
    let y_id = *Identity::generate(&mut OsRng).hash();

    // One emission, heard by X direct and by N through Y.
    let raw = phone.direct_announce(TEST_TIME_MS);
    let mut via_y = Packet::unpack(&raw).unwrap();
    via_y.flags.header_type = HeaderType::Type2;
    via_y.flags.transport_type = TransportType::Transport;
    via_y.hops = 1;
    via_y.transport_id = Some(y_id);
    let _ = n.handle_packet(InterfaceId(nb), &pack(&via_y));
    let _ = x.handle_packet(InterfaceId(a), &raw);
    let rebroadcast = of_kind(
        &run_scheduler(&mut x, 1_000, 10),
        PacketType::Announce,
        &dest,
    );
    let _ = feed(&mut n, nb, &rebroadcast);
    let _ = run_scheduler(&mut n, 1_000, 10);
    assert_eq!(
        n.get_path_clone(phone.dest_hash.as_bytes())
            .map(|e| (e.hops, e.next_hop)),
        Some((2, Some(y_id))),
        "N holds D via Y"
    );

    // The phone leaves X only; X takes N's answer to the Pixel.
    let _ = x.handle_interface_peer_lost(InterfaceId(a), phone.identity_hash);
    assert!(!x.has_path(&phone.dest_hash), "the cull fires on X");
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
    let mut n_outs = feed(&mut n, nb, &request);
    n_outs.extend(run_scheduler(&mut n, PATH_REQUEST_GRACE_MS + 1_000, 3));
    let answer = of_kind(&n_outs, PacketType::Announce, &dest);
    assert!(
        !answer.is_empty(),
        "N answers the Pixel from its entry via Y"
    );
    let _ = feed(&mut x, b, &answer);
    let route = Some((3, b, Some(n_id)));
    let state = |x: &EmbeddedNode| {
        x.get_path_clone(phone.dest_hash.as_bytes())
            .map(|e| (e.hops, e.interface_index, e.next_hop))
    };
    assert_eq!(state(&x), route, "X takes D at three hops via N");

    // The Pixel's packet: X to N, N on to Y, and X overhears N's copy.
    let x_out = x.handle_packet(InterfaceId(a), &packet_via(x_id, &dest, 0x54));
    let to_n = of_kind(&[x_out], PacketType::Data, &dest);
    assert_eq!(to_n.len(), 1, "X relays the packet toward N");
    let on_to_y = of_kind(&feed(&mut n, nb, &to_n), PacketType::Data, &dest);
    assert_eq!(on_to_y.len(), 1, "N relays it on");
    assert_eq!(
        Packet::unpack(&on_to_y[0]).unwrap().transport_id,
        Some(y_id),
        "N hands it to Y, not back to X"
    );
    let _ = feed(&mut x, b, &on_to_y);
    assert_eq!(
        x.transport().stats().drops_overheard_transport_id(),
        1,
        "X drops the overheard copy, it names Y as its next hop"
    );
    assert_eq!(
        state(&x),
        route,
        "the overheard copy proves no loop; the route must survive"
    );

    // And the next packet still takes it.
    let x_out = x.handle_packet(InterfaceId(a), &packet_via(x_id, &dest, 0x55));
    assert_eq!(
        of_kind(&[x_out], PacketType::Data, &dest).len(),
        1,
        "X keeps relaying toward N"
    );
}

/// The loop test's setup is the field's: pinned so that test cannot go
/// green by losing its own preconditions. X re-learns D at three hops via
/// N on B; nothing on X can tell that route from a valid one until a
/// packet comes back (#852 acts there, not at admission).
#[test]
fn the_field_chain_puts_x_back_on_a_route_through_n() {
    let (x, _a, b, n, _nb, phone) = field_state();
    let n_id = *n.identity().hash();
    let entry = x.get_path_clone(phone.dest_hash.as_bytes());
    let state = entry.map(|e| (e.hops, e.interface_index, e.next_hop));
    assert_eq!(
        state,
        Some((3, b, Some(n_id))),
        "unexpected route for D on X"
    );
}

/// What every link of a per-peer interface was handed, by peer.
type LinkLog = Rc<RefCell<Vec<([u8; TRUNCATED_HASHBYTES], Vec<u8>)>>>;

/// The BLE shape: several point-to-point links in ONE interface id, each
/// addressed by the identity its peer reported on peer-up. Only the sends
/// the core's actions reach are modelled (`mvr_ingress_link_exclusion`
/// carries the same fake).
struct PeerLinks {
    id: InterfaceId,
    links: Vec<[u8; TRUNCATED_HASHBYTES]>,
    log: LinkLog,
}

impl Interface for PeerLinks {
    fn id(&self) -> InterfaceId {
        self.id
    }
    fn name(&self) -> &str {
        "ble"
    }
    fn mtu(&self) -> usize {
        500
    }
    fn is_online(&self) -> bool {
        true
    }
    /// An announce with no addressee reaches every live link.
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        for link in &self.links {
            self.log.borrow_mut().push((*link, data.to_vec()));
        }
        Ok(())
    }
    /// Every live link but the one named.
    fn try_send_excluding_peer(
        &mut self,
        data: &[u8],
        peer: &[u8; TRUNCATED_HASHBYTES],
        _high_priority: bool,
    ) -> Result<(), InterfaceError> {
        for link in self.links.iter().filter(|link| *link != peer) {
            self.log.borrow_mut().push((*link, data.to_vec()));
        }
        Ok(())
    }
}

/// The Pixel's path request for `dest`, as it leaves the Pixel.
fn path_request(dest: &crate::DestinationHash) -> Vec<u8> {
    let mut pixel = make_node();
    let _ = add_iface(&mut pixel, "ble", 0);
    let pr_hash = *pixel.transport().path_request_hash();
    let mut request: Vec<Vec<u8>> = wire_out(&pixel.request_path(dest))
        .into_iter()
        .map(|(_, data)| data)
        .filter(|data| {
            Packet::unpack(data)
                .map(|p| p.destination_hash == pr_hash)
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(request.len(), 1, "the Pixel emits one path request");
    request.remove(0)
}

/// The phone's announce as X rebroadcasts it to N: one hop, X's transport id.
fn relayed_by(phone: &mut Phone, x_id: [u8; TRUNCATED_HASHBYTES]) -> Vec<u8> {
    let mut p = Packet::unpack(&phone.direct_announce(TEST_TIME_MS)).unwrap();
    p.flags.header_type = HeaderType::Type2;
    p.flags.transport_type = TransportType::Transport;
    p.hops = 1;
    p.transport_id = Some(x_id);
    pack(&p)
}

/// The announces for `dest` in this log, by the link they reached.
fn announces_by_link(log: &LinkLog, dest: &[u8; 16]) -> Vec<([u8; TRUNCATED_HASHBYTES], Vec<u8>)> {
    log.borrow()
        .iter()
        .filter(|(_, data)| {
            Packet::unpack(data)
                .map(|p| p.flags.packet_type == PacketType::Announce && &p.destination_hash == dest)
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// #854, split horizon, at step 3 of the 848 chain. N holds D via X on its
/// BLE interface, which carries three links: X, the Pixel and a third
/// peer Q, plus a LoRa interface beside it. The Pixel asks for D. N's
/// answer goes to the Pixel and to Q, and not to X, the one neighbour that
/// could only learn from it a route through itself. It does not reach LoRa
/// either, because a path response goes to the requesting interface only.
/// N's transit rebroadcast of D, before that, kept off X's link the same
/// way. X, fed everything its link got, holds no entry for D, so the loop
/// is never offered.
#[test]
fn a_responder_keeps_the_answer_off_the_link_its_route_runs_through() {
    const LORA: usize = 0;
    const BLE: usize = 1;
    let mut phone = make_phone();
    let dest = *phone.dest_hash.as_bytes();

    let mut x = make_node();
    let xb = add_iface(&mut x, "ble", 0);
    let x_id = *x.identity().hash();
    let pixel_id = *Identity::generate(&mut OsRng).hash();
    let q_id = *Identity::generate(&mut OsRng).hash();

    let mut n = make_node();
    n.set_interface_name(LORA, String::from("lora"));
    n.set_interface_name(BLE, String::from("ble"));
    n.set_interface_peer_count(BLE, 3);
    n.set_interface_link_sparing(BLE, true);
    let mut lora = MockInterface::new("lora", LORA as u8);
    let log: LinkLog = Rc::new(RefCell::new(Vec::new()));
    let mut ble = PeerLinks {
        id: InterfaceId(BLE),
        links: std::vec![x_id, pixel_id, q_id],
        log: Rc::clone(&log),
    };
    let dispatch = |outs: Vec<TickOutput>, lora: &mut MockInterface, ble: &mut PeerLinks| {
        let mut ifaces: [&mut dyn Interface; 2] = [lora, ble];
        for out in outs {
            let _ = dispatch_actions(&mut ifaces, out.actions, &BTreeMap::new());
        }
    };

    // N learns D from X's rebroadcast, on X's link.
    let first = n.handle_packet_from_peer(InterfaceId(BLE), x_id, &relayed_by(&mut phone, x_id));
    let route = n
        .get_path_clone(phone.dest_hash.as_bytes())
        .map(|e| (e.hops, e.interface_index, e.next_hop, e.via_peer));
    assert_eq!(
        route,
        Some((2, BLE, Some(x_id), Some(x_id))),
        "N holds D via X"
    );

    // N's own transit rebroadcast: every link but X's, and LoRa.
    let mut outs = std::vec![first];
    outs.extend(run_scheduler(&mut n, 1_000, 10));
    dispatch(outs, &mut lora, &mut ble);
    let links: Vec<_> = announces_by_link(&log, &dest)
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    assert!(!links.is_empty(), "N rebroadcasts D");
    assert!(
        links.iter().all(|l| *l != x_id) && links.contains(&pixel_id) && links.contains(&q_id),
        "the transit rebroadcast reaches the Pixel and Q, never X"
    );
    assert!(
        lora.sent.iter().any(|data| Packet::unpack(data)
            .map(|p| p.flags.packet_type == PacketType::Announce && p.destination_hash == dest)
            .unwrap_or(false)),
        "LoRa is a shared medium and keeps the rebroadcast"
    );
    log.borrow_mut().clear();
    lora.sent.clear();

    // The Pixel asks, on its own link.
    let mut outs = std::vec![n.handle_packet_from_peer(
        InterfaceId(BLE),
        pixel_id,
        &path_request(&phone.dest_hash)
    )];
    outs.extend(run_scheduler(&mut n, PATH_REQUEST_GRACE_MS + 1_000, 3));
    dispatch(outs, &mut lora, &mut ble);
    let answered = announces_by_link(&log, &dest);
    let mut links: Vec<_> = answered.iter().map(|(l, _)| *l).collect();
    links.sort();
    links.dedup();
    let mut want = std::vec![pixel_id, q_id];
    want.sort();
    assert_eq!(links, want, "N answers the Pixel and Q, and not X");
    assert!(
        answered
            .iter()
            .all(|(_, data)| Packet::unpack(data).unwrap().context == PacketContext::PathResponse),
        "what N sent is its path response"
    );
    assert!(
        lora.sent.is_empty(),
        "a path response stays on the requesting interface"
    );

    // X, fed what its link got, learns no route through N.
    let to_x: Vec<Vec<u8>> = log
        .borrow()
        .iter()
        .filter(|(l, _)| *l == x_id)
        .map(|(_, d)| d.clone())
        .collect();
    let _ = feed(&mut x, xb, &to_x);
    assert!(!x.has_path(&phone.dest_hash), "X gains no entry for D");
}

/// The contrast: the same N on one shared interface, LoRa, with no peer
/// links. Everyone on the air hears what the requester hears, so there is
/// no link to spare. The rule does not apply, the answer floods as before,
/// and X takes D at three hops via N (#852 is what closes that loop on a
/// shared medium).
#[test]
fn on_a_shared_medium_the_answer_still_reaches_the_next_hop() {
    let mut phone = make_phone();
    let dest = *phone.dest_hash.as_bytes();

    let mut x = make_node();
    let xl = add_iface(&mut x, "lora", 0);
    let x_id = *x.identity().hash();

    let mut n = make_node();
    let nl = add_iface(&mut n, "lora", 0);
    let n_id = *n.identity().hash();

    let _ = n.handle_packet(InterfaceId(nl), &relayed_by(&mut phone, x_id));
    assert_eq!(
        n.get_path_clone(phone.dest_hash.as_bytes())
            .map(|e| (e.hops, e.next_hop, e.via_peer)),
        Some((2, Some(x_id), None)),
        "N holds D via X, with no peer link to name"
    );
    let _ = run_scheduler(&mut n, 1_000, 10);

    let mut outs = feed(&mut n, nl, &[path_request(&phone.dest_hash)]);
    outs.extend(run_scheduler(&mut n, PATH_REQUEST_GRACE_MS + 1_000, 3));
    let answer = of_kind(&outs, PacketType::Announce, &dest);
    assert!(!answer.is_empty(), "N answers on the shared medium");
    assert!(
        outs.iter()
            .flat_map(|o| o.actions.iter())
            .all(|a| matches!(a, Action::SendPacket { peer: None, .. })),
        "nothing is spared on a shared medium"
    );

    let _ = feed(&mut x, xl, &answer);
    assert_eq!(
        x.get_path_clone(phone.dest_hash.as_bytes())
            .map(|e| (e.hops, e.next_hop)),
        Some((3, Some(n_id))),
        "X hears the answer and takes D via N"
    );
}

/// lnsd's shape: per-peer links that the interface cannot address as a
/// subset (its BLE peripheral notifies every subscribed central at once).
/// An excluding send would reach nobody there, the requester included, so
/// without the driver's declaration the answer floods the interface as
/// before.
#[test]
fn an_interface_that_cannot_spare_a_link_still_answers_on_all_of_them() {
    let mut phone = make_phone();
    let dest = *phone.dest_hash.as_bytes();
    let x_id = *Identity::generate(&mut OsRng).hash();
    let pixel_id = *Identity::generate(&mut OsRng).hash();

    let mut n = make_node();
    let nb = add_iface(&mut n, "ble", 0);
    n.set_interface_peer_count(nb, 2);
    let _ = n.handle_packet_from_peer(InterfaceId(nb), x_id, &relayed_by(&mut phone, x_id));
    let _ = run_scheduler(&mut n, 1_000, 10);

    let mut outs = std::vec![n.handle_packet_from_peer(
        InterfaceId(nb),
        pixel_id,
        &path_request(&phone.dest_hash)
    )];
    outs.extend(run_scheduler(&mut n, PATH_REQUEST_GRACE_MS + 1_000, 3));
    assert!(
        !of_kind(&outs, PacketType::Announce, &dest).is_empty(),
        "N answers"
    );
    assert!(
        outs.iter().flat_map(|o| o.actions.iter()).all(|a| matches!(
            a,
            Action::SendPacket {
                iface,
                peer: None,
                ..
            } if iface.0 == nb
        )),
        "the answer goes to the whole interface, X's link included"
    );
}
