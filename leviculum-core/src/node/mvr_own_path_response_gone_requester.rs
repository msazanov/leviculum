//! mvr (924, 923 follow-up): the answer to a path request for one of the
//! node's OWN destinations is held for the path-request grace, addressed to
//! the requesting interface, and outlives that interface when it goes.
//!
//! ## The field record
//!
//! The one `no-such-interface` drop of the miauhaus soak:
//!
//! ```text
//! PKT_TX node=miauhaus hops=0 iface=iface:6 len=167 ph=6e4e2e75e99d70f7 t=32674764
//! PKT_DROP node=miauhaus hops=0 iface_out=6 len=167 ph=6e4e2e75e99d70f7 reason=no-such-interface t=32674764
//! ```
//!
//! 923 showed that a relayed PATH_RESPONSE is HEADER_2, hops >= 1, at least
//! 183 bytes; this frame is hops 0 and 167 bytes, HEADER_1 (19) plus a
//! 148-byte announce body, a packet the node built itself. It went to
//! interface 6 alone (one `PKT_TX iface=iface:6`, no `ANN_TX iface=all`),
//! which is the shape of the answer to a path request for a local
//! destination: `handle_path_request` hands the request to NodeCore
//! (`TransportEvent::PathRequestReceived`), which generates a fresh announce
//! and stores it as an `AnnounceEntry` with `target_interface:
//! Some(requesting_interface)` and `retransmit_at_ms = now +
//! PATH_REQUEST_GRACE_MS` (400 ms). A local destination opens no discovery
//! window, so 923's cull does not reach it.
//!
//! ## What this pins
//!
//! The hypothesis held (924 step 1, red before the fix): the request arrived
//! on interface 6, interface 6 went inside the grace, and when the grace ran
//! out the announce table sent the answer to the gone index, which the
//! driver dropped as `no-such-interface`
//! (`[(InterfaceId(6), NoSuchInterface)]`, an Announce, HEADER_1, hops 0,
//! 167 bytes). Now `handle_interface_down` removes an announce entry
//! targeted at the gone interface beside 923's cull, and counts it in the
//! same debug line.
//!
//! Host model: one node, deterministic, sub-second, no driver.

extern crate std;

use std::string::String;
use std::vec;
use std::vec::Vec;

use alloc::collections::BTreeMap;
use rand_core::OsRng;

use crate::constants::PATH_REQUEST_GRACE_MS;
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, PacketType};
use crate::test_log_capture::with_captured_logs;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::Storage;
use crate::transport::{dispatch_actions, Action, DropReason, InterfaceId, TickOutput};
use crate::DestinationHash;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The requester's interface, the one that goes away.
const GONE: usize = 6;
/// The interface the driver still has.
const KEPT: usize = 0;

fn node(transport: bool) -> Node {
    NodeCoreBuilder::new().enable_transport(transport).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    )
}

/// A transport node as miauhaus runs it, with one local destination
/// (no ratchet, no app data).
fn host() -> (Node, DestinationHash) {
    let mut host = node(true);
    host.set_interface_name(KEPT, String::from("kept"));
    host.set_interface_name(GONE, String::from("gone"));
    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr924",
        &["own"],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    host.register_destination(dest);
    (host, dest_hash)
}

/// Every frame an output carries, whichever way it is addressed.
fn frames(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .collect()
}

/// A requester on `GONE` asks the host for its own destination.
fn ask_on_gone(host: &mut Node, dest_hash: &DestinationHash) -> TickOutput {
    let mut requester = node(false);
    let all = frames(&requester.request_path(dest_hash));
    assert_eq!(all.len(), 1, "fixture: one path request frame");
    host.handle_packet(InterfaceId(GONE), &all[0])
}

/// `GONE` goes; returns the teardown's debug line.
fn teardown_gone(host: &mut Node) -> String {
    let (_, logs) = with_captured_logs(|| host.handle_interface_down(InterfaceId(GONE)));
    logs.lines()
        .find(|l| l.contains("went down"))
        .unwrap_or_else(|| panic!("no teardown line\n--- logs ---\n{logs}"))
        .into()
}

/// The grace runs out; returns the tick's output.
fn grace_expires(host: &mut Node) -> TickOutput {
    host.transport()
        .clock()
        .advance(PATH_REQUEST_GRACE_MS + 100);
    host.handle_timeout()
}

/// The answer addressed to `iface`, if any.
fn answer_to(output: &TickOutput, iface: usize) -> Option<Packet> {
    output.actions.iter().find_map(|a| match a {
        Action::SendPacket { iface: i, data, .. } if i.0 == iface => Packet::unpack(data).ok(),
        _ => None,
    })
}

/// Dispatch `output` into a driver slice that has only `KEPT`, as lnsd's
/// registry does once `GONE` was removed.
fn dispatch_without_gone(output: TickOutput) -> (Vec<(InterfaceId, DropReason)>, String) {
    let mut kept = MockInterface::new("kept", KEPT as u8);
    with_captured_logs(|| {
        let mut slice: Vec<&mut dyn crate::traits::Interface> = vec![&mut kept];
        dispatch_actions(&mut slice, output.actions, &BTreeMap::new()).drops
    })
}

/// THE pin (924 step 1 was this test, red): the own-destination answer
/// held for the grace goes with the interface it was owed to.
#[test]
fn an_own_path_response_whose_requester_went_is_culled_with_it() {
    let (mut host, dest_hash) = host();
    let immediate = ask_on_gone(&mut host, &dest_hash);
    assert!(
        answer_to(&immediate, GONE).is_none(),
        "fixture: the answer is held for the grace, not sent with the request"
    );
    assert!(
        host.transport()
            .storage()
            .get_announce(dest_hash.as_bytes())
            .is_some_and(|e| e.target_interface == Some(GONE)),
        "fixture: an announce entry targeted at {GONE} holds the answer"
    );

    let line = teardown_gone(&mut host);
    let out = grace_expires(&mut host);
    let addressed_to_gone = answer_to(&out, GONE);
    let (drops, logs) = dispatch_without_gone(out);

    // Before the fix: `[(InterfaceId(6), NoSuchInterface)]`, the answer an
    // Announce, HEADER_1, hops 0, 167 bytes: the soak frame.
    assert!(
        drops.is_empty(),
        "no dead send: {drops:?} answer={:?}\n--- logs ---\n{logs}",
        addressed_to_gone.map(|p| (
            p.flags.packet_type,
            p.flags.header_type,
            p.hops,
            p.packed_size()
        ))
    );
    assert!(
        host.transport()
            .storage()
            .get_announce(dest_hash.as_bytes())
            .is_none(),
        "the held answer went with {GONE}"
    );
    assert!(
        line.contains("1 targeted announces"),
        "the teardown counts the culled answer: {line}"
    );
}

/// Positive control, and the frame step 1 named: with the requester's
/// interface present, the grace sends the answer there, and it is the soak
/// frame's shape.
#[test]
fn control_a_present_requester_gets_the_own_path_response() {
    let (mut host, dest_hash) = host();
    let _ = ask_on_gone(&mut host, &dest_hash);
    let out = grace_expires(&mut host);
    let response = answer_to(&out, GONE)
        .unwrap_or_else(|| panic!("no answer addressed to {GONE}: {:?}", out.actions));
    assert_eq!(response.flags.packet_type, PacketType::Announce);
    assert_eq!(response.flags.header_type, HeaderType::Type1);
    assert_eq!(response.hops, 0, "the node's own announce");
    assert_eq!(
        response.packed_size(),
        167,
        "19-byte HEADER_1 + 148-byte announce"
    );
}
