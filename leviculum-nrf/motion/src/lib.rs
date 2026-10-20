//! The SenseCAP T1000-E's QMA6100P accelerometer, read as one bit: is the
//! board moving or still (Codeberg #457, step one of two).
//!
//! # What this is for, and what it is not
//!
//! Lew's decision of 2026-10-20: accelerometer motion **gates** the GNSS
//! movement proof of #401, it does not replace it and does not add to it.
//! The announce rule (three consecutive fixes past 50 m, the HDOP gate, the
//! 15 minute cap, `leviculum-announce-policy`'s `MovementDetector`) stays
//! exactly as it is. Step one, this crate, is the driver and a
//! `MOTION state=moving|still` line with no effect on anything. Step two
//! lets the GNSS receiver sleep while the board is still and wakes it on
//! motion; the displacement proof then runs as it does today. The design
//! record is `docs/src/concepts/announce-cadence.md`.
//!
//! # Sources, and their standing
//!
//! Seeed publishes no T1000-E schematic, so the wiring is a foreign
//! firmware hint (Meshtastic, `variants/nrf52840/tracker-t1000-e/`): the
//! bus on SDA P0.26 / SCL P0.27 (`PIN_WIRE_SDA`, `PIN_WIRE_SCL`), the part
//! at 0x12 (`QMA6100P_ADDR`, `src/configuration.h:253`), its rail on P1.07
//! (`PIN_3V3_ACC_EN`, raised by `initVariant`) and its INT1 on P1.02
//! (`QMA_6100P_INT_PIN`). The registers are the part's own, from the QST
//! QMA6100P preliminary datasheet (document 13-52-20, rev. A1, §9 "Register
//! Definition"), which ships inside the Arduino library Meshtastic pins
//! (`meshtastic/QMA6100P_Arduino_Library` at 14c900b8,
//! `Documentation/QMA6100P/2208161803_QST-QMA6100P_C2887190.pdf`).
//!
//! # Why it polls, although INT1 is wired
//!
//! The part has an any-motion engine and the variant wires its INT1, so the
//! order for this pass preferred the interrupt. Two findings argue against
//! building on it before the rig has seen it work:
//!
//! - **The one running reference maps the wrong bit.** Meshtastic sets
//!   `int1_any_mot` through the library's bitfield, which declares it as
//!   the LAST member of `sfe_qma6100p_int_map1_t` (bit 7,
//!   `src/QMA6100P_regs.h`). The datasheet puts `INT1_ANY_MOT` at bit 0 of
//!   0x1A and `INT1_NO_MOT` at bit 7, so Meshtastic maps the no-motion
//!   interrupt it never enables, and no firmware we can read has ever shown
//!   the pin toggling. `INT_ST0` has the same mirror image in the library.
//! - **The any-motion threshold has no unit we can state.** `ANY_MOT_TH`
//!   (0x2E) counts in "16 LSB" of a slope scaled by the output data rate,
//!   and the bandwidth table that gives the rate is a picture in the
//!   datasheet, not text.
//!
//! Polling the data registers needs neither: [`POLL_INTERVAL_MS`] apart the
//! task reads the three axes, and [`MotionDetector`] compares successive
//! samples against a threshold this crate states in milli-g. The cost is
//! four 8-byte bus transactions a second at 100 kHz, under 4 ms of bus time
//! a second, on an MCU that never sleeps anyway. Step two may move to the
//! interrupt once the rig has seen INT1 fall with bit 0 of 0x1A set.

#![cfg_attr(not(test), no_std)]

use core::fmt;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Register map (datasheet §9.1, the register table; §9.2 for each field)
// ---------------------------------------------------------------------------

/// 7-bit I²C address with AD0 tied low (datasheet Table 8; Meshtastic
/// `QMA6100P_ADDR 0x12`, which is the address its scan finds on this board).
pub const I2C_ADDR: u8 = 0x12;

