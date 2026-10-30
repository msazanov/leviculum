//! mvr (931, Codeberg #468): an interface removed for good takes the node's
//! own links on it with it.
//!
//! ## The gap
//!
//! `handle_interface_down` removes everything the transport keyed by the
//! interface, but not the node's own `links`: a link keeps its
//! `attached_interface`, and every later send on it is addressed to the dead
//! index and dropped as `no-such-interface` until the link's stale timeout
//! (919, `mvr_interface_gone_dispatch.rs`). That is right when the interface
//! can come back under the same index (lnsd re-registers a re-dialled TCP
//! client and a re-attached bootstrap interface under their old index), and
//! pointless when it cannot: the application keeps a link nothing can carry.
//!
//! ## What this pins
//!
//! A `Teardown::Final` teardown closes every own link on the interface at
//! once: the link leaves the table, the application reads `LinkClosed` with
//! `InterfaceRemoved`, no close frame is addressed to the gone index, a
//! later send finds no link, and nothing the node does afterwards reaches
//! the dispatch addressed there. A `Teardown::Transient` teardown keeps the
//! link, as 919 pins.
//!
//! Host model: two endpoints, deterministic, sub-second, no driver.

extern crate std;

use std::string::String;

use super::mvr_interface_gone_dispatch::{dispatch_without_gone, linked_over_gone, Pair, GONE};
use crate::link::LinkCloseReason;
use crate::node::{NodeEvent, SendError, Teardown};
use crate::test_log_capture::with_captured_logs;
use crate::transport::{Action, InterfaceId, TickOutput};

/// The `IFACE_DOWN` line of a captured teardown.
fn iface_down_line(logs: &str) -> String {
    logs.lines()
        .find(|l| l.contains("IFACE_DOWN"))
        .unwrap_or_else(|| panic!("no IFACE_DOWN line\n--- logs ---\n{logs}"))
        .into()
}

fn addressed_to_gone(output: &TickOutput) -> bool {
    output
        .actions
        .iter()
        .any(|a| matches!(a, Action::SendPacket { iface, .. } if iface.0 == GONE))
}

#[test]
fn a_final_teardown_closes_the_own_link_on_the_interface() {
    let Pair {
        mut initiator,
        link_id,
        dest_hash,
    } = linked_over_gone();

    let (out, logs) = with_captured_logs(|| {
        initiator.handle_interface_teardown(InterfaceId(GONE), Teardown::Final)
    });

    let closed: std::vec::Vec<_> = out
        .events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::LinkClosed {
                link_id: id,
                reason,
                is_initiator,
                destination_hash,
            } => Some((*id, *reason, *is_initiator, *destination_hash)),
            _ => None,
        })
        .collect();
    assert_eq!(
        closed,
        std::vec![(link_id, LinkCloseReason::InterfaceRemoved, true, dest_hash)],
        "the application learns the link went with its interface"
    );
    assert!(
        initiator.link(&link_id).is_none(),
        "the link left the table"
    );
    assert!(
        !addressed_to_gone(&out),
        "no close frame for a medium that is gone: {:?}",
        out.actions
    );
    let line = iface_down_line(&logs);
    assert!(line.contains("teardown=\"final\""), "{line}");
    assert!(line.contains("links_closed=1"), "{line}");

    assert!(
        matches!(
            initiator.send_on_link(&link_id, b"after the interface went"),
            Err(SendError::NoLink)
        ),
        "a later send finds no link"
    );

    // Past every keepalive and stale deadline: nothing the node still holds
    // is addressed to the gone index.
    initiator.transport().clock().advance(30 * 60 * 1000);
    let later = initiator.handle_timeout();
    assert!(
        !addressed_to_gone(&later),
        "nothing addressed to {GONE} after the teardown: {:?}",
        later.actions
    );
    let (drops, logs) = dispatch_without_gone(later);
    assert!(
        drops.is_empty(),
        "no no-such-interface drop: {drops:?}\n--- logs ---\n{logs}"
    );
    let (drops, _) = dispatch_without_gone(out);
    assert!(
        drops.is_empty(),
        "the teardown itself drops nothing: {drops:?}"
    );
}

#[test]
fn a_transient_teardown_keeps_the_own_link_and_says_so() {
    let Pair {
        mut initiator,
        link_id,
        ..
    } = linked_over_gone();

    let (out, logs) = with_captured_logs(|| {
        initiator.handle_interface_teardown(InterfaceId(GONE), Teardown::Transient)
    });

    assert!(
        !out.events
            .iter()
            .any(|e| matches!(e, NodeEvent::LinkClosed { .. })),
        "a transient teardown closes nothing"
    );
    assert_eq!(
        initiator
            .link(&link_id)
            .and_then(|l| l.attached_interface()),
        Some(GONE),
        "the link keeps its attachment for an interface that may return"
    );
    let line = iface_down_line(&logs);
    assert!(line.contains("teardown=\"transient\""), "{line}");
    assert!(line.contains("links_closed=0"), "{line}");
}
