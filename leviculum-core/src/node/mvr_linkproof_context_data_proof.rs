//! mvr: a 96 B packet proof on an established link that carries the
//! LINKPROOF context byte (0xFD) instead of NONE.
//!
//! Ratspeak (rsReticulum) proves every link packet that way (the link
//! manager of its rns-runtime crate, line 1765, an external tree), where
//! Python sends data proofs with context NONE (`Link.py:392`). The
//! reference matches every inbound proof against its receipts whatever the
//! context, except LRPROOF and RESOURCE_PRF (`Transport.py:2171`, `:2238`,
//! `:2244-2281`). Before the fix `handle_link_proof` counted a 96 B proof
//! as a data proof only with context NONE; a LINKPROOF one went down the
//! link request proof path and was refused there because the link is not
//! PendingOutgoing, so the receipt never turned Delivered and every
//! Ratspeak delivery stayed Unproven (#232).
//!
//! Three cases: the LINKPROOF proof confirms delivery; the NONE proof still
//! does; an LRPROOF context proof on an established link is still refused,
//! as the reference only offers those to `pending_links`
//! (`Transport.py:2213-2236`).
//!
//! Sans-I/O: direct initiator <-> responder over one mesh hop. The proof is
//! the responder's own, re-packed with the context byte swapped; the
//! signature covers the proved packet's hash, not the proof packet, so the
//! swap leaves it valid.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::{Packet, PacketContext, PacketType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::NoStorage;
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

fn add_iface(node: &mut EndpointNode, name: &'static str) -> usize {
    let idx = node
        .transport
        .register_interface(std::boxed::Box::new(MockInterface::new(name, 0)));
    node.set_interface_name(idx, String::from(name));
    idx
}

fn action_data(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|a| match a {
            Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
        })
        .collect()
}

fn deliver_all(target: &mut EndpointNode, iface: usize, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for pkt in packets {
        out.extend(action_data(&target.handle_packet(InterfaceId(iface), &pkt)));
    }
    out
}

/// Drive an initiator <-> responder link to Active on both sides; the
/// responder proves every packet. Returns `(initiator, responder, i_iface,
/// r_iface, link_id)`.
fn establish() -> (EndpointNode, EndpointNode, usize, usize, LinkId) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut responder =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["linkproofctx"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    responder.register_destination(dest);

    let mut initiator =
        NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let r_iface = add_iface(&mut responder, "R_mesh");
    let i_iface = add_iface(&mut initiator, "I_mesh");

    let (link_id, _routed, out) = initiator.connect(dest_hash, &signing_key).expect("connect");
    let mut for_responder = action_data(&out);
    for _ in 0..8 {
        if for_responder.is_empty() {
            break;
        }
        let back = deliver_all(&mut responder, r_iface, for_responder);
        for_responder = deliver_all(&mut initiator, i_iface, back);
    }
    assert_eq!(
        initiator.active_link_count(),
        1,
        "precondition: initiator link active"
    );
    assert_eq!(
        responder.active_link_count(),
        1,
        "precondition: responder link active"
    );
    (initiator, responder, i_iface, r_iface, link_id)
}

/// Send one link packet, let the responder prove it, swap the proof's
/// context byte to `context`, deliver it to the initiator and return the
/// initiator's events.
fn prove_with_context(context: PacketContext) -> Vec<NodeEvent> {
    let (mut initiator, mut responder, i_iface, r_iface, link_id) = establish();

    let out = initiator
        .send_on_link(&link_id, b"lxmf-ish")
        .expect("send_on_link on an active link");
    let replies = deliver_all(&mut responder, r_iface, action_data(&out));

    let mut proofs: Vec<Packet> = replies
        .iter()
        .filter_map(|raw| Packet::unpack(raw).ok())
        .filter(|p| p.flags.packet_type == PacketType::Proof)
        .collect();
    assert_eq!(proofs.len(), 1, "responder must prove the link packet once");
    let mut proof = proofs.remove(0);
    assert_eq!(
        proof.context,
        PacketContext::None,
        "our own data proofs carry NONE"
    );
    assert_eq!(
        proof.data.len(),
        96,
        "a link packet proof is hash + signature"
    );
    proof.context = context;

    let mut raw = std::vec![0u8; proof.packed_size()];
    let len = proof.pack(&mut raw).expect("re-pack proof");
    raw.truncate(len);
    initiator.handle_packet(InterfaceId(i_iface), &raw).events
}

fn delivered(events: &[NodeEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, NodeEvent::LinkDeliveryConfirmed { .. }))
}

/// The Ratspeak shape: a 96 B proof with context LINKPROOF confirms the
/// receipt like a NONE proof does.
#[test]
fn linkproof_context_data_proof_confirms_delivery() {
    let events = prove_with_context(PacketContext::LinkProof);
    assert!(
        delivered(&events),
        "a 96 B LINKPROOF-context proof on an established link must confirm \
         delivery (Transport.py:2244-2281 matches it against the receipts)\n\
         events: {events:?}"
    );
}

/// No regression: the NONE proof Python and we send still confirms.
#[test]
fn none_context_data_proof_confirms_delivery() {
    let events = prove_with_context(PacketContext::None);
    assert!(delivered(&events), "events: {events:?}");
}

/// An LRPROOF-context proof is a link request proof whatever its length;
/// on an established link it is refused and confirms nothing.
#[test]
fn lrproof_context_proof_on_established_link_is_refused() {
    let events = prove_with_context(PacketContext::Lrproof);
    assert!(
        !delivered(&events),
        "an LRPROOF-context proof must never confirm a data receipt\nevents: {events:?}"
    );
}
