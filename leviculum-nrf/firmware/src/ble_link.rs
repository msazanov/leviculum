//! The BLE link's node side (step 4 of
//! `docs/src/concepts/firmware-layers.md`, first slice, 998).
//!
//! What lives here is what the measurement of step 4's call surface found
//! to be the orchestration's half of the seam that is **not** a message:
//!
//! * **the carrier gate** — the BLE on/off the media profile pushes
//!   ([`set_active`]) and every backend task reads back ([`carrier`],
//!   [`carrier_on`], [`carrier_off`]). Until this slice the backend read
//!   the media profile upward (`media::ble_active`) on every gate step;
//!   now the value is pushed down on the three transitions the profile
//!   has (boot declare, boot narrowing, a host apply) and the latches
//!   live beside it.
//! * **the pacing value** — the operator's inter-packet-gap override
//!   ([`set_tx_gap_ms`]), pushed by the control task and read by the
//!   backend's pumps at each packet.
//! * **the link table** — the per-connection HVN drain table
//!   ([`HVN_DRAIN`]), whose claim index is the per-link identity
//!   everything else is keyed by; its `claimed()` is the "live links"
//!   fact the status page and the control report read.
//! * **the heap-census facts** — the per-link and per-channel held-bytes
//!   mirrors the backend publishes and the census line reads
//!   ([`session_census`], [`defrag_held_bytes`]).
//!
//! The backend below (`leviculum-nrf`'s `ble/`: the SoftDevice bring-up,
//! advertising, scanning, GATT, the notify pump, the defragmenters)
//! publishes into and reads from this module by direct call — per
//! fragment on the hot path, which is why these are values and not
//! channel messages (the per-packet decision of 998: every operation
//! here is one atomic access, and the packets themselves already ride
//! the node-facing channels). What is still to move in step 4's
//! remaining slices: those channels, the peer events, the fan-out and
//! the `Interface` implementation ([`crate::seams::BleLinkEvent`]).

use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use leviculum_ble_tx::{CarrierPath, CarrierState, CarrierStep, StandDown};
use leviculum_heap_budget::MAX_LINKS;

/// Whether BLE is carrying Reticulum traffic right now, as the media
/// profile last pushed it ([`set_active`]). The boot state is `off`:
/// nothing may advertise before the profile has been read and pushed.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Push the media profile's BLE verdict and wake every BLE protocol
/// task to re-read it.
///
/// Called by the media profile wherever its state changes: at the boot
/// declare, at the boot narrowing, and on a host apply — after the new
/// state is already in force, so a woken task cannot read the old one.
/// Before the tasks exist — or on a boot that never spawned them — this
/// just latches the wakes, which is the required no-op: with `ble=off`
/// at boot no link, no advertisement and no scan exist to stop.
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
    for wake in &CARRIER_WAKES {
        wake.signal(());
    }
}

/// The BLE carrier as the media profile last pushed it. Every backend
/// path that reads the carrier asks this for its verdict
/// ([`leviculum_ble_tx::CarrierState::step`]), so all of them agree when
/// the operator switches it off.
pub fn carrier() -> CarrierState {
    CarrierState::from_active(ACTIVE.load(Ordering::Relaxed))
}

/// The carrier-wake latches, one per waiter (see [`CarrierWaiter`]).
///
/// A runtime `ble=off` used to be a mute; off means off on the air: each
/// protocol task holds one of these latches, [`set_active`] pokes them
/// all, and the woken task re-reads [`carrier`] — a live session
/// disconnects its link (through the same teardown a peer walking out
/// of range takes, so the `Lost` report and the cull are identical),
/// and the advertise/scan futures are dropped and not re-entered until
/// the carrier reads on again.
///
/// One latch per waiter, not one shared: `Signal::wait` consumes the
/// latch, so a shared one would wake whichever task polled first and
/// starve the others. Within a waiter only one of [`carrier_on`] /
/// [`carrier_off`] is ever awaited at a time, and both re-check the
/// carrier after every wake, so a stale latched wake (a LoRa-only
/// profile change, a flip-and-back while the task was busy) is a no-op.
///
/// Sized without the backend's task counts: the advertise/scan waiters
/// are at most one per connection the SoftDevice grants plus the one
/// central scanner, which the backend bounds by `MAX_LINKS` (its
/// `CONN_COUNT <= MAX_LINKS` assert), and the sessions are one per
/// drain slot.
static CARRIER_WAKES: [Signal<CriticalSectionRawMutex, ()>; MAX_LINKS + MAX_LINKS] =
    [const { Signal::new() }; MAX_LINKS + MAX_LINKS];

