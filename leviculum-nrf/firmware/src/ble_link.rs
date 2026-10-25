//! The BLE link's node side (step 4 of
//! `docs/src/concepts/firmware-layers.md`; the values 998, the messages
//! and the `Interface` implementation 999).
//!
//! The value half is what the measurement of step 4's call surface found
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
//! The message half (999) is [`crate::seams::BleLinkEvent`]: the peer
//! transitions and the completed inbound packets, which the backend
//! hands to [`deliver`] and the node loop reads from [`channels`]; the
//! outbound packets ride the same struct's sender, consumed by the
//! backend's fan-out ([`outgoing_receiver`]). The BLE [`Interface`]
//! implementation ([`BleInterface`]) lives beside the channels it feeds.
//!
//! The backend below (`leviculum-nrf`'s `ble/`: the SoftDevice bring-up,
//! advertising, scanning, GATT, the notify pump, the defragmenters)
//! publishes into and reads from this module by direct call — per
//! fragment on the hot path, which is why the value half is values and
//! not channel messages (the per-packet decision of 998: every operation
//! here is one atomic access, and the packets themselves already ride
//! the node-facing channels). What is still to move in step 4's
//! remaining slice: the identity-to-slot registry's split and the
//! fan-out plan (`trait BleLink` and its host fake).

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};

use crate::seams::BleLinkEvent;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_sync::signal::Signal;
use leviculum_ble_tx::{CarrierPath, CarrierState, CarrierStep, Enqueue, Fill, StandDown, TxAim};
use leviculum_core::traits::{Interface, InterfaceError};
use leviculum_core::InterfaceId;
use leviculum_heap_budget::{INTERFACE_MTU, MAX_LINKS, QUEUE_DEPTH};

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

/// Inbound events toward the main loop: completed packets, as
/// [`BleLinkEvent::Inbound`] and nothing else — [`deliver`] is the one
/// sender and routes by variant.
static BLE_INCOMING: Channel<CriticalSectionRawMutex, BleLinkEvent, QUEUE_DEPTH> = Channel::new();

/// Outbound packets with what the core said about their links: the peer
/// these bytes are FOR (the #376 delivery hint), the peer that already
/// HEARD them (the #422 ingress link of a broadcast), or neither. The
/// backend's fan-out ([`outgoing_receiver`]) turns the statement into
/// links; see [`leviculum_ble_tx::TxAim`].
static BLE_OUTGOING: Channel<CriticalSectionRawMutex, (TxAim, Vec<u8>), QUEUE_DEPTH> =
    Channel::new();

/// Peer-event reports toward the main loop (Codeberg #365): the
/// [`BleLinkEvent::PeerUp`]/[`BleLinkEvent::PeerLost`] variants and
/// nothing else, [`deliver`] routing by variant. `PeerUp` and `PeerLost`
/// share this ONE ordered channel on purpose: a link flap is a `Lost`
/// followed by an `Up`, and the cull must land before the pull decision
/// looks at the path table — on separate channels the `Up` could win the
/// race, see the still-standing direct entry, skip the pull, and then
/// have the `Lost` cull re-arm exactly the trap the pull exists to
/// clear. The depth is [`PEER_EVENT_DEPTH`].
static BLE_PEER_EVENTS: Channel<CriticalSectionRawMutex, BleLinkEvent, PEER_EVENT_DEPTH> =
    Channel::new();

/// Depth 8: a carrier-off teardown drops every live link at once, and
/// with the SoftDevice's four connections that is up to four `PeerLost`
/// reports in one burst before the main loop runs — twice that leaves
/// room for the relink `PeerUp`s that follow (#372).
const PEER_EVENT_DEPTH: usize = 8;

