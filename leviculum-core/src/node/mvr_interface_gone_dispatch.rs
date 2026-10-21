//! mvr (919): a frame addressed to an interface the driver no longer has is
//! dropped as `no-such-interface`, and the path table is not why.
//!
//! ## The field record
//!
//! The miauhaus soak (public Reticulum, lnsd 61c5844, 101 h, 45 M events)
//! carried exactly one line of its kind:
//!
//! ```text
//! PKT_DROP node=miauhaus hops=0 iface_out=6 len=167 ph=6e4e2e75e99d70f7 reason=no-such-interface t=32674764
//! ```
//!
//! `hops=0` is a frame this node originated. miauhaus loses interfaces at
//! runtime (discovered peers torn down by autoconnect, TCP sessions closing),
//! and each loss runs `handle_interface_down` and then removes the handle from
//! the driver's registry, the slice `dispatch_actions` routes into.
//!
//! ## What this pins
//!
//! - **The path table is clean.** `handle_interface_down` removes every path
//!   entry on the interface (`remove_paths_for_interface`), so a routing
//!   decision taken AFTER the teardown cannot pick the dead index from the
//!   path table. Asserted below before and after the drop.
//! - **Two things can still address the dead index**, and both reach the
//!   same drop:
//!   1. a link this node holds keeps its `attached_interface` after the
//!      interface is gone; every later send on it (channel data, keepalive,
//!      close) is addressed there. Python keeps the attachment too
//!      (`Link.attached_interface`), and an interface that comes back under
//!      the same index carries the link on, which is why this is pinned and
//!      not changed here;
//!   2. a `TickOutput` committed from an API task BEFORE the loop processed
//!      the disconnect waits in the driver's dispatch channel and is routed
//!      after the registry dropped the handle (`dispatch_committed`, the
//!      event loop's branch 2 in `leviculum-std/src/driver/mod.rs`).
//! - **The drop line has the one `PKT_DROP` shape**: `dst`, `type`, `hops`
//!   read from the frame's own bytes, `iface_in=None` as for any originated
//!   packet, `iface_out` naming the index it wanted. The soak line had none
//!   of `dst`, `type`, `iface_in`, which is what its analyser flagged.
//!
//! Host model: two endpoints, deterministic, sub-second, no driver.

extern crate std;

use std::format;
use std::string::String;
use std::vec;
use std::vec::Vec;

use alloc::collections::BTreeMap;
use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::test_log_capture::with_captured_logs;
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{dispatch_actions, Action, DropReason, InterfaceId, TickOutput};
use crate::DestinationHash;

type Node = NodeCore<OsRng, MockClock, MemoryStorage>;

/// The index the link is formed on and that later goes away.
const GONE: usize = 6;
/// An interface the driver still has.
const KEPT: usize = 0;

fn node() -> Node {
    NodeCoreBuilder::new().build(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    )
}

/// The one frame an output carries, whichever way it is addressed.
fn frame(output: &TickOutput) -> Vec<u8> {
    let frames: Vec<&Vec<u8>> = output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data,
        })
        .collect();
    assert_eq!(
        frames.len(),
        1,
        "expected one frame, got {:?}",
        output.actions
    );
    frames[0].clone()
}

struct Pair {
    initiator: Node,
    link_id: LinkId,
    dest_hash: DestinationHash,
}

/// The initiator learns a path to the responder on `GONE` and forms a link
/// over it.
fn linked_over_gone() -> Pair {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut responder = node();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvr919",
        &["gone"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    let dest_hash = *dest.hash();
    responder.register_destination(dest);

    let mut initiator = node();
    let announce = responder.announce_destination(&dest_hash, None).unwrap();
    let _ = initiator.handle_packet(InterfaceId(GONE), &frame(&announce));
    assert!(
        initiator.has_path(&dest_hash),
        "fixture: the announce on {GONE} gives the initiator a path"
    );

    let (link_id, _, out) = initiator.connect(dest_hash, &signing_key).unwrap();
    let out = responder.handle_packet(InterfaceId(GONE), &frame(&out));
    let out = initiator.handle_packet(InterfaceId(GONE), &frame(&out));
    assert!(
        out.events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkEstablished { .. })),
        "fixture: the link is established"
    );
    let _ = responder.handle_packet(InterfaceId(GONE), &frame(&out));
    Pair {
        initiator,
        link_id,
        dest_hash,
    }
}

