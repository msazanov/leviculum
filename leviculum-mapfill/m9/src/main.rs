//! Bare measurement firmware for the ThinkNode M9: the map filler's draw
//! time and PSRAM cost on the device (#474). Not a node: no radio, no
//! stack, nothing on the air.
//!
//! It holds the Florence z15 fixture in flash and, every cycle, draws it
//! under both layer profiles into a 240x320 Rgb565 frame in internal SRAM
//! and then into the same frame in PSRAM, three repetitions each. After
//! every draw it pushes the frame to the ST7789. Every number goes out
//! over UART0 (the CH340K, 115200 8N1) as one line:
//!
//! ```text
//! MAPFILL profile=full|stripped fb=sram|psram phase=<p> cycles=<n> ms=<ms> rep=<r>
//! ```
//!
//! with `phase` one of `clear`, `project_clip`, `fill`, `stroke`, `total`
//! (which adds `bytes=`, the bytes the draw wrote) and `flush` (the panel
//! transfer), plus one `phase=counts` line per draw. Cycles are the Xtensa
//! `CCOUNT` register at 240 MHz. The cycle repeats every ten seconds, so a
//! capture opened after the boot still sees a complete set.
//!
//! Pins and clocks are the community port's (cedrivard/Leviculum 7a699446,
//! `leviculum-esp/src/boards/thinknode_m9.rs`): SPI2 on SCK 40, MOSI 47,
//! MISO 38 at 40 MHz for the panel; LCD CS 16, DC 15, RESET 14, backlight
//! 17 (on when low); VEXT 18 (on when low); the LR1110's NSS 39 and the
//! card's CS 48 parked high.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ptr::addr_of_mut;

use embedded_graphics::prelude::Point;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::main;
use esp_hal::psram::{Psram, PsramConfig, PsramMode, PsramSize, SpiRamFreq};
use esp_hal::spi::master::{Config as SpiConfig, Spi, SpiDma};
use esp_hal::spi::Mode;
use esp_hal::time::Rate;
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_hal::Blocking;
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::fixture::Fixture;
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{render, Clock, RenderStats, Work};
use leviculum_mapfill::style::Profile;

esp_bootloader_esp_idf::esp_app_desc!();

const W: u32 = 240;
const H: u32 = 320;
const PIXELS: usize = (W * H) as usize;
const FRAME_BYTES: usize = PIXELS * 2;
/// `CpuClock::max()` on the ESP32-S3.
const CPU_MHZ: u64 = 240;
/// The host harness's capacities (`measure/src/main.rs`); the Florence
/// fixture's largest feature has 1,051 vertices.
const EDGES: usize = 4096;
const POINTS: usize = 4096;
const RINGS: usize = 1024;
const REPS: u32 = 3;
/// The panel clock: SolarOS's M9 value, the community port's `LCD_SPI_FREQ_HZ`.
const LCD_SPI_MHZ: u32 = 40;
const PSRAM_BYTES: usize = 8 << 20;

const FIXTURE: &[u8] = include_bytes!("../../fixtures/firenze-z15.lmf");

// Internal SRAM: the frame (150 KiB) and the filler's working storage.
static mut FB_SRAM: [u16; PIXELS] = [0; PIXELS];
static mut EDGE_SLOTS: [Edge; EDGES] = [Edge::EMPTY; EDGES];
static mut ACTIVE: [u16; EDGES] = [0; EDGES];
static mut HEADS: [u16; H as usize] = [0; H as usize];
static mut POINTS_BUF: [Point; POINTS] = [Point::new(0, 0); POINTS];
static mut RING_ENDS: [u16; RINGS] = [0; RINGS];

