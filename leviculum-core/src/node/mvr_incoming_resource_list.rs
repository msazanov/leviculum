//! mvr (Codeberg #657, audit D12): a second incoming resource on a link is
//! taken or refused, never ignored.
//!
//! The reference keeps a list of incoming resources per link
//! (`reference/Reticulum/RNS/Link.py:1308-1309`) and offers every part to
//! each of them (`reference/Reticulum/RNS/Link.py:1163-1166`). A link of
//! ours held one, and an advertisement for a different resource while it
//! ran was dropped without a word: its sender re-advertised into silence
//! until its advertisement budget ran out. On a propagation link a peer's
//! sync and a client's message meet on one link, which is exactly two.
//!
//! One sender cannot run two outgoing resources on one link (ours refuses
//! with `TransferInProgress`, a stock Python sender queues the second
//! behind `ready_for_new_resource`, `reference/Reticulum/RNS/Link.py:1331`),
//! so the harness plays a sender that does: it advertises the first, parks
//! it outside the link, advertises the second, and swaps the parked one
//! back in for every packet that names it.

extern crate std;

use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::destination::{Destination, DestinationType, Direction, ProofStrategy};
use crate::identity::Identity;
use crate::link::LinkId;
use crate::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use crate::packet::{Packet, PacketContext};
use crate::resource::outgoing::OutgoingResource;
use crate::resource::{
    ResourceAdvertisement, ResourceError, ResourceStrategy, HASHMAP_IS_EXHAUSTED,
};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, NoStorage};
use crate::transport::{Action, InterfaceId, TickOutput};

type EndpointNode = NodeCore<OsRng, MockClock, NoStorage>;

/// One handshake leg; a LAN-like link.
const LEG_MS: u64 = 20;

/// Poll step of the simulated clock.
const STEP_MS: u64 = 50;

/// Simulated horizon: far past any resource watchdog at this RTT.
const HORIZON_MS: u64 = 30_000;

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

fn is_context(pkt: &[u8], context: PacketContext) -> bool {
    crate::packet::peek_wire_class(pkt).is_some_and(|w| w.context == context.to_byte())
}

fn count_context(packets: &[Vec<u8>], context: PacketContext) -> usize {
    packets.iter().filter(|p| is_context(p, context)).count()
}

fn payload(seed: usize, len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 31 + seed) % 251) as u8).collect()
}

struct Pair {
    sender: EndpointNode,
    receiver: EndpointNode,
    s_iface: usize,
    r_iface: usize,
    sender_link: LinkId,
    /// The sender's other outgoing resource, outside the link's one slot.
    parked: Option<OutgoingResource>,
}

impl Pair {
    fn advance(&mut self, ms: u64) {
        self.sender.transport.clock().advance(ms);
        self.receiver.transport.clock().advance(ms);
    }

    fn now_ms(&self) -> u64 {
        self.sender.transport.clock().now_ms()
    }

    /// Advertise `data` from the sender; returns its hash and advertisement.
    fn advertise(&mut self, data: &[u8]) -> ([u8; 32], Vec<u8>) {
        let (hash, out) = self
            .sender
            .send_resource(&self.sender_link, data, None, false)
            .expect("sender advertises the resource");
        let mut adv = action_data(&out);
        assert_eq!(count_context(&adv, PacketContext::ResourceAdv), 1);
        (hash, adv.remove(0))
    }

    /// Move the sender's current outgoing resource out of its link, so a
    /// second one can be advertised beside it.
    fn park(&mut self) {
        let link = self.sender.links.get_mut(&self.sender_link).expect("link");
        self.parked = Some(link.take_outgoing_resource().expect("an outgoing resource"));
    }

