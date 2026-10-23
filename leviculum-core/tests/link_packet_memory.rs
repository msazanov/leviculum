//! Real 16 KiB link consumers, including their incremental receive allocation peak.
//!
//! Only allocations born inside a receiver call are attributed. The caller's
//! wire frame and the already resident node/link/resource tables are funded by
//! their owners, not counted again as a link-dispatch transient. Deallocations
//! remain attributed after the call returns. Large-buffer bounds name the
//! buffers that overlap in the production consumer, not a copied source test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Mutex, PoisonError};

use rand_core::OsRng;

use leviculum_core::constants::{
    AES_BLOCK_SIZE, CHANNEL_ENVELOPE_HEADER_SIZE, HEADER_MINSIZE, TOKEN_OVERHEAD,
};
use leviculum_core::link::Link;
use leviculum_core::node::request::RequestPolicy;
use leviculum_core::packet::{
    packet_hash, packet_hash_from_packet, truncated_packet_hash, HeaderType, Packet, PacketContext,
    PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::resource::{resource_sdu, ResourceStrategy, RESOURCE_RANDOM_HASH_SIZE};
use leviculum_core::traits::Clock;
use leviculum_core::{
    Action, Destination, DestinationHash, DestinationType, Direction, Identity, InterfaceId,
    LinkId, MemoryStorage, NodeCore, NodeCoreBuilder, NodeEvent, ProofStrategy, TickOutput,
};

const LINK_MTU: usize = 16_384;
const LARGE_BUFFER_MIN: usize = LINK_MTU / 2;
const START_MS: u64 = 1_000_000;
const IFACE: InterfaceId = InterfaceId(0);

struct ReceiverAlloc;

#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    peak: usize,
    large_peak: usize,
    large_count: usize,
    large_sizes: [usize; 8],
    snapshot_overflow: bool,
}

#[derive(Default)]
struct Tracker {
    blocks: HashMap<usize, usize>,
    live: usize,
    large_live: usize,
    large_count: usize,
    sample: Sample,
}

impl Tracker {
    fn insert(&mut self, address: usize, size: usize) {
        self.blocks.insert(address, size);
        self.live += size;
        self.sample.peak = self.sample.peak.max(self.live);
        if size >= LARGE_BUFFER_MIN {
            self.large_live += size;
            self.large_count += 1;
            if self.large_live > self.sample.large_peak {
                self.sample.large_peak = self.large_live;
                self.sample.large_count = self.large_count;
                self.sample.large_sizes = [0; 8];
                for (index, &size) in self
                    .blocks
                    .values()
                    .filter(|&&size| size >= LARGE_BUFFER_MIN)
                    .enumerate()
                {
                    if let Some(slot) = self.sample.large_sizes.get_mut(index) {
                        *slot = size;
                    } else {
                        self.sample.snapshot_overflow = true;
                    }
                }
                self.sample.large_sizes.sort_unstable();
            }
        }
    }

    fn remove(&mut self, address: usize) {
        if let Some(size) = self.blocks.remove(&address) {
            self.live -= size;
            if size >= LARGE_BUFFER_MIN {
                self.large_live -= size;
                self.large_count -= 1;
            }
        }
    }
}

static TRACKER: Mutex<Option<Tracker>> = Mutex::new(None);

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static IN_TRACKER: Cell<bool> = const { Cell::new(false) };
}

fn with_tracker<T>(f: impl FnOnce(&mut Tracker) -> T) -> Option<T> {
    IN_TRACKER
        .try_with(|inside| {
            if inside.replace(true) {
                return None;
            }
            let mut tracker = TRACKER.lock().unwrap_or_else(PoisonError::into_inner);
            let result = f(tracker.get_or_insert_with(Tracker::default));
            drop(tracker);
            inside.set(false);
            Some(result)
        })
        .ok()
        .flatten()
}

fn armed() -> bool {
    ARMED.try_with(Cell::get).unwrap_or(false)
}

// SAFETY: the same layouts and addresses are forwarded to System. Tracker
// allocations are excluded by a per-thread reentrancy guard. The default
// realloc implementation records allocate-before-free overlap rather than
// silently erasing an old live block during a capacity change.
unsafe impl GlobalAlloc for ReceiverAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = System.alloc(layout);
        if !pointer.is_null() && armed() {
            with_tracker(|tracker| tracker.insert(pointer as usize, layout.size()));
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        with_tracker(|tracker| tracker.remove(pointer as usize));
        System.dealloc(pointer, layout);
    }
}

#[global_allocator]
static ALLOCATOR: ReceiverAlloc = ReceiverAlloc;

struct Window;

