#![no_std]
//! The LoRa transmit path's channel-access policy: when a packet that is
//! ready to go may actually key the radio.
//!
//! Two decisions live here, extracted from `lora_task` so a host test can
//! drive them against scripted radios and seeded randomness:
//!
//! 1. **Acquisition jitter** — a randomised pre-TX wait, spent listening,
//!    before the first CAD of a channel acquisition. Two senders whose
//!    transmissions are triggered by the same event (a co-started probe
//!    announce, a rebroadcast of the same received frame) reach their
//!    radios phase-locked; CAD alone cannot separate them, because both
//!    probe a channel on which neither has keyed yet. The jitter de-tiles
//!    them so at most one is inside the other's CAD window.
//!
//! 2. **The CAD retry gate** — the bounded listen-before-talk state
//!    machine around the SX1262's channel-activity detection: clear
//!    transmits, busy backs off a random contention window that doubles
//!    per retry, and after [`CAD_MAX_RETRIES`] attempts the frame is
//!    transmitted anyway. The give-up is deliberate: the layers above
//!    assume the interface eventually transmits, so a frame must not
//!    starve behind a busy channel forever. (The reference firmware
//!    instead waits indefinitely and lets its airtime lock be the only
//!    bound; our bounded forced TX is a deviation, justified because a
//!    silent starvation is indistinguishable from packet loss to every
//!    layer above.)
//!
//! # Reference
//!
//! The jitter's parameters are the RNode firmware's CSMA draw, band 1
//! here and the wider bands in the next section, taken from the vendored
//! source
//! (`reference/RNode_Firmware`):
//!
//! * every transmission waits DIFS = `CSMA_SIFS_MS + 2 * csma_slot_ms`
//!   with SIFS = 0, i.e. two slots (Config.h:102, Config.h:119), then a
//!   contention window of `random(cw_min, cw_max)` slots
//!   (RNode_Firmware.ino:1625-1627, Arduino `random` upper-exclusive);
//!   at ≤ 7 % own airtime the band-1 window is `cw_min = 0`,
//!   `cw_max = band * CSMA_CW_PER_BAND_WINDOWS - 1` = 14
//!   (Config.h:108-111, RNode_Firmware.ino:1614-1617), so the
//!   idle-channel draw is uniform over 0..=13 slots. The reference
//!   reaches that value only by transiting a band: `cw_max` is
//!   *declared* as `CSMA_CW_PER_BAND_WINDOWS` itself, 15
//!   (Config.h:127), and `update_csma_parameters` rewrites it only when
//!   the band CHANGES (RNode_Firmware.ino:1614), which band 1 never
//!   does from boot. A reference board therefore draws over 15 slots
//!   until its first excursion above 7 % airtime and over 14 after it.
//!   We mirror the post-excursion window; the difference is one slot,
//!   and which of the two is the right target is open on Codeberg #347;
//! * the slot is 12 symbol times, clamped to at most 100 ms and at least
//!   24 ms — or 6 ms when the modulation runs faster than 30 kbps
//!   (Config.h:104-107, Utilities.h:1244-1252, bitrate Utilities.h:1237);
//! * there is **no host-visible way to disable this**: `tx_queue_handler`
//!   (RNode_Firmware.ino:1623) runs it for every queued packet.
//!
//! # The airtime band (leviculum#637)
//!
//! The reference widens that window with its own keyed airtime. The rule,
//! to the line:
//!
//! * **Input.** `airtime`, the node's OWN transmit share over the current
//!   and previous 7.5 s bins (`update_airtime`, RNode_Firmware.ino:693-698),
//!   not the channel load: `total_channel_util`, which adds everyone else's
//!   carrier, feeds only the stat frame and the long bins
//!   (RNode_Firmware.ino:1458-1464). The board keeps the same figure in its
//!   duty ledger, [`load::ChannelLedger::airtime_short_bp`].
//! * **Band.** `update_csma_parameters` (RNode_Firmware.ino:1603-1619)
//!   takes `airtime_pct = (int)(airtime*100)`; at or below
//!   `CSMA_BAND_1_MAX_AIRTIME`, 7, it is band 1, above it
//!   `map(airtime_pct + 7, 7, CSMA_BAND_N_MIN_AIRTIME, 2, CSMA_CW_BANDS)`
//!   with 85 and 4 (Config.h:108, Config.h:111-112), capped at 4. Arduino's
//!   `map` is the truncating `(x - in_min) * (out_max - out_min) /
//!   (in_max - in_min) + out_min`, so with the `+ 7` the edges fall at
//!   8 %, 39 % and 78 % (band 4 starts below the 85 the constant names).
//!   [`csma_band`].
//! * **Window.** A band change rewrites `cw_min = (band - 1) * 15` and
//!   `cw_max = band * 15 - 1` (`CSMA_CW_PER_BAND_WINDOWS`, Config.h:110;
//!   RNode_Firmware.ino:1615-1617), and the draw is `random(cw_min,
//!   cw_max)`, upper-exclusive: 0..=13, 15..=28, 30..=43 and 45..=58 slots.
//!   The bands are disjoint, so a node in band 2 never keys before one in
//!   band 1 released by the same frame. [`band_window`].
//! * **When.** The band is re-evaluated on every `update_airtime`: after
//!   each transmission (RNode_Firmware.ino:644) and once per second of
//!   status sampling (RNode_Firmware.ino:1467, `:709`). The draw takes the
//!   band in force when a contention starts (`csma_cw == -1`,
//!   RNode_Firmware.ino:1625-1627) and keeps it until the queue is flushed
//!   (`:1646`).
//!
//! [`ChannelAccess::set_airtime_short`] is that re-evaluation; the board's
//! transmit path calls it with the duty ledger's figure before every ask for
//! the acquisition wait, which is at least as fresh as the reference's. A
//! fresh draw takes the band in force and keeps it for the acquisition. An
//! access never told its airtime stays in band 1, which is the host's RNode
//! interface: the modem under it runs this rule itself, on its own keyed
//! time, and a second band on the host would count the same airtime twice.
//!
//! The CAD retry gate's doubling window below is a separate, second
//! mechanism and is not banded: it reacts to a probe that found a preamble,
//! which the reference has no counterpart for (it waits on `medium_free`).
//! [`widest_acquisition_wait_ms`] and the post-TX window it sizes still
//! budget a band-1 peer; a peer above 7 % airtime can key up later than
//! that window. Widening it is a separate radio-behaviour change.
//!
//! # The sub-slot offset (#438, arm off by default)
//!
//! The draw above is slotted: fourteen equally likely whole slots. Two
//! senders released by one frame-end serve the same DIFS, so their
//! stagger is `|cw_a - cw_b|` whole slots, and an equal draw puts both
//! key-ups on the same millisecond. Order 823 read 324 shared transitions
//! from six full hardware runs: at SF7/BW250, 23 of 290 co-releases
//! (7.9 %, Wilson 95 % CI 5.3 to 11.6 %) met inside the preamble, against
//! 1/14 = 7.14 % for "only an equal draw kills" and 20.4 % for "a
//! one-slot stagger kills too", which the data rejects at 5.2 sd. The
//! modem heard every 24 ms stagger; the kills sit at 0 to 7 ms. The blind
//! window is a fraction of a slot, and two equal draws collide only
//! because nothing separates them inside it.
//!
//! The arm ([`ChannelAccess::set_subslot_offset`]) adds a second,
//! independent draw to every fresh acquisition: a uniform offset over
//! `[0, slot)` ms, on top of DIFS plus the slots. Two equal slot draws
//! then still collide only when their offsets land within the blind
//! window of each other, about `blind / slot` (7/24 at BW250), so the
//! equal-draw kill rate falls from 1/14 to about 1/48 without one extra
//! slot of window; the mean wait grows by half a slot, the widest by
//! [`subslot_offset_max_ms`]. Where the blind window is a whole slot or
//! more (the preamble at SF8 and SF10 can be) the offset buys nothing,
//! and it costs nothing more there either.
//!
//! The reference firmware does not do this. It is a deviation under the
//! project's deviation rule: wire and semantics unchanged (a wait inside
//! one slot), Priority 1 improved by the measured kill mode. It ships OFF
//! until the rig A/B decides it; off, the random stream is the one an
//! access always drew, bit for bit.
//!
//! # The preamble-wide slot (#438, arm off by default)
//!
//! Order 857 read the remaining kills of `bench_dual_pair_fast`: they sit
//! inside the preamble (27 ms there), where no carrier sense on either
//! firmware hears the other sender, and one contention slot (24 ms on
//! both firmwares) is narrower than that preamble. A one-slot stagger
//! therefore still lands both key-ups inside each other's blind window.
//! The arm ([`ChannelAccess::set_slot_preamble_wide`]) counts every wait
//! of this access, DIFS, the contention draw and the CAD backoff alike,
//! in [`preamble_wide_slot_ms`], the reference slot or the preamble's
//! airtime, whichever is longer, so a one-slot stagger clears the
//! preamble. Nothing else changes: the draw, the window, the quiet-time
//! discharge. The reference keeps its 24 ms slot, and so does every
//! figure here that budgets a PEER's wait ([`widest_acquisition_wait_ms`],
//! [`post_tx_rx_window_ms`]); whether the deviation is worth its cost in
//! throughput is the question the rig A/B answers.

pub mod load;

/// Maximum CAD attempts before a frame is transmitted even though the
/// channel appears busy — the documented give-up that keeps a frame from
/// starving forever (see the crate docs for why this deviates from the
/// reference's unbounded wait).
pub const CAD_MAX_RETRIES: u8 = 8;
/// Initial contention window (slots) for the CAD backoff. Starting at 2
/// guarantees a non-zero-slot random choice on the first retry so two
/// nodes that simultaneously detect traffic desynchronize meaningfully.
pub const CAD_CW_INITIAL: u8 = 2;
/// Maximum contention window (slots) after exponential back-off.
///
/// The reference has no doubling window on a busy probe; its widest draw is
/// band 4's 45..=58 slots, taken on airtime (see the crate docs). Ours is
/// 0..=63, reached by observed CAD-busy. In slot times the two ceilings are
/// 5.8 s and 6.3 s at the 100 ms slot.
pub const CAD_CW_MAX: u8 = 64;

/// Airtime bands (`CSMA_CW_BANDS`, reference Config.h:108).
pub const CSMA_CW_BANDS: u8 = 4;
/// Slots per band (`CSMA_CW_PER_BAND_WINDOWS`, reference Config.h:110).
pub const CSMA_CW_PER_BAND_WINDOWS: u32 = 15;
/// The highest own-airtime percentage still in band 1
/// (`CSMA_BAND_1_MAX_AIRTIME`, reference Config.h:111).
pub const CSMA_BAND_1_MAX_AIRTIME_PCT: u32 = 7;
/// The `in_max` of the band map (`CSMA_BAND_N_MIN_AIRTIME`, reference
/// Config.h:112). With the map's `+ 7` shift band 4 starts at 78 %.
pub const CSMA_BAND_N_MIN_AIRTIME_PCT: u32 = 85;

/// The reference's CSMA band for an own short-term airtime in hundredths of
/// a percent (`update_csma_parameters`, RNode_Firmware.ino:1603-1612; the
/// rule is in the crate docs).
pub fn csma_band(airtime_short_bp: u16) -> u8 {
    // `(int)(airtime*100)`: hundredths of a percent truncated to percent.
    let pct = (airtime_short_bp / 100) as u32;
    if pct <= CSMA_BAND_1_MAX_AIRTIME_PCT {
        return 1;
    }
    let at = pct + CSMA_BAND_1_MAX_AIRTIME_PCT;
    // Arduino `map(at, 7, 85, 2, 4)`, integer and truncating.
    let band = (at - CSMA_BAND_1_MAX_AIRTIME_PCT) * (CSMA_CW_BANDS as u32 - 2)
        / (CSMA_BAND_N_MIN_AIRTIME_PCT - CSMA_BAND_1_MAX_AIRTIME_PCT)
        + 2;
    band.min(CSMA_CW_BANDS as u32) as u8
}

