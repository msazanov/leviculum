//! SenseCAP T1000-E pin mappings and transceiver facts (Codeberg #406)
//!
//! A card-sized tracker: an nRF52840 and a Semtech LR1110 (LoRa plus GNSS
//! and WiFi scanning, of which this firmware uses the LoRa modem only),
//! an Airoha AG3335 GNSS receiver, a QMA6100P accelerometer, a buzzer, one
//! LED and one button. No display. It is the first board on this firmware
//! whose radio is not an SX1262, so it is also the first one driven
//! through `leviculum_lr11xx` (`crate::radio` picks the driver for the
//! `bsp-t1000e` build). Same Adafruit nRF52 bootloader family as the other
//! boards: measured on the rig unit (2026-10-18), Board-ID
//! `nRF52840-T1000-E-v1`, SoftDevice S140 7.3.0, bootloader USB 2886:0057.
//!
//! # Every wiring fact here is a foreign-firmware hint
//!
//! Seeed does not publish the T1000-E schematic (the wiki page
//! <https://wiki.seeedstudio.com/t1000_e_intro/> links no design files;
//! checked for the review of Codeberg #451). So nothing below has the
//! standing of a schematic citation, which is the reference this tree
//! holds board files to: the pins come from Meshtastic's board definition,
//! `variants/nrf52840/tracker-t1000-e/variant.h`, and the RF switch table
//! from `rfswitch.h` beside it, with the Seeed wiki corroborating the pin
//! list. Each alias names the `define` it was read from, and
//! `leviculum-nrf/reference-pins.toml` records the LoRa set so
//! `scripts/check-nrf-board-pins.sh` re-reads that header whenever a
//! Meshtastic tree is present.
//!
//! # What first boot refutes
//!
//! The saving grace of an LR1110 is that a wrong transceiver guess fails
//! loudly in `init_radio`: after `Calibrate` the driver reads `GetErrors`
//! and refuses a nonzero word as `Calibration(errors=0x....)`, which the
//! LoRa task prints as `[LORA] init FAILED: ...`. The two guesses in
//! [`LR1110`] that can be wrong and the bit that refutes each:
//!
//! * the TCXO supply (`V1_6`) or its start-up time (300 steps, ~9 ms):
//!   `HF_XOSC_START`, bit 5, `errors=0x0020`. A wrong voltage usually,
//!   a start-up time too short sometimes; raise the steps before doubting
//!   the voltage, since the voltage is the one fact the variant states
//!   (`LR11X0_DIO3_TCXO_VOLTAGE 1.6`).
//! * a 32.768 kHz crystal on DIO10/DIO11 (`LfClk::Xtal`, waited for):
//!   `LF_XOSC_START`, bit 6, `errors=0x0040`. Then the crystal is absent
//!   and the answer is `LfClk::Rc` with `wait_32k_ready: false`.
//!
//! This is the procedure the driver's README documents for the ThinkNode
//! M9 ("Worked example"), where both `Rc` and `Xtal` came up clean. The
//! regulator guess (`DcDc`) is not refuted by `GetErrors`: a board without
//! the inductor runs, badly. It is what Meshtastic sets on every LR1110
//! variant (`src/mesh/LR11x0Interface.cpp:122-124`, "currently all LR1110
//! variant files use the DC-DC regulator option"), which is the best
//! evidence available until a schematic or a current measurement.
//!
//! Note what the variant's `USE_LFXO` is NOT: it selects the nRF52840's own
//! 32 kHz crystal for the MCU's LF clock, a different crystal on a
//! different chip, and says nothing about the LR1110's DIO10/DIO11. The
//! `Xtal` guess rests on the LR1110's GNSS engine, which needs a 32 kHz
//! reference and which this product ships with; it is a guess, and the bit
//! above is how it gets checked.
//!
//! # Transmit power is unverified
//!
//! No Seeed PA characterisation exists that we could find, so the PA table
//! is the Semtech LR1110MB1DXS shield's (same part, same two-PA
//! architecture). The power this board radiates for a configured number is
//! therefore **unverified until a rig measurement**. What keeps that honest
//! in the meantime is the clamp-and-announce contract (#349): the
//! `[SX_TX_POWER]` line states the request, the `SetTxParams` byte and the
//! four `SetPaCfg` bytes of the row that went out, so a capture says which
//! row of which table a number came from.