impl Drop for Window {
    fn drop(&mut self) {
        ARMED.with(|armed| armed.set(false));
    }
}

fn measure<T>(operation: impl FnOnce() -> T) -> (T, Sample) {
    assert!(!armed());
    with_tracker(|tracker| {
        tracker.blocks.clear();
        tracker.live = 0;
        tracker.large_live = 0;
        tracker.large_count = 0;
        tracker.sample = Sample::default();
    });
    ARMED.with(|armed| armed.set(true));
    let window = Window;
    let output = operation();
    drop(window);
    let sample = with_tracker(|tracker| tracker.sample).unwrap();
    (output, sample)
}

fn assert_bound(label: &str, sample: Sample, sources: &[(&str, usize)]) {
    let bound: usize = sources.iter().map(|(_, size)| size).sum();
    let large_count = sources
        .iter()
        .filter(|(_, size)| *size >= LARGE_BUFFER_MIN)
        .count();
    println!(
        "LINK_RX_MEMORY path={label} mtu={LINK_MTU} measured_peak={} large_peak={} large_count={} large_sizes={:?} source_bound={bound} source={sources:?}",
        sample.peak, sample.large_peak, sample.large_count, sample.large_sizes,
    );
    assert!(
        !sample.snapshot_overflow,
        "{label}: unenumerated large allocations"
    );
    assert!(
        sample.large_peak <= bound,
        "{label}: overlapping large buffers exceed their source bound"
    );
    assert!(
        sample.large_count <= large_count,
        "{label}: an additional whole-payload copy is live"
    );
    assert!(
        sample.peak >= sample.large_peak,
        "the allocation instrument is inconsistent"
    );
}

#[derive(Clone)]
struct StepClock(Rc<Cell<u64>>);

impl Clock for StepClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
}

type Node = NodeCore<OsRng, StepClock, MemoryStorage>;

struct Pair {
    sender: Node,
    receiver: Node,
    link_id: LinkId,
    destination: DestinationHash,
    clock: StepClock,
}

fn packets(output: TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .into_iter()
        .map(|action| match action {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data,
        })
        .collect()
}

fn linked_pair(request_header: HeaderType, receiver_builder: NodeCoreBuilder) -> Pair {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut destination = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "linkmemory",
        &["endpoint"],
    )
    .unwrap();
    destination.set_accepts_links(true);
    destination.set_proof_strategy(ProofStrategy::All);
    let destination_hash = *destination.hash();
    let announce = destination
        .announce(None, &mut OsRng, START_MS, START_MS / 1000)
        .unwrap();
    let mut raw_announce = vec![0; announce.packed_size()];
    announce.pack(&mut raw_announce).unwrap();

    let clock = StepClock(Rc::new(Cell::new(START_MS)));
    let mut sender =
        NodeCoreBuilder::new().build(OsRng, clock.clone(), MemoryStorage::with_defaults());
    let mut receiver = receiver_builder.max_incoming_resource_size(LINK_MTU).build(
        OsRng,
        clock.clone(),
        MemoryStorage::with_defaults(),
    );
    sender.set_interface_hw_mtu(IFACE.0, LINK_MTU as u32);
    receiver.set_interface_hw_mtu(IFACE.0, LINK_MTU as u32);
    receiver.register_destination(destination);
    receiver.register_request_handler(destination_hash, "/memory", RequestPolicy::AllowAll);
    let _ = sender.handle_packet(IFACE, &raw_announce);
    let (link_id, _, output) = sender.connect(destination_hash, &signing_key).unwrap();
    let mut to_receiver = packets(output);
    assert_eq!(to_receiver.len(), 1);
    let mut request = Packet::unpack(&to_receiver[0]).unwrap();
    assert_eq!(request.data.len(), 67, "negotiated MTU must be signalled");
    request.flags.header_type = request_header;
    request.transport_id =
        (request_header == HeaderType::Type2).then_some(*receiver.identity().hash());
    request.flags.transport_type = if request_header == HeaderType::Type2 {
        TransportType::Transport
    } else {
        TransportType::Broadcast
    };
    let mut raw_request = vec![0; request.packed_size()];
    request.pack(&mut raw_request).unwrap();
    assert_eq!(
        Link::calculate_link_id(&raw_request),
        link_id,
        "routing cannot change link identity"
    );
    to_receiver[0] = raw_request;

    for _ in 0..8 {
        if to_receiver.is_empty() {
            break;
        }
        let mut back = Vec::new();
        for raw in to_receiver {
            back.extend(packets(receiver.handle_packet(IFACE, &raw)));
        }
        to_receiver = Vec::new();
        for raw in back {
            to_receiver.extend(packets(sender.handle_packet(IFACE, &raw)));
        }
    }
    assert!(to_receiver.is_empty(), "handshake did not settle");
    assert!(sender.link(&link_id).unwrap().is_active());
    assert!(receiver.link(&link_id).unwrap().is_active());
    assert_eq!(
        sender.link(&link_id).unwrap().negotiated_mtu(),
        LINK_MTU as u32
    );
    assert_eq!(
        receiver.link(&link_id).unwrap().negotiated_mtu(),
        LINK_MTU as u32
    );
    Pair {
        sender,
        receiver,
        link_id,
        destination: destination_hash,
        clock,
    }
}

