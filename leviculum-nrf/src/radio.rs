//! The LoRa transceiver behind [`crate::lora`], chosen by the build.
//!
//! Every board on this firmware but one carries an SX1262 and drives it
//! through [`crate::sx1262`]. The SenseCAP T1000-E carries an LR1110 and
//! drives it through `leviculum_lr11xx`, whose method set is the SX1262
//! driver's name for name (`leviculum-lr11xx/src/driver.rs`, module doc).
//! This module is the one place that knows which of the two a build links:
//! `lora.rs` names [`Radio`], [`RxStatus`], [`Error`] and the three calls
//! below, and stays one text with no `cfg` inside a function. Nothing above
//! `lora.rs` learns the part at all (`rule:interface-isolation`).
//!
//! The selection is the `radio-lr11xx` feature, which `bsp-t1000e` pulls;
//! every other BSP pulls `radio-sx1262`. The binaries are bound to their
//! part through `required-features` in `Cargo.toml`, so a build compiles the
//! binaries whose radio it links and no others.
//!
//! Where the two parts differ in shape, the difference is stated here:
//!
//! * **Bring-up.** The SX1262 is reset and initialised with the carrier
//!   frequency (it calibrates its image band there); the LR11xx resets with
//!   a BUSY bound of its own and calibrates its image band in
//!   `configure_lora`, so its initialisation takes no frequency. What the
//!   chip answered is printed by [`Ready`]'s `Display`: the SX1262's status
//!   byte, the LR11xx's `GetVersion` triple.
//! * **The `[SX_TX_POWER]` fact.** The SX1262 driver states it itself, from
//!   inside `configure_lora`. The LR11xx driver returns its program and
//!   states nothing, so [`configure_lora`] states it here, with no `ocp`
//!   key: the part has no over-current register, and a zero would be an
//!   invented reading (`leviculum_log_line::facts::TxPowerProgrammed::ocp`).
//! * **The front end.** The SX1262 takes a TCXO select byte and an optional
//!   host RX-enable line ([`crate::boards`], "The antenna switch is a
//!   two-shaped thing"); the LR11xx takes its board's whole transceiver
//!   description, RF switch included, as a `leviculum_lr11xx::Board`. That
//!   is [`FrontEnd`].
//!
//! The rest is the same in both: `RxStatus` carries `rssi` and `snr`, and
//! `Error::Crc` carries a `CrcErrFrame` with `len`, `rssi` and `snr` (neither
//! part has a frequency-error readout).

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_nrf::gpio::Output;
use embassy_nrf::spim::Spim;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

/// The SPI device the radio sits on: SPIM2 behind a shared-bus mutex, with
/// the chip select it owns.
pub type Spi = SpiDevice<'static, NoopRawMutex, Spim<'static>, Output<'static>>;

pub use part::{
    bring_up, configure_lora, new, Error, FrontEnd, Radio, Ready, RxStatus, TxPowerProgram,
    IFACE_NAME,
};

#[cfg(not(feature = "radio-lr11xx"))]
mod part {
    use embassy_nrf::gpio::{AnyPin, Input, Level, Output, OutputDrive};
    use embassy_nrf::Peri;

    pub use crate::sx1262::{Error, RxStatus};

    /// The radio this build drives.
    pub type Radio = crate::sx1262::Sx1262<super::Spi>;

    /// What `configure_lora` programmed for the transmit power.
    pub type TxPowerProgram = leviculum_core::sx126x::TxPowerProgram;

    /// The interface name the board binaries register for this carrier.
    pub const IFACE_NAME: &str = "lora_sx1262";

    /// What the SX1262 takes from the board beyond its bus and its three lines.
    pub struct FrontEnd {
        /// `SetDIO3AsTcxoCtrl` voltage select byte
        /// ([`crate::boards::BoardConfig::lora_tcxo_voltage_reg`]).
        pub tcxo_voltage_reg: u8,
        /// The board's external RX-enable line, or `None` where DIO2 owns
        /// the whole antenna switch (T114, RAK4631). The driver does the
        /// switching; see `boards/solarnode.rs` for the board that needs it.
        pub rx_enable: Option<Peri<'static, AnyPin>>,
    }