/// `CHIP_ID` (0x00), read-only.
pub const REG_CHIP_ID: u8 = 0x00;
/// What [`REG_CHIP_ID`] reads on a QMA6100P (datasheet "Register 0x00",
/// default 0x90; library `QMA6100P_CHIP_ID 0x90`).
pub const CHIP_ID: u8 = 0x90;
/// `DXL` (0x01), first of the six data registers DXL, DXM, DYL, DYM, DZL,
/// DZM. Each axis is 14-bit two's complement: the low register carries
/// `ACC<5:0>` in bits 7..2 and `NEWDATA` in bit 0, the high one
/// `ACC<13:6>`.
pub const REG_DATA: u8 = 0x01;
/// `FSR` (0x0F): `RANGE<3:0>` in bits 3..0.
pub const REG_FSR: u8 = 0x0F;
/// `PM` (0x11): `MODE_BIT` in bit 7, 1 = active. Standby after power-on and
/// after a soft reset (datasheet §6.1).
pub const REG_PM: u8 = 0x11;
/// `S_RESET` / `SR` (0x36): writing 0xB6 resets every register; "after
/// soft-reset, user should write 0x00 back" (datasheet "Register 0x36").
pub const REG_SOFT_RESET: u8 = 0x36;

/// The soft-reset command for [`REG_SOFT_RESET`].
pub const SOFT_RESET_CMD: u8 = 0xB6;
/// `RANGE = 0001`: ±2 g at 244 µg/LSB (datasheet "Register 0x0f").
///
/// The smallest range, because the question is whether the board moves,
/// not how hard: a hand lifting it is tens of milli-g, and the finest step
/// resolves that best. Meshtastic sets 32 g
/// (`QMA_6100P_MPU_ACCEL_SCALE SFE_QMA6100P_RANGE32G`), where one LSB is
/// 3.91 mg and our threshold would be eight counts.
pub const FSR_RANGE_2G: u8 = 0x01;
/// [`REG_PM`] with only `MODE_BIT` set: active, master clock and reset
/// clock at their reset defaults. The byte Meshtastic's read-modify-write
/// after a soft reset produces (`enableAccel`, `src/QMA6100P.cpp`).
pub const PM_ACTIVE: u8 = 0x80;
/// One LSB at [`FSR_RANGE_2G`], in micro-g.
pub const LSB_UG_2G: u32 = 244;

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// Wait after raising the rail before the first bus transaction: the
/// datasheet's supply ramp (`PSUP`, 50 ms) plus its power-on start-up time
/// (`Tstart`, 10 ms max).
pub const POWER_UP_MS: u32 = 60;
/// Wait after the soft-reset command before writing 0x00 back. The library
/// polls the register for up to 10 ms (`softwareReset`); a fixed wait of
/// the same length needs no read the part may still NAK.
pub const SOFT_RESET_MS: u32 = 10;
/// Wait after the reset is released, before configuring: Meshtastic's
/// `delay(20)` around the reset (`QMA6100PSensor.cpp`, `init`).
pub const RESET_SETTLE_MS: u32 = 20;
/// How often the task reads the three axes: four times a second.
///
/// Fast enough that a slow tilt (a quarter turn over two seconds) shows
/// as several samples each tens of milli-g from the last, slow enough that
/// the bus is idle more than 99 % of the time.
pub const POLL_INTERVAL_MS: u64 = 250;

// ---------------------------------------------------------------------------
// The transition rule
// ---------------------------------------------------------------------------

/// How long no sample may exceed the threshold before the board counts as
/// still: thirty seconds.
///
/// Long enough that a board carried in a pocket, which pauses for seconds
/// at a time (a traffic light, a stop to read a sign), stays moving
/// through the pause; short enough that a board put down is called still
/// well inside a minute, which is what step two needs to put the receiver
/// to sleep. A constant, not a knob: step two decides whether it holds.
pub const STILL_HOLD_MS: u64 = 30_000;

/// The change between two successive samples that counts as motion: 30 mg,
/// summed over the three axes.
///
/// A board at rest changes by the part's noise only, a few milli-g per
/// axis; a quarter turn over two seconds moves one g between two axes in
/// eight samples, about 125 mg a sample. Thirty sits well clear of both.
/// The rig sets it for good (tilt, then leave it), which is why it is
/// stated in milli-g and converted below rather than given in counts.
pub const MOTION_THRESHOLD_MG: u32 = 30;
/// [`MOTION_THRESHOLD_MG`] in LSB at [`FSR_RANGE_2G`]: 30 000 / 244 = 122.
pub const MOTION_THRESHOLD_COUNTS: u32 = MOTION_THRESHOLD_MG * 1_000 / LSB_UG_2G;