/// A backend task's handle on its carrier-wake latch (see
/// `CARRIER_WAKES`).
#[derive(Clone, Copy)]
pub enum CarrierWaiter {
    /// Peripheral advertise/accept task `i` — fewer than `MAX_LINKS - 1`
    /// of them, which is the backend's own connection-count bound.
    Peripheral(usize),
    /// The central scan/initiate task.
    Central,
    /// A live session, keyed by its drain-table slot.
    Session(usize),
}

impl CarrierWaiter {
    /// The path this waiter's [`carrier_off`] stands down.
    fn path(self) -> CarrierPath {
        match self {
            CarrierWaiter::Peripheral(_) => CarrierPath::Advertising,
            CarrierWaiter::Central => CarrierPath::Scanning,
            CarrierWaiter::Session(_) => CarrierPath::Session,
        }
    }

    fn index(self) -> usize {
        match self {
            CarrierWaiter::Peripheral(i) => i,
            CarrierWaiter::Central => MAX_LINKS - 1,
            CarrierWaiter::Session(slot) => MAX_LINKS + slot,
        }
    }
}

/// Resolve when the BLE carrier reads off, with how the waiter's path
/// stands down. Selected against a live session or an advertise/scan
/// future; never resolves while the carrier stays on.
pub async fn carrier_off(waiter: CarrierWaiter) -> StandDown {
    let wake = &CARRIER_WAKES[waiter.index()];
    loop {
        if let CarrierStep::StandDown(down) = carrier().step(waiter.path()) {
            return down;
        }
        wake.wait().await;
    }
}

/// Resolve when the BLE carrier reads on — the gate at the top of each
/// backend task's loop, so a switched-off carrier neither advertises
/// nor scans nor accepts.
pub async fn carrier_on(waiter: CarrierWaiter) {
    let wake = &CARRIER_WAKES[waiter.index()];
    while !carrier().is_on() {
        wake.wait().await;
    }
}

/// The operator's inter-packet-gap override (#376, `TYPE_BLE_TX_GAP`),
/// in milliseconds. With no override — the boot state, and what every
/// reset restores — the pumps serve the compiled
/// [`leviculum_ble_tx::DEFAULT_TX_GAP_MS`] (100 ms, the measured desk
/// value; the justification lives on that constant). The knob stays a
/// measurement instrument: any set value overrides the default, `0`
/// disables the gap entirely, and nothing is persisted, exactly like
/// the LoRa transmit spacing (#345): a bench must not be able to leave
/// a board silently mispaced after the session that paced it.
///
/// `u16::MAX` is the no-override sentinel; the control envelope refuses
/// anything above `BLE_TX_GAP_MAX_MS`, so no operator value can collide
/// with it (asserted below).
const TX_GAP_OVERRIDE_NONE: u16 = u16::MAX;
const _: () = assert!(leviculum_core::envelope::BLE_TX_GAP_MAX_MS < TX_GAP_OVERRIDE_NONE);
static TX_GAP_OVERRIDE_MS: AtomicU16 = AtomicU16::new(TX_GAP_OVERRIDE_NONE);

/// Set the inter-packet gap override. Called from the serial control
/// task; the backend's per-connection pumps read it at each packet, so
/// it takes effect from the next packet on every live link without
/// touching the links.
pub fn set_tx_gap_ms(gap_ms: u16) {
    TX_GAP_OVERRIDE_MS.store(gap_ms, Ordering::Relaxed);
    crate::log::log_fmt("[BLE ] ", format_args!("tx_gap_ms={}", gap_ms));
}

/// The gap the pumps serve right now: the override if one was set this
/// boot, [`leviculum_ble_tx::DEFAULT_TX_GAP_MS`] otherwise.
pub fn tx_gap_ms() -> u16 {
    let raw = TX_GAP_OVERRIDE_MS.load(Ordering::Relaxed);
    leviculum_ble_tx::effective_tx_gap_ms((raw != TX_GAP_OVERRIDE_NONE).then_some(raw))
}