    /// Build the driver. Touches nothing on the chip.
    pub fn new(
        spi: super::Spi,
        reset: Output<'static>,
        busy: Input<'static>,
        irq: Input<'static>,
        front_end: FrontEnd,
    ) -> Radio {
        // Released at construction: the chip is not listening yet, and the
        // very first thing the LoRa task does to it is a reset.
        let rx_enable = front_end
            .rx_enable
            .map(|pin| Output::new(pin, Level::Low, OutputDrive::Standard));
        Radio::new(spi, reset, busy, irq, front_end.tcxo_voltage_reg, rx_enable)
    }

    /// What the chip answered at the end of its bring-up.
    pub struct Ready(crate::sx1262::ChipStatus);

    impl core::fmt::Display for Ready {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "status=0x{:02X}", self.0.raw)
        }
    }

    /// Reset and initialise the chip.
    pub async fn bring_up(radio: &mut Radio, freq_hz: u32) -> Result<Ready, Error> {
        radio.reset().await;
        let _ = radio.wait_busy().await;
        radio.init_radio(freq_hz).await.map(Ready)
    }

    /// Program the modem and the PA. The driver states `[SX_TX_POWER]`
    /// itself, from inside the call.
    pub async fn configure_lora(
        radio: &mut Radio,
        freq_hz: u32,
        sf: u8,
        bw: u8,
        cr: u8,
        power_dbm: i8,
        preamble_len: u16,
    ) -> Result<TxPowerProgram, Error> {
        radio
            .configure_lora(freq_hz, sf, bw, cr, power_dbm, preamble_len)
            .await
    }
}

#[cfg(feature = "radio-lr11xx")]
mod part {
    use embassy_nrf::gpio::{Input, Output};

    pub use leviculum_lr11xx::{Error, RxStatus, TxPowerProgram};

    /// The radio this build drives.
    pub type Radio =
        leviculum_lr11xx::Lr11xx<super::Spi, Output<'static>, Input<'static>, Input<'static>>;

    /// The interface name the board binaries register for this carrier.
    pub const IFACE_NAME: &str = "lora_lr11xx";

    /// The board's transceiver description: TCXO, RF switch, regulator, LF
    /// clock, PA table. Lives in the board file next to its `BoardConfig`
    /// (`boards/t1000e.rs`).
    pub type FrontEnd = &'static leviculum_lr11xx::Board;

    /// Build the driver. Touches nothing on the chip.
    pub fn new(
        spi: super::Spi,
        reset: Output<'static>,
        busy: Input<'static>,
        irq: Input<'static>,
        front_end: FrontEnd,
    ) -> Radio {
        let mut radio = Radio::new(spi, reset, busy, irq, front_end);
        // The driver's `[SX_RX_*]` lines go where the SX1262's go.
        radio.set_log(crate::log::log_fmt);
        radio
    }

    /// What the chip answered at the end of its bring-up: `GetVersion`.
    pub struct Ready(leviculum_lr11xx::Version);

    impl core::fmt::Display for Ready {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(
                f,
                "hw=0x{:02X} type=0x{:02X} fw=0x{:04X}",
                self.0.hw, self.0.kind, self.0.fw
            )
        }
    }

    /// Reset and initialise the chip. The frequency is not needed here: the
    /// LR11xx calibrates its image band in `configure_lora`.
    pub async fn bring_up(radio: &mut Radio, _freq_hz: u32) -> Result<Ready, Error> {
        radio.reset().await?;
        radio.init_radio().await.map(Ready)
    }

    /// Program the modem and the PA, then state `[SX_TX_POWER]` from what
    /// the driver reports it wrote, without an `ocp` key (module doc).
    pub async fn configure_lora(
        radio: &mut Radio,
        freq_hz: u32,
        sf: u8,
        bw: u8,
        cr: u8,
        power_dbm: i8,
        preamble_len: u16,
    ) -> Result<TxPowerProgram, Error> {
        let programmed = radio
            .configure_lora(freq_hz, sf, bw, cr, power_dbm, preamble_len)
            .await?;
        leviculum_log_line::facts::tx_power_programmed(
            &mut crate::lora::FirmwareLog,
            &leviculum_log_line::facts::TxPowerProgrammed {
                requested_dbm: programmed.requested_dbm,
                programmed_dbm: programmed.programmed_dbm,
                pa_config: programmed.pa_config,
                ramp: programmed.ramp,
                ocp: None,
                clamped: programmed.clamped,
            },
        );
        Ok(programmed)
    }
}