/// Route one backend report onto the channel the node loop reads it
/// from, answering whether it was queued.
///
/// A peer transition never blocks a connection task: with the
/// `PEER_EVENT_DEPTH`-deep queue full the oldest pending report wins
/// and this one is dropped ([`leviculum_ble_tx::peer_event_verdict`]) —
/// the affected node then falls back to the pre-#365 behaviour (a
/// dropped `PeerLost` ages the paths out via ordinary expiry, a dropped
/// `PeerUp` waits for the peer's periodic announce). An inbound packet
/// that finds its channel full is dropped too
/// ([`leviculum_ble_tx::incoming_verdict`]; the backend counts the drop
/// in its `rx_full=` counter off this answer); a queued one enters
/// census custody ([`incoming_held_add`] — the main loop subtracts on
/// receive) and the interface's byte count. A refused `try_send` after a
/// `Queue` verdict cannot happen on the one thread-mode executor every
/// producer runs on; were it to, it is the same drop.
pub fn deliver(event: BleLinkEvent) -> bool {
    match event {
        BleLinkEvent::Inbound { link, packet } => {
            let bytes = packet.capacity();
            let len = packet.len();
            let fill = Fill::new(BLE_INCOMING.len(), BLE_INCOMING.capacity());
            let queued = leviculum_ble_tx::incoming_verdict(fill) == Enqueue::Queue
                && BLE_INCOMING
                    .try_send(BleLinkEvent::Inbound { link, packet })
                    .is_ok();
            if queued {
                // #388 census: BLE_INCOMING custody until the main loop
                // subtracts on receive. #235: the byte counter takes the
                // packet, not the allocation — the reference counts
                // payload bytes.
                incoming_held_add(bytes);
                crate::iface_bytes::note_rx(crate::iface_bytes::BLE, len);
            }
            queued
        }
        BleLinkEvent::PeerUp { identity, .. } | BleLinkEvent::PeerLost { identity, .. } => {
            let kind = match event {
                BleLinkEvent::PeerUp { .. } => leviculum_ble_tx::PeerEventKind::Up,
                _ => leviculum_ble_tx::PeerEventKind::Lost,
            };
            let fill = Fill::new(BLE_PEER_EVENTS.len(), BLE_PEER_EVENTS.capacity());
            let queued = leviculum_ble_tx::peer_event_verdict(fill, kind) == Enqueue::Queue
                && BLE_PEER_EVENTS.try_send(event).is_ok();
            if !queued {
                let kind = match kind {
                    leviculum_ble_tx::PeerEventKind::Up => "up",
                    leviculum_ble_tx::PeerEventKind::Lost => "lost",
                };
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_PEER_EVENT_DROPPED kind={} peer={:02x}{:02x}{:02x}{:02x}",
                        kind, identity[0], identity[1], identity[2], identity[3]
                    ),
                );
            }
            queued
        }
    }
}

/// The node loop's ends of the three channels.
pub struct BleChannels {
    /// Completed inbound packets ([`BleLinkEvent::Inbound`] only).
    pub incoming_rx: Receiver<'static, CriticalSectionRawMutex, BleLinkEvent, QUEUE_DEPTH>,
    /// The outbound side carries the core's link statement alongside
    /// the bytes; see `BLE_OUTGOING`.
    pub outgoing_tx: Sender<'static, CriticalSectionRawMutex, (TxAim, Vec<u8>), QUEUE_DEPTH>,
    /// Peer transitions (Codeberg #365,
    /// [`BleLinkEvent::PeerUp`]/[`BleLinkEvent::PeerLost`] only); the
    /// main loop feeds `PeerLost` to `handle_interface_peer_lost` and
    /// `PeerUp` to `handle_interface_peer_up`. See `BLE_PEER_EVENTS`
    /// for why both ride one ordered channel.
    pub peer_event_rx: Receiver<'static, CriticalSectionRawMutex, BleLinkEvent, PEER_EVENT_DEPTH>,
}

/// The binaries' handle on the channels, taken once at boot.
pub fn channels() -> BleChannels {
    BleChannels {
        incoming_rx: BLE_INCOMING.receiver(),
        outgoing_tx: BLE_OUTGOING.sender(),
        peer_event_rx: BLE_PEER_EVENTS.receiver(),
    }
}

/// The backend's end of the outbound channel: its fan-out is the ONE
/// consumer, because a Reticulum interface is a broadcast domain — one
/// flooded packet must reach every live link, which two competing
/// receivers would round-robin instead.
pub fn outgoing_receiver(
) -> Receiver<'static, CriticalSectionRawMutex, (TxAim, Vec<u8>), QUEUE_DEPTH> {
    BLE_OUTGOING.receiver()
}

/// Distinct live peer identities behind the interface (Codeberg #365),
/// as the backend's registry last published it ([`set_live_peer_count`])
/// — the fact [`BleInterface::peer_count`] reads until the registry
/// split moves the link table itself up here (step 4's remaining
/// slice). Distinct identities, not links: during a displacement's
/// hand-over one peer briefly holds two links and is still one peer.
static LIVE_PEER_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Publish the registry's distinct-peer count. Called by the backend
/// under the same registry lock that mutated it, so the mirror never
/// lags a transition the main loop can already observe.
pub fn set_live_peer_count(count: usize) {
    LIVE_PEER_COUNT.store(count, Ordering::Relaxed);
}

/// The BLE [`Interface`]: what the node core hands an outbound packet
/// to. It feeds `BLE_OUTGOING`; the backend's fan-out places each
/// packet on the link of the peer the core addressed it to — or, for a
/// broadcast, on every live link (#376).
pub struct BleInterface {
    sender: Sender<'static, CriticalSectionRawMutex, (TxAim, Vec<u8>), QUEUE_DEPTH>,
    /// The carrier-off drop run — see the LoRa interface for why these
    /// are counted rather than logged one line per packet.
    drops: leviculum_media_state::DropRun,
}