fn payload(length: usize, salt: u8) -> Vec<u8> {
    (0..length)
        .map(|index| (index % 251) as u8 ^ salt)
        .collect()
}

fn send_channel(pair: &mut Pair, data: &[u8]) -> TickOutput {
    match pair.sender.send_on_link(&pair.link_id, data) {
        Ok(output) => output,
        Err(leviculum_core::node::SendError::PacingDelay { ready_at_ms }) => {
            assert!(ready_at_ms > pair.clock.now_ms());
            pair.clock.0.set(ready_at_ms);
            pair.sender.send_on_link(&pair.link_id, data).unwrap()
        }
        Err(error) => panic!("channel send failed: {error:?}"),
    }
}

fn encoded_bin(length: usize, salt: u8) -> Vec<u8> {
    assert!(length > 258 && length <= u16::MAX as usize);
    let mut value = Vec::with_capacity(length);
    value.push(0xc5);
    value.extend_from_slice(&((length - 3) as u16).to_be_bytes());
    value.extend_from_slice(&payload(length - 3, salt));
    assert_eq!(value.len(), length);
    value
}

fn one_packet(output: TickOutput, context: PacketContext) -> Vec<u8> {
    let mut frames = packets(output);
    assert_eq!(frames.len(), 1);
    let raw = frames.pop().unwrap();
    assert_eq!(Packet::unpack(&raw).unwrap().context, context);
    assert!(
        raw.len() <= LINK_MTU,
        "the fixture exceeded its real negotiated carrier MTU"
    );
    raw
}

fn finish_proofs(sender: &mut Node, link_id: &LinkId, output: TickOutput, hash: [u8; 32]) {
    let mut confirmed = false;
    let mut validated = false;
    for raw in packets(output) {
        let proof = Packet::unpack(&raw).unwrap();
        validated |= proof.flags.packet_type == PacketType::Proof
            && sender
                .link(link_id)
                .unwrap()
                .validate_data_proof(proof.data.as_slice(), &hash);
        let output = sender.handle_packet(IFACE, &raw);
        confirmed |= output.events.iter().any(|event| {
            matches!(event,
                NodeEvent::LinkDeliveryConfirmed { packet_hash, .. } if *packet_hash == hash
            )
        });
    }
    assert!(
        validated,
        "the receiver's proof does not validate against the original wire hash"
    );
    assert!(
        confirmed,
        "the sender did not accept the proof and clear its receipt"
    );
}

fn plain_data(pair: &mut Pair) {
    let data = payload(pair.sender.link(&pair.link_id).unwrap().mdu(), 0x31);
    let (hash, output) = pair
        .sender
        .send_packet_on_link(&pair.link_id, &data)
        .unwrap();
    let raw = one_packet(output, PacketContext::None);
    assert_eq!(hash, packet_hash(&raw));
    let encrypted = raw.len() - HEADER_MINSIZE;
    let (output, sample) = measure(|| pair.receiver.handle_packet(IFACE, &raw));
    assert!(output.events.iter().any(|event| matches!(event,
        NodeEvent::LinkDataReceived { data: received, .. } if *received == data
    )));
    assert_bound(
        "plain-max-mdu",
        sample,
        &[
            ("Packet::unpack-owned-ciphertext", encrypted),
            ("plain-decrypt-output-capacity", encrypted),
        ],
    );
    finish_proofs(&mut pair.sender, &pair.link_id, output, hash);
}

fn channel_data(pair: &mut Pair) {
    let data = payload(
        pair.sender.link(&pair.link_id).unwrap().mdu() - CHANNEL_ENVELOPE_HEADER_SIZE,
        0x52,
    );
    let output = send_channel(pair, &data);
    let raw = one_packet(output, PacketContext::Channel);
    let hash = packet_hash(&raw);
    let encrypted = raw.len() - HEADER_MINSIZE;
    let (output, sample) = measure(|| pair.receiver.handle_packet(IFACE, &raw));
    assert!(output.events.iter().any(|event| matches!(event,
        NodeEvent::MessageReceived { data: received, sequence: 0, .. } if *received == data
    )));
    assert_bound(
        "channel-max-mdu",
        sample,
        &[
            ("Packet::unpack-owned-ciphertext", encrypted),
            ("channel-decrypt-output-capacity", encrypted),
            ("Envelope::unpack-owned-data", data.len()),
        ],
    );
    finish_proofs(&mut pair.sender, &pair.link_id, output, hash);
}