/// The log token naming the part, the `src=` of every line.
pub const SRC: &str = "qma6100p";

// ---------------------------------------------------------------------------
// The bus
// ---------------------------------------------------------------------------

/// A bus transaction failed: a NAK, a bus error or a timeout. The firmware
/// does not need to tell them apart, only that the part did not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusError;

/// The register access the driver needs, implemented by the firmware over
/// TWIM and by the tests over a fake register file. Every method addresses
/// the part at [`I2C_ADDR`].
// The firmware's executor is single-threaded and nothing here crosses a
// thread, so the futures need no `Send` bound, which is what this lint is
// about.
#[allow(async_fn_in_trait)]
pub trait RegisterBus {
    /// Write one byte to `reg`.
    async fn write_reg(&mut self, reg: u8, value: u8) -> Result<(), BusError>;
    /// Read `buf.len()` bytes starting at `reg` (the part auto-increments).
    async fn read_regs(&mut self, reg: u8, buf: &mut [u8]) -> Result<(), BusError>;
    /// Wait `ms` milliseconds.
    async fn delay_ms(&mut self, ms: u32);
}

/// Why [`init`] gave up. `Display` is the `reason=` token of
/// `MOTION_INIT absent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    /// The chip-ID read was not acknowledged: nothing answers at
    /// [`I2C_ADDR`].
    NoAnswer,
    /// Something answered with another chip ID.
    WrongChip(u8),
    /// The part answered the ID and then failed a later transaction.
    Bus,
    /// The configuration did not read back as written.
    Readback { reg: u8, got: u8 },
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitError::NoAnswer => f.write_str("no-answer"),
            InitError::WrongChip(id) => write!(f, "chip-id-0x{id:02x}"),
            InitError::Bus => f.write_str("bus-error"),
            InitError::Readback { reg, got } => write!(f, "readback-0x{reg:02x}-0x{got:02x}"),
        }
    }
}

/// Bring the part from power-on to active at ±2 g.
///
/// The order is the reference's (`QMA6100PSingleton::init`): identify,
/// soft reset, range, active. Its offset calibration is left out — this
/// driver compares samples with each other, so a constant offset cancels —
/// and so is all of its interrupt set-up (module doc). Then the two
/// configuration registers are read back, because a write is not
/// acknowledged with what the part kept.
///
/// The caller raises the rail and waits [`POWER_UP_MS`] first.
pub async fn init<B: RegisterBus>(bus: &mut B) -> Result<(), InitError> {
    let mut id = [0u8; 1];
    bus.read_regs(REG_CHIP_ID, &mut id)
        .await
        .map_err(|_| InitError::NoAnswer)?;
    if id[0] != CHIP_ID {
        return Err(InitError::WrongChip(id[0]));
    }

    bus.write_reg(REG_SOFT_RESET, SOFT_RESET_CMD)
        .await
        .map_err(|_| InitError::Bus)?;
    bus.delay_ms(SOFT_RESET_MS).await;
    bus.write_reg(REG_SOFT_RESET, 0x00)
        .await
        .map_err(|_| InitError::Bus)?;
    bus.delay_ms(RESET_SETTLE_MS).await;

    bus.write_reg(REG_FSR, FSR_RANGE_2G)
        .await
        .map_err(|_| InitError::Bus)?;
    bus.write_reg(REG_PM, PM_ACTIVE)
        .await
        .map_err(|_| InitError::Bus)?;

    for (reg, want) in [(REG_FSR, FSR_RANGE_2G), (REG_PM, PM_ACTIVE)] {
        let mut got = [0u8; 1];
        bus.read_regs(reg, &mut got)
            .await
            .map_err(|_| InitError::Bus)?;
        if got[0] != want {
            return Err(InitError::Readback { reg, got: got[0] });
        }
    }
    Ok(())
}

/// Read the three axes, [`REG_DATA`] onwards.
pub async fn read_sample<B: RegisterBus>(bus: &mut B) -> Result<Sample, BusError> {
    let mut raw = [0u8; 6];
    bus.read_regs(REG_DATA, &mut raw).await?;
    Ok(Sample::from_regs(&raw))
}

// ---------------------------------------------------------------------------
// Samples and the detector
// ---------------------------------------------------------------------------

/// One reading of the three axes in LSB (14-bit, sign-extended).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub x: i16,
    pub y: i16,
    pub z: i16,
}