    /// The resource a packet bound for the sender names, if any.
    fn named_resource(&self, pkt: &[u8]) -> Option<[u8; 32]> {
        let packet = Packet::unpack(pkt).ok()?;
        let named = match packet.context {
            PacketContext::ResourceReq | PacketContext::ResourceRcl => {
                let link = self.sender.links.get(&self.sender_link)?;
                let mut plain = std::vec![0u8; packet.data.len()];
                let len = link.decrypt(packet.data.as_slice(), &mut plain).ok()?;
                plain.truncate(len);
                if packet.context == PacketContext::ResourceRcl {
                    plain.get(..32)?.to_vec()
                } else if plain.first() == Some(&HASHMAP_IS_EXHAUSTED) {
                    plain.get(5..37)?.to_vec()
                } else {
                    plain.get(1..33)?.to_vec()
                }
            }
            PacketContext::ResourcePrf => packet.data.as_slice().get(..32)?.to_vec(),
            _ => return None,
        };
        <[u8; 32]>::try_from(named.as_slice()).ok()
    }

    /// Hand one packet to the sender, with the parked resource swapped into
    /// the link's slot when the packet names it.
    fn deliver_to_sender(&mut self, pkt: &[u8]) -> TickOutput {
        let for_parked = match (&self.parked, self.named_resource(pkt)) {
            (Some(parked), Some(named)) => *parked.resource_hash() == named,
            _ => false,
        };
        if !for_parked {
            return self.sender.handle_packet(InterfaceId(self.s_iface), pkt);
        }
        let link = self.sender.links.get_mut(&self.sender_link).expect("link");
        let current = link.take_outgoing_resource();
        if let Some(parked) = self.parked.take() {
            link.set_outgoing_resource(parked);
        }
        let out = self.sender.handle_packet(InterfaceId(self.s_iface), pkt);
        let link = self.sender.links.get_mut(&self.sender_link).expect("link");
        self.parked = link.take_outgoing_resource();
        if let Some(current) = current {
            link.set_outgoing_resource(current);
        }
        out
    }
}

/// Establish a link; the receiver (the link's responder) accepts every
/// resource and holds at most `bound` incoming ones on a link.
fn establish(bound: usize) -> Pair {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut receiver = NodeCoreBuilder::new()
        .max_incoming_resources_per_link(bound)
        .build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrinlist",
        &["two"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    dest.set_resource_strategy(ResourceStrategy::AcceptAll);
    let dest_hash = *dest.hash();
    receiver.register_destination(dest);
    let r_iface = add_iface(&mut receiver, "R_mesh");

    let mut sender = NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
    let s_iface = add_iface(&mut sender, "S_mesh");

    let (sender_link, _routed, out) = sender.connect(dest_hash, &signing_key).expect("connect");
    let mut pair = Pair {
        sender,
        receiver,
        s_iface,
        r_iface,
        sender_link,
        parked: None,
    };
    let mut established = false;
    let mut for_receiver = action_data(&out);
    for _ in 0..8 {
        if for_receiver.is_empty() {
            break;
        }
        pair.advance(LEG_MS);
        let mut back = Vec::new();
        for pkt in for_receiver {
            let o = pair.receiver.handle_packet(InterfaceId(pair.r_iface), &pkt);
            established |= o
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::LinkEstablished { .. }));
            back.extend(action_data(&o));
        }
        pair.advance(LEG_MS);
        for_receiver = Vec::new();
        for pkt in back {
            let o = pair.sender.handle_packet(InterfaceId(pair.s_iface), &pkt);
            for_receiver.extend(action_data(&o));
        }
    }
    assert!(established, "receiver side must reach Active");
    pair
}

/// Advertise A and B on one link, B while A is under way (A's request is
/// answered, none of A's parts delivered yet). Returns both hashes, the
/// receiver's answer to A, and its answer to B.
fn advertise_two(pair: &mut Pair) -> ([u8; 32], [u8; 32], Vec<Vec<u8>>, TickOutput) {
    let (hash_a, adv_a) = pair.advertise(&payload(7, 3000));
    let answer_a = action_data(
        &pair
            .receiver
            .handle_packet(InterfaceId(pair.r_iface), &adv_a),
    );
    assert_eq!(count_context(&answer_a, PacketContext::ResourceReq), 1);
    pair.park();
    let (hash_b, adv_b) = pair.advertise(&payload(11, 2000));
    assert_ne!(hash_a, hash_b);
    let answer_b = pair
        .receiver
        .handle_packet(InterfaceId(pair.r_iface), &adv_b);
    (hash_a, hash_b, answer_a, answer_b)
}