fn request_and_response(pair: &mut Pair) {
    let mdu = pair.sender.link(&pair.link_id).unwrap().mdu();
    let encoded_hash = [0xc4, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let data = encoded_bin(mdu - 1 - 9 - encoded_hash.len(), 0x73);
    let (request_id, output) = pair
        .sender
        .send_request(&pair.link_id, "/memory", Some(&data), None)
        .unwrap();
    let raw = one_packet(output, PacketContext::Request);
    assert_eq!(request_id, truncated_packet_hash(&raw));
    let encrypted = raw.len() - HEADER_MINSIZE;
    let (output, sample) = measure(|| pair.receiver.handle_packet(IFACE, &raw));
    assert!(output.events.iter().any(|event| matches!(event,
        NodeEvent::RequestReceived { request_id: received_id, data: received, .. }
            if *received_id == request_id && *received == data
    )));
    assert_bound(
        "request-max-mdu",
        sample,
        &[
            ("Packet::unpack-owned-ciphertext", encrypted),
            ("request-decrypt-output-capacity", encrypted),
            ("request-raw-msgpack-value", data.len()),
        ],
    );
    drop(output);

    // An independently generated application response, not a forwarding echo.
    let response = encoded_bin(mdu - 1 - encoded_hash.len(), 0x94);
    let output = pair
        .receiver
        .send_response(&pair.link_id, &request_id, &response)
        .unwrap();
    let raw = one_packet(output, PacketContext::Response);
    let encrypted = raw.len() - HEADER_MINSIZE;
    let (output, sample) = measure(|| pair.sender.handle_packet(IFACE, &raw));
    assert!(output.events.iter().any(|event| matches!(event,
        NodeEvent::ResponseReceived { request_id: received_id, response_data: received, metadata: None, .. }
            if *received_id == request_id && *received == response
    )));
    assert_bound(
        "response-max-mdu",
        sample,
        &[
            ("Packet::unpack-owned-ciphertext", encrypted),
            ("response-decrypt-output-capacity", encrypted),
            ("response-raw-msgpack-value", response.len()),
        ],
    );
}

fn resource_data(pair: &mut Pair) {
    pair.receiver
        .set_resource_strategy(&pair.link_id, ResourceStrategy::AcceptAll)
        .unwrap();
    let data = payload(
        resource_sdu(LINK_MTU as u32) - TOKEN_OVERHEAD - AES_BLOCK_SIZE - RESOURCE_RANDOM_HASH_SIZE,
        0xB5,
    );
    let (resource_hash, output) = pair
        .sender
        .send_resource(&pair.link_id, &data, None, false)
        .unwrap();
    let advertisement = one_packet(output, PacketContext::ResourceAdv);
    let (output, advertisement_sample) =
        measure(|| pair.receiver.handle_packet(IFACE, &advertisement));
    assert_bound("resource-advertisement", advertisement_sample, &[]);
    let mut parts = Vec::new();
    for request in packets(output) {
        parts.extend(packets(pair.sender.handle_packet(IFACE, &request)));
    }
    assert_eq!(
        parts.len(),
        1,
        "the fixture must assemble one full-sized part in one reception"
    );
    let raw = parts.pop().unwrap();
    assert_eq!(
        Packet::unpack(&raw).unwrap().context,
        PacketContext::Resource
    );
    assert!(raw.len() <= LINK_MTU);
    let transfer_size = raw.len() - HEADER_MINSIZE;
    assert!(transfer_size >= LARGE_BUFFER_MIN);
    let (output, sample) = measure(|| pair.receiver.handle_packet(IFACE, &raw));
    assert!(output.events.iter().any(|event| matches!(event,
        NodeEvent::ResourceCompleted { resource_hash: received_hash, data: received, is_sender: false, metadata: None, .. }
            if *received_hash == resource_hash && *received == data
    )));
    // During concatenation: packet + received part + stream. During decryption:
    // packet + stream + decrypt output. The part is consumed before decrypt.
    assert_bound(
        "resource-full-sized-part-and-assembly",
        sample,
        &[
            ("Packet::unpack-owned-resource-part", transfer_size),
            ("received-part-or-assembly-stream", transfer_size),
            ("assembly-stream-or-decrypt-output", transfer_size),
        ],
    );
    let mut proved = false;
    for raw in packets(output) {
        let output = pair.sender.handle_packet(IFACE, &raw);
        proved |= output.events.iter().any(|event| {
            matches!(event,
                NodeEvent::ResourceCompleted { resource_hash: received_hash, is_sender: true, .. }
                    if *received_hash == resource_hash
            )
        });
    }
    assert!(
        proved,
        "the real sender did not validate the resource completion proof"
    );
}

fn oversized_invalid_link_request(pair: &mut Pair) {
    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::LinkRequest,
        },
        hops: 0,
        transport_id: None,
        destination_hash: pair.destination.into_bytes(),
        context: PacketContext::None,
        // Mode zero is unsupported; the oversized payload must not be repacked
        // before that existing validation rejects it.
        data: PacketData::Owned(vec![0; LINK_MTU - HEADER_MINSIZE]),
    };
    let mut raw = vec![0; packet.packed_size()];
    packet.pack(&mut raw).unwrap();
    let before = pair.receiver.link_count();
    let (output, sample) = measure(|| pair.receiver.handle_packet(IFACE, &raw));
    assert_eq!(
        pair.receiver.link_count(),
        before,
        "invalid oversized requests cannot create a link"
    );
    assert!(
        output.actions.is_empty(),
        "invalid oversized requests cannot be proved"
    );
    assert_bound(
        "invalid-oversized-link-request",
        sample,
        &[("Packet::unpack-owned-invalid-request", packet.data.len())],
    );
}

