//! 1.3" OLED status display on the WisMesh Pocket V2 (RAK19026 VC baseboard).
//!
//! Builds an I²C bus on TWISPI0 with SDA on P0.13 / SCL on P0.14, runtime-
//! probes for an OLED at 0x3C (and 0x3D as fallback), and hands an SSD1306
//! to `leviculum_firmware::display` (#469 980), which composes the status
//! page at ~1 Hz and owns the panel's power; this file is the bus, the
//! probe and the panel's `Panel` seam.
//!
//! Chip-detection follows Meshtastic's `src/detect/ScanI2CTwoWire.cpp:52-87`
//! exactly: send register address `0x00`, read one byte, mask the lower
//! nibble, and classify per the lookup table. SH1106 detection is logged
//! but not currently rendered; the only sh1106 crate on crates.io still
//! pins embedded-hal 0.2.3 which is not directly usable with embassy-nrf
//! 0.9's TWIM. SH1106 hardware is uncommon on RAK19026 VC and adding it
//! is a follow-up.

use display_interface::DisplayError;
use embassy_executor::Spawner;
use embassy_nrf::peripherals;
use embassy_nrf::twim::{self, Twim};
use embassy_nrf::{bind_interrupts, Peri};
use embassy_time::{Duration, Timer};

use ssd1306::mode::BufferedGraphicsMode;
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};
use static_cell::StaticCell;

bind_interrupts!(pub struct DisplayIrqs {
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
});

/// I²C addresses Meshtastic probes for an OLED. Order matters — 0x3C first.
const PROBE_ADDRS: [u8; 2] = [0x3C, 0x3D];

/// What the chip-probe found. Only the first variant is renderable today.
#[derive(Clone, Copy, Debug)]
enum DetectedKind {
    Ssd1306,
    Sh1106,
    None,
}

#[derive(Clone, Copy, Debug)]
struct Detected {
    kind: DetectedKind,
    addr: u8,
}

/// Address-only ACK probe — matches Meshtastic's `scanPort` quick-test
/// (`beginTransmission(addr); endTransmission()` with no payload). A
/// SSD1306/SH1106 in display-off mode will still ACK its own address even
/// when it would NACK a register read; using `write_read` for the probe
/// (as we did initially) loses those slaves.
async fn ack_probe(twim: &mut Twim<'_>, addr: u8) -> bool {
    twim.write(addr, &[]).await.is_ok()
}

/// Read the SSD1306/SH1106 status byte at register 0x00 with the
/// stabilization loop from `meshtastic/src/detect/ScanI2CTwoWire.cpp:52-87`.
/// Only called after `ack_probe` confirmed the slave is alive.
async fn classify(twim: &mut Twim<'_>, addr: u8) -> DetectedKind {
    let mut r: u8 = 0;
    let mut prev: u8 = 0xFF;
    let mut tries: u8 = 0;
    while r != prev && tries < 4 {
        prev = r;
        let mut buf = [0u8; 1];
        if twim.write_read(addr, &[0x00], &mut buf).await.is_err() {
            return DetectedKind::None;
        }
        r = buf[0] & 0x0F;
        tries += 1;
    }
    match r {
        0x00 | 0x08 => DetectedKind::Sh1106,
        0x03..=0x07 => DetectedKind::Ssd1306,
        _ => DetectedKind::None,
    }
}

/// Probe both standard addresses; the first hit wins.
///
/// If the slave ACKs its address but `classify()` returns `None` (typical
/// when an SSD1306 is in display-off and won't honour a status read), we
/// default to SSD1306. The RAK19026 VC's RAK-vendor sample driver hard-
/// codes SSD1306, so this is the correct default for this carrier; SH1106
/// detection is best-effort and currently not rendered anyway.
async fn detect(twim: &mut Twim<'_>) -> Detected {
    for &addr in &PROBE_ADDRS {
        if ack_probe(twim, addr).await {
            let kind = classify(twim, addr).await;
            let kind = match kind {
                DetectedKind::None => DetectedKind::Ssd1306,
                k => k,
            };
            return Detected { kind, addr };
        }
    }
    Detected {
        kind: DetectedKind::None,
        addr: 0,
    }
}