// ST7789 opcodes (Sitronix ST7789V datasheet, section 9.1).
const SWRESET: u8 = 0x01;
const SLPOUT: u8 = 0x11;
const NORON: u8 = 0x13;
const INVON: u8 = 0x21;
const DISPON: u8 = 0x29;
const CASET: u8 = 0x2A;
const RASET: u8 = 0x2B;
const RAMWR: u8 = 0x2C;
const MADCTL: u8 = 0x36;
const COLMOD: u8 = 0x3A;
/// Portrait, RGB order: 240 columns, 320 rows, the panel's native shape.
/// The community port runs landscape (0xB0); which corner this puts the
/// origin in is what the first flash shows.
const MADCTL_PORTRAIT: u8 = 0x00;
const COLMOD_RGB565: u8 = 0x55;

/// The Xtensa cycle counter.
struct Ccount;

impl Clock for Ccount {
    fn now(&mut self) -> u32 {
        esp_hal::xtensa_lx::timer::get_cycle_count()
    }
}

type Out = Uart<'static, Blocking>;

fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// `cycles` as milliseconds with three decimals.
struct Ms(u64);

impl core::fmt::Display for Ms {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let us = self.0 / CPU_MHZ;
        write!(f, "{}.{:03}", us / 1000, us % 1000)
    }
}

struct Panel {
    spi: SpiDma<'static, Blocking>,
    cs: Output<'static>,
    dc: Output<'static>,
}

impl Panel {
    fn cmd(&mut self, op: u8, data: &[u8]) -> Result<(), esp_hal::spi::Error> {
        self.cs.set_low();
        let r = self.send(op, data);
        self.cs.set_high();
        r
    }

    fn send(&mut self, op: u8, data: &[u8]) -> Result<(), esp_hal::spi::Error> {
        self.dc.set_low();
        self.spi.write(&[op])?;
        self.dc.set_high();
        if !data.is_empty() {
            self.spi.write(data)?;
        }
        Ok(())
    }

    /// The datasheet's power-on sequence with its delays.
    fn init(
        &mut self,
        rst: &mut Output<'static>,
        delay: &Delay,
    ) -> Result<(), esp_hal::spi::Error> {
        rst.set_low();
        delay.delay_millis(1);
        rst.set_high();
        delay.delay_millis(120);
        self.cmd(SWRESET, &[])?;
        delay.delay_millis(150);
        self.cmd(SLPOUT, &[])?;
        delay.delay_millis(120);
        self.cmd(COLMOD, &[COLMOD_RGB565])?;
        self.cmd(MADCTL, &[MADCTL_PORTRAIT])?;
        self.cmd(INVON, &[])?;
        self.cmd(NORON, &[])?;
        delay.delay_millis(10);
        self.cmd(DISPON, &[])?;
        Ok(())
    }

    /// The whole frame in one chip-select window.
    fn flush(&mut self, bytes: &[u8]) -> Result<(), esp_hal::spi::Error> {
        let (x1, y1) = ((W - 1) as u16, (H - 1) as u16);
        self.cs.set_low();
        let r = (|| {
            self.send(CASET, &[0, 0, (x1 >> 8) as u8, x1 as u8])?;
            self.send(RASET, &[0, 0, (y1 >> 8) as u8, y1 as u8])?;
            self.send(RAMWR, bytes)
        })();
        self.cs.set_high();
        r
    }
}

struct Bufs {
    edges: &'static mut [Edge; EDGES],
    active: &'static mut [u16; EDGES],
    heads: &'static mut [u16; H as usize],
    points: &'static mut [Point; POINTS],
    ring_ends: &'static mut [u16; RINGS],
}

fn draw(
    fx: &Fixture<'_>,
    profile: Profile,
    fb: &mut [u16],
    b: &mut Bufs,
) -> Option<(RenderStats, u32)> {
    let mut frame = Frame::new(fb, W, H)?;
    let mut scratch = Scratch {
        edges: &mut b.edges[..],
        heads: &mut b.heads[..],
        active: &mut b.active[..],
    };
    let mut work = Work {
        points: &mut b.points[..],
        ring_ends: &mut b.ring_ends[..],
    };
    let mut clock = Ccount;
    let t0 = clock.now();
    let Ok(st) = render(fx, profile, &mut frame, &mut scratch, &mut work, &mut clock);
    Some((st, clock.now().wrapping_sub(t0)))
}