#[test]
fn negotiated_16k_link_consumers_preserve_payloads_proofs_ids_and_allocation_bounds() {
    for header_type in [HeaderType::Type1, HeaderType::Type2] {
        let mut pair = linked_pair(header_type, NodeCoreBuilder::new());
        plain_data(&mut pair);
        channel_data(&mut pair);
        request_and_response(&mut pair);
        resource_data(&mut pair);
        oversized_invalid_link_request(&mut pair);
    }

    let pair = linked_pair(HeaderType::Type1, NodeCoreBuilder::new());
    let raw = pair
        .sender
        .link(&pair.link_id)
        .unwrap()
        .build_data_packet(
            &payload(pair.sender.link(&pair.link_id).unwrap().mdu(), 0xD6),
            &mut OsRng,
        )
        .unwrap();
    let mut packet = Packet::unpack(&raw).unwrap();
    for header_type in [HeaderType::Type1, HeaderType::Type2] {
        packet.flags.header_type = header_type;
        packet.transport_id = (header_type == HeaderType::Type2).then_some([0xE7; 16]);
        let (hash, sample) = measure(|| packet_hash_from_packet(&packet));
        assert_eq!(hash, packet_hash(&raw));
        assert_eq!(
            sample.peak, 0,
            "hashing canonical packet parts must allocate nothing"
        );
    }
    legacy_channel_admission_control();
}

fn collect_messages(output: &mut TickOutput, seen: &mut HashMap<u16, Vec<u8>>) {
    for event in std::mem::take(&mut output.events) {
        if let NodeEvent::MessageReceived { sequence, data, .. } = event {
            assert!(
                seen.insert(sequence, data).is_none(),
                "a channel message was delivered twice"
            );
        }
    }
}

fn legacy_channel_admission_control() {
    let mut pair = linked_pair(HeaderType::Type1, NodeCoreBuilder::new());
    let data0 = payload(64, 0x65);
    let data1 = payload(
        pair.sender.link(&pair.link_id).unwrap().mdu() - CHANNEL_ENVELOPE_HEADER_SIZE,
        0x76,
    );
    let output = send_channel(&mut pair, &data0);
    let missing = one_packet(output, PacketContext::Channel);
    let output = send_channel(&mut pair, &data1);
    let later = one_packet(output, PacketContext::Channel);
    let mut seen = HashMap::new();
    let mut output = pair.receiver.handle_packet(IFACE, &later);
    collect_messages(&mut output, &mut seen);
    assert!(seen.is_empty());
    finish_proofs(&mut pair.sender, &pair.link_id, output, packet_hash(&later));
    let mut output = pair.receiver.handle_packet(IFACE, &missing);
    collect_messages(&mut output, &mut seen);
    assert_eq!(seen.get(&0), Some(&data0));
    assert_eq!(
        seen.get(&1),
        Some(&data1),
        "default non-RP nodes keep legacy large out-of-order admission"
    );
    finish_proofs(
        &mut pair.sender,
        &pair.link_id,
        output,
        packet_hash(&missing),
    );
}