/// The single failure mode: an advertisement for a second resource while
/// the first is under way draws no answer.
#[test]
fn second_advertisement_during_a_transfer_is_requested() {
    let mut pair = establish(crate::resource::RESOURCE_MAX_INCOMING_PER_LINK);
    let (_a, hash_b, _answer_a, answer_b) = advertise_two(&mut pair);
    assert_eq!(
        count_context(&action_data(&answer_b), PacketContext::ResourceReq),
        1,
        "the second advertisement must be answered with its request"
    );
    assert!(
        answer_b.events.iter().any(|e| matches!(
            e,
            NodeEvent::ResourceTransferStarted { resource_hash, is_sender: false, .. }
                if *resource_hash == hash_b
        )),
        "the receiver starts the second transfer"
    );
}

/// Beyond the bound the advertisement is refused with an RCL naming it,
/// and the sender fails that transfer at once.
#[test]
fn advertisement_beyond_the_bound_is_refused_with_rcl() {
    let mut pair = establish(1);
    let (_a, hash_b, _answer_a, answer_b) = advertise_two(&mut pair);
    let answer_b = action_data(&answer_b);
    assert_eq!(
        count_context(&answer_b, PacketContext::ResourceRcl),
        1,
        "a full link refuses the advertisement, it does not ignore it"
    );
    assert_eq!(pair.named_resource(&answer_b[0]), Some(hash_b));
    let out = pair.deliver_to_sender(&answer_b[0]);
    assert!(
        out.events.iter().any(|e| matches!(
            e,
            NodeEvent::ResourceFailed {
                resource_hash,
                error: ResourceError::RejectedByRemote,
                is_sender: true,
                ..
            } if *resource_hash == hash_b
        )),
        "the sender learns the refusal at once: {:?}",
        out.events
    );
}

/// The sender's advertisement `adv`, re-sealed with the compressed flag set,
/// as a Python sender emits it whenever bz2 shrinks the data
/// (`reference/Reticulum/RNS/Resource.py:248`). This build's own sender
/// compresses only with the `compression` feature, so the flag is forged.
fn mark_compressed(pair: &Pair, adv: &[u8]) -> Vec<u8> {
    let link = pair.sender.links.get(&pair.sender_link).expect("link");
    let packet = Packet::unpack(adv).expect("advertisement packet");
    let mut plain = std::vec![0u8; packet.data.len()];
    let len = link
        .decrypt(packet.data.as_slice(), &mut plain)
        .expect("decrypt advertisement");
    plain.truncate(len);
    let mut parsed = ResourceAdvertisement::unpack(&plain).expect("advertisement");
    parsed.flags.compressed = true;
    link.build_data_packet_with_context(&parsed.pack(), PacketContext::ResourceAdv, &mut OsRng)
        .expect("re-sealed advertisement")
}

