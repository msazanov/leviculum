//! Shared state for RAK19026 baseboard peripherals (display + GNSS + battery).
//!
//! This module is the rendezvous point between the producer tasks (GNSS,
//! battery) and the consumer task (display). It is compiled whenever any
//! one of the three feature flags is enabled; each individual `Watch` /
//! struct definition is then gated on the producing peripheral's feature.

#![allow(dead_code)]

#[cfg(any(feature = "display", feature = "gnss", feature = "battery"))]
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
#[cfg(any(feature = "display", feature = "gnss", feature = "battery"))]
use embassy_sync::watch::Watch;

/// Compact GNSS fix snapshot, updated by `gnss::gnss_task` when an RMC or
/// GGA sentence parses cleanly. The struct and its fold of each sentence
/// are host-tested in `leviculum-gnss-time` (`fix`).
#[cfg(feature = "gnss")]
pub use leviculum_gnss_time::GnssFix;

/// Latest GNSS fix snapshot. Capacity-3 watch: one slot for the producer,
/// one for the display consumer, one for the main loop's calendar
/// seeding (#166).
#[cfg(feature = "gnss")]
pub static GNSS_FIX: Watch<CriticalSectionRawMutex, GnssFix, 3> = Watch::new();

/// The runtime GNSS presence answer (Codeberg #240), re-exported from
/// the pure crate that owns the sweep/hysteresis policy.
#[cfg(feature = "gnss")]
pub use leviculum_gnss_presence::Presence as GnssPresence;

/// Settled GNSS presence snapshot, published by `gnss::gnss_task` on
/// every state transition. The watch starts empty — "no value yet" IS
/// the detection phase, exactly like `GNSS_FIX` before the first
/// sentence.
///
/// Only [`GnssPresence::Fix`] may feed a position or a timebase;
/// [`GnssPresence::NoFix`] and [`GnssPresence::NoHardware`] are
/// operator-facing diagnoses (wait, versus check the wiring).
#[cfg(feature = "gnss")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GnssPresenceState {
    pub state: GnssPresence,
    /// The locked baud rate; 0 while `NoHardware` (no baud found).
    pub baud: u32,
}

/// Latest presence snapshot. Capacity-3 watch, same shape as
/// `GNSS_FIX`: display consumer, the #236 position tracker, one spare.
#[cfg(feature = "gnss")]
pub static GNSS_PRESENCE: Watch<CriticalSectionRawMutex, GnssPresenceState, 3> = Watch::new();

/// What the accelerometer says, as the GNSS task reads it (#457 step
/// two), re-exported from the pure crate whose `PowerGate` decides on it.
#[cfg(feature = "gnss")]
pub use leviculum_gnss_presence::Motion;

/// The board's moving/still state, written by `motion::motion_task` on
/// each `MOTION` edge and read by `gnss::gnss_task`, which sleeps the
/// receiver on `Still` and wakes it on `Moving`.
///
/// A `Watch`, like the GNSS and battery snapshots above: the reader wants
/// the current state, not every edge (an edge it misses is one it would
/// have undone), and `changed()` is a future the GNSS task can select
/// against its UART read. It starts empty and stays empty on a board
/// without the part or whose part did not answer (`MOTION_INIT absent`),
/// and an empty watch is a receiver that runs as it always did.
/// Capacity 1: the GNSS task is the one reader.
#[cfg(feature = "gnss")]
pub static MOTION_STATE: Watch<CriticalSectionRawMutex, Motion, 1> = Watch::new();

/// Battery snapshot published by the SAADC task and consumed by the display.
///
/// `voltage_mv` is the pack voltage in millivolts at the battery terminal
/// (already through the board's own divider multiplier — 1.73 on the
/// Pocket V2, 4.916 on the T114). `percent` is mapped from `voltage_mv`
/// via the per-cell LiPo OCV curve in `leviculum-battery-scale`, scaled
/// by the detected cell count. `cell_count` is 1 or 2, classified from
/// the first reading of each boot; it is not persisted, so a boot whose
/// first reading is implausible falls back to 1S and says so.
///
/// `percent` is `None` when `voltage_mv` is outside the band `cell_count`
/// implies (`leviculum_battery_scale::pack_percent`): the percentage is
/// derived through the classification, so a pack the classification
/// cannot explain gets no percentage rather than a wrong one. The voltage
/// is never withheld — it is a measurement, not a derivation, and it is
/// what a reader can still act on.
#[cfg(feature = "battery")]
#[derive(Clone, Copy, Debug)]
pub struct BatteryState {
    pub voltage_mv: u16,
    pub percent: Option<u8>,
    pub cell_count: u8,
}

#[cfg(feature = "battery")]
impl BatteryState {
    pub const fn empty() -> Self {
        Self {
            voltage_mv: 0,
            percent: None,
            cell_count: 1,
        }
    }
}

/// Latest battery snapshot. Same capacity-2 watch shape as `GNSS_FIX`.
#[cfg(feature = "battery")]
pub static BATTERY_STATE: Watch<CriticalSectionRawMutex, BatteryState, 2> = Watch::new();

/// Raised by the user-button task on every press (a debounced glitch is
/// not one) and taken by the display task, which owns the display's power
/// state and its one-minute timeout (`leviculum_screen::power::DisplayWake`).
/// A `Signal` rather than a watch because a press carries no value: two
/// presses the display task has not seen yet ask for the same thing as one.
#[cfg(feature = "display")]
pub static DISPLAY_WAKE: embassy_sync::signal::Signal<CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();

/// One-shot signals raised by `lora.rs` whenever a packet boundary
/// completes. The LED pulse tasks in `led.rs` block on these and emit a
/// short flash each time. Coalescing is intentional — a Signal stores at
/// most one pending event, so a burst of TX-completes inside an 80 ms
/// window collapses to a single flash. That matches what the eye can
/// resolve anyway.
#[cfg(feature = "display")]
pub static LORA_TX_FLASH: embassy_sync::signal::Signal<CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();

#[cfg(feature = "display")]
pub static LORA_RX_FLASH: embassy_sync::signal::Signal<CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();