#[embassy_executor::task]
pub async fn display_task(
    twispi0: Peri<'static, peripherals::TWISPI0>,
    sda: Peri<'static, peripherals::P0_13>,
    scl: Peri<'static, peripherals::P0_14>,
    identity_hash: [u8; 16],
) {
    static TX_BUF: StaticCell<[u8; 64]> = StaticCell::new();

    let mut config = twim::Config::default();
    config.frequency = twim::Frequency::K400;
    // The RAK19026 VC baseboard exposes I²C1 (SDA P0.13 / SCL P0.14) without
    // external pull-up resistors — internal nRF52840 pull-ups are required.
    config.sda_pullup = true;
    config.scl_pullup = true;

    // TX_BUF is owned exclusively by this single, never-respawned task.
    // StaticCell::init hands out a unique `&'static mut` with no aliasing
    // hazard (it panics if init runs twice, which cannot happen here).
    let tx_buf: &'static mut [u8] = TX_BUF.init([0u8; 64]);
    let mut twim = Twim::new(twispi0, DisplayIrqs, sda, scl, config, tx_buf);

    // OLEDs (and several baseboard sensors) need a few hundred ms after the
    // 3V3-S rail comes up before they ACK on I²C. Give the bus 500 ms quiet
    // time so the very first probe doesn't see a still-resetting chip.
    Timer::after(Duration::from_millis(500)).await;

    let detected = detect(&mut twim).await;
    // The bring-up result goes through the gate-free path: it is written
    // about a second into boot, before the host attaches, and a gated line
    // written then is dropped, not delayed (Codeberg #296).
    crate::log::log_fmt_critical(
        "[DISP] ",
        format_args!("OLED probe: {:?} at 0x{:02X}", detected.kind, detected.addr),
    );

    let display = match detected.kind {
        DetectedKind::Ssd1306 => {
            let interface = I2CDisplayInterface::new_custom_address(twim, detected.addr);
            let mut d = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
                .into_buffered_graphics_mode();
            if let Err(e) = d.init() {
                crate::log::log_fmt_critical("[DISP] ", format_args!("init failed: {:?}", e));
                return;
            }
            crate::log::log_fmt_critical("[DISP] ", format_args!("SSD1306 init ok"));
            d
        }
        DetectedKind::Sh1106 => {
            crate::log::log_fmt_critical(
                "[DISP] ",
                format_args!("SH1106 detected — not rendered (driver pending)"),
            );
            return;
        }
        DetectedKind::None => {
            crate::log::log_fmt_critical(
                "[DISP] ",
                format_args!("no OLED found on TWISPI0 — display task exiting"),
            );
            return;
        }
    };

    leviculum_firmware::display::run(
        Ssd1306Panel(display),
        leviculum_firmware::clock::EmbassyClock,
        "leviculum RAK4631",
        &identity_hash,
        crate::panel_facts::status_facts,
    )
    .await
}

/// The SSD1306 in buffered-graphics mode, on the I2C bus the probe found
/// it on: its buffer is the frame, its flush sends what changed.
type Ssd1306Display = Ssd1306<
    I2CInterface<Twim<'static>>,
    DisplaySize128x64,
    BufferedGraphicsMode<DisplaySize128x64>,
>;

/// The Pocket V2's panel behind `leviculum_firmware::seams::Panel`, with
/// power control (`set_display_on`).
struct Ssd1306Panel(Ssd1306Display);

impl leviculum_firmware::seams::Panel for Ssd1306Panel {
    type Target = Ssd1306Display;
    type Error = DisplayError;
    const WIDTH: u32 = 128;

    fn frame(&mut self) -> &mut Ssd1306Display {
        &mut self.0
    }

    fn flush(&mut self) -> impl core::future::Future<Output = Result<(), DisplayError>> {
        core::future::ready(self.0.flush())
    }

    fn has_power_control(&self) -> bool {
        true
    }

    fn set_power(&mut self, on: bool) -> Result<(), DisplayError> {
        self.0.set_display_on(on)
    }
}

/// Convenience wrapper invoked from the bin file.
pub fn init(
    spawner: &Spawner,
    twispi0: Peri<'static, peripherals::TWISPI0>,
    sda: Peri<'static, peripherals::P0_13>,
    scl: Peri<'static, peripherals::P0_14>,
    identity_hash: [u8; 16],
) {
    spawner.must_spawn(display_task(twispi0, sda, scl, identity_hash));
}