fn report(out: &mut Out, tag: (&str, &str, u32), st: &RenderStats, total: u32) {
    let (p, fb, rep) = tag;
    let ph = st.phases;
    for (name, c) in [
        ("clear", ph.clear),
        ("project_clip", ph.project_clip),
        ("fill", ph.fill),
        ("stroke", ph.stroke),
    ] {
        let _ = writeln!(
            out,
            "MAPFILL profile={p} fb={fb} phase={name} cycles={c} ms={} rep={rep}",
            Ms(c)
        );
    }
    let bytes = FRAME_BYTES as u64 + 2 * u64::from(st.fill.pixels + st.stroke.pixels);
    let _ = writeln!(
        out,
        "MAPFILL profile={p} fb={fb} phase=total cycles={total} ms={} bytes={bytes} rep={rep}",
        Ms(u64::from(total))
    );
    let _ = writeln!(
        out,
        "MAPFILL profile={p} fb={fb} phase=counts features={} culled={} vertices={} fill_edges={} fill_spans={} fill_pixels={} stroke_edges={} stroke_spans={} stroke_pixels={} overflows={} rep={rep}",
        st.features,
        st.culled,
        st.vertices,
        st.fill.edges,
        st.fill.spans,
        st.fill.pixels,
        st.stroke.edges,
        st.stroke.spans,
        st.stroke.pixels,
        st.overflows
    );
}