impl Sample {
    /// Decode the six data registers. Each axis is the high byte over the
    /// low one, shifted right by two: the arithmetic shift drops `NEWDATA`
    /// and the unused bit 1 and keeps the sign (datasheet "Register
    /// 0x01 ~ 0x02"; the library's `getRawAccelRegisterData` does the same).
    ///
    /// `NEWDATA` is not consulted: an axis that has not been updated reads
    /// its previous value, which is a change of zero, the right answer.
    pub fn from_regs(raw: &[u8; 6]) -> Self {
        let axis = |lo: u8, hi: u8| i16::from_le_bytes([lo, hi]) >> 2;
        Self {
            x: axis(raw[0], raw[1]),
            y: axis(raw[2], raw[3]),
            z: axis(raw[4], raw[5]),
        }
    }

    /// The change from `prev`, summed over the axes, in LSB.
    pub fn delta(&self, prev: &Sample) -> u32 {
        let d = |a: i16, b: i16| (i32::from(a) - i32::from(b)).unsigned_abs();
        d(self.x, prev.x) + d(self.y, prev.y) + d(self.z, prev.z)
    }
}

/// What the detector believes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionState {
    /// After boot, before the first motion or the first full hold.
    Unknown,
    Moving,
    Still,
}

impl MotionState {
    /// The `state=` token. `Unknown` appears only on [`Stat`]: no
    /// transition leads into it.
    pub fn as_str(self) -> &'static str {
        match self {
            MotionState::Unknown => "unknown",
            MotionState::Moving => "moving",
            MotionState::Still => "still",
        }
    }
}

/// A change of state, printed as `MOTION <this>`.
///
/// `hold_ms` is the time that justified it. Into `still`: how long no
/// sample exceeded the threshold, at least [`STILL_HOLD_MS`]. Into
/// `moving`: how long the board had been in the state it left (still, or
/// unknown since [`MotionDetector::new`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub state: MotionState,
    pub hold_ms: u64,
}

impl fmt::Display for Transition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "state={} hold_ms={} src={SRC}",
            self.state.as_str(),
            self.hold_ms
        )
    }
}

/// The moving/still rule:
///
/// - **moving** on the first sample whose [`Sample::delta`] from the one
///   before exceeds [`MOTION_THRESHOLD_COUNTS`];
/// - **still** on the first sample at which no sample has exceeded it for
///   [`STILL_HOLD_MS`] (counted from [`MotionDetector::new`] if none ever
///   has);
/// - a transition only on a change: a state that holds prints nothing.
///
/// The first sample has nothing to compare with and counts as quiet.
#[derive(Clone, Debug)]
pub struct MotionDetector {
    state: MotionState,
    prev: Option<Sample>,
    quiet_since_ms: u64,
    state_since_ms: u64,
    window: Window,
}

/// What [`MotionDetector::stat_due`] has seen since its last line.
#[derive(Clone, Copy, Debug, Default)]
struct Window {
    start_ms: u64,
    samples: u32,
    max_delta: u32,
    over: u32,
}

impl MotionDetector {
    pub fn new(now_ms: u64) -> Self {
        Self {
            state: MotionState::Unknown,
            prev: None,
            quiet_since_ms: now_ms,
            state_since_ms: now_ms,
            window: Window {
                start_ms: now_ms,
                ..Window::default()
            },
        }
    }

    pub fn state(&self) -> MotionState {
        self.state
    }

    /// Feed one sample taken at `now_ms`; `Some` on a change of state.
    pub fn on_sample(&mut self, sample: Sample, now_ms: u64) -> Option<Transition> {
        let delta = self.prev.map(|prev| sample.delta(&prev));
        let moved = delta.is_some_and(|d| d > MOTION_THRESHOLD_COUNTS);
        self.prev = Some(sample);
        self.window.samples += 1;
        self.window.max_delta = self.window.max_delta.max(delta.unwrap_or(0));
        self.window.over += u32::from(moved);

        if moved {
            self.quiet_since_ms = now_ms;
            if self.state == MotionState::Moving {
                return None;
            }
            return Some(self.enter(MotionState::Moving, now_ms, self.state_since_ms));
        }

        let quiet_ms = now_ms.saturating_sub(self.quiet_since_ms);
        if self.state != MotionState::Still && quiet_ms >= STILL_HOLD_MS {
            return Some(self.enter(MotionState::Still, now_ms, self.quiet_since_ms));
        }
        None
    }

