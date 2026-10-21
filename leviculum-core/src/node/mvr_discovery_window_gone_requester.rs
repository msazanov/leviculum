//! mvr (923, 919 follow-up): a discovery path request window keeps its
//! requesting interface after that interface went, and its answer is
//! addressed to the dead index.
//!
//! ## The field record
//!
//! Around the one `no-such-interface` drop of the miauhaus soak
//! (`events.log` line 3962530, verbatim):
//!
//! ```text
//! PR_RETRY_WITHHELD node=miauhaus dst=d8a7a1e5900559bf2db50e29ccf8324d iface_req=iface:6 t=32673251
//! PKT_TX node=miauhaus hops=0 iface=iface:6 len=167 ph=6e4e2e75e99d70f7 t=32674764
//! PKT_DROP node=miauhaus hops=0 iface_out=6 len=167 ph=6e4e2e75e99d70f7 reason=no-such-interface t=32674764
//! ```
//!
//! Six `PR_RETRY_WITHHELD ... iface_req=iface:6` lines, all after
//! interface 6 lost its name (`iface:6` is how `iface_name` renders an
//! index the registry does not know). `handle_interface_down` removes the
//! interface's mode with its name, so `interface_mode(6)` reads `Full`,
//! which does not discover paths, and every retry tick of a window opened
//! from 6 is withheld.
//!
//! ## What this pins
//!
//! The hypothesis held (923 step 1, red before the fix): a path request for
//! an unknown destination opened a window on a discovering interface; the
//! interface went; the window survived it, its retries were withheld under
//! the gone index's default mode, and the destination's announce on another
//! interface was answered with a PATH_RESPONSE addressed to the gone index,
//! which the driver dropped as `no-such-interface`. That answer is an
//! announce, context PATH_RESPONSE, HEADER_2, hops 1 for a neighbour's
//! announce, 183 bytes for one without ratchet or app data
//! ([`control_a_present_requester_gets_the_path_response`] names it). The
//! soak frame was hops=0 and 167 bytes, which no relayed PATH_RESPONSE can
//! be (HEADER_2 is 35 bytes, a minimal announce body 148), so the window is
//! the cause of the six withheld lines and of a dead send of its own, not of
//! that particular drop.
//!
//! Now `handle_interface_down` culls what the gone interface asked for:
//!
//! - a window it opened goes, with nobody left to answer it;
//! - a window it opened that others were batched onto (#433) is handed to
//!   the lowest of them, whose answer still lands;
//! - a batched requester on it leaves the window someone else opened.
//!
//! Host model: a relay and two endpoints, deterministic, sub-second, no
//! driver.

extern crate std;

use std::string::String;
use std::vec;
use std::vec::Vec;

use alloc::collections::BTreeMap;
use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, PacketContext, PacketType};
use crate::test_log_capture::with_captured_logs;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{InterfaceMode, Storage};
use crate::transport::{dispatch_actions, Action, DropReason, InterfaceId, TickOutput};
use crate::DestinationHash;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The requester's interface, the one that goes away.
const GONE: usize = 6;
/// The interface the answer arrives on and the driver still has.
const KEPT: usize = 0;
/// A second requester's interface, also kept.
const OTHER: usize = 2;

fn node(transport: bool) -> Node {
    NodeCoreBuilder::new().enable_transport(transport).build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    )
}