/// A band's `(cw_min, cw_max)`, drawn as `random(cw_min, cw_max)`,
/// upper-exclusive (reference RNode_Firmware.ino:1615-1617, `:1626`). A band
/// outside 1..=4 is taken as the nearest one.
pub fn band_window(band: u8) -> (u32, u32) {
    let band = band.clamp(1, CSMA_CW_BANDS) as u32;
    (
        (band - 1) * CSMA_CW_PER_BAND_WINDOWS,
        band * CSMA_CW_PER_BAND_WINDOWS - 1,
    )
}

/// DIFS in slots: `CSMA_SIFS_MS + 2 * csma_slot_ms` with SIFS = 0
/// (reference Config.h:102, Config.h:119).
pub const JITTER_DIFS_SLOTS: u64 = 2;
/// Number of equally likely jitter draws in every band; band 1's
/// `random(0, 14)` is uniform over 0..=13 slots (reference Config.h:108-111,
/// RNode_Firmware.ino:1626, Arduino `random` upper-exclusive).
pub const JITTER_CW_SLOTS: u32 = 14;
/// One jitter slot is 12 symbol times (reference Config.h:107).
pub const JITTER_SLOT_SYMBOLS: u64 = leviculum_core::rnode::CSMA_SLOT_SYMBOLS;
/// Slot ceiling in ms (reference Config.h:104).
pub const JITTER_SLOT_MAX_MS: u64 = leviculum_core::rnode::CSMA_SLOT_MAX_MS;
/// Slot floor in ms (reference Config.h:105).
pub const JITTER_SLOT_MIN_MS: u64 = leviculum_core::rnode::CSMA_SLOT_MIN_MS;
/// Slot floor at fast rates: `CSMA_SLOT_MIN_MS - CSMA_SLOT_MIN_FAST_DELTA`
/// = 24 - 18 (reference Config.h:105-106, Utilities.h:1246).
pub const JITTER_SLOT_MIN_FAST_MS: u64 =
    leviculum_core::rnode::CSMA_SLOT_MIN_MS - leviculum_core::rnode::CSMA_SLOT_MIN_FAST_DELTA;
/// Rates above this count as fast (reference Config.h:87,
/// `LORA_FAST_THRESHOLD_BPS`).
pub const JITTER_FAST_THRESHOLD_BPS: u64 = leviculum_core::rnode::LORA_FAST_THRESHOLD_BPS as u64;

/// The reference's CSMA slot for a modulation: 12 symbol times clamped to
/// `[24, 100]` ms, floor 6 ms at rates above 30 kbps (Utilities.h:
/// 1244-1252).
///
/// The derivation itself is [`leviculum_core::rnode::csma_slot_ms`], where it
/// sits beside the airtime and preamble arithmetic it shares its reference
/// lines with, is pinned against a literal float transcription of those
/// lines over every PHY the reference admits, and is mirrored into
/// `periculum-wire`. It is reached through this name because the slot is a
/// channel-access quantity to every caller here, and because there must be
/// exactly one of it: the CAD backoff in the firmware's transmit path used
/// to derive a second, unclamped slot of its own from a 500-byte airtime,
/// 27 times this one at SF12 (Codeberg #147).
pub fn jitter_slot_ms(bw_hz: u32, sf: u8, cr_denom: u8) -> u64 {
    leviculum_core::rnode::csma_slot_ms(bw_hz, sf, cr_denom)
}

/// The widest wait a node can owe before it keys up on a fresh channel
/// acquisition: DIFS plus the top of the contention window, at its
/// modulation ([`JITTER_DIFS_SLOTS`], [`JITTER_CW_SLOTS`],
/// [`jitter_slot_ms`]).
///
/// This is the quantity anyone sizing a *listening* window against a peer
/// has to budget, because it is the latest the peer can key up — not the
/// latest it can finish a frame. The draw is uniform over the window, so
/// this is a ceiling reached once per `JITTER_CW_SLOTS` acquisitions and
/// not a typical wait; the mean is DIFS plus half the window.
///
/// It is a wait over QUIET medium: a frame the node receives during it
/// pauses it rather than serving it ([`Listened`], #438), so in wall-clock
/// the key-up comes this long after the release plus the airtime of every
/// frame heard in between.
pub fn widest_acquisition_wait_ms(bw_hz: u32, sf: u8, cr_denom: u8) -> u64 {
    (JITTER_DIFS_SLOTS + JITTER_CW_SLOTS as u64 - 1) * jitter_slot_ms(bw_hz, sf, cr_denom)
}

/// The airtime of the preamble the reference firmware programs at a
/// modulation, in whole milliseconds rounded up: the symbols
/// [`leviculum_core::rnode::derive_preamble_symbols`] derives, times the
/// symbol time `2^sf / bw`, evaluated as one exact rational so the
/// rounding happens once. `0` for a degenerate modulation (no bandwidth,
/// a spreading factor no modem has).
pub fn preamble_airtime_ms(bw_hz: u32, sf: u8, cr_denom: u8) -> u64 {
    if bw_hz == 0 || sf == 0 || sf > 12 {
        return 0;
    }
    let symbols = u64::from(leviculum_core::rnode::derive_preamble_symbols(
        sf, cr_denom, bw_hz,
    ));
    (symbols * (1u64 << sf) * 1_000).div_ceil(u64::from(bw_hz))
}

/// The CSMA slot of an access armed with the preamble-wide slot
/// ([`ChannelAccess::set_slot_preamble_wide`], the crate docs' "The
/// preamble-wide slot"): the reference slot ([`jitter_slot_ms`]) or the
/// preamble's airtime ([`preamble_airtime_ms`]), whichever is longer.
/// 37 ms at the default SF8/BW125 against the reference's 24.
pub fn preamble_wide_slot_ms(bw_hz: u32, sf: u8, cr_denom: u8) -> u64 {
    core::cmp::max(
        jitter_slot_ms(bw_hz, sf, cr_denom),
        preamble_airtime_ms(bw_hz, sf, cr_denom),
    )
}

/// The slot an access counts its own waits in: [`preamble_wide_slot_ms`]
/// with the arm on, the reference's [`jitter_slot_ms`] with it off. The one
/// expression [`ChannelAccess::set_phy`] enforces and the `[LORA_CSMA_TX]
/// slot_ms=` line prints.
pub fn access_slot_ms(bw_hz: u32, sf: u8, cr_denom: u8, preamble_wide: bool) -> u64 {
    if preamble_wide {
        preamble_wide_slot_ms(bw_hz, sf, cr_denom)
    } else {
        jitter_slot_ms(bw_hz, sf, cr_denom)
    }
}

/// The widest sub-slot offset an armed access adds to a fresh acquisition
/// wait: one CSMA slot less a millisecond, because the offset is drawn
/// uniformly over `[0, slot)` ([`ChannelAccess::set_subslot_offset`]).
///
/// Not part of [`widest_acquisition_wait_ms`]: the arm is off by default,
/// and the windows sized on that figure stay what they were until the rig
/// A/B decides the arm. A caller that publishes a ceiling for an armed
/// access adds this term itself.
pub fn subslot_offset_max_ms(bw_hz: u32, sf: u8, cr_denom: u8) -> u64 {
    jitter_slot_ms(bw_hz, sf, cr_denom).saturating_sub(1)
}

/// Ceiling on the post-TX receive window, in ms. A bound on how long one
/// yield may keep the transmit path off its queue when the channel is
/// genuinely silent; it binds only at SF12/125 kHz, where one reply
/// airtime alone is 9.3 s.
pub const POST_TX_WINDOW_MAX_MS: u64 = 10_000;

/// How long the transmit path listens after a transmission before it
/// drains the next outgoing frame: one reply airtime plus the peer's whole
/// turnaround.
///
/// `reply_airtime_ms` is a full single-frame reply on the wire at the live
/// modulation, and `host_margin_ms` the peer's host-side processing before
/// it starts contending (`PACING_MARGIN_MS`); the caller supplies both
/// because the airtime formula and that constant live in `leviculum-core`.
/// The modulation is taken rather than a slot, because the slot the peer
/// draws in is this crate's [`jitter_slot_ms`] and no other — passing one
/// in is how the term below came to be wrong.
///
/// **The turnaround is the peer's widest time to KEY UP, not to finish its
/// frame** ([`widest_acquisition_wait_ms`]): the receiver stops its
/// timeout on preamble detect and then runs to packet completion whatever
/// the length (`SET_STOP_RX_TIMER_ON_PREAMBLE`,
/// `leviculum-nrf/src/sx1262.rs`), so a reply that starts inside the
/// window is heard in full even when it ends outside it.
///
/// Until Codeberg #423 the turnaround budgeted the peer's DIFS and nothing
/// else — two slots of the *backoff* slot the CAD gate uses, written
/// before the peer had a contention window to draw at all. It was covered
/// at every slow PHY only by the reply-airtime term's slack, and at
/// SF7/500 kHz there was not enough of that slack: 270 ms of window
/// against a peer that can wait 360 ms, so a peer drawing the top of its
/// window keyed up after the board had already left the window. The
/// coverage is now structural — the term the window must cover is a
/// summand of the window — instead of an accident of how much airtime the
/// modulation happens to charge.
///
/// **A reception pauses the peer's wait (#438)**, so the peer keys up later
/// in wall-clock than the turnaround by the airtime of every frame it hears
/// in between. Not widened for it, because a frame this node hears as well
/// ends this window first (the window returns on a reception), and so cannot
/// carry the peer past it. What can is a frame only the peer hears, a hidden
/// third sender, by that frame's airtime; before #438 the same frame released
/// the peer at its end, which lay just as far past. One more reply airtime
/// would cover one such frame and not two, and would be paid on every yield
/// into a silent channel (at SF8/125 kHz 728 ms on top of the 1188 ms window). A
/// peer that keys after the window is not talked over either: what follows
/// the window is the idle receive or the next acquisition's own
/// listened-through wait, and both hear it.
pub fn post_tx_rx_window_ms(
    reply_airtime_ms: u64,
    host_margin_ms: u64,
    bw_hz: u32,
    sf: u8,
    cr_denom: u8,
) -> u32 {
    let turnaround = host_margin_ms + widest_acquisition_wait_ms(bw_hz, sf, cr_denom);
    // Clamp to >= 1 ms: the SX1262 reads a zero timeout as "no timeout".
    reply_airtime_ms
        .saturating_add(turnaround)
        .clamp(1, POST_TX_WINDOW_MAX_MS) as u32
}

/// Ceiling on a listening wait the transmit path spends inside the channel
/// access: a CAD backoff, or a part of the acquisition jitter. Neither comes
/// near it at any PHY the board runs (the widest backoff is
/// `(CAD_CW_MAX - 1)` slots of at most 100 ms); it bounds a wait whose inputs
/// went wrong, so one turn cannot park the transmit path for longer.
pub const LISTEN_WINDOW_MAX_MS: u64 = 10_000;

/// The receive window that spends a listening wait of `wait_ms`: clamped to
/// at least 1 ms, because the SX1262 reads a zero timeout as "no timeout",
/// and to at most [`LISTEN_WINDOW_MAX_MS`].
pub fn listen_window_ms(wait_ms: u64) -> u32 {
    wait_ms.clamp(1, LISTEN_WINDOW_MAX_MS) as u32
}

/// How a listening wait inside the channel access ended, as far as the
/// booking of it cares: whether the radio decoded a frame header in it, and
/// if so how long that frame was on the air.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEnd {
    /// No header decoded: the window ran out, the software wait expired on a
    /// preamble that never became a frame, the radio errored, or the window
    /// was stood down for a radio config. Whatever of it was spent on a
    /// carrier is the overrun past the budget.
    NoFrame,
    /// A frame whose header decoded, delivered or failed on its payload CRC,
    /// and its on-air time at the live modulation.
    Frame { airtime_ms: u64 },
}

