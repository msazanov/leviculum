//! Battery voltage monitor, on whichever board is asking (Codeberg #380).
//!
//! # What the board says now
//!
//! Once at boot and then every 30 s:
//!
//! ```text
//! BATTERY mv=<ewma> min_mv=<n> max_mv=<n> pct=<n|none> cells=<n>S t=<ms>
//! ```
//!
//! and, only when that `pct=` starts or stops being derivable, one line
//! of its own:
//!
//! ```text
//! BATTERY_PCT reportable=<0|1> pack_mv=<n> cells=<n>S band_lo_mv=<n> band_hi_mv=<n> t=<ms>
//! ```
//!
//! Before this the module sampled the pack, fed [`BATTERY_STATE`](leviculum_firmware::board_state::BATTERY_STATE) for
//! the display, and said nothing at all — one `[BAT] init` line at
//! startup and then silence for the rest of the boot. A Pocket V2 that
//! restarted twice during a 90 minute field walk on battery produced
//! zero battery lines, so the one question worth asking afterwards
//! (*how close to the edge was the pack?*) had no data behind it at all.
//!
//! # What 1 Hz can and cannot catch
//!
//! The sampler runs at 1 Hz and the report holds the minimum and the
//! maximum across its period, so a sag that *lasts* — the pack drooping
//! under a long transmit, a cell aging into a high internal resistance
//! — shows up as a `min_mv` far below `mv` instead of being averaged
//! into invisibility.
//!
//! It does not catch the transient that actually resets a board. A
//! brownout is microseconds wide, the sampler is a second apart, and
//! the reset takes the log with it; the field Pocket's every boot came
//! up with `reset_reason=0x00000000` and `BOOT_TRACE prev_magic=absent`,
//! i.e. retained RAM had lost power and there was nothing to read. What
//! this buys is the *margin* — the distance between the pack and the
//! edge, minute by minute — which is the thing that has never once been
//! measured. Whether transmitting at 22 dBm closes that distance needs a
//! boot counter in non-volatile storage and is not this.
//!
//! # Why a percentage can be missing
//!
//! The cell count is one reading's worth of decision, taken at boot and
//! held for the boot; every per-cell voltage after it is the pack voltage
//! divided by it. `119067f1` caught the first reading that is not a pack
//! at all. What it could not catch is a first reading that is inside the
//! plausible band and still wrong, or a classification that was right at
//! boot and is not any more — and the percentage that goes on the air
//! carries neither a unit nor a cell count, so nothing a remote reader
//! has can tell a wrong one from a right one.
//!
//! So the percentage is reported only while the pack voltage stays inside
//! the band its classification implies
//! ([`leviculum_battery_scale::pack_percent`]). Outside it the board
//! publishes no percentage and says so once, on the transition rather
//! than once per sample. The voltage is never withheld: `mv=` is a
//! measurement and stays whatever the ADC said, and it is the number a
//! reader can still act on. Nor is the classification revised — that is a
//! boot-time decision and re-taking it at runtime is a different
//! question; this only declines to build on it.
//!
//! # Where the numbers live
//!
//! Nothing quantitative is in this file. The ADC gain, the acquisition
//! window, the conversion, the cell classification and the line's bytes
//! are all in [`leviculum_battery_scale`], where a host test can hold
//! them — notably the full scale, which is *derived from* the gain this
//! module configures rather than written down beside it, and the
//! acquisition window, which is derived from the board's divider. The
//! divider itself and the pins come from the board file
//! (`boards/t114.rs`, `rak4631.rs`, `solarnode.rs`, `t1000e.rs`),
//! which stays their single source.
//!
//! # Which half lives here
//!
//! Since #469 991 the sampling task, its cadences, the classification at
//! boot, the filter, the window and the lines are the orchestration's
//! ([`leviculum_firmware::battery`]). This file configures the SAADC
//! channel and the divider's enable line and hands the orchestration a
//! [`BatteryGauge`],
//! [`SaadcGauge`], that takes one reading with the board's
//! [`BatteryScale`].

use embassy_executor::Spawner;
use embassy_nrf::gpio::{AnyPin, Level, Output, OutputDrive};
use embassy_nrf::peripherals;
use embassy_nrf::saadc::{self, AnyInput, ChannelConfig, Config, Reference, Resolution, Saadc};
use embassy_nrf::{bind_interrupts, Peri};
use embassy_time::{Duration, Timer};

use leviculum_battery_scale::{AcquisitionTime, AdcGain};

/// Re-exported so a bin can build its board's scale without naming the
/// crate: the board file states the divider, the bin picks the
/// constructor that matches how it states it.
pub use leviculum_battery_scale::{BatteryScale, Divider};