use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::peripherals;
use embassy_nrf::Peri;
use leviculum_lr11xx::board::{
    PaTable, RfSwitch, Tcxo, LR1110MB1DXS_PA_TABLE, RFSW0, RFSW1, RFSW2, RFSW3,
    SEMTECH_RSSI_CALIBRATION,
};
use leviculum_lr11xx::{Board, Chip, LfClk, RegMode, TcxoVoltage};

// LR1110 LoRa transceiver (variant.h, the `LORA_*`/`PIN_SPI_*` block;
// "SPI shared with nothing else": `SPI_INTERFACES_COUNT 1`)
/// LR1110 SPI clock (`PIN_SPI_SCK (0 + 11)`, `LORA_SCK`)
pub type LoRaSck = peripherals::P0_11;
/// LR1110 SPI MOSI (`PIN_SPI_MOSI (32 + 9)`, `LORA_MOSI`)
pub type LoRaMosi = peripherals::P1_09;
/// LR1110 SPI MISO (`PIN_SPI_MISO (32 + 8)`, `LORA_MISO`)
pub type LoRaMiso = peripherals::P1_08;
/// LR1110 chip-select, active low (`PIN_SPI_NSS (0 + 12)`, `LORA_CS`)
pub type LoRaCs = peripherals::P0_12;
/// LR1110 NRESET, active low (`LORA_RESET (32 + 10)`)
pub type LoRaReset = peripherals::P1_10;
/// LR1110 BUSY, high = busy (`LORA_DIO2 (0 + 7)`, `LR1110_BUSY_PIN`).
/// Meshtastic names the BUSY line "DIO2" in RadioLib's SX126x idiom; on the
/// LR1110 it is the BUSY pin.
pub type LoRaBusy = peripherals::P0_07;
/// LR1110 IRQ, the part's DIO9 (`LORA_DIO1 (32 + 1)`, `LR1110_IRQ_PIN`).
/// "DIO1" again in RadioLib's idiom; the LR1110 raises interrupts on DIO9.
pub type LoRaIrq = peripherals::P1_01;

/// LR1110 SPI frequency in Hz (4 MHz, as on every board here; the LR1110
/// takes up to 16 MHz).
pub const LORA_SPI_FREQ_HZ: u32 = 4_000_000;
/// The top of the PA table below, +22 dBm on the HP PA.
pub const LORA_MAX_POWER_DBM: i8 = 22;

/// The LR1110's transceiver facts, as `leviculum_lr11xx` takes them.
/// Foreign-firmware hints, every field: see the module doc for the source
/// of each and for the `GetErrors` bit that refutes the two that can be
/// wrong at first boot.
pub const LR1110: Board = Board {
    name: "t1000e",
    chip: Chip::Lr1110,
    // `LR11X0_DIO3_TCXO_VOLTAGE 1.6` (variant.h). The start-up time is not
    // stated anywhere we could read: 300 steps (~9 ms) is the Semtech
    // shield's, generous against RadioLib's ~164-step default, and is to be
    // tightened once `GetErrors` stays clean.
    tcxo: Some(Tcxo {
        voltage: TcxoVoltage::V1_6,
        startup_steps: 300,
    }),
    // rfswitch.h, `rfswitch_table`, columns DIO5 DIO6 DIO7 DIO8 =
    // RFSW0..RFSW3: STBY all low; RX DIO5+DIO8; TX DIO5+DIO6+DIO8; TX_HP
    // DIO6+DIO8; TX_HF all low; GNSS DIO7; WIFI all low. DIO8 (RFSW3) is
    // high in every RX and TX mode, which is why the driver names RFSW3
    // and RFSW4 at all (`leviculum_lr11xx::board::RFSW3`).
    rf_switch: RfSwitch {
        enable: RFSW0 | RFSW1 | RFSW2 | RFSW3,
        standby: 0,
        rx: RFSW0 | RFSW3,
        tx: RFSW0 | RFSW1 | RFSW3,
        tx_hp: RFSW1 | RFSW3,
        tx_hf: 0,
        gnss: RFSW2,
        wifi: 0,
    },
    // Meshtastic, `src/mesh/LR11x0Interface.cpp:122-124`, for every LR1110.
    reg_mode: RegMode::DcDc,
    // The refutable guess: bit 6 of `GetErrors` says no (module doc).
    lf_clk: LfClk::Xtal,
    wait_32k_ready: true,
    // Firmware choice, the shield's default.
    rx_boosted: false,
    // The Semtech reference layout's tables; no Seeed figures exist.
    rssi_calibration: Some(SEMTECH_RSSI_CALIBRATION),
    // The LR1110MB1DXS shield's table, -17 to +22 dBm. Radiated power
    // unverified until a rig measurement (module doc).
    pa: PaTable::new(&LR1110MB1DXS_PA_TABLE, -17),
};