/// One listening wait as the transmit path lived it: the wall-clock it
/// took, the window it was armed with, and how it ended.
///
/// The reference counts its contention wait only over a free medium: a busy
/// channel restarts DIFS and pauses the contention window
/// (`reference/RNode_Firmware/RNode_Firmware.ino:1631-1646`). A wait here is
/// spent in a receive window that returns when a frame completes, so the
/// wall-clock of a window a reception ended is quiet time plus that
/// reception, and only the quiet part has de-tiled anything (#438). Two
/// measures of the reception, one per outcome:
///
/// * **A decoded frame** ([`WindowEnd::Frame`], `rx_success` and a payload
///   CRC failure): its airtime. The timer stops on preamble detect
///   (`SET_STOP_RX_TIMER_ON_PREAMBLE`) and the window then runs to the frame
///   end, so the frame's whole airtime sits inside the wall-clock whether it
///   ended inside the budget or past it; the overrun alone would book a frame
///   that ended inside the budget as quiet. Capped at the elapsed, for a
///   frame that was already on the air when the window opened. The CRC
///   failure counts here too, because its length came out of a header that
///   passed its own CRC.
/// * **No decoded frame** ([`WindowEnd::NoFrame`], `rx_timeout`, a carrier
///   held past the budget by the preamble stop, an error): the overrun past
///   the budget. There is no length to price, and a window that ran its
///   course in quiet overruns by the arming turnaround only, which the debt
///   it was armed against cannot go below anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listened {
    /// Wall-clock from arming the window to its return.
    pub elapsed_ms: u64,
    /// The window it was armed with.
    pub budget_ms: u64,
    pub end: WindowEnd,
}

impl Listened {
    /// A wait served in full on a quiet medium: a sleep nothing cuts short.
    pub const fn quiet(ms: u64) -> Self {
        Self {
            elapsed_ms: ms,
            budget_ms: ms,
            end: WindowEnd::NoFrame,
        }
    }

    /// The part of the wait a reception took ([`Listened`] says which
    /// measure per outcome).
    pub fn rx_ms(&self) -> u64 {
        match self.end {
            WindowEnd::NoFrame => self.elapsed_ms.saturating_sub(self.budget_ms),
            WindowEnd::Frame { airtime_ms } => airtime_ms.min(self.elapsed_ms),
        }
    }

    /// The part of the wait the medium was free, which is what a contention
    /// wait is counted in. At least 1 ms, so a radio that returns at once,
    /// or a channel that is never quiet, still drains the debt and the
    /// transmit path cannot spin on a wait forever.
    pub fn quiet_ms(&self) -> u64 {
        self.elapsed_ms.saturating_sub(self.rx_ms()).max(1)
    }
}

/// What the SX1262's channel-activity detection reported for one probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CadOutcome {
    /// No LoRa preamble on the channel.
    Clear,
    /// A preamble was detected: somebody is keyed.
    Busy,
    /// The detection itself failed: the radio, not the channel, refused.
    Error,
}

/// What follows a key-up, by the CAD outcome that led to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterKeyUp {
    /// Listen for the peer's answer: the burst check, the post-TX ack
    /// window, and the peer-turn yield, after which the channel counts as
    /// released.
    AckWindow,
    /// Straight to the next turn: no burst check, no ack window, and the
    /// channel is not released, so the next acquisition owes no fresh jitter.
    NextTurn,
}

/// What the transmit path does after keying a packet the gate let through.
///
/// A packet keyed on a clear channel, or forced out after the busy budget
/// ran out, opens the ack window like any transmission. A packet forced out
/// because the radio could not run CAD at all goes straight to the next turn:
/// the firmware has done so since the retry gate was written, and the
/// interface counts its airtime toward the burst bound all the same. Was the
/// `continue` at the end of the CAD error arm in the firmware's `lora_task`,
/// moved here so a host test pins it (leviculum#413): no rig cell makes the
/// radio refuse a CAD.
pub fn after_key_up(outcome: CadOutcome) -> AfterKeyUp {
    match outcome {
        CadOutcome::Clear | CadOutcome::Busy => AfterKeyUp::AckWindow,
        CadOutcome::Error => AfterKeyUp::NextTurn,
    }
}

/// What the transmit path must do next, after asking the gate about a CAD
/// outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Key the radio. `forced` is true when the retry budget ran out with
    /// the channel still busy; `retries` is the attempt count for the
    /// `[LORA_CSMA_TX]` line.
    Transmit { forced: bool, retries: u8 },
    /// Listen for this many backoff slots (caller multiplies by the CSMA
    /// slot, [`ChannelAccess::jitter_slot`]), then CAD again.
    Backoff { slots: u64 },
    /// CAD itself failed; retry it immediately. Bounded by the same
    /// retry budget as busy, so a radio that cannot CAD still transmits.
    Retry,
}

/// The channel-access state one LoRa transmit path owns: the seeded
/// randomness, the owed acquisition jitter, and the CAD retry gate.
///
/// Everything is a function of the seed and the fed-in CAD outcomes, so a
/// host test scripts a radio ("busy, busy, clear") and asserts the exact
/// decisions.
#[derive(Debug, Clone)]
pub struct ChannelAccess {
    rng: u32,
    jitter_slot_ms: u64,
    jitter_owed: bool,
    jitter_remaining_ms: u64,
    jitter_was_drawn: bool,
    band: u8,
    drawn_band: u8,
    subslot: bool,
    preamble_wide: bool,
    phy: Option<(u32, u8, u8)>,
    drawn_slots: u64,
    drawn_offset_ms: u64,
    last_quiet_ms: u64,
    last_rx_ms: u64,
    attempt: u8,
    cw: u8,
    backoff_remaining_ms: u64,
    backoff_cut: bool,
}