use leviculum_firmware::battery::GaugeFacts;
use leviculum_firmware::clock::EmbassyClock;
use leviculum_firmware::seams::BatteryGauge;

bind_interrupts!(pub struct BatteryIrqs {
    SAADC => saadc::InterruptHandler;
});

/// How long the divider needs after its enable pin is asserted before
/// the voltage on the ADC pin is the pack's. Meshtastic waits the same
/// 10 ms on the same divider (`meshtastic/src/Power.cpp:243`).
const DIVIDER_SETTLE_MS: u64 = 10;

/// A board's divider-enable line: the pin, and the level that switches
/// the divider ON.
///
/// The polarity is part of the pin, not a property of this module. The
/// T114 asserts high into a load switch; the SenseCAP Solar Node's
/// `BAT_READ` sinks the low side of the XIAO's 1 M/510 k divider and is
/// therefore active LOW (`boards/solarnode.rs`, which cites the variant
/// that states it). Getting it backwards does not fail loudly — nothing
/// refuses to compile and nothing errors at run time — so the board that
/// knows says so here rather than leaving a convention for the next
/// board to discover.
///
/// What it *does* produce is a reading outside any pack, and which side
/// depends on the board. A sinking enable left inactive stops being a
/// return and becomes a pull-up through the divider's low leg, so the
/// solar node reads 9.5 V and upwards — `classify_cell_count` calls that
/// implausible and the boot line says so
/// (`battery-scale`'s `a_divider_left_disabled_reads_implausible_on_this_board`).
/// A switching enable left inactive leaves the pin floating instead.
/// Both are rejected; neither is a pack.
pub struct DividerEnable {
    /// The board's `AdcCtrl` pin.
    pub pin: Peri<'static, AnyPin>,
    /// The level that switches the divider on.
    pub active: Level,
}

/// The enable line once it is a driven output: the pin plus the level
/// that means "on".
struct EnableLine {
    out: Output<'static>,
    active: Level,
}

impl EnableLine {
    /// Drive the pin to its inactive level and take it. Inactive at
    /// construction, because the divider is switched on per sample and
    /// not for the lifetime of the task.
    fn new(enable: DividerEnable) -> Self {
        let inactive = match enable.active {
            Level::High => Level::Low,
            Level::Low => Level::High,
        };
        Self {
            out: Output::new(enable.pin, inactive, OutputDrive::Standard),
            active: enable.active,
        }
    }

    fn on(&mut self) {
        self.out.set_level(self.active);
    }

    fn off(&mut self) {
        self.out.set_level(match self.active {
            Level::High => Level::Low,
            Level::Low => Level::High,
        });
    }
}

/// Translate the gain the arithmetic uses into the gain the channel is
/// configured with.
///
/// This `match` is the whole seam between the two: the scale carries one
/// gain, the register is written from it, and the full scale the
/// conversion divides by is derived from it
/// ([`AdcGain::full_scale_mv`]). A gain changed in one place therefore
/// cannot fail to change in the other — which is what happened before,
/// when the register value came from an embassy default
/// (`ChannelConfig::single_ended`, `embassy-nrf-0.9.0/src/saadc.rs:104`)
/// and the 3600 it implies was a hard-coded constant in the conversion.
fn saadc_gain(gain: AdcGain) -> saadc::Gain {
    match gain {
        AdcGain::OneSixth => saadc::Gain::GAIN1_6,
        AdcGain::OneFifth => saadc::Gain::GAIN1_5,
        AdcGain::OneQuarter => saadc::Gain::GAIN1_4,
        AdcGain::OneThird => saadc::Gain::GAIN1_3,
        AdcGain::OneHalf => saadc::Gain::GAIN1_2,
        AdcGain::Unity => saadc::Gain::GAIN1,
        AdcGain::Two => saadc::Gain::GAIN2,
        AdcGain::Four => saadc::Gain::GAIN4,
    }
}

/// The same seam for the acquisition window: the scale carries the one
/// the board's divider needs, this writes it.
///
/// It is a second embassy default that was never a statement
/// (`Time::_10US`, `embassy-nrf-0.9.0/src/saadc.rs:109`) and it is the
/// one the T114's divider happened to satisfy and the Solar Node's does
/// not — 338 kΩ of source against the 100 kΩ the part specifies 10 µs
/// for. Getting it wrong reads low and stays plausible, which is the
/// failure mode this whole crate exists to make impossible.
fn saadc_time(time: AcquisitionTime) -> saadc::Time {
    match time {
        AcquisitionTime::ThreeUs => saadc::Time::_3US,
        AcquisitionTime::FiveUs => saadc::Time::_5US,
        AcquisitionTime::TenUs => saadc::Time::_10US,
        AcquisitionTime::FifteenUs => saadc::Time::_15US,
        AcquisitionTime::TwentyUs => saadc::Time::_20US,
        AcquisitionTime::FortyUs => saadc::Time::_40US,
    }
}