impl BleInterface {
    pub fn new(
        sender: Sender<'static, CriticalSectionRawMutex, (TxAim, Vec<u8>), QUEUE_DEPTH>,
    ) -> Self {
        Self {
            sender,
            drops: leviculum_media_state::DropRun::new(),
        }
    }

    /// Distinct live peer identities behind this interface (Codeberg
    /// #365), for the main loop's `set_interface_peer_count` mirror —
    /// the sibling of the `is_online` mirror. Zero with the carrier
    /// off: links torn down by a media switch cannot be asked anything,
    /// whatever the registry still holds mid-teardown.
    pub fn peer_count(&self) -> usize {
        if carrier().is_on() {
            LIVE_PEER_COUNT.load(Ordering::Relaxed)
        } else {
            0
        }
    }
}

impl Interface for BleInterface {
    fn id(&self) -> InterfaceId {
        InterfaceId(2)
    }
    fn name(&self) -> &str {
        crate::iface_bytes::names()[crate::iface_bytes::BLE]
    }
    fn mtu(&self) -> usize {
        INTERFACE_MTU
    }
    fn is_online(&self) -> bool {
        // Online means "a frame handed to this interface can reach a
        // peer": the carrier must be on AND at least one link must be
        // live — with no link the fan-out drops every packet silently.
        // The main loop mirrors this into the core, where a path over
        // an offline interface is no path (Codeberg #365).
        carrier().is_on() && HVN_DRAIN.claimed() > 0
    }
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        self.queue(TxAim::Flood, data)
    }
    /// The #376 delivery hint, carried to the backend's fan-out: this
    /// interface holds several point-to-point links, so a routed packet
    /// belongs on the addressed peer's link alone. `None` keeps the
    /// broadcast fan-out.
    fn try_send_to_peer(
        &mut self,
        data: &[u8],
        peer: Option<&[u8; 16]>,
        _high_priority: bool,
    ) -> Result<(), InterfaceError> {
        self.queue(peer.map_or(TxAim::Flood, |peer| TxAim::Peer(*peer)), data)
    }
    /// The #422 ingress link: these bytes arrived on `peer`'s link, so
    /// every OTHER link of this interface is a relay target. Excluding
    /// the whole interface, which is what the core does for a
    /// single-link medium, is what kept a path request from crossing
    /// the board between the host's link and the neighbour board's.
    fn try_send_excluding_peer(
        &mut self,
        data: &[u8],
        peer: &[u8; 16],
        _high_priority: bool,
    ) -> Result<(), InterfaceError> {
        self.queue(TxAim::FloodExcept(*peer), data)
    }
}

