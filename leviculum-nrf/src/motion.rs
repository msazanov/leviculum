//! The T1000-E's QMA6100P accelerometer as a moving/still line (Codeberg
//! #457, step one).
//!
//! One task: raise the part's rail, bring it up over TWIM, then read its
//! three axes every [`POLL_INTERVAL_MS`] and print
//! `MOTION state=moving|still hold_ms=<n> src=qma6100p` on each change,
//! and every [`leviculum_motion::STAT_INTERVAL_MS`] a `MOTION_STAT` line
//! with the window's sample count, largest delta and over-threshold count.
//! Each edge is also written to [`crate::baseboard::MOTION_STATE`], which
//! the GNSS task reads to sleep its receiver while the board is still
//! (step two); the announce cadence and telemetry do not read it. The
//! register map, the init order, the
//! transition rule and why the task polls although INT1 is wired are all
//! in `leviculum_motion`, which is host-tested; this file is the bus.
//!
//! A part that does not answer, or answers as something else, prints
//! `MOTION_INIT absent ... reason=<why>` and the task returns after
//! dropping the bus and the rail, so the pins go back to the reset state
//! they had before this module existed.

use embassy_nrf::gpio::{AnyPin, Level, Output, OutputDrive};
use embassy_nrf::twim::{self, Twim};
use embassy_nrf::{bind_interrupts, peripherals, Peri};
use embassy_time::{with_timeout, Duration, Instant, Timer};

use leviculum_motion::{
    BusError, InitLine, MotionDetector, MotionState, ReadHealth, RegisterBus, I2C_ADDR,
    POLL_INTERVAL_MS, POWER_UP_MS,
};

// TWISPI0 rather than TWISPI1: it is the instance `set_irq_priorities`
// already places at P5, outside the SoftDevice's reserved band. The
// SSD1306 display binds the same handler, and `lib.rs` refuses a build
// with both features.
bind_interrupts!(pub struct MotionIrqs {
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
});

/// Longest a single register transaction may take. A read of six bytes at
/// 100 kHz is under a millisecond; a bus held low by a dead part would
/// otherwise stall the task for good.
const TRANSACTION_TIMEOUT_MS: u64 = 20;

/// The pins and the peripheral the task owns, all named in
/// `boards/t1000e.rs`.
pub struct MotionWiring {
    pub twim: Peri<'static, peripherals::TWISPI0>,
    pub sda: Peri<'static, AnyPin>,
    pub scl: Peri<'static, AnyPin>,
    /// The part's rail, active high, raised by the task.
    pub power_enable: Peri<'static, AnyPin>,
}

/// TWIM as `leviculum_motion`'s bus, every transaction bounded by
/// [`TRANSACTION_TIMEOUT_MS`].
struct TwimBus<'d> {
    twim: Twim<'d>,
}

impl RegisterBus for TwimBus<'_> {
    async fn write_reg(&mut self, reg: u8, value: u8) -> Result<(), BusError> {
        match with_timeout(
            Duration::from_millis(TRANSACTION_TIMEOUT_MS),
            self.twim.write(I2C_ADDR, &[reg, value]),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            _ => Err(BusError),
        }
    }

    async fn read_regs(&mut self, reg: u8, buf: &mut [u8]) -> Result<(), BusError> {
        match with_timeout(
            Duration::from_millis(TRANSACTION_TIMEOUT_MS),
            self.twim.write_read(I2C_ADDR, &[reg], buf),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            _ => Err(BusError),
        }
    }

    async fn delay_ms(&mut self, ms: u32) {
        Timer::after_millis(u64::from(ms)).await;
    }
}

/// Spawn the motion task.
pub fn init(spawner: &embassy_executor::Spawner, wiring: MotionWiring) {
    spawner.must_spawn(motion_task(wiring));
}

#[embassy_executor::task]
async fn motion_task(wiring: MotionWiring) {
    let rail = Output::new(wiring.power_enable, Level::High, OutputDrive::Standard);

    let mut config = twim::Config::default();
    // 100 kHz, the Arduino `Wire` default Meshtastic runs this part at
    // ("I2C is a bit finicky", variant.h). The internal pull-ups are on,
    // as the Adafruit core's `Wire.begin` sets them; no schematic says
    // whether the board has its own, and both together are harmless.
    config.frequency = twim::Frequency::K100;
    config.sda_pullup = true;
    config.scl_pullup = true;
    // Only register-address and register-value bytes from the stack go
    // out, never flash, so the RAM staging buffer can be this small.
    let mut tx_buf = [0u8; 4];
    let twim = Twim::new(
        wiring.twim,
        MotionIrqs,
        wiring.sda,
        wiring.scl,
        config,
        &mut tx_buf,
    );
    let mut bus = TwimBus { twim };

    Timer::after_millis(u64::from(POWER_UP_MS)).await;
    let result = leviculum_motion::init(&mut bus).await;
    crate::log::log_fmt_critical("MOTION_INIT ", format_args!("{}", InitLine(result)));
    if result.is_err() {
        // Bus first, then the rail: the pins return to their reset state
        // and the board is exactly what it was without this module.
        drop(bus);
        drop(rail);
        return;
    }

    let mut detector = MotionDetector::new(now_ms());
    let mut health = ReadHealth::default();
    loop {
        Timer::after_millis(POLL_INTERVAL_MS).await;
        let sample = leviculum_motion::read_sample(&mut bus).await;
        if let Some(edge) = health.on_read(sample.is_ok()) {
            crate::log::log_fmt_critical("MOTION_READ ", format_args!("{edge}"));
        }
        if let Ok(sample) = sample {
            // Critical, not gated: a transition is an edge nothing repeats,
            // and the first `still` falls thirty seconds after boot, which
            // is when the runtime gate opens at the latest.
            if let Some(transition) = detector.on_sample(sample, now_ms()) {
                crate::log::log_fmt_critical("MOTION ", format_args!("{transition}"));
                publish(transition.state);
            }
        }
        // Drain-gated, like `[BAT]`: a periodic line whose next instance
        // says what a dropped one did.
        if let Some(stat) = detector.stat_due(now_ms()) {
            crate::log::log_fmt("MOTION_STAT ", format_args!("{stat}"));
        }
    }
}

/// Hand an edge to the GNSS task (#457 step two). Only edges reach
/// here, and only after a good init, so a board whose part is absent
/// never writes the watch. `Unknown` is the detector's state before its
/// first edge and never one it moves into.
#[cfg(feature = "gnss")]
fn publish(state: MotionState) {
    use crate::baseboard::{Motion, MOTION_STATE};
    let motion = match state {
        MotionState::Moving => Motion::Moving,
        MotionState::Still => Motion::Still,
        MotionState::Unknown => return,
    };
    MOTION_STATE.sender().send(motion);
}

/// No receiver to gate in this build.
#[cfg(not(feature = "gnss"))]
fn publish(_state: MotionState) {}

fn now_ms() -> u64 {
    Instant::now().as_millis()
}