/// A relay as miauhaus runs it: transport on, the requester's interface
/// in a `DISCOVER_PATHS_FOR` mode so an unknown destination opens a
/// window there.
fn relay() -> Node {
    let mut relay = node(true);
    relay.set_interface_name(KEPT, String::from("kept"));
    relay.set_interface_name(GONE, String::from("gone"));
    relay.set_interface_name(OTHER, String::from("other"));
    relay.set_interface_mode(GONE, InterfaceMode::Gateway);
    relay.set_interface_mode(OTHER, InterfaceMode::Gateway);
    relay
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

/// The destination nobody near the relay has heard of yet, and the node
/// that will announce it.
fn unknown_destination() -> (Node, DestinationHash) {
    let mut responder = node(false);
    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr923",
        &["window"],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    responder.register_destination(dest);
    (responder, dest_hash)
}

/// A path request for `dest_hash` as an endpoint puts it on the wire.
fn path_request(dest_hash: &DestinationHash) -> Vec<u8> {
    let mut requester = node(false);
    let out = requester.request_path(dest_hash);
    let all = frames(&out);
    assert_eq!(all.len(), 1, "fixture: one path request frame");
    all[0].clone()
}

/// A requester on `iface` asks the relay for `dest_hash`.
fn ask_on(relay: &mut Node, iface: usize, dest_hash: &DestinationHash) {
    let _ = relay.handle_packet(InterfaceId(iface), &path_request(dest_hash));
}

/// The interface the relay's window for `dest_hash` answers first.
fn window_owner(relay: &Node, dest_hash: &DestinationHash) -> Option<usize> {
    relay
        .transport()
        .storage()
        .get_discovery_path_request(dest_hash.as_bytes())
        .map(|(iface, _)| iface)
}

/// `GONE` goes; returns the teardown's debug line.
fn teardown_gone(relay: &mut Node) -> String {
    let (_, logs) = with_captured_logs(|| relay.handle_interface_down(InterfaceId(GONE)));
    logs.lines()
        .find(|l| l.contains("went down"))
        .unwrap_or_else(|| panic!("no teardown line\n--- logs ---\n{logs}"))
        .into()
}

/// The responder's announce lands on `KEPT`; returns the relay's output.
fn answer_arrives(
    relay: &mut Node,
    responder: &mut Node,
    dest_hash: &DestinationHash,
) -> TickOutput {
    let announce = frames(&responder.announce_destination(dest_hash, None).unwrap());
    relay.handle_packet(InterfaceId(KEPT), &announce[0])
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

/// The answer the window owes, as the relay emits it for `iface`.
fn answer_to(output: &TickOutput, iface: usize) -> Option<Packet> {
    output.actions.iter().find_map(|a| match a {
        Action::SendPacket { iface: i, data, .. } if i.0 == iface => Packet::unpack(data).ok(),
        _ => None,
    })
}

/// THE pin (923 step 1 was this test, red): a window the gone interface
/// opened goes with it, so no retry is withheld under the dead index and the
/// answer is addressed nowhere the driver does not have.
#[test]
fn a_window_whose_requester_went_is_culled_with_it() {
    let (mut responder, dest_hash) = unknown_destination();
    let mut relay = relay();
    ask_on(&mut relay, GONE, &dest_hash);
    assert_eq!(
        window_owner(&relay, &dest_hash),
        Some(GONE),
        "fixture: the request on {GONE} opened a discovery window there"
    );

    let line = teardown_gone(&mut relay);
    relay.transport().clock().advance(5_000);
    let (_, tick_logs) = with_captured_logs(|| relay.handle_timeout());
    let out = answer_arrives(&mut relay, &mut responder, &dest_hash);
    let addressed_to_gone = answer_to(&out, GONE).is_some();
    let (drops, logs) = dispatch_without_gone(out);

    // Before the fix: `[(InterfaceId(6), NoSuchInterface)]`, the answer
    // addressed to the index the driver no longer has.
    assert!(
        drops.is_empty(),
        "no dead send: {drops:?}\n--- logs ---\n{logs}"
    );
    assert!(!addressed_to_gone, "nothing is addressed to {GONE}");
    assert!(
        !tick_logs.contains("PR_RETRY_WITHHELD"),
        "no retry is withheld under the gone index\n--- logs ---\n{tick_logs}"
    );
    assert_eq!(relay.transport().stats().path_request_retry_withholds(), 0);
    assert!(
        line.contains("1 discovery windows (0 handed on, 0 batched requesters)"),
        "the teardown counts the culled window: {line}"
    );
}

/// Positive control, and the frame step 1 named: with the requester's
/// interface present the window answers it with a PATH_RESPONSE.
#[test]
fn control_a_present_requester_gets_the_path_response() {
    let (mut responder, dest_hash) = unknown_destination();
    let mut relay = relay();
    ask_on(&mut relay, GONE, &dest_hash);
    let out = answer_arrives(&mut relay, &mut responder, &dest_hash);
    let response = answer_to(&out, GONE)
        .unwrap_or_else(|| panic!("no answer addressed to {GONE}: {:?}", out.actions));
    assert_eq!(response.flags.packet_type, PacketType::Announce);
    assert_eq!(response.flags.header_type, HeaderType::Type2);
    assert_eq!(response.context, PacketContext::PathResponse);
    assert_eq!(
        response.hops, 1,
        "a neighbour's announce, counted on receipt"
    );
    assert_eq!(
        response.packed_size(),
        183,
        "35-byte HEADER_2 + 148-byte announce"
    );
}

/// A window the gone interface opened, with a requester batched onto it
/// from a kept interface (#433), is handed to that requester: its answer
/// still lands, and only there.
#[test]
fn a_batched_requester_takes_over_a_window_whose_opener_went() {
    let (mut responder, dest_hash) = unknown_destination();
    let mut relay = relay();
    ask_on(&mut relay, GONE, &dest_hash);
    ask_on(&mut relay, OTHER, &dest_hash);
    assert_eq!(
        window_owner(&relay, &dest_hash),
        Some(GONE),
        "fixture: opened on {GONE}"
    );

    let line = teardown_gone(&mut relay);
    let owner = window_owner(&relay, &dest_hash);

    let out = answer_arrives(&mut relay, &mut responder, &dest_hash);
    assert!(
        answer_to(&out, OTHER).is_some(),
        "{OTHER} is still answered"
    );
    assert!(
        answer_to(&out, GONE).is_none(),
        "{GONE} is not: {:?}",
        out.actions
    );
    assert_eq!(owner, Some(OTHER), "the window's owner after the teardown");
    assert!(
        line.contains("0 discovery windows (1 handed on, 0 batched requesters)"),
        "the teardown counts the handed-on window: {line}"
    );
}

/// A requester on the gone interface batched onto a window a kept interface
/// opened leaves it; the opener is answered as before.
#[test]
fn a_batched_requester_on_the_gone_interface_leaves_the_window() {
    let (mut responder, dest_hash) = unknown_destination();
    let mut relay = relay();
    ask_on(&mut relay, OTHER, &dest_hash);
    ask_on(&mut relay, GONE, &dest_hash);

    let line = teardown_gone(&mut relay);
    let owner = window_owner(&relay, &dest_hash);

    let out = answer_arrives(&mut relay, &mut responder, &dest_hash);
    assert!(answer_to(&out, OTHER).is_some(), "the opener is answered");
    assert!(
        answer_to(&out, GONE).is_none(),
        "{GONE} is not: {:?}",
        out.actions
    );
    let (drops, _) = dispatch_without_gone(out);
    assert!(
        !drops.contains(&(InterfaceId(GONE), DropReason::NoSuchInterface)),
        "no dead send: {drops:?}"
    );
    assert_eq!(owner, Some(OTHER), "the window's owner after the teardown");
    assert!(
        line.contains("0 discovery windows (0 handed on, 1 batched requesters)"),
        "the teardown counts the batched requester: {line}"
    );
}