/// Review finding L-0028: without the `compression` feature (every board
/// build) a compressed advertisement is refused with an RCL before any part
/// is requested, and the sender fails it at once, instead of the whole
/// transfer crossing the air to fail at assembly.
#[cfg(not(feature = "compression"))]
#[test]
fn compressed_advertisement_is_refused_without_the_feature() {
    let mut pair = establish(crate::resource::RESOURCE_MAX_INCOMING_PER_LINK);
    let (hash, adv) = pair.advertise(&payload(7, 3000));
    let adv = mark_compressed(&pair, &adv);
    let iface = InterfaceId(pair.r_iface);
    #[cfg(feature = "tracing")]
    let answer = {
        let (answer, logs) = crate::test_log_capture::with_captured_logs(|| {
            pair.receiver.handle_packet(iface, &adv)
        });
        let line = logs
            .lines()
            .find(|l| l.contains("RESOURCE_ADV_REFUSED"))
            .expect("the refusal emits RESOURCE_ADV_REFUSED");
        assert!(line.contains("reason=\"compressed\""), "{line}");
        answer
    };
    #[cfg(not(feature = "tracing"))]
    let answer = pair.receiver.handle_packet(iface, &adv);
    assert!(
        !answer
            .events
            .iter()
            .any(|e| matches!(e, NodeEvent::ResourceTransferStarted { .. })),
        "no transfer starts: {:?}",
        answer.events
    );
    let answer = action_data(&answer);
    assert_eq!(count_context(&answer, PacketContext::ResourceReq), 0);
    assert_eq!(
        count_context(&answer, PacketContext::ResourceRcl),
        1,
        "the advertisement is refused, not requested"
    );
    assert_eq!(pair.named_resource(&answer[0]), Some(hash));
    let out = pair.deliver_to_sender(&answer[0]);
    assert!(
        out.events.iter().any(|e| matches!(
            e,
            NodeEvent::ResourceFailed {
                resource_hash,
                error: ResourceError::RejectedByRemote,
                is_sender: true,
                ..
            } if *resource_hash == hash
        )),
        "the sender learns the refusal at once: {:?}",
        out.events
    );
}

/// With the feature (every host build) the same advertisement is taken:
/// the refusal is for a build that cannot decompress, nothing else.
#[cfg(feature = "compression")]
#[test]
fn compressed_advertisement_is_requested_with_the_feature() {
    let mut pair = establish(crate::resource::RESOURCE_MAX_INCOMING_PER_LINK);
    let (hash, adv) = pair.advertise(&payload(7, 3000));
    let adv = mark_compressed(&pair, &adv);
    let answer = pair.receiver.handle_packet(InterfaceId(pair.r_iface), &adv);
    assert!(
        answer.events.iter().any(|e| matches!(
            e,
            NodeEvent::ResourceTransferStarted { resource_hash, is_sender: false, .. }
                if *resource_hash == hash
        )),
        "the receiver starts the transfer: {:?}",
        answer.events
    );
    let answer = action_data(&answer);
    assert_eq!(count_context(&answer, PacketContext::ResourceReq), 1);
    assert_eq!(count_context(&answer, PacketContext::ResourceRcl), 0);
}

/// What [`drive`] saw: the data the receiver completed, by hash, the
/// receiver's failed hashes, how many of the driven hashes the sender saw
/// proven, and how many times the sender re-advertised.
struct Outcome {
    received: Vec<([u8; 32], Vec<u8>)>,
    failed: Vec<[u8; 32]>,
    proven: usize,
    readvertised: usize,
}

/// Drive the link until both transfers end or the horizon passes, starting
/// with `first` bound for the sender.
fn drive(pair: &mut Pair, first: Vec<Vec<u8>>, hashes: [[u8; 32]; 2]) -> Outcome {
    let mut to_sender = first;
    let mut to_receiver: Vec<Vec<u8>> = Vec::new();
    let mut received = Vec::new();
    let mut failed = Vec::new();
    let mut proven = 0usize;
    let mut readvertised = 0usize;
    while pair.now_ms() < TEST_TIME_MS + HORIZON_MS {
        let mut events = Vec::new();
        for _ in 0..256 {
            if to_sender.is_empty() && to_receiver.is_empty() {
                break;
            }
            let mut next_to_sender = Vec::new();
            for pkt in core::mem::take(&mut to_receiver) {
                let o = pair.receiver.handle_packet(InterfaceId(pair.r_iface), &pkt);
                events.extend(o.events.iter().cloned());
                next_to_sender.extend(action_data(&o));
            }
            for pkt in core::mem::take(&mut to_sender) {
                let o = pair.deliver_to_sender(&pkt);
                events.extend(o.events.iter().cloned());
                to_receiver.extend(action_data(&o));
            }
            to_sender = next_to_sender;
        }
        for ev in events {
            match ev {
                NodeEvent::ResourceCompleted {
                    is_sender: false,
                    resource_hash,
                    data,
                    ..
                } => received.push((resource_hash, data)),
                NodeEvent::ResourceCompleted {
                    is_sender: true,
                    resource_hash,
                    ..
                } if hashes.contains(&resource_hash) => proven += 1,
                NodeEvent::ResourceFailed {
                    is_sender: false,
                    resource_hash,
                    ..
                } => failed.push(resource_hash),
                _ => {}
            }
        }
        if proven + failed.len() >= 2 {
            break;
        }
        pair.advance(STEP_MS);
        to_sender.extend(action_data(&pair.receiver.handle_timeout()));
        let from_sender = action_data(&pair.sender.handle_timeout());
        readvertised += count_context(&from_sender, PacketContext::ResourceAdv);
        to_receiver.extend(from_sender);
    }
    Outcome {
        received,
        failed,
        proven,
        readvertised,
    }
}