/// Where `BLE_GATTS_EVT_HVN_TX_COMPLETE` goes — and the link table.
///
/// The HVN queue is per connection, so its drain edge is too. Routing it
/// by connection handle is what keeps link A's drain out of link B's
/// fragment pump; see [`leviculum_ble_tx::drain`] for the failure mode
/// and its host tests. A connection claims its slot for its whole
/// lifetime, so the drain table is also the link table as the node sees
/// it — `claimed()` is the live-link count the status page and the
/// control report read — and a second registry that could disagree with
/// it is never built.
pub static HVN_DRAIN: leviculum_ble_tx::DrainRouter<MAX_LINKS> =
    leviculum_ble_tx::DrainRouter::new();

/// Last-observed heap bytes held by each link's defragmenter (#388
/// census). Published by the backend's two inbound paths after every
/// fragment (the defragmenter itself lives in the session task's local
/// state, out of the census walker's reach), zeroed when the session's
/// slot is torn down. Indexed by the drain-slot, the per-link identity
/// both roles already use.
static DEFRAG_HELD: [core::sync::atomic::AtomicUsize; MAX_LINKS] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; MAX_LINKS];

/// Sum of the per-link defragmenter holdings, for the heap census.
pub fn defrag_held_bytes() -> usize {
    DEFRAG_HELD
        .iter()
        .map(|slot| slot.load(Ordering::Relaxed))
        .sum()
}

/// Publish one link's current defragmenter holding (0 on teardown).
pub fn defrag_held_set(slot_index: usize, bytes: usize) {
    if let Some(slot) = DEFRAG_HELD.get(slot_index) {
        slot.store(bytes, Ordering::Relaxed);
    }
}

/// Heap bytes each link's session currently holds (#388 census): the
/// packets queued on its private outbound queue plus the packet its
/// pump is fragmenting right now. The backend adds when it queues on a
/// link and subtracts when a packet has left or died; reset at claim
/// (after the stale-tenancy drain) and at teardown, so a decrement lost
/// to a cancelled pump future cannot linger. The mechanics and their
/// host tests live in [`leviculum_ble_tx::session_census`].
static SESSION_HELD: leviculum_ble_tx::SessionHeld<MAX_LINKS> =
    leviculum_ble_tx::SessionHeld::new();

/// Heap bytes of packets handed over by the core, not yet picked up by
/// the backend's fan-out (#388 census).
static OUTGOING_HELD: leviculum_ble_tx::HeldBytes = leviculum_ble_tx::HeldBytes::new();

/// Heap bytes of packets reassembled by a session, not yet consumed by
/// the main loop, which subtracts on receive via [`incoming_held_sub`]
/// (#388 census).
static INCOMING_HELD: leviculum_ble_tx::HeldBytes = leviculum_ble_tx::HeldBytes::new();

/// The BLE sessions' census slice (#388): per-drain-slot held bytes and
/// the two node-facing channels' holdings. The census line prints the
/// per-slot values as `ble_s0..ble_s3` and their sum plus the channels
/// as `ble_sessions=`.
pub fn session_census() -> ([usize; MAX_LINKS], usize) {
    let mut slots = [0usize; MAX_LINKS];
    for (index, value) in slots.iter_mut().enumerate() {
        *value = SESSION_HELD.get(index);
    }
    (slots, OUTGOING_HELD.get() + INCOMING_HELD.get())
}

/// The backend queued one packet of `bytes` on a link's outbound queue.
pub fn session_held_add(slot_index: usize, bytes: usize) {
    SESSION_HELD.add(slot_index, bytes);
}

/// A session pump finished with (or abandoned) a packet of `bytes`.
pub fn session_held_sub(slot_index: usize, bytes: usize) {
    SESSION_HELD.sub(slot_index, bytes);
}

/// The slot's queue custody is known empty: claim (post-drain) and
/// teardown.
pub fn session_held_reset(slot_index: usize) {
    SESSION_HELD.reset(slot_index);
}

/// A displacement moved one queued packet from `old_slot` to `new_slot`
/// (#376): move its bytes with it.
pub fn session_held_moved(old_slot: usize, new_slot: usize, bytes: usize) {
    SESSION_HELD.sub(old_slot, bytes);
    SESSION_HELD.add(new_slot, bytes);
}

