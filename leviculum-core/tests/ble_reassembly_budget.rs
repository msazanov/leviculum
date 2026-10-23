//! Production BLE reassembly under a refusing Llff allocator. Other threads
//! and fixture packets keep using System; only the receiver's poll/process
//! window enters the same allocator implementation used by the firmware.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, UnsafeCell};
use std::future::{pending, Future};
use std::mem::{align_of, size_of};
use std::pin::pin;
use std::ptr;
use std::sync::{Mutex, MutexGuard, Once};
use std::task::{Context, Waker};

use embedded_alloc::LlffHeap;
use leviculum_core::framing::ble::{
    build_fragment_header, fragment_packet, BleDefragmenter, BleReassemblyLimits, DefragResult,
    FRAGMENT_TYPE_CONTINUE, FRAGMENT_TYPE_END, FRAGMENT_TYPE_LONE, FRAGMENT_TYPE_START,
    REASSEMBLY_TIMEOUT_MS,
};

const RP_LIMITS: BleReassemblyLimits = BleReassemblyLimits::new(564, 38);
const ARENA_BYTES: usize = 32 * 1024;

#[repr(align(16))]
struct Arena(UnsafeCell<[u8; ARENA_BYTES]>);

// SAFETY: initialization is once-only; afterwards Llff serializes allocation
// and deallocation, and each returned block has its usual exclusive ownership.
unsafe impl Sync for Arena {}

static ARENA: Arena = Arena(UnsafeCell::new([0; ARENA_BYTES]));
static HEAP: LlffHeap = LlffHeap::empty();
static INIT: Once = Once::new();
static INSTRUMENT: Mutex<()> = Mutex::new(());

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static LIMIT: Cell<usize> = const { Cell::new(0) };
    static ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static REFUSED: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

// Llff leaves no allocated-block header. It rounds requests to a word and
// reserves at least two words, enough to become a free Hole on deallocation.
fn allocation_bytes(bytes: usize) -> usize {
    if bytes == 0 {
        0
    } else {
        bytes
            .max(2 * size_of::<usize>())
            .div_ceil(align_of::<usize>())
            * align_of::<usize>()
    }
}

struct RefusingAlloc;

// SAFETY: each allocation is delegated unchanged to System or Llff. Its address
// identifies which allocator owns it, including frees after a measuring window.
// The default realloc implementation preserves old/new overlap in the census.
unsafe impl GlobalAlloc for RefusingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !ARMED.try_with(Cell::get).unwrap_or(false) {
            return System.alloc(layout);
        }
        ATTEMPTS.with(|n| n.set(n.get() + 1));
        let limit = LIMIT.with(Cell::get);
        if allocation_bytes(layout.size()) > limit.saturating_sub(HEAP.used()) {
            REFUSED.with(|n| n.set(n.get() + 1));
            return ptr::null_mut();
        }
        let block = GlobalAlloc::alloc(&HEAP, layout);
        if block.is_null() {
            REFUSED.with(|n| n.set(n.get() + 1));
        } else {
            PEAK.with(|n| n.set(n.get().max(HEAP.used())));
        }
        block
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        let start = ARENA.0.get() as usize;
        if (start..start + ARENA_BYTES).contains(&(block as usize)) {
            GlobalAlloc::dealloc(&HEAP, block, layout);
        } else {
            System.dealloc(block, layout);
        }
    }
}

#[global_allocator]
static ALLOC: RefusingAlloc = RefusingAlloc;

struct Probe {
    _lock: MutexGuard<'static, ()>,
}

impl Probe {
    fn new(limit: usize) -> Self {
        let lock = INSTRUMENT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        INIT.call_once(|| {
            // SAFETY: the aligned static arena lives for the entire test binary
            // and is initialized once before any receiver allocation.
            unsafe { HEAP.init(ARENA.0.get() as usize, ARENA_BYTES) };
        });
        assert_eq!(HEAP.used(), 0, "the previous receiver released every block");
        LIMIT.with(|n| n.set(limit));
        ATTEMPTS.with(|n| n.set(0));
        REFUSED.with(|n| n.set(0));
        PEAK.with(|n| n.set(0));
        ARMED.with(|n| n.set(true));
        Self { _lock: lock }
    }

