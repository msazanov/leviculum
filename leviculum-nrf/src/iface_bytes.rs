//! Per-interface byte counters (Codeberg #235, #346).
//!
//! Until this module the board had no idea how many bytes any of its three
//! carriers had moved: `transport_stats.rs` counts packets and drop reasons,
//! and its header said in so many words that byte counters did not exist. A
//! `/status` answer needs them — `rnstatus` reads `interfaces[].rxb`/`txb`
//! unguarded (`reference/Reticulum/RNS/Utilities/rnstatus.py:575-576`), and a
//! mast node queried over the air is queried precisely to learn what it has
//! carried.
//!
//! # Where the bytes are counted
//!
//! The reference's semantics, not the radio's: Python counts the *unframed
//! packet* length, on receive where the interface hands the packet up
//! (`RNodeInterface.process_incoming`, `RNodeInterface.py:702`) and on
//! transmit where the interface accepts it for the medium
//! (`RNodeInterface.process_outgoing`, `RNodeInterface.py:725` — `datalen`,
//! the unescaped payload, counted around the serial write). So:
//!
//! * **tx** is counted in each `Interface::try_send` impl, on the success
//!   path only. A packet the media profile drops with the carrier off, or one
//!   the outbound queue refuses, never entered the medium and is not counted.
//!   The serial interface counts later, in the transport writer once the
//!   frame reached the host (`leviculum_usb_policy::tx_counted`): its queue
//!   takes packets a host that closes the port never reads.
//! * **rx** is counted where the medium's task hands the packet to the main
//!   loop's channel, which is this firmware's `process_incoming`.
//!
//! KISS/split framing overhead is therefore outside the count, exactly as it
//! is in the reference. The numbers here are what feeds both the periodic
//! `[TRANSPORT]` lines and the `/status` bundle, so the board's own
//! instrument and its remote answer cannot disagree.
//!
//! # What lives here and what in core
//!
//! The arithmetic (saturating totals, the speed window and its floor) is
//! [`leviculum_core::interface_counters`], shared with every carrier owner.
//! This module keeps what is the board's: the interface table, the
//! synchronisation, and the one sampler call per `[TRANSPORT]` emission.
//!
//! # Why a critical-section cell and not an atomic
//!
//! The counters are `u64` and are written from the medium tasks and read from
//! the main loop. `AtomicU64` does not exist on `thumbv7em-none-eabihf`
//! (32-bit max atomic width), and a pair of `AtomicU32` halves cannot be read
//! consistently. `embassy_sync`'s blocking mutex over a `Cell` is the
//! allocation-free option that is already linked: the update is a handful of
//! instructions inside a critical section, on a path that runs once per
//! packet.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use leviculum_core::interface_counters::{InterfaceCounters, InterfaceTraffic};

/// Interface index of the USB CDC serial interface (`InterfaceId(0)`).
pub const SERIAL: usize = 0;
/// Interface index of the LoRa interface (`InterfaceId(1)`).
pub const LORA: usize = 1;
/// Interface index of the BLE interface (`InterfaceId(2)`).
pub const BLE: usize = 2;
/// Number of interfaces every firmware binary registers.
pub const IFACE_COUNT: usize = 3;

/// The name each interface reports, indexed by interface id.
///
/// The one table: the `Interface::name()` impls read it, the `[TRANSPORT]`
/// lines print it, and the `/status` bundle answers with it. A remote
/// `rnstatus -R` and a debug capture therefore name the same carrier the same
/// way, which is the whole point of feeding both from one set of counters.
pub const NAMES: [&str; IFACE_COUNT] = ["serial_usb", "lora_sx1262", "ble"];

/// Register every interface's ingress-control flag with the node, from
/// [`leviculum_heap_budget::BOARD_INGRESS_CONTROL`]: off on [`SERIAL`], on
/// for [`LORA`] and [`BLE`] (Codeberg #606, the reasons at the table). The
/// held-announce cap is budgeted for the interfaces this leaves on, so the
/// binaries take the flags from here and not from a line of their own.
pub fn apply_board_ingress_control<R, C, S>(node: &mut leviculum_core::node::NodeCore<R, C, S>)
where
    R: rand_core::CryptoRngCore,
    C: leviculum_core::traits::Clock,
    S: leviculum_core::traits::Storage,
{
    for (iface, enabled) in leviculum_heap_budget::BOARD_INGRESS_CONTROL
        .iter()
        .enumerate()
    {
        node.set_interface_ingress_control(iface, *enabled);
    }
}

static COUNTERS: [Mutex<CriticalSectionRawMutex, Cell<InterfaceCounters>>; IFACE_COUNT] =
    [const { Mutex::new(Cell::new(InterfaceCounters::new())) }; IFACE_COUNT];

/// Read live totals and previous sampled speeds without changing the window.
pub fn snapshot(iface: usize) -> InterfaceTraffic {
    COUNTERS
        .get(iface)
        .map_or_else(InterfaceTraffic::default, |counter| {
            counter.lock(|cell| cell.get().snapshot())
        })
}

fn update(iface: usize, f: impl FnOnce(&mut InterfaceCounters)) {
    if let Some(counter) = COUNTERS.get(iface) {
        counter.lock(|cell| {
            let mut value = cell.get();
            f(&mut value);
            cell.set(value);
        });
    }
}

/// Count a completed receive ownership handoff.
pub fn note_rx(iface: usize, bytes: usize) {
    update(iface, |counter| counter.note_rx(bytes));
}

/// Count accepted unframed bytes, not framing, retries or media-off discards.
pub fn note_tx(iface: usize, bytes: usize) {
    update(iface, |counter| counter.note_tx(bytes));
}

/// Sample once before the periodic transport log.
pub fn sample(now_ms: u64) {
    for iface in 0..IFACE_COUNT {
        update(iface, |counter| counter.sample(now_ms));
    }
}