#[main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let Ok(uart) = Uart::new(p.UART0, UartConfig::default().with_baudrate(115_200)) else {
        halt()
    };
    let mut out: Out = uart.with_tx(p.GPIO43).with_rx(p.GPIO44);
    let delay = Delay::new();

    // The bus supply on, and every chip select on SPI2 high before the bus
    // exists, so the radio and the card ignore the panel's traffic.
    let _vext = Output::new(p.GPIO18, Level::Low, OutputConfig::default());
    let _lora_nss = Output::new(p.GPIO39, Level::High, OutputConfig::default());
    let _sd_cs = Output::new(p.GPIO48, Level::High, OutputConfig::default());
    let cs = Output::new(p.GPIO16, Level::High, OutputConfig::default());
    let dc = Output::new(p.GPIO15, Level::High, OutputConfig::default());
    let mut rst = Output::new(p.GPIO14, Level::High, OutputConfig::default());
    let mut backlight = Output::new(p.GPIO17, Level::High, OutputConfig::default());

    let (Ok(rx), Ok(tx)) = (esp_hal::dma_rx_buffer!(4096), esp_hal::dma_tx_buffer!(4096)) else {
        let _ = writeln!(out, "MAPFILL error=dma-buffers");
        halt()
    };
    let config = SpiConfig::default()
        .with_frequency(Rate::from_mhz(LCD_SPI_MHZ))
        .with_mode(Mode::_0);
    let Ok(spi) = Spi::new(p.SPI2, config) else {
        let _ = writeln!(out, "MAPFILL error=spi-config");
        halt()
    };
    let spi = spi
        .with_sck(p.GPIO40)
        .with_mosi(p.GPIO47)
        .with_miso(p.GPIO38)
        .with_dma(p.DMA_CH0)
        .with_buffers(rx, tx);
    let mut panel = Panel { spi, cs, dc };
    if let Err(e) = panel.init(&mut rst, &delay) {
        let _ = writeln!(out, "MAPFILL error=panel-init spi={e:?}");
    }
    backlight.set_low();

    let psram = Psram::new(
        p.PSRAM,
        PsramConfig {
            mode: PsramMode::OctalSpi,
            size: PsramSize::Size(PSRAM_BYTES),
            ram_frequency: SpiRamFreq::Freq80m,
            ..Default::default()
        },
    );
    let (pstart, psize) = psram.raw_parts();
    // SAFETY: the driver mapped `psize` bytes at `pstart` and nothing else
    // in this image touches PSRAM; the frame is the first PIXELS u16 of it,
    // and the mapping is page-aligned.
    let fb_psram: Option<&'static mut [u16]> = (psize >= FRAME_BYTES)
        .then(|| unsafe { core::slice::from_raw_parts_mut(pstart.cast::<u16>(), PIXELS) });

    // SAFETY: main runs once and never returns; these are the only
    // references ever taken to the statics.
    let (fb_sram, mut bufs) = unsafe {
        (
            &mut *addr_of_mut!(FB_SRAM),
            Bufs {
                edges: &mut *addr_of_mut!(EDGE_SLOTS),
                active: &mut *addr_of_mut!(ACTIVE),
                heads: &mut *addr_of_mut!(HEADS),
                points: &mut *addr_of_mut!(POINTS_BUF),
                ring_ends: &mut *addr_of_mut!(RING_ENDS),
            },
        )
    };
    let mut fb_psram = fb_psram;

    let fx = match Fixture::parse(FIXTURE) {
        Ok(f) => f,
        Err(e) => {
            let _ = writeln!(out, "MAPFILL error=fixture {e:?}");
            halt()
        }
    };

    for cycle in 1u32.. {
        let _ = writeln!(
            out,
            "MAPFILL boot sha={} opt={} cycle={cycle} fixture_bytes={} features={} psram_bytes={psize} fb_sram={:p} fb_psram={:p}",
            env!("MAPFILL_SHA"),
            env!("MAPFILL_OPT"),
            FIXTURE.len(),
            fx.len(),
            fb_sram.as_ptr(),
            pstart,
        );
        for (fb_name, fb) in [
            ("sram", Some(&mut fb_sram[..])),
            ("psram", fb_psram.as_deref_mut()),
        ] {
            let Some(fb) = fb else {
                let _ = writeln!(out, "MAPFILL fb={fb_name} skipped=absent");
                continue;
            };
            for profile in [Profile::Full, Profile::Stripped] {
                for rep in 1..=REPS {
                    let Some((st, total)) = draw(&fx, profile, fb, &mut bufs) else {
                        let _ = writeln!(out, "MAPFILL error=frame fb={fb_name}");
                        continue;
                    };
                    let tag = (profile.name(), fb_name, rep);
                    report(&mut out, tag, &st, total);
                    let Some(frame) = Frame::new(fb, W, H) else {
                        continue;
                    };
                    let t0 = esp_hal::xtensa_lx::timer::get_cycle_count();
                    let r = panel.flush(frame.bytes());
                    let c = esp_hal::xtensa_lx::timer::get_cycle_count().wrapping_sub(t0);
                    match r {
                        Ok(()) => {
                            let _ = writeln!(
                                out,
                                "MAPFILL profile={} fb={fb_name} phase=flush cycles={c} ms={} bytes={FRAME_BYTES} spi_mhz={LCD_SPI_MHZ} rep={rep}",
                                profile.name(),
                                Ms(u64::from(c))
                            );
                        }
                        Err(e) => {
                            let _ = writeln!(out, "MAPFILL error=flush spi={e:?}");
                        }
                    }
                }
            }
        }
        let _ = writeln!(out, "MAPFILL cycle={cycle} done");
        delay.delay_millis(10_000);
    }
    halt()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    // SAFETY: the panicking context owns nothing any more; a fresh driver
    // on UART0 is the only way left to say why.
    let uart0 = unsafe { esp_hal::peripherals::UART0::steal() };
    if let Ok(u) = Uart::new(uart0, UartConfig::default().with_baudrate(115_200)) {
        let mut u = u;
        let _ = writeln!(u, "MAPFILL panic {info}");
    }
    halt()
}