/// xorshift32 step. The all-zero state is a fixed point, which is why the
/// constructor refuses a zero seed.
fn xorshift32(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

impl ChannelAccess {
    /// A fresh access state. `seed` must come from per-board entropy (the
    /// hardware RNG): two boards seeded identically draw identical jitter
    /// and identical backoffs at identical draw counts, which re-creates
    /// exactly the phase lock the jitter exists to break. A zero seed is
    /// replaced (xorshift32's zero state is a fixed point).
    ///
    /// Boot counts as a released channel: the very first transmission owes
    /// jitter, because co-started boards are the canonical phase-locked
    /// senders.
    pub const fn new(seed: u32) -> Self {
        Self {
            rng: if seed == 0 { 0x9E37_79B9 } else { seed },
            jitter_slot_ms: JITTER_SLOT_MIN_MS,
            jitter_owed: true,
            jitter_remaining_ms: 0,
            jitter_was_drawn: false,
            band: 1,
            drawn_band: 1,
            subslot: false,
            preamble_wide: false,
            phy: None,
            drawn_slots: 0,
            drawn_offset_ms: 0,
            last_quiet_ms: 0,
            last_rx_ms: 0,
            attempt: 0,
            cw: CAD_CW_INITIAL,
            backoff_remaining_ms: 0,
            backoff_cut: false,
        }
    }

    /// Adopt a modulation. Called at radio bring-up and on every runtime
    /// reconfiguration, beside the backoff slot recomputation.
    pub fn set_phy(&mut self, bw_hz: u32, sf: u8, cr_denom: u8) {
        self.phy = Some((bw_hz, sf, cr_denom));
        self.jitter_slot_ms = access_slot_ms(bw_hz, sf, cr_denom, self.preamble_wide);
    }

    /// Arm or disarm the preamble-wide slot ([`preamble_wide_slot_ms`], the
    /// crate docs' "The preamble-wide slot"). Recomputes the slot at the
    /// modulation in force, so the order against [`Self::set_phy`] does not
    /// matter; a wait already drawn keeps the slot it was drawn in. Off is
    /// the reference slot, as it always was.
    pub fn set_slot_preamble_wide(&mut self, on: bool) {
        self.preamble_wide = on;
        if let Some((bw_hz, sf, cr_denom)) = self.phy {
            self.jitter_slot_ms = access_slot_ms(bw_hz, sf, cr_denom, on);
        }
    }

    /// Whether the preamble-wide slot is armed.
    pub const fn slot_preamble_wide(&self) -> bool {
        self.preamble_wide
    }

    /// The jitter slot in force, for the `[LORA_JITTER]` line.
    pub const fn jitter_slot(&self) -> u64 {
        self.jitter_slot_ms
    }

    /// Re-evaluate the airtime band from the node's own short-term airtime
    /// in hundredths of a percent ([`csma_band`]), the reference's
    /// `update_csma_parameters`. Takes effect at the next fresh draw; a wait
    /// already drawn keeps its band.
    pub fn set_airtime_short(&mut self, airtime_short_bp: u16) {
        self.band = csma_band(airtime_short_bp);
    }

    /// The band the current acquisition's wait was drawn in, for the
    /// `[LORA_JITTER]` and `[LORA_CSMA_TX]` lines.
    pub const fn drawn_band(&self) -> u8 {
        self.drawn_band
    }

    /// Arm or disarm the sub-slot offset ([`subslot_offset_max_ms`], the
    /// crate docs' "The sub-slot offset"). Takes effect at the next fresh
    /// draw. Off is the policy as it was, to the bit: an access never told
    /// draws exactly the stream it drew before the arm existed.
    pub fn set_subslot_offset(&mut self, on: bool) {
        self.subslot = on;
    }

    /// Whether the sub-slot offset is armed.
    pub const fn subslot_offset(&self) -> bool {
        self.subslot
    }

    /// The contention slots the current acquisition drew (`random(cw_min,
    /// cw_max)`, without DIFS), for the census line that has to say which
    /// draw keyed.
    pub const fn drawn_slots(&self) -> u64 {
        self.drawn_slots
    }

    /// The sub-slot offset the current acquisition drew, in ms; `0` with the
    /// arm off.
    pub const fn drawn_offset_ms(&self) -> u64 {
        self.drawn_offset_ms
    }

    /// The channel was yielded back (a post-TX listening window ran, or is
    /// about to): the next acquisition owes jitter again. Any remainder
    /// of a previous, abandoned draw is dropped — a new acquisition draws
    /// afresh rather than inheriting a leftover.
    pub fn channel_released(&mut self) {
        self.jitter_owed = true;
        self.jitter_remaining_ms = 0;
        self.backoff_remaining_ms = 0;
    }

    /// A new packet starts contending: reset the retry gate. Does not
    /// touch the owed jitter — a burst continuation is a new packet but
    /// not a new acquisition. A backoff the previous packet left unserved
    /// is that packet's, and goes with it.
    pub fn begin_packet(&mut self) {
        self.attempt = 0;
        self.cw = CAD_CW_INITIAL;
        self.backoff_remaining_ms = 0;
    }

    /// The randomised pre-TX wait this acquisition still owes, in ms: on
    /// the first ask after a channel release, DIFS plus a uniform draw over
    /// the window of the band in force ([`JITTER_DIFS_SLOTS`],
    /// [`band_window`]; 0..=13 slots in band 1); on every
    /// later ask, whatever is LEFT of that draw after the caller has
    /// reported what it actually listened through ([`Self::jitter_spent`]),
    /// and `0` once the whole wait has been served.
    ///
    /// Asking is not serving. The caller spends the wait listening, so a
    /// peer that keys during it is heard rather than talked over — but the
    /// listen returns early on a reception, and a wait that ends that way
    /// has de-tiled nothing: the frame that ended it releases every other
    /// waiting node at the same instant, which is the phase lock the draw
    /// exists to break. Handing the value out therefore cannot discharge
    /// the debt; only listening it through can.
    pub fn acquisition_jitter_ms(&mut self) -> u64 {
        if self.jitter_owed {
            self.jitter_owed = false;
            let (cw_min, cw_max) = band_window(self.band);
            // Band 1 is `% JITTER_CW_SLOTS` from 0, the draw it always was,
            // so the random stream of an unbanded access is unchanged.
            let draw = (cw_min + xorshift32(&mut self.rng) % (cw_max - cw_min)) as u64;
            // The sub-slot offset is a second draw from the same stream, taken
            // only when armed: with the arm off the stream (and with it the
            // CAD backoff ladder behind this draw) is the one it always was.
            let offset = if self.subslot && self.jitter_slot_ms > 0 {
                xorshift32(&mut self.rng) as u64 % self.jitter_slot_ms
            } else {
                0
            };
            self.drawn_band = self.band;
            self.drawn_slots = draw;
            self.drawn_offset_ms = offset;
            self.jitter_remaining_ms = (JITTER_DIFS_SLOTS + draw) * self.jitter_slot_ms + offset;
            self.jitter_was_drawn = true;
        } else {
            self.jitter_was_drawn = false;
        }
        self.jitter_remaining_ms
    }

    /// Report the window the caller listened through for the wait
    /// [`Self::acquisition_jitter_ms`] last handed it. Only its quiet part
    /// is booked ([`Listened::quiet_ms`]): a reception that ended the window
    /// pauses the wait, it does not serve it, so two draws cut by the same
    /// frame keep the stagger they drew. The debt saturates at zero.
    pub fn jitter_spent(&mut self, window: Listened) {
        let quiet = self.book(window);
        self.jitter_remaining_ms = self.jitter_remaining_ms.saturating_sub(quiet);
    }

    /// Record a window for the log line that resumes it, and return its
    /// quiet part.
    fn book(&mut self, window: Listened) -> u64 {
        self.last_quiet_ms = window.quiet_ms();
        self.last_rx_ms = window.rx_ms();
        self.last_quiet_ms
    }

    /// The quiet part of the last window booked, for the resume lines.
    pub const fn last_quiet_ms(&self) -> u64 {
        self.last_quiet_ms
    }

    /// The reception part of the last window booked, for the resume lines.
    pub const fn last_rx_ms(&self) -> u64 {
        self.last_rx_ms
    }

    /// The CAD backoff this packet still owes before it probes again, in
    /// ms: the slots of the last [`Verdict::Backoff`] in the CSMA slot, less
    /// what [`Self::backoff_spent`] booked of it. A backoff is a wait like
    /// the acquisition's and is counted the same way, over quiet medium
    /// only, so a backed-off node does not re-probe at the end of the frame
    /// that cut its window, alongside everyone else that frame released.
    pub const fn backoff_owed_ms(&self) -> u64 {
        self.backoff_remaining_ms
    }

    /// Report the window the caller listened through for the backoff
    /// [`Self::backoff_owed_ms`] handed it; as [`Self::jitter_spent`].
    pub fn backoff_spent(&mut self, window: Listened) {
        let quiet = self.book(window);
        self.backoff_remaining_ms = self.backoff_remaining_ms.saturating_sub(quiet);
        self.backoff_cut = true;
    }

    /// Whether the backoff [`Self::backoff_owed_ms`] hands out is what a
    /// window left of it rather than a fresh one, for the resume line.
    pub const fn backoff_was_cut(&self) -> bool {
        self.backoff_cut
    }

    /// Whether the last [`Self::acquisition_jitter_ms`] was a fresh draw
    /// rather than the remainder of one already part-served. Only the log
    /// line reads this, so a run's records distinguish an acquisition's
    /// one draw from the windows that resume it.
    pub const fn jitter_was_drawn(&self) -> bool {
        self.jitter_was_drawn
    }

    /// The retry count so far, for the `[LORA_CAD]` log lines.
    pub const fn retries(&self) -> u8 {
        self.attempt
    }

    /// The channel was probed and found clear: transmit.
    pub fn cad_clear(&mut self) -> Verdict {
        Verdict::Transmit {
            forced: false,
            retries: self.attempt,
        }
    }

    /// The channel was probed and found busy: back off a random number of
    /// slots inside the doubling contention window — or transmit anyway
    /// once the retry budget is spent (the documented give-up).
    pub fn cad_busy(&mut self) -> Verdict {
        self.attempt += 1;
        if self.attempt >= CAD_MAX_RETRIES {
            return Verdict::Transmit {
                forced: true,
                retries: self.attempt,
            };
        }
        let slots = (xorshift32(&mut self.rng) as u64) % (self.cw as u64);
        self.cw = core::cmp::min(self.cw.saturating_mul(2), CAD_CW_MAX);
        self.backoff_remaining_ms = slots.saturating_mul(self.jitter_slot_ms);
        self.backoff_cut = false;
        Verdict::Backoff { slots }
    }

    /// Feed one CAD outcome to the gate: the single entry the transmit path
    /// asks, so the three outcomes cannot drift apart in the caller.
    pub fn cad(&mut self, outcome: CadOutcome) -> Verdict {
        match outcome {
            CadOutcome::Clear => self.cad_clear(),
            CadOutcome::Busy => self.cad_busy(),
            CadOutcome::Error => self.cad_error(),
        }
    }

    /// The receive window a [`Verdict::Backoff`] of `slots` is listened
    /// through: the slots counted in the reference CSMA slot this access
    /// draws its jitter in (there is one slot on a shared channel; the second
    /// one the firmware used to derive from a 500-byte airtime was 27x it at
    /// SF12, Codeberg #147), as a [`listen_window_ms`].
    pub fn backoff_window_ms(&self, slots: u64) -> u32 {
        listen_window_ms(slots.saturating_mul(self.jitter_slot_ms))
    }

    /// The CAD operation itself errored: count it against the same budget
    /// and retry immediately (no backoff listen — the radio, not the
    /// channel, refused), forcing the TX once the budget is spent so a
    /// radio that cannot CAD still transmits.
    pub fn cad_error(&mut self) -> Verdict {
        self.attempt += 1;
        if self.attempt >= CAD_MAX_RETRIES {
            return Verdict::Transmit {
                forced: true,
                retries: self.attempt,
            };
        }
        Verdict::Retry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- The reference slot, checked against hand-derived values -------

    #[test]
    fn the_slot_matches_the_reference_at_the_bench_phy() {
        // SF7/BW125: symbol 1.024 ms, 12 symbols = 12.288 ms, bitrate
        // 5468 bps (not fast) -> clamped up to the 24 ms floor. This is
        // the corpus-wide bench PHY, so it is the value the acceptance
        // ladder runs under.
        assert_eq!(jitter_slot_ms(125_000, 7, 5), 24);
    }

    #[test]
    fn the_slot_scales_with_the_symbol_time_between_the_clamps() {
        // SF9/BW125: symbol 4.096 ms -> 49.152 ms -> 49.
        assert_eq!(jitter_slot_ms(125_000, 9, 5), 49);
        // SF10/BW125: symbol 8.192 ms -> 98.304 ms -> 98, just under the
        // ceiling.
        assert_eq!(jitter_slot_ms(125_000, 10, 5), 98);
    }

    #[test]
    fn the_slot_ceiling_binds_at_slow_modulations() {
        // SF12/BW125: 12 symbols are 393 ms; the reference caps the slot
        // at 100 ms (Config.h:104).
        assert_eq!(jitter_slot_ms(125_000, 12, 5), 100);
        assert_eq!(jitter_slot_ms(125_000, 11, 8), 100);
    }

    #[test]
    fn the_fast_rate_floor_is_6_ms() {
        // SF5/BW500: bitrate 5*4*500000/(5*32) = 62500 bps > 30 kbps,
        // symbol 0.064 ms, 12 symbols = 0.768 ms -> fast floor 6 ms
        // (Config.h:105-106).
        assert_eq!(jitter_slot_ms(500_000, 5, 5), 6);
    }

    #[test]
    fn the_widest_acquisition_wait_is_a_function_of_the_modulation() {
        // The table `docs/src/concepts/csma-transmit-window.md` quotes for
        // question 1 of Codeberg #347, pinned where it is computed rather
        // than transcribed into prose that cannot go red. Each row is
        // DIFS plus the widest draw, i.e. the ceiling a caller sizes a
        // delivery window with; the mean is DIFS plus half the window.
        //
        // The span the rows cover is the argument: 90 ms at SF5/500 kHz
        // against 1500 ms at SF12/125 kHz, a factor of 16 that no
        // millisecond constant tracks. A change to the slot derivation,
        // the DIFS width or the number of draws moves at least one row.
        let widest = widest_acquisition_wait_ms;
        let narrowest = |bw, sf, cr| JITTER_DIFS_SLOTS * jitter_slot_ms(bw, sf, cr);

        // SF7 and SF8 at 125 kHz sit on the 24 ms slot floor together, so
        // the bench PHY and the project default draw the same window.
        assert_eq!((narrowest(125_000, 7, 5), widest(125_000, 7, 5)), (48, 360));
        assert_eq!((narrowest(125_000, 8, 5), widest(125_000, 8, 5)), (48, 360));
        // Between the clamps the window tracks the symbol time.
        assert_eq!((narrowest(125_000, 9, 5), widest(125_000, 9, 5)), (98, 735));
        assert_eq!(
            (narrowest(125_000, 10, 5), widest(125_000, 10, 5)),
            (196, 1470)
        );
        // At SF12 the slot ceiling binds, so the window stops growing.
        assert_eq!(
            (narrowest(125_000, 12, 5), widest(125_000, 12, 5)),
            (200, 1500)
        );
        // Above 30 kbps the fast floor binds instead.
        assert_eq!((narrowest(500_000, 5, 5), widest(500_000, 5, 5)), (12, 90));

        // The reference's own inter-frame gap, measured off the air during
        // Codeberg #344, had a median of 205 ms over 81 gaps at the bench
        // PHY. The window's median draw is DIFS plus half of 13 slots,
        // which is what that measurement is a measurement OF: a model that
        // missed it by a slot would put this at 180 or 228.
        let median_ms = JITTER_DIFS_SLOTS * jitter_slot_ms(125_000, 7, 5)
            + (JITTER_CW_SLOTS as u64 - 1) * jitter_slot_ms(125_000, 7, 5) / 2;
        assert_eq!(median_ms, 204);
    }

    #[test]
    fn a_degenerate_modulation_cannot_divide_by_zero() {
        // A corrupt flash page decodes to whatever it decodes to; the
        // slot function must stay total.
        assert_eq!(jitter_slot_ms(0, 7, 5), JITTER_SLOT_MAX_MS);
        assert_eq!(jitter_slot_ms(125_000, 0, 5), JITTER_SLOT_MAX_MS);
        assert_eq!(jitter_slot_ms(125_000, 7, 0), JITTER_SLOT_MAX_MS);
        assert_eq!(jitter_slot_ms(125_000, 40, 5), JITTER_SLOT_MAX_MS);
    }

    // --- The preamble-wide slot (#438 arm) ------------------------------

    #[test]
    fn the_preamble_wide_slot_at_the_default_phy_is_37_ms_and_24_off() {
        // SF8/BW125/CR4:5: bitrate 3125 bps (not fast), so the preamble
        // target is 24 ms, 11.7 symbols, below the 18-symbol floor; 18
        // symbols of 2.048 ms are 36.864 ms, rounded up to 37.
        assert_eq!(
            leviculum_core::rnode::derive_preamble_symbols(8, 5, 125_000),
            18
        );
        assert_eq!(
            preamble_airtime_ms(125_000, 8, 5),
            (18 * 256 * 1_000_u64).div_ceil(125_000)
        );
        assert_eq!(preamble_airtime_ms(125_000, 8, 5), 37);
        assert_eq!(access_slot_ms(125_000, 8, 5, true), 37);
        // Off, the reference slot: 12 symbols are 24.6 ms, truncated to 24.
        assert_eq!(access_slot_ms(125_000, 8, 5, false), 24);
        assert_eq!(
            access_slot_ms(125_000, 8, 5, false),
            jitter_slot_ms(125_000, 8, 5)
        );
    }

    #[test]
    fn the_preamble_wide_slot_at_sf7_bw250_is_one_preamble() {
        // SF7/BW250: symbol 0.512 ms, bitrate 10937 bps (not fast), so the
        // 24 ms target is 46.875 symbols, ceil 47, above the floor; 47
        // symbols are 24.064 ms, rounded up to 25 against the reference's
        // 24 ms floor slot.
        assert_eq!(
            leviculum_core::rnode::derive_preamble_symbols(7, 5, 250_000),
            47
        );
        assert_eq!(
            preamble_airtime_ms(250_000, 7, 5),
            (47 * 128 * 1_000_u64).div_ceil(250_000)
        );
        assert_eq!(jitter_slot_ms(250_000, 7, 5), 24);
        assert_eq!(access_slot_ms(250_000, 7, 5, true), 25);
    }

    #[test]
    fn the_preamble_wide_slot_at_sf12_bw125_is_not_clamped_to_the_ceiling() {
        // SF12/BW125: symbol 32.768 ms, the 18-symbol floor binds, 589.824
        // ms of preamble, rounded up to 590. The reference slot stops at its
        // 100 ms ceiling; the arm does not, because the point of the arm is
        // a stagger that clears the preamble, and the ceiling would undo it.
        assert_eq!(
            leviculum_core::rnode::derive_preamble_symbols(12, 5, 125_000),
            18
        );
        assert_eq!(
            preamble_airtime_ms(125_000, 12, 5),
            (18 * 4096 * 1_000_u64).div_ceil(125_000)
        );
        assert_eq!(jitter_slot_ms(125_000, 12, 5), JITTER_SLOT_MAX_MS);
        assert_eq!(access_slot_ms(125_000, 12, 5, true), 590);
    }

    #[test]
    fn the_preamble_wide_slot_never_shrinks_the_reference_slot() {
        for &bw in &[7_800u32, 62_500, 125_000, 250_000, 500_000] {
            for sf in 5..=12u8 {
                for cr in 5..=8u8 {
                    let wide = access_slot_ms(bw, sf, cr, true);
                    assert!(
                        wide >= jitter_slot_ms(bw, sf, cr),
                        "bw={bw} sf={sf} cr={cr}"
                    );
                    assert!(
                        wide >= preamble_airtime_ms(bw, sf, cr),
                        "bw={bw} sf={sf} cr={cr}"
                    );
                }
            }
        }
        // A degenerate modulation has no preamble to widen to, so the slot
        // is the reference's degenerate one.
        assert_eq!(preamble_airtime_ms(0, 8, 5), 0);
        assert_eq!(access_slot_ms(0, 8, 5, true), JITTER_SLOT_MAX_MS);
    }

    #[test]
    fn an_armed_access_counts_every_wait_in_the_preamble_wide_slot() {
        // The order against set_phy does not matter: the firmware arms
        // after bring-up, a reconfiguration re-derives under the arm.
        let mut before = ChannelAccess::new(0x0438_0870);
        before.set_slot_preamble_wide(true);
        before.set_phy(125_000, 8, 5);
        let mut after = ChannelAccess::new(0x0438_0870);
        after.set_phy(125_000, 8, 5);
        after.set_slot_preamble_wide(true);
        assert_eq!(before.jitter_slot(), 37);
        assert_eq!(after.jitter_slot(), 37);
        assert!(after.slot_preamble_wide());
        // The draw is DIFS plus whole slots of 37 ms, the same slot count an
        // unarmed access with the same seed draws.
        let mut plain = ChannelAccess::new(0x0438_0870);
        plain.set_phy(125_000, 8, 5);
        let wide = after.acquisition_jitter_ms();
        let narrow = plain.acquisition_jitter_ms();
        assert_eq!(after.drawn_slots(), plain.drawn_slots());
        assert_eq!(wide, (JITTER_DIFS_SLOTS + after.drawn_slots()) * 37);
        assert_eq!(narrow, (JITTER_DIFS_SLOTS + plain.drawn_slots()) * 24);
        // The CAD backoff counts in the same slot.
        after.jitter_spent(Listened::quiet(wide));
        after.begin_packet();
        match after.cad(CadOutcome::Busy) {
            Verdict::Backoff { slots } => {
                assert_eq!(after.backoff_owed_ms(), slots * 37);
            }
            other => panic!("busy must back off, got {other:?}"),
        }
        // A reconfiguration re-derives under the arm, and disarming
        // returns to the reference slot.
        after.set_phy(250_000, 7, 5);
        assert_eq!(after.jitter_slot(), 25);
        after.set_slot_preamble_wide(false);
        assert_eq!(after.jitter_slot(), 24);
    }

    #[test]
    fn an_access_never_told_draws_in_the_reference_slot() {
        let mut access = ChannelAccess::new(5);
        assert!(!access.slot_preamble_wide());
        access.set_phy(125_000, 8, 5);
        assert_eq!(access.jitter_slot(), jitter_slot_ms(125_000, 8, 5));
        // Arming before any modulation is known keeps the boot slot until
        // set_phy brings one.
        let mut early = ChannelAccess::new(5);
        early.set_slot_preamble_wide(true);
        assert_eq!(early.jitter_slot(), JITTER_SLOT_MIN_MS);
    }

    // --- The post-TX receive window (Codeberg #423) ---------------------

    /// The reply the post-TX window is sized against: a full single-frame
    /// LoRa packet on the wire, header plus the largest unsplit payload,
    /// at the programmed preamble the reference derives for the PHY.
    ///
    /// Computed from `leviculum-core` rather than transcribed, so the rows
    /// below are the numbers the firmware's own call produces and cannot
    /// drift from the airtime formula the radio is charged with. A config
    /// may override the preamble (`preamble_symbols`,
    /// `leviculum-std/src/interfaces/serial.rs`), which moves the rows but
    /// not the coverage: the peer's turnaround is a summand of the window,
    /// so any airtime at all still covers it.
    fn reply_airtime_ms(bw: u32, sf: u8, cr: u8) -> u64 {
        let bytes = (leviculum_core::rnode::MAX_SINGLE_PAYLOAD + 1) as u32;
        let preamble = leviculum_core::rnode::derive_preamble_symbols(sf, cr, bw);
        leviculum_core::rnode::airtime_ms_with_preamble(bytes, bw, sf, cr, preamble)
    }

    /// The window `leviculum-nrf/src/lora.rs` opened before #423: one reply
    /// airtime plus `PACING_MARGIN_MS` plus two slots of the *backoff*
    /// slot, `max(24, airtime(500)/10)`, which was the CAD gate's own slot
    /// and not the one a peer draws its contention window in. Reconstructed
    /// history twice over: that second slot is gone since #147 and the gate
    /// now counts in [`jitter_slot_ms`] like everything else.
    fn window_before_423(bw: u32, sf: u8, cr: u8) -> u64 {
        let backoff_slot = core::cmp::max(
            JITTER_SLOT_MIN_MS,
            leviculum_core::rnode::airtime_ms(500, bw, sf, cr) / 10,
        );
        (reply_airtime_ms(bw, sf, cr) + leviculum_core::rnode::PACING_MARGIN_MS + 2 * backoff_slot)
            .clamp(1, POST_TX_WINDOW_MAX_MS)
    }

    #[test]
    fn budgeting_only_the_peers_difs_closed_the_window_before_it_could_key_up() {
        // The defect of Codeberg #423, as the arithmetic states it. At
        // SF7/500 kHz the pre-#423 window was 270 ms and a peer's widest
        // wait before it keys up is 360 ms, so a peer that drew the top of
        // its contention window keyed after the board had stopped
        // listening. Nothing about the peer is hypothetical here: it draws
        // the window THIS crate hands its own transmit path.
        assert_eq!(window_before_423(500_000, 7, 5), 270);
        assert_eq!(widest_acquisition_wait_ms(500_000, 7, 5), 360);
        assert!(window_before_423(500_000, 7, 5) < widest_acquisition_wait_ms(500_000, 7, 5));

        // And it is the fast PHYs only, which is why the defect survived a
        // corpus that runs at SF7..SF12/125 kHz: there the airtime term is
        // large enough to cover the missing contention window by accident.
        // SF7/250 kHz is the last row that still covers, and by 36 ms.
        assert!(window_before_423(125_000, 7, 5) > widest_acquisition_wait_ms(125_000, 7, 5));
        assert_eq!(
            window_before_423(250_000, 7, 5) - widest_acquisition_wait_ms(250_000, 7, 5),
            36
        );

        // What the term should have been, at the one PHY that broke: the
        // peer's whole wait, not its DIFS.
        assert!(
            post_tx_rx_window_ms(
                reply_airtime_ms(500_000, 7, 5),
                leviculum_core::rnode::PACING_MARGIN_MS,
                500_000,
                7,
                5,
            ) as u64
                >= widest_acquisition_wait_ms(500_000, 7, 5)
        );
    }

    #[test]
    fn the_post_tx_window_is_one_reply_plus_the_peers_whole_turnaround() {
        // The table `docs/src/concepts/csma-transmit-window.md` quotes for
        // question 4, pinned where it is computed. Each row is one reply
        // airtime plus 100 ms of peer host processing plus the peer's
        // widest wait, so a change to any of the three moves a row.
        let window = |bw, sf, cr| {
            post_tx_rx_window_ms(
                reply_airtime_ms(bw, sf, cr),
                leviculum_core::rnode::PACING_MARGIN_MS,
                bw,
                sf,
                cr,
            )
        };
        assert_eq!(window(125_000, 7, 5), 876);
        assert_eq!(window(125_000, 8, 5), 1188);
        assert_eq!(window(125_000, 9, 5), 2127);
        assert_eq!(window(125_000, 10, 5), 3948);
        // At SF12 one reply airtime alone is 9348 ms, so the ceiling binds.
        assert_eq!(window(125_000, 12, 5), POST_TX_WINDOW_MAX_MS as u32);
        assert_eq!(window(250_000, 7, 5), 680);
        assert_eq!(window(500_000, 7, 5), 582);
        assert_eq!(window(500_000, 5, 5), 231);
    }

    #[test]
    fn the_post_tx_window_covers_the_peers_keyup_at_every_modulation() {
        // The property the row-by-row table is one sample of, and the
        // reason the term is a summand rather than a margin that happens
        // to be large enough: the window cannot be shorter than the peer's
        // host turnaround plus the widest wait it can draw, at any
        // modulation a config can ask for.
        let margin = leviculum_core::rnode::PACING_MARGIN_MS;
        for &bw in &[7_800u32, 62_500, 125_000, 250_000, 500_000] {
            for sf in 5..=12u8 {
                for cr in 5..=8u8 {
                    let owed = margin + widest_acquisition_wait_ms(bw, sf, cr);
                    let window =
                        post_tx_rx_window_ms(reply_airtime_ms(bw, sf, cr), margin, bw, sf, cr)
                            as u64;
                    assert!(
                        window >= owed,
                        "bw={bw} sf={sf} cr={cr}: window {window} < owed {owed}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_windows_ceiling_cannot_cut_into_the_peers_turnaround() {
        // The one way the coverage above could stop holding is the clamp,
        // so the ceiling is pinned against the widest turnaround any
        // modulation can produce: DIFS plus the window at the 100 ms slot
        // ceiling, plus the host margin. 1600 ms against 10000 ms.
        let widest_possible = leviculum_core::rnode::PACING_MARGIN_MS
            + (JITTER_DIFS_SLOTS + JITTER_CW_SLOTS as u64 - 1) * JITTER_SLOT_MAX_MS;
        assert_eq!(widest_possible, 1600);
        assert!(POST_TX_WINDOW_MAX_MS >= widest_possible);
        // And a degenerate modulation still yields a window the SX1262
        // accepts: a zero timeout means "listen forever" to the chip.
        assert!(post_tx_rx_window_ms(0, 0, 0, 0, 0) >= 1);
    }

    // --- The acquisition jitter ----------------------------------------

    #[test]
    fn boot_owes_jitter_and_the_draw_is_difs_plus_a_bounded_window() {
        // Every seed must land inside DIFS + 0..=13 slots; run a spread
        // of seeds so the bound is a property, not a lucky draw.
        for seed in 1..=1_000u32 {
            let mut access = ChannelAccess::new(seed);
            access.set_phy(125_000, 7, 5);
            let jitter = access.acquisition_jitter_ms();
            assert!(jitter.is_multiple_of(24), "whole slots only, got {jitter}");
            assert!(
                (48..=48 + 13 * 24).contains(&jitter),
                "seed {seed} drew {jitter}"
            );
        }
    }

    #[test]
    fn jitter_is_owed_once_per_acquisition_not_once_per_packet() {
        let mut access = ChannelAccess::new(7);
        access.set_phy(125_000, 7, 5);
        let drawn = access.acquisition_jitter_ms();
        assert!(drawn > 0);
        // The transmit path listens the wait through; only then does it
        // probe the channel and key up.
        access.jitter_spent(Listened::quiet(drawn));
        assert_eq!(access.acquisition_jitter_ms(), 0);
        // Burst continuation: more packets, same acquisition, no new wait.
        access.begin_packet();
        assert_eq!(access.acquisition_jitter_ms(), 0);
        access.begin_packet();
        assert_eq!(access.acquisition_jitter_ms(), 0);
        // The channel is yielded back: the next acquisition owes again.
        access.channel_released();
        assert!(access.acquisition_jitter_ms() > 0);
    }

    #[test]
    fn a_jitter_wait_cut_short_by_a_reception_is_still_owed() {
        // The minimal reproducer for the bench_dual_pair_fast red of
        // 2026-09-17. The wait is spent listening, so the transmit path's
        // `rx_window` returns the moment a frame arrives: on lnode_a a
        // 288 ms draw was cut at 143 ms by an incoming proof, and the
        // path then went straight to CAD and keyed up 11 ms after that
        // frame ended — phase-locked with every other node the same frame
        // had just released, which is the exact collision the draw exists
        // to break.
        //
        // No transmission happened, so the acquisition is not over and
        // the debt must survive the interruption. Asking is not serving.
        let mut access = ChannelAccess::new(0x5EED_0001);
        access.set_phy(250_000, 7, 5);
        let drawn = access.acquisition_jitter_ms();
        assert!(drawn > 0, "boot owes jitter");
        assert_ne!(
            access.acquisition_jitter_ms(),
            0,
            "a wait that was never served was forgiven by being asked for"
        );

        // Part-served: the remainder is what is still owed, and it is a
        // resume, not a second draw.
        let served = drawn / 3;
        access.jitter_spent(Listened::quiet(served));
        assert_eq!(access.acquisition_jitter_ms(), drawn - served);
        assert!(!access.jitter_was_drawn(), "the resume redrew the window");

        // Listened through: only now may the acquisition probe the channel.
        access.jitter_spent(Listened::quiet(drawn - served));
        assert_eq!(access.acquisition_jitter_ms(), 0);
    }

    /// A seed whose first draw on the bench PHY of #438's kills (SF7, 250
    /// kHz, 24 ms slot) takes `slots` contention slots in band 1.
    fn seed_drawing(slots: u64) -> u32 {
        (1..10_000u32)
            .find(|&seed| {
                let mut access = ChannelAccess::new(seed);
                access.set_phy(250_000, 7, 5);
                access.acquisition_jitter_ms();
                access.drawn_slots() == slots
            })
            .unwrap_or_else(|| panic!("no seed below 10000 draws {slots} slots"))
    }

    #[test]
    fn a_reception_that_ends_the_wait_pays_none_of_it() {
        // 844's mechanism (#438): the wait is one receive window, the window
        // returns when a foreign frame completes, and the whole elapsed was
        // booked as served. A draw cut after `quiet` ms by a frame of
        // `airtime` ms has listened `quiet` ms through and owes the rest.
        let mut access = ChannelAccess::new(seed_drawing(4));
        access.set_phy(250_000, 7, 5);
        let drawn = access.acquisition_jitter_ms();
        assert_eq!(drawn, (JITTER_DIFS_SLOTS + 4) * 24);
        let (quiet, airtime) = (30, 400);
        access.jitter_spent(Listened {
            elapsed_ms: quiet + airtime,
            budget_ms: drawn,
            end: WindowEnd::Frame {
                airtime_ms: airtime,
            },
        });
        assert_eq!(
            access.acquisition_jitter_ms(),
            drawn - quiet,
            "the reception was booked as listened through"
        );
        assert!(!access.jitter_was_drawn(), "the cut redrew the window");
        assert_eq!((access.last_quiet_ms(), access.last_rx_ms()), (30, 400));
    }

    #[test]
    fn two_draws_cut_by_one_frame_keep_their_stagger() {
        // The kill shape itself: draws of 4 and 13 slots both saturated
        // during the same foreign frame, both boards left the wait at that
        // frame end and keyed within a millisecond. Counted over quiet medium
        // only, the frame pauses both waits and the 9 slots between them
        // survive it.
        let mut short = ChannelAccess::new(seed_drawing(4));
        let mut long = ChannelAccess::new(seed_drawing(13));
        let mut owed = [0u64; 2];
        for (access, owed) in [&mut short, &mut long].into_iter().zip(owed.iter_mut()) {
            access.set_phy(250_000, 7, 5);
            let drawn = access.acquisition_jitter_ms();
            // The frame begins 20 ms into both windows and lasts 400 ms,
            // longer than either draw.
            access.jitter_spent(Listened {
                elapsed_ms: 20 + 400,
                budget_ms: drawn,
                end: WindowEnd::Frame { airtime_ms: 400 },
            });
            *owed = access.acquisition_jitter_ms();
        }
        assert_eq!(
            owed[1] - owed[0],
            9 * 24,
            "one frame collapsed the stagger: owed {owed:?}"
        );
    }

    #[test]
    fn a_window_that_ran_out_books_its_budget_whatever_the_overrun() {
        // A timed-out window overruns its budget by the arming turnaround,
        // and a preamble that never became a frame by the stop-on-preamble
        // hold: neither may leave a few-ms resume behind, and neither may
        // be booked as quiet past the budget.
        let mut access = ChannelAccess::new(seed_drawing(4));
        access.set_phy(250_000, 7, 5);
        let drawn = access.acquisition_jitter_ms();
        access.jitter_spent(Listened {
            elapsed_ms: drawn + 3,
            budget_ms: drawn,
            end: WindowEnd::NoFrame,
        });
        assert_eq!(access.acquisition_jitter_ms(), 0);

        let carrier = Listened {
            elapsed_ms: 100 + 500,
            budget_ms: 100,
            end: WindowEnd::NoFrame,
        };
        assert_eq!((carrier.quiet_ms(), carrier.rx_ms()), (100, 500));
    }

    #[test]
    fn a_window_that_heard_nothing_but_a_frame_still_drains_a_millisecond() {
        // A frame already on the air when the window opened leaves no quiet
        // at all; the floor keeps the transmit path from spinning on a
        // channel that never falls silent.
        let window = Listened {
            elapsed_ms: 150,
            budget_ms: 300,
            end: WindowEnd::Frame { airtime_ms: 400 },
        };
        assert_eq!((window.quiet_ms(), window.rx_ms()), (1, 150));
    }

    #[test]
    fn a_backoff_cut_by_a_reception_is_still_owed() {
        // `Verdict::Backoff` used to listen its window and go straight back
        // to CAD whatever ended it, so a backed-off node re-probed at the end
        // of the frame that cut it short, with everyone else that frame
        // released.
        let mut access = bench_access(0x5EED_0845);
        let slots = loop {
            match access.cad(CadOutcome::Busy) {
                Verdict::Backoff { slots } if slots >= 2 => break slots,
                Verdict::Backoff { .. } => {}
                other => panic!("expected a backoff, got {other:?}"),
            }
        };
        let window = slots * 24;
        assert_eq!(access.backoff_owed_ms(), window);
        assert!(
            !access.backoff_was_cut(),
            "a fresh backoff reads as a resume"
        );
        access.backoff_spent(Listened {
            elapsed_ms: 10 + 200,
            budget_ms: window,
            end: WindowEnd::Frame { airtime_ms: 200 },
        });
        assert_eq!(
            access.backoff_owed_ms(),
            window - 10,
            "the reception was booked as backoff served"
        );
        assert!(access.backoff_was_cut());
        access.backoff_spent(Listened::quiet(window - 10));
        assert_eq!(access.backoff_owed_ms(), 0);

        // A remainder belongs to its packet: the next one starts owing none.
        access.cad(CadOutcome::Busy);
        access.begin_packet();
        assert_eq!(access.backoff_owed_ms(), 0);
    }

    #[test]
    fn spending_more_than_is_owed_does_not_wrap_the_debt() {
        // The caller reports wall-clock elapsed, which overshoots the
        // window by the radio's own turnaround; that must land on zero,
        // not on u64::MAX minus epsilon.
        let mut access = ChannelAccess::new(0x5EED_0002);
        access.set_phy(250_000, 7, 5);
        let drawn = access.acquisition_jitter_ms();
        access.jitter_spent(Listened::quiet(drawn + 10_000));
        assert_eq!(access.acquisition_jitter_ms(), 0);
    }

    #[test]
    fn a_release_redraws_rather_than_resuming_a_part_served_wait() {
        // A yield handed the channel back: that is a new acquisition and
        // owes a full fresh draw, not the leftover of an abandoned one.
        let mut access = ChannelAccess::new(0x5EED_0003);
        access.set_phy(250_000, 7, 5);
        let drawn = access.acquisition_jitter_ms();
        access.jitter_spent(Listened::quiet(drawn / 2));
        access.channel_released();
        let redrawn = access.acquisition_jitter_ms();
        assert!(access.jitter_was_drawn(), "a release did not redraw");
        assert!(
            redrawn >= JITTER_DIFS_SLOTS * access.jitter_slot(),
            "a fresh acquisition drew only {redrawn}, i.e. a leftover"
        );
    }

    #[test]
    fn the_same_seed_draws_the_same_jitter_and_different_seeds_de_tile() {
        // Determinism is what makes the draw testable; per-board seeds are
        // what make it useful. Both properties in one place: identical
        // seeds reproduce, and across a small population of seeds more
        // than one distinct value appears.
        let draw = |seed: u32| {
            let mut access = ChannelAccess::new(seed);
            access.set_phy(125_000, 7, 5);
            access.acquisition_jitter_ms()
        };
        assert_eq!(draw(42), draw(42));
        let mut distinct = [false; 14];
        for seed in 1..=100u32 {
            distinct[((draw(seed) - 48) / 24) as usize] = true;
        }
        assert!(
            distinct.iter().filter(|d| **d).count() > 1,
            "a hundred boards all drew the same slot"
        );
    }

    #[test]
    fn a_zero_seed_is_replaced_not_a_fixed_point() {
        // xorshift32(0) == 0 forever; the constructor must not let a
        // zeroed RNG turn the jitter into a constant.
        let mut access = ChannelAccess::new(0);
        access.set_phy(125_000, 7, 5);
        let first = access.acquisition_jitter_ms();
        access.channel_released();
        let second = access.acquisition_jitter_ms();
        access.channel_released();
        let third = access.acquisition_jitter_ms();
        // Not all three equal — the RNG is stepping.
        assert!(
            !(first == second && second == third),
            "zero-seeded RNG is stuck at {first}"
        );
    }

    #[test]
    fn the_draw_is_roughly_uniform_over_the_fourteen_slots() {
        // 14_000 draws over the 14 values: each bucket should be near
        // 1000. A crude 3-sigma-ish band suffices to catch a modulo or
        // shift mistake without turning the test statistical.
        let mut access = ChannelAccess::new(0xC0FF_EE11);
        access.set_phy(125_000, 7, 5);
        let mut buckets = [0u32; 14];
        for _ in 0..14_000 {
            access.channel_released();
            let jitter = access.acquisition_jitter_ms();
            buckets[((jitter - 48) / 24) as usize] += 1;
        }
        for (slot, count) in buckets.iter().enumerate() {
            assert!(
                (900..=1100).contains(count),
                "slot {slot} drew {count} of 14000"
            );
        }
    }

    // --- The CAD retry gate --------------------------------------------

    #[test]
    fn a_clear_channel_transmits_unforced() {
        let mut access = ChannelAccess::new(3);
        access.begin_packet();
        assert_eq!(
            access.cad_clear(),
            Verdict::Transmit {
                forced: false,
                retries: 0
            }
        );
    }

    #[test]
    fn busy_backs_off_inside_the_doubling_window_and_then_gives_up() {
        // The scripted radio: busy on every probe. The gate must back off
        // with slot counts inside the advertised window (2, 4, 8, ... 64)
        // and force the TX on the 8th attempt — the frame must not starve.
        let mut access = ChannelAccess::new(0xB00_57ED);
        access.begin_packet();
        let mut cw = CAD_CW_INITIAL as u64;
        for attempt in 1..CAD_MAX_RETRIES {
            match access.cad_busy() {
                Verdict::Backoff { slots } => {
                    assert!(slots < cw, "attempt {attempt}: {slots} >= window {cw}");
                    cw = core::cmp::min(cw * 2, CAD_CW_MAX as u64);
                }
                other => panic!("attempt {attempt}: expected backoff, got {other:?}"),
            }
        }
        assert_eq!(
            access.cad_busy(),
            Verdict::Transmit {
                forced: true,
                retries: CAD_MAX_RETRIES
            }
        );
    }

    #[test]
    fn a_new_packet_resets_the_retry_budget() {
        let mut access = ChannelAccess::new(9);
        access.begin_packet();
        for _ in 0..CAD_MAX_RETRIES {
            access.cad_busy();
        }
        access.begin_packet();
        assert_eq!(access.retries(), 0);
        // With a fresh budget a busy can never force.
        assert!(matches!(access.cad_busy(), Verdict::Backoff { .. }));
    }

    #[test]
    fn cad_errors_spend_the_same_budget_and_still_transmit() {
        // A radio that cannot CAD (SPI fault, chip wedged) must not
        // starve the frame either: errors retry immediately and force at
        // the same bound.
        let mut access = ChannelAccess::new(11);
        access.begin_packet();
        for _ in 1..CAD_MAX_RETRIES {
            assert_eq!(access.cad_error(), Verdict::Retry);
        }
        assert_eq!(
            access.cad_error(),
            Verdict::Transmit {
                forced: true,
                retries: CAD_MAX_RETRIES
            }
        );
    }

    #[test]
    fn busy_and_error_mix_shares_one_budget() {
        // 4 busies and 4 errors together exhaust the 8-attempt budget.
        let mut access = ChannelAccess::new(13);
        access.begin_packet();
        for _ in 0..4 {
            assert!(matches!(access.cad_busy(), Verdict::Backoff { .. }));
        }
        for _ in 0..3 {
            assert_eq!(access.cad_error(), Verdict::Retry);
        }
        assert_eq!(
            access.cad_error(),
            Verdict::Transmit {
                forced: true,
                retries: CAD_MAX_RETRIES
            }
        );
    }

    // --- Per-board seeding (Codeberg #268) ------------------------------

    /// The number of backoffs a board walks before the retry budget forces
    /// the transmission: every attempt but the last yields a `Backoff`.
    const LADDER_LEN: usize = CAD_MAX_RETRIES as usize - 1;

    /// The backoff ladder one board walks when every CAD comes back busy,
    /// driven exactly the way `lora_task` drives this type: construct from
    /// the board's seed, adopt the bench PHY, start a packet, spend the
    /// acquisition jitter (it comes off the same stream, so a ladder taken
    /// without it is a different sequence), then CAD until the gate forces.
    fn busy_backoff_ladder(seed: u32) -> [u64; LADDER_LEN] {
        let mut access = ChannelAccess::new(seed);
        access.set_phy(125_000, 7, 5);
        access.begin_packet();
        let _ = access.acquisition_jitter_ms();
        let mut ladder = [0u64; LADDER_LEN];
        for (attempt, slots) in ladder.iter_mut().enumerate() {
            match access.cad_busy() {
                Verdict::Backoff { slots: drawn } => *slots = drawn,
                other => panic!("attempt {attempt}: expected a backoff, got {other:?}"),
            }
        }
        assert!(
            matches!(access.cad_busy(), Verdict::Transmit { forced: true, .. }),
            "the ladder must end at the forced TX"
        );
        ladder
    }

    #[test]
    fn a_board_walks_the_same_ladder_twice_from_its_own_seed() {
        // Per-board entropy must not cost reproducibility: a seed still
        // determines the whole sequence, which is what lets a host test
        // assert exact decisions and a rig run be replayed.
        assert_eq!(
            busy_backoff_ladder(0x5EED_1234),
            busy_backoff_ladder(0x5EED_1234)
        );
        assert_eq!(busy_backoff_ladder(1), busy_backoff_ladder(1));
    }

    #[test]
    fn two_boards_with_their_own_seeds_do_not_share_a_backoff_ladder() {
        // Codeberg #268: every board seeded this PRNG with one compile-time
        // constant, so two boards with equal draw counts — the normal case
        // early in a scenario, where the rig powers them together and they
        // react to the same third-party frame — drew byte-identical
        // backoffs and collided again on every one of the eight retries.
        //
        // The defect is stated first, so the test reads as a claim about
        // seeding rather than about xorshift32: one shared seed IS one
        // shared ladder, and the fix is that boards no longer share the
        // seed.
        assert_eq!(
            busy_backoff_ladder(0xDEAD_BEEF),
            busy_backoff_ladder(0xDEAD_BEEF)
        );

        // Distinct seeds: no pair of boards may walk the same ladder. The
        // population is deliberately more than a handful — the first
        // backoff is drawn from a window of CAD_CW_INITIAL slots, so two
        // boards agreeing on their FIRST retry is expected roughly half
        // the time and only the full ladder separates them.
        const BOARDS: usize = 64;
        let ladders: [[u64; LADDER_LEN]; BOARDS] = core::array::from_fn(|i| {
            busy_backoff_ladder(0x9E37_79B9u32.wrapping_mul(i as u32 + 1))
        });
        for (i, a) in ladders.iter().enumerate() {
            for (j, b) in ladders.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "boards {i} and {j} walk the same ladder {a:?}");
            }
        }
    }

    #[test]
    fn the_jitter_a_board_owes_is_drawn_from_the_same_stream_as_its_backoffs() {
        // #268 is one seed feeding both draws, so a fix that de-tiled the
        // jitter but left the backoff on a second, still-shared stream
        // would look fixed and collide on every retry. Spending the jitter
        // has to move the ladder.
        let mut with_jitter = ChannelAccess::new(0x00C0_FFEE);
        with_jitter.set_phy(125_000, 7, 5);
        with_jitter.begin_packet();
        let _ = with_jitter.acquisition_jitter_ms();

        let mut without_jitter = ChannelAccess::new(0x00C0_FFEE);
        without_jitter.set_phy(125_000, 7, 5);
        without_jitter.begin_packet();

        let mut moved = false;
        for _ in 0..LADDER_LEN {
            match (with_jitter.cad_busy(), without_jitter.cad_busy()) {
                (Verdict::Backoff { slots: a }, Verdict::Backoff { slots: b }) => moved |= a != b,
                (a, b) => panic!("expected two backoffs, got {a:?} and {b:?}"),
            }
        }
        assert!(moved, "the backoff ladder ignored the jitter draw");
    }

    // --- The CAD outcome, the key-up that follows, and the backoff window
    //     (leviculum#413) ------------------------------------------------

    /// A gate on the bench PHY, fresh for a packet.
    fn bench_access(seed: u32) -> ChannelAccess {
        let mut access = ChannelAccess::new(seed);
        access.set_phy(125_000, 7, 5);
        access.begin_packet();
        access
    }

    #[test]
    fn the_single_cad_entry_gives_each_outcome_its_own_arm() {
        // Against a twin fed through the three named calls, so the entry
        // adds no rule of its own.
        let mut via_entry = bench_access(0x5EED_0413);
        let mut named = bench_access(0x5EED_0413);
        let script = [
            CadOutcome::Busy,
            CadOutcome::Error,
            CadOutcome::Busy,
            CadOutcome::Clear,
        ];
        for outcome in script {
            let expected = match outcome {
                CadOutcome::Clear => named.cad_clear(),
                CadOutcome::Busy => named.cad_busy(),
                CadOutcome::Error => named.cad_error(),
            };
            assert_eq!(via_entry.cad(outcome), expected, "{outcome:?}");
        }
        // Busy, error, busy: three attempts spent, then a clear transmits
        // unforced with them on the line.
        assert_eq!(via_entry.retries(), 3);
    }

    #[test]
    fn a_radio_that_cannot_cad_is_retried_at_once_then_forced_out() {
        let mut access = bench_access(0x5EED_0180);
        for _ in 1..CAD_MAX_RETRIES {
            assert_eq!(access.cad(CadOutcome::Error), Verdict::Retry);
        }
        assert_eq!(
            access.cad(CadOutcome::Error),
            Verdict::Transmit {
                forced: true,
                retries: CAD_MAX_RETRIES
            }
        );
    }

    #[test]
    fn only_a_key_up_forced_by_cad_errors_skips_the_ack_window() {
        assert_eq!(after_key_up(CadOutcome::Clear), AfterKeyUp::AckWindow);
        assert_eq!(after_key_up(CadOutcome::Busy), AfterKeyUp::AckWindow);
        assert_eq!(after_key_up(CadOutcome::Error), AfterKeyUp::NextTurn);
    }

    #[test]
    fn a_backoff_is_listened_through_in_whole_csma_slots() {
        // SF7/BW125: the 24 ms slot floor. SF12/BW125: the 100 ms ceiling.
        let access = bench_access(1);
        assert_eq!(access.backoff_window_ms(3), 3 * 24);
        let mut slow = ChannelAccess::new(1);
        slow.set_phy(125_000, 12, 5);
        assert_eq!(slow.backoff_window_ms(63), 6_300);
    }

    #[test]
    fn a_backoff_of_no_slots_still_arms_a_window() {
        // `slots` is drawn from 0..cw, so zero is an ordinary draw; a zero
        // timeout would arm the radio with no timeout at all.
        assert_eq!(bench_access(1).backoff_window_ms(0), 1);
        assert_eq!(listen_window_ms(0), 1);
    }

    #[test]
    fn no_listening_wait_parks_the_transmit_path_past_the_ceiling() {
        assert_eq!(listen_window_ms(LISTEN_WINDOW_MAX_MS), 10_000);
        assert_eq!(listen_window_ms(LISTEN_WINDOW_MAX_MS + 1), 10_000);
        assert_eq!(bench_access(1).backoff_window_ms(u64::MAX), 10_000);
        // And the widest wait the gate can ask for at the slowest PHY is
        // below it, so the ceiling never shortens a real backoff.
        let mut slow = ChannelAccess::new(1);
        slow.set_phy(125_000, 12, 8);
        let widest = (CAD_CW_MAX as u64 - 1) * slow.jitter_slot();
        assert!(widest < LISTEN_WINDOW_MAX_MS, "{widest}");
        assert_eq!(slow.backoff_window_ms(CAD_CW_MAX as u64 - 1), widest as u32);
    }

    // --- The airtime band (leviculum#637, audit 2026-10-11 D3) ---------

    /// The slots an acquisition at `airtime_bp` draws, over a spread of
    /// seeds and channel releases: (lowest, highest, widest wait in ms).
    fn band_draws(airtime_bp: u16, bw: u32, sf: u8, cr: u8) -> (u64, u64, u64) {
        let (mut lo, mut hi, mut widest) = (u64::MAX, 0, 0);
        for seed in 1..=200u32 {
            let mut access = ChannelAccess::new(seed);
            access.set_phy(bw, sf, cr);
            access.set_airtime_short(airtime_bp);
            for _ in 0..20 {
                access.channel_released();
                let wait = access.acquisition_jitter_ms();
                assert!(access.jitter_was_drawn());
                let slot = access.jitter_slot();
                assert!(wait.is_multiple_of(slot), "whole slots only, got {wait}");
                let slots = wait / slot - JITTER_DIFS_SLOTS;
                lo = lo.min(slots);
                hi = hi.max(slots);
                widest = widest.max(wait);
            }
        }
        (lo, hi, widest)
    }

    #[test]
    fn the_band_follows_the_reference_map_to_the_percent() {
        // `update_csma_parameters` (RNode_Firmware.ino:1603-1619): at most
        // 7 % is band 1; above it, `map(pct + 7, 7, 85, 2, 4)`, Arduino's
        // truncating `(x - 7) * 2 / 78 + 2`, capped at 4. The +7 shift puts
        // the band edges at 39 % and 78 %, not at the constants' 85 %.
        let pct = |p: u16| p * 100;
        assert_eq!(csma_band(0), 1);
        assert_eq!(csma_band(pct(7)), 1);
        // `(int)(airtime*100)` truncates, so 7.99 % is still 7.
        assert_eq!(csma_band(pct(8) - 1), 1);
        assert_eq!(csma_band(pct(8)), 2);
        assert_eq!(csma_band(pct(38) + 99), 2);
        assert_eq!(csma_band(pct(39)), 3);
        assert_eq!(csma_band(pct(77) + 99), 3);
        assert_eq!(csma_band(pct(78)), 4);
        assert_eq!(csma_band(pct(100)), 4);
        assert_eq!(csma_band(u16::MAX), 4);
        // Each band's `cw_min = (band - 1) * 15`, `cw_max = band * 15 - 1`.
        assert_eq!(band_window(1), (0, 14));
        assert_eq!(band_window(2), (15, 29));
        assert_eq!(band_window(3), (30, 44));
        assert_eq!(band_window(4), (45, 59));
        // Band 1 is the window the jitter has always drawn in.
        assert_eq!(band_window(1).1 - band_window(1).0, JITTER_CW_SLOTS);
    }

    #[test]
    fn an_acquisition_draws_in_the_band_of_its_own_airtime() {
        // The red this order opened on: the window was band 1 whatever the
        // board had keyed, so beside reference modems that widen theirs a
        // busy board keyed up in less than its share of the wait. Airtime
        // 0, 30, 60 and 90 % land in bands 1, 2, 3 and 4, each drawn
        // uniformly over `random(cw_min, cw_max)`, upper-exclusive
        // (RNode_Firmware.ino:1626): every edge is reached, none passed.
        for (bp, band, lo, hi) in [
            (0u16, 1u8, 0u64, 13u64),
            (3_000, 2, 15, 28),
            (6_000, 3, 30, 43),
            (9_000, 4, 45, 58),
        ] {
            let (got_lo, got_hi, _) = band_draws(bp, 125_000, 7, 5);
            assert_eq!((got_lo, got_hi), (lo, hi), "airtime {bp} bp");
            let mut access = ChannelAccess::new(1);
            access.set_airtime_short(bp);
            access.acquisition_jitter_ms();
            assert_eq!(access.drawn_band(), band, "airtime {bp} bp");
        }
    }

    #[test]
    fn below_the_first_edge_the_draws_are_the_band_one_draws_of_before() {
        // At or below 7 % nothing changes: the same seed draws the same
        // waits whether the board was told its airtime or not, so a board
        // on a quiet channel keys exactly as it did, and so does the host's
        // RNode interface, which never tells it.
        for bp in [0u16, 400, 700, 799] {
            let mut told = ChannelAccess::new(0x0637);
            let mut untold = ChannelAccess::new(0x0637);
            told.set_airtime_short(bp);
            for _ in 0..50 {
                told.channel_released();
                untold.channel_released();
                assert_eq!(told.acquisition_jitter_ms(), untold.acquisition_jitter_ms());
                assert_eq!(told.drawn_band(), 1);
            }
        }
    }

    #[test]
    fn the_band_is_taken_at_the_draw_and_kept_for_the_acquisition() {
        // The reference draws `csma_cw` once per contention and keeps it
        // until the queue is flushed (RNode_Firmware.ino:1625-1646); a band
        // change mid-wait moves the next contention, not this one.
        let mut access = ChannelAccess::new(9);
        access.set_airtime_short(9_000);
        let drawn = access.acquisition_jitter_ms();
        access.set_airtime_short(0);
        access.jitter_spent(Listened::quiet(1));
        assert_eq!(access.acquisition_jitter_ms(), drawn - 1);
        assert!(!access.jitter_was_drawn());
        assert_eq!(access.drawn_band(), 4);
        access.channel_released();
        access.acquisition_jitter_ms();
        assert_eq!(access.drawn_band(), 1);
    }

    #[test]
    fn the_widest_band_draw_stays_inside_the_listening_ceiling() {
        // Band 4 at the 100 ms slot ceiling: DIFS plus 58 slots, 6 s.
        let (_, hi, widest) = band_draws(load::FULL_BP as u16, 125_000, 12, 8);
        assert_eq!(hi, 58);
        assert_eq!(widest, (JITTER_DIFS_SLOTS + 58) * JITTER_SLOT_MAX_MS);
        assert!(widest <= LISTEN_WINDOW_MAX_MS, "{widest}");
        assert_eq!(listen_window_ms(widest), widest as u32);
    }

    // --- The sub-slot offset (#438) -------------------------------------

    /// The draws a seeded access makes over sixteen acquisitions, each
    /// followed by one busy CAD: `[wait_ms, backoff_slots, ...]`.
    fn acquisition_trace(access: &mut ChannelAccess) -> [u64; 32] {
        let mut out = [0u64; 32];
        for pair in out.chunks_mut(2) {
            access.channel_released();
            pair[0] = access.acquisition_jitter_ms();
            access.begin_packet();
            pair[1] = match access.cad_busy() {
                Verdict::Backoff { slots } => slots,
                other => panic!("one busy CAD cannot exhaust the budget: {other:?}"),
            };
        }
        out
    }

    #[test]
    fn with_the_arm_off_the_draws_are_the_ones_before_it_existed() {
        // Recorded off the code BEFORE the arm was added (order 824), seed
        // 0x1234_5678 at SF7/BW250 (24 ms slot). The busy CAD after every
        // acquisition pins the stream behind the draw too: an arm that took
        // a random number while off would shift every backoff here.
        const GOLDEN: [u64; 32] = [
            168, 1, 192, 0, 48, 1, 312, 1, 264, 1, 192, 0, 96, 1, 144, 1, 240, 1, 216, 0, 264, 0,
            240, 1, 144, 1, 96, 1, 96, 1, 336, 0,
        ];
        let mut never_told = ChannelAccess::new(0x1234_5678);
        never_told.set_phy(250_000, 7, 5);
        assert_eq!(acquisition_trace(&mut never_told), GOLDEN);

        let mut told_off = ChannelAccess::new(0x1234_5678);
        told_off.set_phy(250_000, 7, 5);
        told_off.set_subslot_offset(true);
        told_off.set_subslot_offset(false);
        assert_eq!(acquisition_trace(&mut told_off), GOLDEN);
        assert_eq!(told_off.drawn_offset_ms(), 0);
    }

    /// `n` fresh acquisitions of an armed access: `(slots, offset_ms, wait_ms)`.
    fn armed_draws(seed: u32, bw: u32, sf: u8, n: usize) -> impl Iterator<Item = (u64, u64, u64)> {
        let mut access = ChannelAccess::new(seed);
        access.set_phy(bw, sf, 5);
        access.set_subslot_offset(true);
        (0..n).map(move |_| {
            access.channel_released();
            let wait = access.acquisition_jitter_ms();
            (access.drawn_slots(), access.drawn_offset_ms(), wait)
        })
    }

    #[test]
    fn an_armed_wait_is_the_slotted_draw_plus_its_offset() {
        let slot = jitter_slot_ms(250_000, 7, 5);
        let mut first_of_slot: [Option<(u64, u64)>; JITTER_CW_SLOTS as usize] =
            [None; JITTER_CW_SLOTS as usize];
        let mut separated = 0;
        for (slots, offset, wait) in armed_draws(0x1234_5678, 250_000, 7, 2_000) {
            assert_eq!(wait, (JITTER_DIFS_SLOTS + slots) * slot + offset);
            // Two acquisitions that drew the same slot differ by exactly
            // their offsets: the tie the arm exists to break.
            match first_of_slot[slots as usize] {
                None => first_of_slot[slots as usize] = Some((offset, wait)),
                Some((first_offset, first_wait)) => {
                    assert_eq!(wait.abs_diff(first_wait), offset.abs_diff(first_offset));
                    if offset != first_offset {
                        separated += 1;
                    }
                }
            }
        }
        // 1986 same-slot repeats; with 24 offsets about 1/24 of them land on
        // the first one's offset. All but a handful must be apart.
        assert!(separated > 1_800, "{separated}");
    }

    #[test]
    fn the_offset_never_reaches_a_slot_at_any_slot_width() {
        // The fast floor, the bench floor, between the clamps, the ceiling.
        for (bw, sf, slot) in [
            (500_000, 5, 6),
            (250_000, 7, 24),
            (125_000, 10, 98),
            (125_000, 12, 100),
        ] {
            assert_eq!(jitter_slot_ms(bw, sf, 5), slot);
            assert_eq!(subslot_offset_max_ms(bw, sf, 5), slot - 1);
            let widest = armed_draws(0xC0FF_EE01, bw, sf, 10_000)
                .map(|(_, offset, _)| offset)
                .max();
            // Every value is reached, the top one included, and none past it.
            assert_eq!(widest, Some(slot - 1), "slot {slot}");
        }
    }

    #[test]
    fn the_offset_is_flat_over_the_slot_with_mean_half_a_slot() {
        let slot = jitter_slot_ms(250_000, 7, 5) as usize;
        const N: usize = 10_000;
        let mut counts = [0u32; 24];
        let mut sum = 0u64;
        for (_, offset, _) in armed_draws(0x0BAD_5EED, 250_000, 7, N) {
            counts[offset as usize] += 1;
            sum += offset;
        }
        // Whole milliseconds over [0, slot): the mean is (slot - 1) / 2 =
        // 11.5 ms, half a slot less the half millisecond the integer draw
        // rounds away. Its standard error over 10 000 draws is 0.07 ms.
        let mean = sum as f64 / N as f64;
        assert!(
            (mean - (slot as f64 - 1.0) / 2.0).abs() < 0.3,
            "mean {mean}"
        );
        // Flat: chi-square over the 24 values, 23 degrees of freedom, below
        // the p = 0.001 critical value 49.73.
        let expected = N as f64 / slot as f64;
        let chi2: f64 = counts
            .iter()
            .map(|&c| (c as f64 - expected) * (c as f64 - expected) / expected)
            .sum();
        assert!(chi2 < 49.73, "chi2 {chi2} counts {counts:?}");
    }

    #[test]
    fn the_offset_is_independent_of_the_slot_it_follows() {
        // The offset is the next number of the same xorshift stream as the
        // slot draw. Were the two correlated, the offsets of two equal slot
        // draws would cluster and the tie would survive. Chi-square over the
        // 14 x 24 joint cells, 335 degrees of freedom: below the p = 0.001
        // critical value, about 422.
        const N: usize = 50_000;
        let mut joint = [[0u32; 24]; JITTER_CW_SLOTS as usize];
        for (slots, offset, _) in armed_draws(0x5EED_1438, 250_000, 7, N) {
            joint[slots as usize][offset as usize] += 1;
        }
        let expected = N as f64 / (JITTER_CW_SLOTS as f64 * 24.0);
        let chi2: f64 = joint
            .iter()
            .flatten()
            .map(|&c| (c as f64 - expected) * (c as f64 - expected) / expected)
            .sum();
        assert!(chi2 < 422.0, "chi2 {chi2}");
    }
}