/// The board's battery gauge as the orchestration reads it: the SAADC
/// channel behind the divider, the divider's enable line where the board
/// has one, and the board's scale.
pub struct SaadcGauge {
    adc: Saadc<'static, 1>,
    divider_enable: Option<EnableLine>,
    scale: BatteryScale,
}

impl BatteryGauge for SaadcGauge {
    /// Take one reading, in millivolts at the battery terminal.
    ///
    /// The divider-enable pin, where the board has one, is asserted only
    /// for the duration of the sample: on the T114 the divider is 490 kΩ
    /// across the pack and on the solar node 1.51 MΩ, and leaving either
    /// enabled between samples would drain the pack for nothing 99 % of
    /// the time.
    async fn sample_mv(&mut self) -> u16 {
        if let Some(enable) = self.divider_enable.as_mut() {
            enable.on();
            Timer::after(Duration::from_millis(DIVIDER_SETTLE_MS)).await;
        }
        let mut buf = [0i16; 1];
        self.adc.sample(&mut buf).await;
        if let Some(enable) = self.divider_enable.as_mut() {
            enable.off();
        }
        self.scale.raw_to_battery_mv(buf[0])
    }
}

#[embassy_executor::task]
pub async fn battery_task(
    saadc_periph: Peri<'static, peripherals::SAADC>,
    adc_pin: AnyInput<'static>,
    divider_enable: Option<DividerEnable>,
    scale: BatteryScale,
) {
    let mut config = Config::default();
    config.resolution = Resolution::_12BIT;
    let mut ch = ChannelConfig::single_ended(adc_pin);
    // All three of these are what `single_ended` would have defaulted to
    // on the two boards that predate the Solar Node. They are written
    // down because the conversion depends on them and a default is not a
    // statement: `leviculum_battery_scale` derives the full-scale
    // millivolts from this gain, so the two move together or a test
    // fails. The acquisition time is the one that is no longer the same
    // on every board — see `saadc_time`.
    ch.reference = Reference::INTERNAL;
    ch.gain = saadc_gain(scale.gain());
    ch.time = saadc_time(scale.acquisition_time());
    let adc = Saadc::new(saadc_periph, BatteryIrqs, config, [ch]);

    // The enable pin starts INACTIVE — which is not the same as low, see
    // `EnableLine`: the divider is switched on per sample, not for the
    // lifetime of the task.
    let divider_enable = divider_enable.map(EnableLine::new);

    let facts = GaugeFacts {
        full_scale_mv: scale.terminal_full_scale_mv(),
        acquisition_us: scale.acquisition_time().micros(),
    };
    let gauge = SaadcGauge {
        adc,
        divider_enable,
        scale,
    };
    leviculum_firmware::battery::run(gauge, EmbassyClock, facts).await
}

/// Convenience wrapper invoked from the bin file.
///
/// `adc_pin` is the board's `BatteryAdc`, `divider_enable` its
/// `AdcCtrl` *and the level that asserts it* where it has one (the T114
/// does, active high; the solar node does, active low; the RAK and the
/// T1000-E do not), and `scale` is built from the board file's own statement of its
/// divider. All of them come from the board file, which stays their
/// single source — this module holds no copy of any of them.
///
/// The scale rather than a bare multiplier, because how a board states
/// its divider is itself board knowledge: the T114 and the RAK state a
/// ratio and use [`BatteryScale::for_board`]; the solar node states its
/// two resistors and uses [`BatteryScale::for_divider`], which is what
/// lets the acquisition window be derived instead of defaulted; the
/// T1000-E states a ratio whose resistors nobody publishes and uses
/// [`BatteryScale::for_unsized_divider`], the longest window.
///
/// The ADC's gain is not a per-board choice and comes from
/// [`leviculum_battery_scale::CONFIGURED_GAIN`] either way. A variant
/// header's `AREF_VOLTAGE` is *that* firmware's ADC configuration and
/// not a property of the board's divider; folding it in here a second
/// time is exactly the double count that crate's module doc exists to
/// prevent.
pub fn init(
    spawner: &Spawner,
    saadc_periph: Peri<'static, peripherals::SAADC>,
    adc_pin: impl saadc::Input + 'static,
    divider_enable: Option<DividerEnable>,
    scale: BatteryScale,
) {
    spawner.must_spawn(battery_task(
        saadc_periph,
        adc_pin.degrade_saadc(),
        divider_enable,
        scale,
    ));
}