/// Dispatch `output` into a driver slice that has only `KEPT`, as lnsd's
/// registry does once `GONE` was removed, and return the drops and the
/// captured log.
fn dispatch_without_gone(output: TickOutput) -> (Vec<(InterfaceId, DropReason)>, String) {
    let mut kept = MockInterface::new("kept", KEPT as u8);
    let (drops, logs) = with_captured_logs(|| {
        let mut slice: Vec<&mut dyn crate::traits::Interface> = vec![&mut kept];
        dispatch_actions(&mut slice, output.actions, &BTreeMap::new()).drops
    });
    assert!(kept.sent.is_empty(), "the kept interface is no fallback");
    (drops, logs)
}

/// The value of `key` on the captured `PKT_DROP` line, quotes stripped (the
/// test capture renders a `&str` field quoted, the event log does not).
fn drop_field(logs: &str, key: &str) -> String {
    let line = logs
        .lines()
        .find(|l| l.contains("PKT_DROP") && l.contains("no-such-interface"))
        .unwrap_or_else(|| panic!("no no-such-interface PKT_DROP\n--- logs ---\n{logs}"));
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("PKT_DROP without {key}=: {line}"))
        .trim_matches('"')
        .into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Mechanism 1: the link outlives its interface, the path does not.
#[test]
fn a_link_send_after_its_interface_went_is_dropped_by_name_and_no_path_survives() {
    let Pair {
        mut initiator,
        link_id,
        dest_hash,
    } = linked_over_gone();

    let _ = initiator.handle_interface_down(InterfaceId(GONE));
    assert!(
        !initiator.has_path(&dest_hash) && initiator.path_count() == 0,
        "handle_interface_down removed the path entry on {GONE}; a later \
         routing decision cannot take the dead index from the path table"
    );
    assert_eq!(
        initiator
            .link(&link_id)
            .and_then(|l| l.attached_interface()),
        Some(GONE),
        "the link keeps its attachment to the interface that went"
    );

    let out = initiator
        .send_on_link(&link_id, b"after the interface went")
        .unwrap();
    assert!(
        out.actions
            .iter()
            .any(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == GONE)),
        "the send is addressed to the dead index: {:?}",
        out.actions
    );

    let (drops, logs) = dispatch_without_gone(out);
    assert_eq!(
        drops,
        vec![(InterfaceId(GONE), DropReason::NoSuchInterface)]
    );
    assert_eq!(drop_field(&logs, "dst"), hex(link_id.as_bytes()));
    assert_eq!(drop_field(&logs, "type"), "Data");
    assert_eq!(drop_field(&logs, "hops"), "0");
    assert_eq!(drop_field(&logs, "iface_in"), "None");
    assert_eq!(drop_field(&logs, "iface_out"), format!("{GONE}"));
    assert!(!drop_field(&logs, "ph").is_empty());
    assert!(
        !initiator.has_path(&dest_hash),
        "the drop recreated no path entry"
    );
}

/// Mechanism 2: an output committed before the teardown and dispatched after
/// it, the order lnsd's dispatch channel allows.
#[test]
fn an_output_committed_before_the_teardown_is_dropped_by_name() {
    let Pair {
        mut initiator,
        link_id,
        dest_hash,
    } = linked_over_gone();

    let committed = initiator
        .send_on_link(&link_id, b"committed first")
        .unwrap();
    assert!(initiator.has_path(&dest_hash), "fixture: still routed");
    let _ = initiator.handle_interface_down(InterfaceId(GONE));

    let (drops, logs) = dispatch_without_gone(committed);
    assert_eq!(
        drops,
        vec![(InterfaceId(GONE), DropReason::NoSuchInterface)]
    );
    assert_eq!(drop_field(&logs, "dst"), hex(link_id.as_bytes()));
    assert_eq!(drop_field(&logs, "iface_in"), "None");
    assert!(
        !initiator.has_path(&dest_hash) && initiator.path_count() == 0,
        "the path entry on {GONE} went with the interface"
    );
}

/// Control: the same send with the interface still in the slice goes out and
/// drops nothing, so the two drops above are about the slice and nothing
/// else.
#[test]
fn control_the_same_send_with_the_interface_present_goes_out() {
    let Pair {
        mut initiator,
        link_id,
        ..
    } = linked_over_gone();
    let out = initiator
        .send_on_link(&link_id, b"interface present")
        .unwrap();
    let mut gone = MockInterface::new("gone", GONE as u8);
    let result = {
        let mut slice: Vec<&mut dyn crate::traits::Interface> = vec![&mut gone];
        dispatch_actions(&mut slice, out.actions, &BTreeMap::new())
    };
    assert!(result.is_clean(), "nothing lost");
    assert_eq!(gone.sent.len(), 1, "the frame went out on {GONE}");
}