    /// The window's `MOTION_STAT` line once [`STAT_INTERVAL_MS`] have passed
    /// since the last one (or since [`MotionDetector::new`]), and a fresh
    /// window from `now_ms`; `None` before that. The task asks on every
    /// poll, whether the read succeeded or not, so a `samples=` short of
    /// [`STAT_INTERVAL_MS`] / [`POLL_INTERVAL_MS`] counts failed reads.
    pub fn stat_due(&mut self, now_ms: u64) -> Option<Stat> {
        if now_ms.saturating_sub(self.window.start_ms) < STAT_INTERVAL_MS {
            return None;
        }
        let w = core::mem::replace(
            &mut self.window,
            Window {
                start_ms: now_ms,
                ..Window::default()
            },
        );
        Some(Stat {
            samples: w.samples,
            max_delta: w.max_delta,
            over: w.over,
            state: self.state,
            quiet_ms: now_ms.saturating_sub(self.quiet_since_ms),
        })
    }

    fn enter(&mut self, state: MotionState, now_ms: u64, since_ms: u64) -> Transition {
        self.state = state;
        self.state_since_ms = now_ms;
        Transition {
            state,
            hold_ms: now_ms.saturating_sub(since_ms),
        }
    }
}

/// How often the detector task prints `MOTION_STAT`: one [`STILL_HOLD_MS`],
/// so a board at rest whose state never leaves `moving` shows, window by
/// window, the sample that keeps restarting the hold, and a board whose
/// `still` edge was due shows `quiet_ms=` past the hold.
pub const STAT_INTERVAL_MS: u64 = 30_000;

/// One window of the detector, printed as `MOTION_STAT <this>` every
/// [`STAT_INTERVAL_MS`] whatever the state, the way `[BAT]` repeats:
/// `samples=<n> max_delta=<counts> over=<n> state=moving|still|unknown
/// quiet_ms=<ms> src=qma6100p`.
///
/// - `samples`: samples fed in the window;
/// - `max_delta`: the largest [`Sample::delta`] among them, in LSB, the
///   noise floor at rest (the threshold is [`MOTION_THRESHOLD_COUNTS`]);
/// - `over`: how many of them exceeded the threshold;
/// - `state`: the detector's state at the end of the window;
/// - `quiet_ms`: time since the last sample over the threshold (since
///   [`MotionDetector::new`] if none ever was).
///
/// It carries `src=` because `MOTION_INIT` is written about 0.1 s after
/// boot, and the T1000-E's debug CDC is up only about 13 s after boot, so
/// the rig never sees the init line: the stat line is how a capture
/// learns the part answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub samples: u32,
    pub max_delta: u32,
    pub over: u32,
    pub state: MotionState,
    pub quiet_ms: u64,
}

impl fmt::Display for Stat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "samples={} max_delta={} over={} state={} quiet_ms={} src={SRC}",
            self.samples,
            self.max_delta,
            self.over,
            self.state.as_str(),
            self.quiet_ms
        )
    }
}

/// The boot line's body, printed as `MOTION_INIT <this>`:
/// `ok src=qma6100p`, or `absent src=qma6100p reason=<InitError>`.
pub struct InitLine(pub Result<(), InitError>);

impl fmt::Display for InitLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Ok(()) => write!(f, "ok src={SRC}"),
            Err(e) => write!(f, "absent src={SRC} reason={e}"),
        }
    }
}

/// The edge of a failing bus after a successful init, so a part that stops
/// answering says so once instead of four times a second.
/// [`ReadHealth::on_read`] returns the body of a `MOTION_READ` line on each
/// change: `state=failed src=qma6100p`, then `state=ok src=qma6100p` when it
/// answers again.
#[derive(Clone, Debug, Default)]
pub struct ReadHealth {
    failing: bool,
}

impl ReadHealth {
    pub fn on_read(&mut self, ok: bool) -> Option<&'static str> {
        if ok == !self.failing {
            return None;
        }
        self.failing = !ok;
        Some(if ok {
            "state=ok src=qma6100p"
        } else {
            "state=failed src=qma6100p"
        })
    }
}