impl BleInterface {
    /// Hand one packet plus the core's link statement to the backend's
    /// fan-out.
    fn queue(&mut self, aim: TxAim, data: &[u8]) -> Result<(), InterfaceError> {
        // The media profile first, then the queue
        // (`leviculum_ble_tx::outbound_verdict`): a switched-off carrier
        // is `Ok` and not `BufferFull` however full the queue is — see
        // the LoRa interface for why.
        let fill = Fill::new(self.sender.len(), self.sender.capacity());
        let verdict = leviculum_ble_tx::outbound_verdict(carrier(), fill);
        if verdict == leviculum_ble_tx::Outbound::Swallow {
            if let Some(run) = self.drops.dropped(data.len()) {
                crate::media::log_tx_drop(self.name(), run);
            }
            return Ok(());
        }
        if let Some(run) = self.drops.resumed() {
            crate::media::log_tx_resumed(self.name(), run);
        }
        // #388 census: the copied packet enters BLE_OUTGOING custody on
        // success (the fan-out subtracts on receive); a refused send is
        // not counted. A `Queue` verdict the channel still refuses cannot
        // happen on the one executor the core and the fan-out share; were
        // it to, it is the same `BufferFull`.
        let bytes = data.len();
        let queued = verdict == leviculum_ble_tx::Outbound::Queue
            && self.sender.try_send((aim, data.to_vec())).is_ok();
        if queued {
            outgoing_held_add(bytes);
            crate::iface_bytes::note_tx(crate::iface_bytes::BLE, bytes);
            return Ok(());
        }
        // Codeberg #344: same silence as the other two. A phone that
        // stops draining the notify path fills this queue, and the board
        // could not tell that from a mesh with nothing to say.
        crate::log::log_fmt(
            "[IFACE_FULL] ",
            format_args!(
                "iface={} depth={} len={}",
                self.name(),
                self.sender.capacity(),
                data.len()
            ),
        );
        Err(InterfaceError::BufferFull)
    }
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
    /// The interface obeys the same pushed value (999), so the gating is
    /// tested in the one test that owns the carrier static: off swallows
    /// (`Ok`, nothing queued) and answers zero peers whatever the mirror
    /// holds, on queues toward the fan-out with the aim the core stated
    /// and reads the published peer count, and online needs a claimed
    /// link besides the carrier. Zero-length packets throughout, so the
    /// census statics other tests own see only 0-byte adds.
    #[test]
    fn carrier_pushes_reach_a_waiter_and_gate_the_interface() {
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

        let fanout_rx = outgoing_receiver();
        let mut iface = BleInterface::new(BLE_OUTGOING.sender());
        set_live_peer_count(2);
        assert_eq!(iface.peer_count(), 0, "a down carrier has no peers");
        assert!(!iface.is_online());
        assert!(iface.try_send(&[]).is_ok(), "a down carrier swallows");
        assert!(fanout_rx.try_receive().is_err(), "swallowed, not queued");

        set_active(true);
        assert_eq!(iface.peer_count(), 2, "the published registry count");
        assert!(!iface.is_online(), "online needs a claimed link too");
        assert!(HVN_DRAIN.claim_indexed(77).is_some());
        assert!(iface.is_online());
        assert!(iface
            .try_send_to_peer(&[], Some(&[0x33; 16]), false)
            .is_ok());
        match fanout_rx.try_receive() {
            Ok((aim, packet)) => {
                assert_eq!(aim, TxAim::Peer([0x33; 16]));
                assert!(packet.is_empty());
            }
            Err(_) => panic!("an up carrier queues toward the fan-out"),
        }

        HVN_DRAIN.release(77);
        assert!(!iface.is_online());
        set_live_peer_count(0);
        set_active(false);
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

    /// `deliver` routes by variant and in order: an inbound packet lands
    /// on the incoming channel and only there, the two peer transitions
    /// ride the ordered peer channel, and each channel's full-queue
    /// verdict refuses the newcomer (the caller's answer is `false`; the
    /// oldest pending report keeps its place). Zero-capacity packets, so
    /// the census statics the other tests own see only 0-byte adds.
    #[test]
    fn deliver_routes_each_variant_to_its_channel() {
        use leviculum_ble_tx::BleRole;

        let chans = channels();
        assert!(deliver(BleLinkEvent::Inbound {
            link: Some([0x11; 16]),
            packet: Vec::new(),
        }));
        assert!(deliver(BleLinkEvent::PeerLost {
            identity: [0x22; 16],
            role: BleRole::Peripheral,
        }));
        assert!(deliver(BleLinkEvent::PeerUp {
            identity: [0x22; 16],
            role: BleRole::Central,
        }));

        match chans.incoming_rx.try_receive() {
            Ok(BleLinkEvent::Inbound { link, packet }) => {
                assert_eq!(link, Some([0x11; 16]));
                assert!(packet.is_empty());
            }
            other => panic!("expected the inbound packet, got {:?}", other),
        }
        assert!(
            chans.incoming_rx.try_receive().is_err(),
            "peer events do not ride the incoming channel"
        );
        match chans.peer_event_rx.try_receive() {
            Ok(BleLinkEvent::PeerLost { identity, role }) => {
                assert_eq!(identity, [0x22; 16]);
                assert_eq!(role, BleRole::Peripheral);
            }
            other => panic!("the flap's Lost must land first, got {:?}", other),
        }
        match chans.peer_event_rx.try_receive() {
            Ok(BleLinkEvent::PeerUp { identity, role }) => {
                assert_eq!(identity, [0x22; 16]);
                assert_eq!(role, BleRole::Central);
            }
            other => panic!("then its Up, got {:?}", other),
        }
        assert!(chans.peer_event_rx.try_receive().is_err());

        for _ in 0..QUEUE_DEPTH {
            assert!(deliver(BleLinkEvent::Inbound {
                link: None,
                packet: Vec::new(),
            }));
        }
        assert!(
            !deliver(BleLinkEvent::Inbound {
                link: None,
                packet: Vec::new(),
            }),
            "a full incoming channel refuses the newcomer"
        );
        for i in 0..PEER_EVENT_DEPTH {
            assert!(deliver(BleLinkEvent::PeerUp {
                identity: [i as u8; 16],
                role: BleRole::Central,
            }));
        }
        assert!(
            !deliver(BleLinkEvent::PeerLost {
                identity: [0xee; 16],
                role: BleRole::Central,
            }),
            "a full peer channel drops the newest report"
        );

        // Leave the statics as this test found them.
        while chans.incoming_rx.try_receive().is_ok() {}
        while chans.peer_event_rx.try_receive().is_ok() {}
    }
}