    fn limit(&self, bytes: usize) {
        LIMIT.with(|n| n.set(bytes));
    }

    fn attempts(&self) -> usize {
        ATTEMPTS.with(Cell::get)
    }

    fn refused(&self) -> usize {
        REFUSED.with(Cell::get)
    }

    fn peak(&self) -> usize {
        PEAK.with(Cell::get)
    }

    fn used(&self) -> usize {
        HEAP.used()
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        ARMED.with(|n| n.set(false));
    }
}

fn heap_peak(limits: BleReassemblyLimits) -> usize {
    let count = usize::from(limits.fragments()).min(limits.packet_bytes());
    let fragments = if count == 0 {
        0
    } else {
        (count - 1) * allocation_bytes(1) + allocation_bytes(limits.packet_bytes() - (count - 1))
    };
    allocation_bytes(limits.spine_bytes()) + fragments + allocation_bytes(limits.packet_bytes())
}

fn frame(kind: u8, sequence: u16, total: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![kind];
    frame.extend_from_slice(&sequence.to_be_bytes());
    frame.extend_from_slice(&total.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[test]
fn valid_maximum_packets_remain_byte_identical_at_every_supported_width() {
    let packet: Vec<u8> = (0..564).map(|i| (i % 251) as u8).collect();
    for mtu in [23, 24, 185, 247, 256, 517] {
        let fragments = fragment_packet(&packet, mtu);
        let probe = Probe::new(heap_peak(RP_LIMITS));
        let mut receiver = BleDefragmenter::new(RP_LIMITS);
        assert_eq!(probe.used(), 0, "the constructor is allocation-free");
        assert_eq!(receiver.process(&fragments[0], 0), DefragResult::NeedMore);
        // START first, then all remaining fragments backwards. Even a large
        // negotiated MTU must still accept an ATT23-fragmented normal packet.
        for index in (2..fragments.len()).rev() {
            assert_eq!(
                receiver.process(&fragments[index], 1),
                DefragResult::NeedMore
            );
        }
        let DefragResult::Complete(received) = receiver.process(&fragments[1], 2) else {
            panic!("a funded legal packet must complete at ATT {mtu}");
        };
        assert_eq!(received.as_slice(), packet.as_slice());
        assert_eq!(
            received.capacity(),
            packet.len(),
            "assembly has exact capacity"
        );
        assert_eq!(
            receiver.last_completed_fragments() as usize,
            fragments.len()
        );
        assert_eq!(receiver.held_bytes(), 0);
        // The returned packet survives disposal/reset of its former assembler.
        receiver.reset();
        drop(receiver);
        assert_eq!(probe.used(), allocation_bytes(564));
        assert_eq!(probe.refused(), 0);
        assert!(probe.peak() <= heap_peak(RP_LIMITS));
        drop(received);
        assert_eq!(probe.used(), 0);
    }
}

#[test]
fn packet_564_is_accepted_but_565_is_refused_before_any_overflow_copy() {
    let lone_564 = frame(FRAGMENT_TYPE_LONE, 0, 1, &[0x31; 564]);
    let lone_565 = frame(FRAGMENT_TYPE_LONE, 0, 1, &[0x32; 565]);
    let fragments_565 = fragment_packet(&[0x33; 565], 517);
    let probe = Probe::new(heap_peak(RP_LIMITS));
    let mut receiver = BleDefragmenter::new(RP_LIMITS);
    let DefragResult::Complete(packet) = receiver.process(&lone_564, 0) else {
        panic!("the exact packet boundary is accepted");
    };
    assert_eq!(packet.as_slice(), &[0x31; 564]);
    assert_eq!(packet.capacity(), 564);
    drop(packet);
    probe.limit(0);
    let before = probe.attempts();
    assert_eq!(receiver.process(&lone_565, 1), DefragResult::Error);
    assert_eq!(
        probe.attempts(),
        before,
        "oversized LONE never reaches the allocator"
    );
    assert_eq!(probe.used(), 0);

    probe.limit(heap_peak(RP_LIMITS));
    assert_eq!(
        receiver.process(&fragments_565[0], 2),
        DefragResult::NeedMore
    );
    probe.limit(probe.used());
    let before = probe.attempts();
    assert_eq!(
        receiver.process_or_abandon(&fragments_565[1], 3),
        DefragResult::Error
    );
    assert_eq!(
        probe.attempts(),
        before,
        "aggregate overflow is checked before copying"
    );
    assert_eq!(receiver.abandoned_count(), 1);
    assert_eq!(receiver.held_bytes(), 0);
    assert_eq!(probe.used(), 0);
    assert_eq!(
        probe.refused(),
        0,
        "rejection is policy, not an allocation failure"
    );
}

#[test]
fn fragment_38_completes_but_counts_39_and_65535_allocate_nothing() {
    let expected: [u8; 38] = std::array::from_fn(|index| index as u8);
    let probe = Probe::new(0);
    let mut receiver = BleDefragmenter::new(RP_LIMITS);
    for total in [39, u16::MAX] {
        let header = build_fragment_header(0, total as usize);
        assert_eq!(receiver.process(&header, 0), DefragResult::Error);
    }
    assert_eq!(probe.attempts(), 0);
    probe.limit(heap_peak(RP_LIMITS));
    for index in 0..38 {
        let header = build_fragment_header(index, 38);
        let mut fragment = [0; 6];
        fragment[..5].copy_from_slice(&header);
        fragment[5] = index as u8;
        match receiver.process(&fragment, index as u64) {
            DefragResult::Complete(packet) => {
                assert_eq!(index, 37);
                assert_eq!(packet.as_slice(), expected.as_slice());
                assert_eq!(packet.capacity(), 38);
            }
            DefragResult::NeedMore => assert!(index < 37),
            DefragResult::Error => panic!("the supported count must remain valid"),
        }
    }
    assert_eq!(receiver.last_completed_fragments(), 38);
    assert_eq!(receiver.held_bytes(), 0);
    assert_eq!(probe.used(), 0);
    assert_eq!(probe.refused(), 0);
}

#[test]
fn duplicate_replacement_subtracts_the_old_bytes_and_funds_the_copy_overlap() {
    let start = frame(FRAGMENT_TYPE_START, 0, 3, &[0x11; 248]);
    let old = frame(FRAGMENT_TYPE_CONTINUE, 1, 3, &[0x22; 200]);
    let replacement = frame(FRAGMENT_TYPE_CONTINUE, 1, 3, &[0x33; 316]);
    let over = frame(FRAGMENT_TYPE_CONTINUE, 1, 3, &[0x44; 317]);
    let end = frame(FRAGMENT_TYPE_END, 2, 3, &[]);
    let probe = Probe::new(heap_peak(RP_LIMITS));
    let mut receiver = BleDefragmenter::new(RP_LIMITS);
    assert_eq!(receiver.process(&start, 0), DefragResult::NeedMore);
    assert_eq!(receiver.process(&old, 1), DefragResult::NeedMore);
    let old_live = probe.used();
    assert_eq!(receiver.process(&replacement, 2), DefragResult::NeedMore);
    assert_eq!(
        receiver.pending_fragments(),
        2,
        "a duplicate consumes no new slot"
    );
    assert!(
        probe.peak() >= old_live + allocation_bytes(316),
        "old and new copies really coexist"
    );
    assert_eq!(
        probe.used(),
        old_live - allocation_bytes(200) + allocation_bytes(316)
    );
    probe.limit(probe.used());
    let before = probe.attempts();
    assert_eq!(receiver.process(&over, 3), DefragResult::Error);
    assert_eq!(probe.attempts(), before);
    probe.limit(heap_peak(RP_LIMITS));
    let DefragResult::Complete(packet) = receiver.process(&end, 4) else {
        panic!("refusing an oversized duplicate must preserve the previous copy");
    };
    assert_eq!(&packet[..248], &[0x11; 248]);
    assert_eq!(&packet[248..], &[0x33; 316]);
    assert_eq!(packet.capacity(), 564);
    assert_eq!(receiver.abandoned_count(), 0);
    assert_eq!(receiver.held_bytes(), 0);
    assert_eq!(probe.refused(), 0);
    drop(packet);
    assert_eq!(probe.used(), 0);
}

#[test]
fn a_shorter_duplicate_releases_its_old_payload_before_the_next_fragment() {
    let start = frame(FRAGMENT_TYPE_START, 0, 3, b"ab");
    let old = frame(FRAGMENT_TYPE_CONTINUE, 1, 3, b"cdef");
    let replacement = frame(FRAGMENT_TYPE_CONTINUE, 1, 3, b"z");
    let end = frame(FRAGMENT_TYPE_END, 2, 3, b"uvw");
    let limits = BleReassemblyLimits::new(6, 3);
    let probe = Probe::new(heap_peak(limits));
    let mut receiver = BleDefragmenter::new(limits);
    for fragment in [&start, &old, &replacement] {
        assert_eq!(receiver.process(fragment, 0), DefragResult::NeedMore);
    }
    let DefragResult::Complete(packet) = receiver.process(&end, 0) else {
        panic!("replacement frees three bytes of the packet allowance");
    };
    assert_eq!(packet.as_slice(), b"abzuvw");
    assert_eq!(packet.capacity(), 6);
    assert_eq!(probe.refused(), 0);
    drop(packet);
    assert_eq!(probe.used(), 0);
}

#[test]
fn reset_abandon_timeout_and_canceled_futures_release_every_owned_block() {
    let fragments = fragment_packet(&[0x71; 564], 23);
    let probe = Probe::new(heap_peak(RP_LIMITS));
    let mut receiver = BleDefragmenter::new(RP_LIMITS);
    assert_eq!(receiver.process(&fragments[0], 10), DefragResult::NeedMore);
    assert!(receiver.is_timed_out(10 + REASSEMBLY_TIMEOUT_MS));
    receiver.reset();
    assert_eq!(
        receiver.abandoned_count(),
        0,
        "timeout reporting belongs to the caller"
    );
    assert_eq!(receiver.held_bytes(), 0);
    assert_eq!(probe.used(), 0);
    assert_eq!(receiver.process(&fragments[0], 20), DefragResult::NeedMore);
    receiver.abandon();
    receiver.abandon();
    assert_eq!(receiver.abandoned_count(), 1);
    assert_eq!(probe.used(), 0);
    drop(receiver);
    {
        let receive = async {
            let mut receiver = BleDefragmenter::new(RP_LIMITS);
            assert_eq!(receiver.process(&fragments[0], 0), DefragResult::NeedMore);
            assert_eq!(receiver.process(&fragments[1], 1), DefragResult::NeedMore);
            pending::<()>().await;
            drop(receiver);
        };
        let mut receive = pin!(receive);
        assert!(receive
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending());
        assert!(
            probe.used() > 0,
            "the canceled future really owns a partial packet"
        );
    }
    assert_eq!(probe.used(), 0);
    assert_eq!(probe.refused(), 0);
}

#[test]
fn allocation_refusal_at_spine_payload_and_assembly_is_fallible_and_releases_on_abandon() {
    let fragments = fragment_packet(&[0x81; 500], 185);
    let probe = Probe::new(0);
    let mut receiver = BleDefragmenter::new(RP_LIMITS);
    assert_eq!(
        receiver.process_or_abandon(&fragments[0], 0),
        DefragResult::Error
    );
    assert_eq!(
        probe.attempts(),
        1,
        "the legal spine was offered to the allocator"
    );
    assert_eq!(probe.refused(), 1);
    assert_eq!(probe.used(), 0);

    let spine = allocation_bytes(3 * size_of::<Option<Box<[u8]>>>());
    probe.limit(spine);
    assert_eq!(
        receiver.process_or_abandon(&fragments[0], 1),
        DefragResult::Error
    );
    assert_eq!(
        probe.refused(),
        2,
        "the payload, not the spine, was refused"
    );
    assert_eq!(receiver.held_bytes(), 0);
    assert_eq!(probe.used(), 0);

    probe.limit(heap_peak(RP_LIMITS));
    assert_eq!(receiver.process(&fragments[0], 2), DefragResult::NeedMore);
    assert_eq!(receiver.process(&fragments[1], 3), DefragResult::NeedMore);
    // Enough for the last fragment, but not for its independent exact packet.
    probe.limit(probe.used() + allocation_bytes(fragments[2].len() - 5));
    assert_eq!(
        receiver.process_or_abandon(&fragments[2], 4),
        DefragResult::Error
    );
    assert_eq!(probe.refused(), 3);
    assert_eq!(receiver.abandoned_count(), 1);
    assert_eq!(receiver.held_bytes(), 0);
    assert_eq!(probe.used(), 0);
}

#[test]
fn three_valid_253_byte_gatt_values_no_longer_admit_a_744_byte_packet() {
    let fragments = [
        frame(FRAGMENT_TYPE_START, 0, 3, &[0x91; 248]),
        frame(FRAGMENT_TYPE_CONTINUE, 1, 3, &[0x92; 248]),
        frame(FRAGMENT_TYPE_END, 2, 3, &[0x93; 248]),
    ];
    let (peak, attempts_before_overflow) = {
        let probe = Probe::new(heap_peak(RP_LIMITS));
        let mut receiver = BleDefragmenter::new(RP_LIMITS);
        for fragment in &fragments[..2] {
            assert_eq!(fragment.len(), 253);
            assert_eq!(
                receiver.process_or_abandon(fragment, 0),
                DefragResult::NeedMore
            );
        }
        probe.limit(probe.used());
        let before = probe.attempts();
        assert_eq!(
            receiver.process_or_abandon(&fragments[2], 1),
            DefragResult::Error
        );
        assert_eq!(
            probe.attempts(),
            before,
            "the third 248-byte payload was never copied"
        );
        assert_eq!(receiver.abandoned_count(), 1);
        assert_eq!(receiver.held_bytes(), 0);
        assert_eq!(probe.used(), 0);
        assert_eq!(probe.refused(), 0);
        (probe.peak(), before)
    };
    println!("BLE_BOUNDARY limit=564 count_limit=38 offered=744 overflow_allocations=0 held_after=0 peak={peak} valid_allocations={attempts_before_overflow}");

    // Positive control: the exact same production receiver and fragments still
    // work when a different carrier explicitly funds that larger packet size.
    let larger = BleReassemblyLimits::new(744, 50);
    let probe = Probe::new(heap_peak(larger));
    let mut receiver = BleDefragmenter::new(larger);
    for fragment in &fragments[..2] {
        assert_eq!(receiver.process(fragment, 0), DefragResult::NeedMore);
    }
    let DefragResult::Complete(packet) = receiver.process(&fragments[2], 1) else {
        panic!("the positive control really delivers the whole train");
    };
    assert_eq!(&packet[..248], &[0x91; 248]);
    assert_eq!(&packet[248..496], &[0x92; 248]);
    assert_eq!(&packet[496..], &[0x93; 248]);
    assert_eq!(packet.capacity(), 744);
    assert_eq!(probe.refused(), 0);
    drop(packet);
    assert_eq!(probe.used(), 0);
}

#[test]
fn a_generic_larger_supported_packet_profile_is_not_silently_shrunk_to_rp_limits() {
    let packet: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let fragments = fragment_packet(&packet, 517);
    let limits = BleReassemblyLimits::new(4096, 274);
    let probe = Probe::new(heap_peak(limits));
    let mut receiver = BleDefragmenter::new(limits);
    for fragment in &fragments[..fragments.len() - 1] {
        assert_eq!(receiver.process(fragment, 0), DefragResult::NeedMore);
    }
    let DefragResult::Complete(received) = receiver.process(fragments.last().unwrap(), 1) else {
        panic!("an explicitly funded larger carrier retains its own packet MTU");
    };
    assert_eq!(received.as_slice(), packet.as_slice());
    assert_eq!(received.capacity(), packet.len());
    assert!(probe.peak() <= heap_peak(limits));
    assert_eq!(probe.refused(), 0);
    drop(received);
    assert_eq!(probe.used(), 0);
}

#[test]
fn the_maximum_packet_fragment_budget_funds_every_shorter_packet_at_every_rp_att_width() {
    let packet = [0xA1; 564];
    for mtu in 23..=256 {
        // The layout helper funds actual fragment_packet allocations, not an
        // alternate encoder. This test measures all the allocated vectors.
        let count = leviculum_core::framing::ble::fragment_count(packet.len(), mtu);
        let mut budget = allocation_bytes(count * size_of::<Vec<u8>>());
        for index in 0..count {
            budget += allocation_bytes(
                5 + leviculum_core::framing::ble::fragment_payload_len(packet.len(), index, mtu),
            );
        }
        let probe = Probe::new(budget);
        for len in 0..=packet.len() {
            let fragments = fragment_packet(&packet[..len], mtu);
            assert!(probe.used() <= budget);
            assert!(fragments.iter().all(|fragment| fragment.len() <= mtu - 3));
            drop(fragments);
            assert_eq!(probe.used(), 0);
        }
        assert_eq!(
            probe.peak(),
            budget,
            "the full-size positive control reaches the funded peak"
        );
        assert_eq!(probe.refused(), 0);
    }
}

#[test]
fn the_full_spine_and_adversarial_payload_distribution_reach_the_funded_heap_peak() {
    // The generic codec permits unequal fragment widths. Physical adapters
    // additionally enforce their GATT value width; fund even this generic
    // worst distribution rather than assuming that every slot is fifteen bytes.
    let mut fragments: Vec<Vec<u8>> = (0..37)
        .map(|index| {
            let kind = if index == 0 {
                FRAGMENT_TYPE_START
            } else {
                FRAGMENT_TYPE_CONTINUE
            };
            frame(kind, index, 38, &[index as u8])
        })
        .collect();
    fragments.push(frame(FRAGMENT_TYPE_END, 37, 38, &[0xB1; 527]));
    let mut expected = [0xB1; 564];
    for (index, byte) in expected[..37].iter_mut().enumerate() {
        *byte = index as u8;
    }
    let probe = Probe::new(heap_peak(RP_LIMITS));
    let mut receiver = BleDefragmenter::new(RP_LIMITS);
    for fragment in &fragments[..37] {
        assert_eq!(receiver.process(fragment, 0), DefragResult::NeedMore);
    }
    let DefragResult::Complete(packet) = receiver.process(&fragments[37], 1) else {
        panic!("the complete finite allocation profile must be fundable");
    };
    assert_eq!(packet.as_slice(), expected.as_slice());
    assert_eq!(packet.capacity(), 564);
    assert_eq!(
        probe.peak(),
        heap_peak(RP_LIMITS),
        "the actual allocator reaches the bound"
    );
    assert_eq!(probe.refused(), 0);
    assert_eq!(receiver.held_bytes(), 0);
    drop(packet);
    assert_eq!(probe.used(), 0);
}