/// One outbound packet of `bytes` entered the node-facing channel's
/// custody.
pub fn outgoing_held_add(bytes: usize) {
    OUTGOING_HELD.add(bytes);
}

/// The backend's fan-out picked one outbound packet of `bytes` up.
pub fn outgoing_held_sub(bytes: usize) {
    OUTGOING_HELD.sub(bytes);
}

/// A session queued one reassembled packet of `bytes` toward the main
/// loop.
pub fn incoming_held_add(bytes: usize) {
    INCOMING_HELD.add(bytes);
}

/// The main loop consumed one inbound packet of `bytes` — the receive
/// side of [`incoming_held_add`], called by the binaries.
pub fn incoming_held_sub(bytes: usize) {
    INCOMING_HELD.sub(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use embassy_futures::poll_once;
    use leviculum_ble_tx::DEFAULT_TX_GAP_MS;

    // Each test owns one static family outright — the statics are shared
    // across the whole test binary and cargo runs tests in parallel.

    /// The carrier starts off, follows every push, and the two waits
    /// resolve exactly on their own edge: `carrier_on` while off stays
    /// pending until the on-push, `carrier_off` while on stays pending
    /// until the off-push and answers with the waiter's own stand-down.
    #[test]
    fn carrier_pushes_reach_a_waiter() {
        assert!(!carrier().is_on());

        let mut on = core::pin::pin!(carrier_on(CarrierWaiter::Central));
        assert!(poll_once(on.as_mut()).is_pending());
        set_active(true);
        assert!(poll_once(on.as_mut()).is_ready());
        assert!(carrier().is_on());

        let mut off = core::pin::pin!(carrier_off(CarrierWaiter::Session(0)));
        assert!(poll_once(off.as_mut()).is_pending());
        set_active(false);
        assert_eq!(
            poll_once(off.as_mut()),
            core::task::Poll::Ready(StandDown::EndSession)
        );
        assert!(!carrier().is_on());
    }

    /// The latch layout keeps the three waiter kinds apart: peripherals
    /// below the central scanner, sessions above both, within the table.
    #[test]
    fn waiter_indices_do_not_collide() {
        // The backend's own bound: its peripheral count stays below the
        // central scanner's slot.
        for i in 0..MAX_LINKS - 1 {
            assert_ne!(
                CarrierWaiter::Peripheral(i).index(),
                CarrierWaiter::Central.index()
            );
        }
        for slot in 0..MAX_LINKS {
            let session = CarrierWaiter::Session(slot).index();
            assert!(session >= MAX_LINKS);
            assert!(session < CARRIER_WAKES.len());
        }
    }

    /// The pacing value serves the compiled default until a push, any
    /// pushed value afterwards (zero included), and the sentinel push
    /// restores the default.
    #[test]
    fn tx_gap_override_and_sentinel() {
        assert_eq!(tx_gap_ms(), DEFAULT_TX_GAP_MS);
        set_tx_gap_ms(20);
        assert_eq!(tx_gap_ms(), 20);
        set_tx_gap_ms(0);
        assert_eq!(tx_gap_ms(), 0);
        set_tx_gap_ms(TX_GAP_OVERRIDE_NONE);
        assert_eq!(tx_gap_ms(), DEFAULT_TX_GAP_MS);
    }

    /// The census facts add up the way the backend publishes them: the
    /// per-slot session custody follows adds, moves and resets, the
    /// defragmenter holdings sum per slot, and the two channel holdings
    /// land in the census total.
    #[test]
    fn census_facts_follow_the_publishes() {
        session_held_add(0, 100);
        session_held_add(1, 50);
        session_held_moved(0, 1, 100);
        let (slots, _) = session_census();
        assert_eq!(slots[0], 0);
        assert_eq!(slots[1], 150);
        session_held_sub(1, 50);
        session_held_reset(1);
        let (slots, _) = session_census();
        assert_eq!(slots[1], 0);

        defrag_held_set(0, 30);
        defrag_held_set(1, 12);
        assert_eq!(defrag_held_bytes(), 42);
        defrag_held_set(0, 0);
        defrag_held_set(1, 0);
        assert_eq!(defrag_held_bytes(), 0);

        outgoing_held_add(7);
        incoming_held_add(5);
        assert_eq!(session_census().1, 12);
        outgoing_held_sub(7);
        incoming_held_sub(5);
        assert_eq!(session_census().1, 0);
    }
}