// Power rails
/// Sensor supply enable, **active HIGH** (`PIN_3V3_EN (32 + 6)`, "Power to
/// Sensors"). Meshtastic's `initVariant` raises it first thing on every
/// boot and never lowers it; the firmware does the same, because which
/// parts hang on this rail is exactly what the missing schematic would say,
/// and the one configuration known to run the radio is the vendor's.
pub type PowerSensors = peripherals::P1_06;

// GNSS (Airoha AG3335), held off in this image
/// GNSS power enable, **active HIGH** (`PIN_GPS_EN (32 + 11)`,
/// `GPS_EN_ACTIVE HIGH`). Driven low at boot, as Meshtastic's `initVariant`
/// does before its GPS code decides: the first image has no AG3335 driver,
/// and a receiver nobody reads should not draw current on a battery
/// tracker.
pub type GnssEnable = peripherals::P1_11;

// LED and button
/// The one LED, **active HIGH** (`PIN_LED1 (0 + 24)`, `LED_STATE_ON 1`).
pub type LedPin = peripherals::P0_24;
/// User button, **active HIGH** with a pull-down (`BUTTON_PIN (0 + 6)`,
/// `BUTTON_ACTIVE_LOW false`, `BUTTON_SENSE_TYPE 0x5`). Not wired to a task
/// in this image.
pub type UserButton = peripherals::P0_06;

/// Create the LED output. **Active HIGH**; off at boot.
pub fn led(pin: Peri<'static, LedPin>) -> Output<'static> {
    Output::new(pin, Level::Low, OutputDrive::Standard)
}

/// Raise the sensor rail ([`PowerSensors`]) and hold it for the life of
/// the board. The returned pin must be kept alive: dropping an embassy
/// `Output` disconnects it.
pub fn power_sensors(pin: Peri<'static, PowerSensors>) -> Output<'static> {
    Output::new(pin, Level::High, OutputDrive::Standard)
}

/// Hold the GNSS receiver off ([`GnssEnable`]). Kept alive by the caller
/// for the same reason as [`power_sensors`].
pub fn gnss_off(pin: Peri<'static, GnssEnable>) -> Output<'static> {
    Output::new(pin, Level::Low, OutputDrive::Standard)
}

/// Runtime board metadata for shared init code (USB / flash / LoRa).
///
/// The three persistence pages are the same three every board uses: they
/// are properties of the Adafruit bootloader's `USER_FLASH_END`
/// (`memory.x`), which this board shares.
///
/// `usb_pid` 0x0004 is the next pid.codes TEST id after the solar node's
/// 0x0003; like those, it is not registered yet.
pub const CONFIG: super::BoardConfig = super::BoardConfig {
    usb_vid: 0x1209,
    usb_pid: 0x0004,
    usb_manufacturer: "leviculum",
    usb_product: "leviculum T1000-E",
    log_prefix: "T1000",
    identity_flash_page: 0xEC000,
    radio_config_flash_page: 0xEB000,
    telemetry_flash_page: 0xEA000,
    // The SX1262's `SetDIO3AsTcxoCtrl` byte. Nothing reads it on this
    // board: its binary hands the driver [`LR1110`], whose `tcxo` field is
    // the TCXO fact for this part. The struct is shared, so the field
    // exists; the value is the SX1262 code for the same 1.6 V
    // (datasheet §13.3.6, Table 13-35), so even a misreading would ask for
    // the right voltage.
    lora_tcxo_voltage_reg: 0x00,
    lora_spi_freq_hz: LORA_SPI_FREQ_HZ,
    lora_max_power_dbm: LORA_MAX_POWER_DBM,
    // No external flash on this board: the variant names none.
    qspi_part: None,
};

/// Panic-LED descriptor — port, pin, active-low flag — for `set_panic_led`.
/// The one LED on P0.24, **active high**.
pub const PANIC_LED_PORT: u8 = 0;
pub const PANIC_LED_PIN: u8 = 24;
pub const PANIC_LED_ACTIVE_LOW: bool = false;