/// End to end: both resources, advertised concurrently on one link, arrive
/// whole and are proven to the sender.
#[test]
fn two_concurrent_resources_on_one_link_both_complete() {
    let mut pair = establish(crate::resource::RESOURCE_MAX_INCOMING_PER_LINK);
    let (hash_a, hash_b, answer_a, answer_b) = advertise_two(&mut pair);
    let mut first = answer_a;
    first.extend(action_data(&answer_b));
    let Outcome {
        received,
        failed,
        proven,
        readvertised,
    } = drive(&mut pair, first, [hash_a, hash_b]);
    assert!(failed.is_empty(), "no transfer failed: {failed:?}");
    let a = received.iter().find(|(h, _)| *h == hash_a);
    let b = received.iter().find(|(h, _)| *h == hash_b);
    assert_eq!(
        a.map(|(_, d)| d.as_slice()),
        Some(payload(7, 3000).as_slice())
    );
    assert_eq!(
        b.map(|(_, d)| d.as_slice()),
        Some(payload(11, 2000).as_slice())
    );
    assert_eq!(proven, 2, "the sender saw both transfers proven");
    assert_eq!(
        readvertised, 0,
        "B was taken when first advertised, not after A freed the link and B's \
         sender re-advertised"
    );
}

/// The sender's cancel names one resource; only that one of the two ends,
/// the other still completes (`reference/Reticulum/RNS/Link.py:1134-1141`).
#[test]
fn cancel_ends_only_the_resource_it_names() {
    let mut pair = establish(crate::resource::RESOURCE_MAX_INCOMING_PER_LINK);
    let (hash_a, hash_b, answer_a, _answer_b) = advertise_two(&mut pair);
    let icl = pair
        .sender
        .links
        .get(&pair.sender_link)
        .expect("link")
        .build_data_packet_with_context(&hash_b, PacketContext::ResourceIcl, &mut OsRng)
        .expect("ICL");
    let out = pair.receiver.handle_packet(InterfaceId(pair.r_iface), &icl);
    assert!(
        out.events.iter().any(|e| matches!(
            e,
            NodeEvent::ResourceFailed { resource_hash, error: ResourceError::Cancelled, .. }
                if *resource_hash == hash_b
        )),
        "the named resource is cancelled"
    );
    let receiver_link = pair
        .receiver
        .links
        .values()
        .find(|l| l.has_incoming_resource())
        .expect("the other resource is still held");
    assert_eq!(receiver_link.incoming_resource_count(), 1);

    // B is gone on the receiver; the sender drops it too, as its own cancel
    // would, and A runs to the end.
    let link = pair.sender.links.get_mut(&pair.sender_link).expect("link");
    link.clear_outgoing_resource();
    let Outcome {
        received, failed, ..
    } = drive(&mut pair, answer_a, [hash_a, hash_b]);
    assert!(failed.is_empty(), "A did not fail: {failed:?}");
    assert_eq!(
        received
            .iter()
            .find(|(h, _)| *h == hash_a)
            .map(|(_, d)| d.as_slice()),
        Some(payload(7, 3000).as_slice())
    );
}
