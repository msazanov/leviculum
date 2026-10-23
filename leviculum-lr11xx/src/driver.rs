//! The async LR11xx driver.
//!
//! Generic over embedded-hal-async: an `SpiDevice` for the bus (chip select
//! included), an `OutputPin` for NRESET, and `Wait + InputPin` for BUSY
//! and the IRQ line (DIO9). Timeouts come from `embassy_time`, which is
//! what every board this crate targets runs; on the host the tests drive
//! the mock time driver.
//!
//! The method set is `leviculum-nrf/src/sx1262.rs`'s, name for name, so
//! the LoRa task that drives the SX1262 drives this part with its radio
//! type swapped. Where this part differs the difference is in the bytes,
//! not in the sequence: a two-transaction read, a 32-bit interrupt word,
//! frequencies in Hz, and a PA table instead of a register.
//!
//! High ACP errata (SWDR001 v3.0.0 README, "Known limitations"; LR1110 fw
//! 0x0303-0x0307, LR1120 fw 0x0101; after wake from sleep with retention;
//! workaround clears bit 30 of register 0x00F30054) does not apply: this
//! driver never issues `SetSleep`.
//!
//! The receive-arming order is `leviculum_rx_arming`'s, through the same
//! `RxPort` and `RxWindowProbe` implementations the SX1262 has, so the
//! guarantees that crate asserts (arm before hand-off, one standby per
//! window, adopt rather than replace) hold on this part by construction.

use embassy_time::{with_timeout, Duration, Instant, Timer};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::{Operation, SpiDevice};

use crate::board::{Board, Chip};
use crate::cmd::{self, irq, op};
use leviculum_core::sx126x::{RxArmClock, RxSite, RxTeardownBy};
use leviculum_rx_arming::RxArmState;

/// Log sink: a tag such as `"[SX_RX_ARM] "` and the formatted rest of the line.
pub type LogFn = fn(&'static str, core::fmt::Arguments<'_>);

/// Received packet status (RSSI and SNR), as the LoRa task reads it.
pub struct RxStatus {
    pub rssi: i16,
    pub snr: i16,
}

/// A non-destructive observation of the receiver's carrier latch and RSSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelSample {
    pub at_ms: u64,
    pub preamble: bool,
    pub header: bool,
    pub rssi_dbm: i16,
}

#[derive(Clone, Copy)]
struct ChannelSampler {
    period: Duration,
    sample: fn(ChannelSample),
}

/// What a frame that failed its payload CRC looked like on the air
/// (Codeberg #258). Same fields as the SX1262's: this part reports no
/// frequency error either.
#[derive(Clone, Copy)]
pub struct CrcErrFrame {
    pub len: u8,
    pub rssi: i16,
    pub snr: i16,
}

/// The transceiver's identity out of `GetVersion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub hw: u8,
    /// 1 LR1110, 2 LR1120, 3 LR1121 (`src/lr11xx_system_types.h:314-316`).
    pub kind: u8,
    pub fw: u16,
}

/// What `configure_lora` programmed for the transmit power, in the shape
/// the firmware's `[SX_TX_POWER]` fact carries. The field names are
/// `leviculum_core::sx126x::TxPowerProgram`'s, minus `ocp`: the LR11xx
/// has no over-current register to read a byte back from, and a zero
/// there would be an invented fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxPowerProgram {
    pub requested_dbm: i8,
    pub programmed_dbm: i8,
    pub pa_config: [u8; 4],
    pub ramp: u8,
    pub clamped: bool,
}

/// Driver errors.
pub enum Error {
    Spi,
    /// A GPIO (NRESET, BUSY or IRQ) returned an error.
    Gpio,
    Busy,
    Timeout,
    Crc(CrcErrFrame),
    /// `await_rx` was called with no window standing.
    NotArmed,
    /// `GetVersion` answered a type that is no LR11xx and not the bootloader (absent part).
    NoRadio {
        hw: u8,
        kind: u8,
        fw: u16,
    },
    /// The part runs its bootloader, not the transceiver firmware.
    Bootloader {
        hw: u8,
        fw: u16,
    },
    /// An LR11xx other than [`Board::chip`] answered.
    WrongChip {
        expected: Chip,
        hw: u8,
        kind: u8,
        fw: u16,
    },
    /// Stat1 reported `Fail` or `Perr` for a command.
    Command(cmd::CmdStatus),
    /// `GetErrors` after `Calibrate` was nonzero (see [`cmd::errors`]).
    Calibration {
        errors: u16,
    },
    /// The receive window ended on a LoRa header error.
    HeaderError,
    /// `transmit` got more than [`cmd::MAX_PAYLOAD`] bytes.
    PayloadTooLong {
        len: usize,
    },
    /// A received frame of `len` bytes does not fit the caller's buffer.
    BufferTooSmall {
        len: u8,
    },
}

/// Hand-written so `Crc` prints as the bare word (rig captures grep `error=Crc`).
impl core::fmt::Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Spi => f.write_str("Spi"),
            Error::Gpio => f.write_str("Gpio"),
            Error::Busy => f.write_str("Busy"),
            Error::Timeout => f.write_str("Timeout"),
            Error::Crc(_) => f.write_str("Crc"),
            Error::NotArmed => f.write_str("NotArmed"),
            Error::NoRadio { hw, kind, fw } => {
                write!(f, "NoRadio(hw=0x{hw:02x} type=0x{kind:02x} fw=0x{fw:04x})")
            }
            Error::Bootloader { hw, fw } => write!(f, "Bootloader(hw=0x{hw:02x} fw=0x{fw:04x})"),
            Error::WrongChip {
                expected,
                hw,
                kind,
                fw,
            } => write!(
                f,
                "WrongChip(expected={expected:?} hw=0x{hw:02x} type=0x{kind:02x} fw=0x{fw:04x})"
            ),
            Error::Command(status) => write!(f, "Command({status:?})"),
            Error::Calibration { errors } => write!(f, "Calibration(errors=0x{errors:04x})"),
            Error::HeaderError => f.write_str("HeaderError"),
            Error::PayloadTooLong { len } => write!(f, "PayloadTooLong(len={len})"),
            Error::BufferTooSmall { len } => write!(f, "BufferTooSmall(len={len})"),
        }
    }
}

/// The two status bytes a direct read answers, decoded.
#[derive(Clone, Copy)]
pub struct ChipStatus {
    pub stat1: cmd::Stat1,
    pub stat2: cmd::Stat2,
    pub irq: u32,
}

/// What `SetRx` was programmed with, carried from the arming half to the
/// awaiting half (see the SX1262's `ArmedWindow`).
#[derive(Clone, Copy)]
struct ArmedWindow {
    /// The bound the loop asked for — what adoption compares, so a
    /// provisional re-arm with the same ask stays the same window.
    timeout_ms: u32,
    /// The bound the chip's timer actually runs:
    /// [`leviculum_core::sx126x::rx_arm_bound_ms`] of the ask, which pads the
    /// two transmit-gating sites by the detector settle. The software wait is
    /// sized from this one.
    bound_ms: u32,
    site: RxSite,
    armed_at_ms: u64,
}

/// Settling is separate from teardown so harvesting through `RxWindowProbe`
/// can re-enter it without making the receive future recursively contain itself.
enum RxSettle {
    Frame(u8, RxStatus),
    /// The last honest latch of a window the chip may still be receiving in.
    WaitExpired(u32),
}

/// BUSY bounds: ordinary commands, calibration/boot, and `CfgLfClk` with a crystal wait.
const BUSY_TIMEOUT_MS: u32 = 100;
const BUSY_CALIBRATE_TIMEOUT_MS: u32 = 500;
const BUSY_LF_CLK_TIMEOUT_MS: u32 = 1_000;

/// The LR11xx driver.
pub struct Lr11xx<SPI, NRST, BUSY, IRQ> {
    spi: SPI,
    nrst: NRST,
    busy: BUSY,
    irq: IRQ,
    board: &'static Board,
    preamble_len: u16,
    /// The modulation the last `configure_lora` programmed, for the RX
    /// extension and the transmit deferral. `bw_hz` is 0 until configured.
    rx_ext_sf: u8,
    rx_ext_bw_hz: u32,
    rx_ext_cr_denom: u8,
    rx_arm: RxArmClock,
    rx_state: RxArmState<ArmedWindow>,
    /// Every IRQ flag the standing window latched, in the SX126x's
    /// positions (`irq::to_sx126x`), OR-ed over each read that concluded
    /// it; cleared when a window is armed. What the firmware's receive
    /// census classifies a window that delivered no frame by.
    rx_window_flags: u16,
    channel_sampler: Option<ChannelSampler>,
    log: Option<LogFn>,
    /// The `CalibImage` band last run, so a retune within it skips the calibration.
    image_band: Option<(u16, u16)>,
    /// Payload length in the last `SetPacketParams`; RX needs 0xFF (explicit headers above it fail).
    pkt_len: u8,
}

impl<SPI, NRST, BUSY, IRQ> Lr11xx<SPI, NRST, BUSY, IRQ>
where
    SPI: SpiDevice,
    NRST: OutputPin,
    BUSY: InputPin + Wait,
    IRQ: InputPin + Wait,
{
    /// Create a driver. Does NOT initialise the part: call `reset()` then
    /// `init_radio()` first.
    pub fn new(spi: SPI, nrst: NRST, busy: BUSY, irq: IRQ, board: &'static Board) -> Self {
        Self {
            spi,
            nrst,
            busy,
            irq,
            board,
            preamble_len: 24,
            rx_ext_sf: 0,
            rx_ext_bw_hz: 0,
            rx_ext_cr_denom: 0,
            rx_arm: RxArmClock::new(),
            rx_state: RxArmState::new(),
            rx_window_flags: 0,
            channel_sampler: None,
            log: None,
            image_band: None,
            pkt_len: 0xFF,
        }
    }

    /// The board this driver was built for.
    pub fn board(&self) -> &'static Board {
        self.board
    }

    /// The IRQ flags the last window latched before it ended, in the
    /// SX126x's positions; `0` until a window concludes. Read after a window
    /// that delivered no frame: a receive that delivered one has already
    /// re-armed, which clears it.
    pub fn rx_window_flags(&self) -> u16 {
        self.rx_window_flags
    }

    /// Sample the channel while waiting in RX. Sampling is disabled by default
    /// and belongs to this driver instance, not a firmware-global callback.
    ///
    /// Neither sample read clears IRQs or leaves RX. Failed reads produce no
    /// sample. The period must be positive, including at the clock's precision.
    ///
    /// # Panics
    ///
    /// Panics if `period` contains zero timer ticks.
    pub fn set_channel_sampler(&mut self, period: Duration, sample: fn(ChannelSample)) {
        assert!(
            period.as_ticks() > 0,
            "channel sample period must be positive"
        );
        self.channel_sampler = Some(ChannelSampler { period, sample });
    }

    /// Route this instance's `[SX_RX_*]` lines to `log`; dropped until set.
    pub fn set_log(&mut self, log: LogFn) {
        self.log = Some(log);
    }

    fn log(&self, prefix: &'static str, args: core::fmt::Arguments<'_>) {
        if let Some(log) = self.log {
            log(prefix, args);
        }
    }

    // Bus primitives

    async fn wait_busy_ms(&mut self, timeout_ms: u32) -> Result<(), Error> {
        if self.busy.is_low().map_err(|_| Error::Gpio)? {
            return Ok(());
        }
        match with_timeout(
            Duration::from_millis(timeout_ms as u64),
            self.busy.wait_for_low(),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(Error::Gpio),
            Err(_) => Err(Error::Busy),
        }
    }

    async fn wait_busy(&mut self) -> Result<(), Error> {
        self.wait_busy_ms(BUSY_TIMEOUT_MS).await
    }

    /// Pulse NRESET (5 ms low) and wait up to 500 ms for BUSY to fall.
    pub async fn reset(&mut self) -> Result<(), Error> {
        self.nrst.set_low().map_err(|_| Error::Gpio)?;
        Timer::after_millis(5).await;
        self.nrst.set_high().map_err(|_| Error::Gpio)?;
        Timer::after_millis(10).await;
        self.wait_busy_ms(BUSY_CALIBRATE_TIMEOUT_MS).await
    }

    /// One write transaction: opcode, then `params`
    /// (`src/lr11xx_hal.h`, `lr11xx_hal_write`). Waits for BUSY before.
    async fn write_command(&mut self, opcode: u16, params: &[u8]) -> Result<(), Error> {
        self.wait_busy().await?;
        let head = opcode.to_be_bytes();
        self.spi
            .transaction(&mut [Operation::Write(&head), Operation::Write(params)])
            .await
            .map_err(|_| Error::Spi)
    }

    /// The two-transaction read (`src/lr11xx_hal.h:118-128`): the command
    /// goes out, BUSY falls, and a second transaction clocks a status byte
    /// then `response.len()` bytes while sending NOPs. Returns Stat1.
    async fn read_command(
        &mut self,
        opcode: u16,
        params: &[u8],
        response: &mut [u8],
    ) -> Result<cmd::Stat1, Error> {
        self.write_command(opcode, params).await?;
        self.wait_busy().await?;
        let mut stat = [0u8; 1];
        // NOPs on MOSI while reading: the part echoes garbage back into
        // its command decoder otherwise (the HAL contract's remark).
        let zeros = [0u8; 256];
        debug_assert!(response.len() <= zeros.len());
        let n = response.len();
        self.spi
            .transaction(&mut [
                Operation::Transfer(&mut stat, &zeros[..1]),
                Operation::Transfer(&mut response[..n], &zeros[..n]),
            ])
            .await
            .map_err(|_| Error::Spi)?;
        Ok(cmd::Stat1::from_raw(stat[0]))
    }

    /// The direct read (`lr11xx_hal_direct_read`): no command, six NOPs
    /// clock out Stat1, Stat2 and the interrupt word.
    pub async fn get_status(&mut self) -> Result<ChipStatus, Error> {
        self.wait_busy().await?;
        let mut rx = [0u8; 6];
        let zeros = [0u8; 6];
        self.spi
            .transaction(&mut [Operation::Transfer(&mut rx, &zeros)])
            .await
            .map_err(|_| Error::Spi)?;
        let (stat1, stat2, irq) = cmd::decode_status(&rx);
        Ok(ChipStatus { stat1, stat2, irq })
    }

    /// The interrupt word, without clearing it.
    pub async fn get_irq_status(&mut self) -> Result<u32, Error> {
        Ok(self.get_status().await?.irq)
    }

    /// Instantaneous receiver RSSI (`GetRssiInst`, SWDR001 v3.0.0,
    /// `src/lr11xx_radio.c:339-355`). This read is allowed during reception.
    pub async fn get_rssi_inst(&mut self) -> Result<i16, Error> {
        let mut raw = [0u8; 1];
        self.read_command(op::GET_RSSI_INST, &[], &mut raw).await?;
        Ok(cmd::decode_rssi_inst(raw[0]))
    }

    async fn clear_irq(&mut self, mask: u32) -> Result<(), Error> {
        self.write_command(op::CLEAR_IRQ, &cmd::clear_irq(mask))
            .await
    }

    /// Standby RC.
    pub async fn set_standby_rc(&mut self) -> Result<(), Error> {
        self.write_command(op::SET_STANDBY, &cmd::set_standby(0x00))
            .await
    }

    /// `GetVersion`.
    pub async fn get_version(&mut self) -> Result<Version, Error> {
        let mut buf = [0u8; 4];
        self.read_command(op::GET_VERSION, &[], &mut buf).await?;
        let (hw, kind, fw) = cmd::decode_version(&buf);
        Ok(Version { hw, kind, fw })
    }

    /// Initialise after `reset()`: SWSD003 v2.4.0 `apps_common_lr11xx_system_init`
    /// (`lr11xx/common/apps_common.c:353-380`), then the LoRa radio settings:
    /// GetVersion, SetRegMode, SetDioAsRfSwitch, SetTcxoMode, CfgLfClk,
    /// ClearErrors, Calibrate, GetErrors, ClearErrors, ClearIrq, SetPacketType,
    /// SetRxTxFallbackMode, SetRxBoosted, SetLoRaPublicNetwork.
    pub async fn init_radio(&mut self) -> Result<Version, Error> {
        let mut buf = [0u8; 4];
        let stat = self.read_command(op::GET_VERSION, &[], &mut buf).await?;
        let (hw, kind, fw) = cmd::decode_version(&buf);
        let b = self.board;
        if kind == cmd::VERSION_TYPE_BOOTLOADER {
            return Err(Error::Bootloader { hw, fw });
        }
        if kind != b.chip as u8 {
            return Err(if matches!(kind, 0x01..=0x03) {
                Error::WrongChip {
                    expected: b.chip,
                    hw,
                    kind,
                    fw,
                }
            } else {
                Error::NoRadio { hw, kind, fw }
            });
        }
        command_ok(stat)?;
        self.write_command(op::SET_REG_MODE, &cmd::set_reg_mode(b.reg_mode as u8))
            .await?;
        let sw = b.rf_switch;
        self.write_command(
            op::SET_DIO_AS_RF_SWITCH,
            &cmd::set_dio_as_rf_switch(
                sw.enable, sw.standby, sw.rx, sw.tx, sw.tx_hp, sw.tx_hf, sw.gnss, sw.wifi,
            ),
        )
        .await?;
        if let Some(tcxo) = b.tcxo {
            self.write_command(
                op::SET_TCXO_MODE,
                &cmd::set_tcxo_mode(tcxo.voltage as u8, tcxo.startup_steps),
            )
            .await?;
        }
        self.write_command(
            op::CFG_LF_CLK,
            &cmd::cfg_lf_clk(b.lf_clk as u8, b.wait_32k_ready),
        )
        .await?;
        self.wait_busy_ms(BUSY_LF_CLK_TIMEOUT_MS).await?;
        self.write_command(op::CLEAR_ERRORS, &[]).await?;
        self.write_command(op::CALIBRATE, &cmd::calibrate(cmd::CALIB_ALL))
            .await?;
        self.wait_busy_ms(BUSY_CALIBRATE_TIMEOUT_MS).await?;
        let mut errors = [0u8; 2];
        let stat = self.read_command(op::GET_ERRORS, &[], &mut errors).await?;
        command_ok(stat)?;
        let errors = cmd::decode_errors(&errors);
        if errors != 0 {
            return Err(Error::Calibration { errors });
        }
        self.write_command(op::CLEAR_ERRORS, &[]).await?;
        self.clear_irq(irq::ALL).await?;
        self.write_command(op::SET_PKT_TYPE, &cmd::set_pkt_type(cmd::PKT_TYPE_LORA))
            .await?;
        self.write_command(
            op::SET_RX_TX_FALLBACK_MODE,
            &cmd::set_rx_tx_fallback_mode(cmd::FALLBACK_STDBY_RC),
        )
        .await?;
        self.write_command(op::SET_RX_BOOSTED, &cmd::set_rx_boosted(b.rx_boosted))
            .await?;
        self.write_command(
            op::SET_LORA_PUBLIC_NETWORK,
            &cmd::set_lora_public_network(cmd::NETWORK_PRIVATE),
        )
        .await?;
        self.image_band = None;
        Ok(Version { hw, kind, fw })
    }

    /// LoRa packet parameters for one frame of `payload_len` bytes.
    async fn set_packet_params(&mut self, payload_len: u8) -> Result<(), Error> {
        // Unknown until the write lands, so a failed write forces the next arm to rewrite it.
        self.pkt_len = 0;
        self.write_command(
            op::SET_PKT_PARAMS,
            &cmd::set_lora_pkt_params(self.preamble_len, true, payload_len, true, false),
        )
        .await?;
        self.pkt_len = payload_len;
        Ok(())
    }

    /// Configure the modem for LoRa TX/RX. Call after `init_radio()`.
    /// The register codes for `bw` and `cr` are the SX126x's, which this
    /// part shares. Returns what the PA was programmed with, for the
    /// firmware's `[SX_TX_POWER]` line.
    pub async fn configure_lora(
        &mut self,
        freq_hz: u32,
        sf: u8,
        bw: u8,
        cr: u8,
        power_dbm: i8,
        preamble_len: u16,
    ) -> Result<TxPowerProgram, Error> {
        // Every command below expects standby: a provisional window may be
        // standing when the loop takes a runtime config.
        self.disarm_rx(RxTeardownBy::Config).await?;
        self.preamble_len = preamble_len;
        self.rx_ext_sf = sf;
        // The SX126x codes (`src/lr11xx_radio_types.h:171-176`); 0 for an
        // unknown code is "not configured".
        self.rx_ext_bw_hz = leviculum_core::sx126x::bandwidth_hz_of_code(bw);
        self.rx_ext_cr_denom = cr.saturating_add(4);
        let band = cmd::image_band_mhz(freq_hz);
        if self.image_band != Some(band) {
            self.write_command(op::CALIBRATE_IMAGE, &cmd::calibrate_image(band.0, band.1))
                .await?;
            self.wait_busy_ms(BUSY_CALIBRATE_TIMEOUT_MS).await?;
            self.image_band = Some(band);
        }
        self.write_command(op::SET_RF_FREQUENCY, &cmd::set_rf_frequency(freq_hz))
            .await?;
        let board = self.board;
        if let Some(set) = &board.rssi_calibration {
            let cal = set.for_freq(freq_hz);
            self.write_command(
                op::SET_RSSI_CALIBRATION,
                &cmd::set_rssi_calibration(&cal.gain_tune, cal.gain_offset),
            )
            .await?;
        }
        // Transmit power: the board table decides the PA and its four
        // bytes for the requested output (Codeberg #349's contract: the
        // request reaches the chip, clamped to what the part can do).
        let choice = self.board.pa_for_dbm(power_dbm);
        let pa_config = choice.row.pa_cfg();
        self.write_command(op::SET_PA_CFG, &pa_config).await?;
        self.write_command(
            op::SET_TX_PARAMS,
            &cmd::set_tx_params(choice.row.power, cmd::RAMP_48_US),
        )
        .await?;
        let ldro = leviculum_core::sx126x::ldro_enabled(self.rx_ext_bw_hz, sf);
        self.write_command(
            op::SET_MODULATION_PARAMS,
            &cmd::set_lora_modulation_params(sf, bw, cr, ldro),
        )
        .await?;
        self.set_packet_params(0xFF).await?;
        // The hardware RX timeout only bounds the preamble wait; once a
        // preamble is detected the reception runs to completion (same
        // reason as the SX1262's `SetStopRxTimerOnPreamble`).
        self.write_command(
            op::STOP_TIMEOUT_ON_PREAMBLE,
            &cmd::stop_timeout_on_preamble(true),
        )
        .await?;
        Ok(TxPowerProgram {
            requested_dbm: power_dbm,
            programmed_dbm: choice.row.power,
            pa_config,
            ramp: cmd::RAMP_48_US,
            clamped: choice.clamped,
        })
    }

    // TX

    /// Transmit a frame. Blocks until TxDone or `timeout_ms`.
    pub async fn transmit(&mut self, data: &[u8], timeout_ms: u32) -> Result<(), Error> {
        if data.len() > cmd::MAX_PAYLOAD {
            return Err(Error::PayloadTooLong { len: data.len() });
        }
        self.disarm_rx(RxTeardownBy::Tx).await?;
        self.set_packet_params(data.len() as u8).await?;
        self.write_command(
            op::SET_DIO_IRQ_PARAMS,
            &cmd::set_dio_irq_params(irq::DIO_TX, 0),
        )
        .await?;
        self.clear_irq(irq::ALL).await?;
        // The payload, in one transaction so no 258-byte stack buffer.
        self.wait_busy().await?;
        let head = op::WRITE_BUFFER8.to_be_bytes();
        self.spi
            .transaction(&mut [Operation::Write(&head), Operation::Write(data)])
            .await
            .map_err(|_| Error::Spi)?;
        // No hardware timeout: the software one below bounds it.
        self.write_command(op::SET_TX, &cmd::set_tx(0)).await?;
        match with_timeout(
            Duration::from_millis(timeout_ms.max(100) as u64),
            self.irq.wait_for_high(),
        )
        .await
        {
            Ok(Ok(())) => {
                let flags = self.get_irq_status().await?;
                self.clear_irq(irq::ALL).await?;
                if flags & irq::TX_DONE != 0 {
                    return Ok(());
                }
                let _ = self.set_standby_rc().await;
                Err(Error::Timeout)
            }
            _ => {
                let _ = self.set_standby_rc().await;
                Err(Error::Timeout)
            }
        }
    }

    // RX

    async fn get_rx_buffer_status(&mut self) -> Result<(u8, u8), Error> {
        let mut buf = [0u8; 2];
        self.read_command(op::GET_RX_BUFFER_STATUS, &[], &mut buf)
            .await?;
        Ok(cmd::decode_rx_buffer_status(&buf))
    }

    async fn read_buffer(&mut self, offset: u8, data: &mut [u8]) -> Result<(), Error> {
        let len = data.len().min(255) as u8;
        self.read_command(
            op::READ_BUFFER8,
            &cmd::read_buffer8(offset, len),
            &mut data[..len as usize],
        )
        .await?;
        Ok(())
    }

    async fn get_packet_status(&mut self) -> Result<RxStatus, Error> {
        let mut buf = [0u8; 3];
        self.read_command(op::GET_PKT_STATUS, &[], &mut buf).await?;
        let (rssi, snr) = cmd::decode_lora_pkt_status(&buf);
        Ok(RxStatus { rssi, snr })
    }

    /// Standby if a window is standing, uninstrumented (see the SX1262's
    /// `standby_rx`). The state clears only after the command completes.
    async fn standby_rx(&mut self) -> Result<(), Error> {
        if !self.rx_state.standby_owed() {
            return Ok(());
        }
        self.set_standby_rc().await?;
        self.rx_state.disarmed();
        self.rx_arm.window_ended(now_ms());
        Ok(())
    }

    /// Stand the receiver down, if a window is standing, and record what
    /// was on the air when it went down.
    pub async fn disarm_rx(&mut self, by: RxTeardownBy) -> Result<(), Error> {
        leviculum_rx_arming::stand_down(self, by.tag()).await
    }

    /// Stand the receiver down for a transmit, waiting first if the window
    /// is holding a frame that is still arriving.
    ///
    /// `OneFrame` is for one externally paced event, such as the outgoing
    /// select arm. `ReleaseFalsePreamble` is for loop-paced retries: it keeps
    /// the full frame bound after a decoded header, but releases a bare
    /// carrier after the programmed preamble and header should have passed.
    /// A completed reception is harvested into `sink` under either policy.
    pub async fn disarm_rx_for_tx<S>(
        &mut self,
        by: RxTeardownBy,
        policy: leviculum_rx_arming::DeferPolicy,
        buf: &mut [u8],
        sink: &mut S,
    ) -> Result<(), Error>
    where
        S: leviculum_rx_arming::FrameSink<Meta = RxStatus>,
    {
        leviculum_rx_arming::stand_down_for_tx(self, by.tag(), policy, buf, sink).await
    }

    /// Arm the receiver. `timeout_ms == 0` is single mode: no hardware
    /// timeout, listen until a frame arrives.
    pub async fn arm_rx(&mut self, timeout_ms: u32, site: RxSite) -> Result<(), Error> {
        self.disarm_rx(RxTeardownBy::Arm).await?;
        if self.pkt_len != 0xFF {
            self.set_packet_params(0xFF).await?;
        }
        self.write_command(
            op::SET_DIO_IRQ_PARAMS,
            &cmd::set_dio_irq_params(irq::DIO_RX, 0),
        )
        .await?;
        self.clear_irq(irq::ALL).await?;
        // The chip's timer runs the settle past the ask on the two
        // transmit-gating sites, so a preamble beginning inside the window's
        // last detector horizon latches and stops it (929, #466); see
        // `rx_arm_bound_ms` for the measured losses. The ask itself stays in
        // the window for adoption.
        let bound_ms = leviculum_core::sx126x::rx_arm_bound_ms(
            timeout_ms,
            site,
            self.rx_ext_bw_hz,
            self.rx_ext_sf,
        );
        // Recorded before the command goes out: a future dropped inside the
        // transaction still owes a standby.
        self.rx_window_flags = 0;
        self.rx_state.arming(ArmedWindow {
            timeout_ms,
            bound_ms,
            site,
            armed_at_ms: now_ms(),
        });
        self.write_command(op::SET_RX, &cmd::set_rx(cmd::ms_to_rtc_steps(bound_ms)))
            .await?;
        let arm = self.rx_arm.arm(site, bound_ms, now_ms());
        self.log("[SX_RX_ARM] ", format_args!("{arm}"));
        Ok(())
    }

    /// Wait for the standing window's terminating IRQ and read the frame
    /// out. The chip has left RX when this returns.
    pub async fn await_rx(&mut self, buf: &mut [u8]) -> Result<(u8, RxStatus), Error> {
        self.wait_rx_ready().await?;
        self.read_ready_rx(buf).await
    }

    /// Wait for the receive IRQ or the software wait bound without consuming
    /// the latch, reading a payload or standing the receiver down.
    ///
    /// This half may be canceled by an outgoing/configuration wake. After it
    /// returns, finish with [`Self::read_ready_rx`] outside that select; the
    /// software wait expiring is not itself a receive verdict.
    pub async fn wait_rx_ready(&mut self) -> Result<(), Error> {
        let Some(&ArmedWindow { bound_ms, .. }) = self.rx_state.window() else {
            return Err(Error::NotArmed);
        };
        let sw_timeout_ms = if bound_ms == 0 {
            60_000u64
        } else {
            bound_ms as u64 + 500
        };
        self.wait_rx_irq(Duration::from_millis(sw_timeout_ms)).await;
        Ok(())
    }

    /// Settle a ready or expired receive window and read/harvest its frame.
    ///
    /// Do not race this half against configuration or outgoing work: IRQ clear
    /// and frame readout are a single ownership handoff, even though the SPI
    /// operations await. Stage any returned packet before another cancelable
    /// radio wait.
    pub async fn read_ready_rx(&mut self, buf: &mut [u8]) -> Result<(u8, RxStatus), Error> {
        if self.rx_state.window().is_none() {
            return Err(Error::NotArmed);
        }
        self.rx_arm.window_ended(now_ms());
        let flags = self.get_irq_status().await?;
        self.finish_rx(flags, buf).await
    }

    /// Slice only the IRQ wait, retaining one absolute bound across sample
    /// reads. The same wait is used by extensions and receive deferrals.
    async fn wait_rx_irq(&mut self, timeout: Duration) {
        let Some(sampler) = self.channel_sampler else {
            let _ = with_timeout(timeout, self.irq.wait_for_high()).await;
            return;
        };
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline
                .as_ticks()
                .saturating_sub(Instant::now().as_ticks());
            if remaining == 0 {
                return;
            }
            let step = Duration::from_ticks(remaining.min(sampler.period.as_ticks()));
            match with_timeout(step, self.irq.wait_for_high()).await {
                Ok(_) => return,
                Err(_) if Instant::now() >= deadline => return,
                Err(_) => self.sample_channel(sampler.sample).await,
            }
        }
    }

    async fn sample_channel(&mut self, sample: fn(ChannelSample)) {
        let Ok(flags) = self.get_irq_status().await else {
            return;
        };
        let Ok(rssi_dbm) = self.get_rssi_inst().await else {
            return;
        };
        sample(ChannelSample {
            at_ms: now_ms(),
            preamble: flags & irq::PREAMBLE_DETECTED != 0,
            header: flags & irq::SYNC_WORD_HEADER_VALID != 0,
            rssi_dbm,
        });
    }

    /// Take a reception the chip has already completed, without waiting.
    async fn take_latched_frame(
        &mut self,
        buf: &mut [u8],
    ) -> Result<Option<(u8, RxStatus)>, Error> {
        if !self.rx_state.standby_owed() {
            return Err(Error::NotArmed);
        }
        let flags = self.get_irq_status().await?;
        if flags & (irq::RX_DONE | irq::TIMEOUT) == 0 {
            return Ok(None);
        }
        self.rx_arm.window_ended(now_ms());
        match self.settle_rx(flags, buf).await? {
            RxSettle::Frame(len, status) => Ok(Some((len, status))),
            RxSettle::WaitExpired(_) => Ok(None),
        }
    }

    /// Finish a wait without discarding a reception still on the air. The
    /// shared policy receives the last honest latch, before any IRQ clear.
    async fn finish_rx(&mut self, flags: u32, buf: &mut [u8]) -> Result<(u8, RxStatus), Error> {
        let flags = match self.settle_rx(flags, buf).await? {
            RxSettle::Frame(len, status) => return Ok((len, status)),
            RxSettle::WaitExpired(flags) => flags,
        };
        match leviculum_rx_arming::stand_down_for_rx(
            self,
            RxTeardownBy::RxWait.tag(),
            leviculum_rx_arming::DeferPolicy::ReleaseFalsePreamble,
            latch_of(flags),
            buf,
        )
        .await
        {
            Ok(Some(frame)) => Ok(frame),
            _ => Err(Error::Timeout),
        }
    }

    /// Classify a window without re-entering the shared teardown policy.
    /// Nonterminal expiry leaves both the receiver and its IRQ latch intact.
    async fn settle_rx(&mut self, mut flags: u32, buf: &mut [u8]) -> Result<RxSettle, Error> {
        if self.rx_state.window().is_none() {
            return Err(Error::NotArmed);
        }
        self.rx_window_flags |= irq::to_sx126x(flags);
        if header_error_only(flags) {
            return self.end_on_header_error(flags).await;
        }
        if let Some(extend_ms) = leviculum_core::sx126x::rx_extend_ms(
            irq::to_sx126x(flags),
            self.rx_ext_bw_hz,
            self.rx_ext_sf,
            self.rx_ext_cr_denom,
            self.preamble_len,
        ) {
            self.log(
                "[SX_RX_EXTEND] ",
                format_args!(
                    "flags={:#010x} extend_ms={} sf={} bw_hz={}",
                    flags, extend_ms, self.rx_ext_sf, self.rx_ext_bw_hz
                ),
            );
            self.wait_rx_irq(Duration::from_millis(extend_ms)).await;
            self.rx_arm.window_ended(now_ms());
            flags = self.get_irq_status().await?;
            self.rx_window_flags |= irq::to_sx126x(flags);
            if header_error_only(flags) {
                return self.end_on_header_error(flags).await;
            }
        }
        // Both terminating IRQs return to standby through the programmed
        // fallback mode. Record that before a fallible readout.
        if flags & (irq::RX_DONE | irq::TIMEOUT) == 0 {
            return Ok(RxSettle::WaitExpired(flags));
        }
        self.rx_state.chip_left_rx();
        self.clear_irq(irq::ALL).await?;
        if flags & irq::RX_DONE != 0 {
            if flags & irq::CRC_ERROR != 0 {
                let (len, _ptr) = self.get_rx_buffer_status().await?;
                let status = self.get_packet_status().await?;
                return Err(Error::Crc(CrcErrFrame {
                    len,
                    rssi: status.rssi,
                    snr: status.snr,
                }));
            }
            let (len, ptr) = self.get_rx_buffer_status().await?;
            if len as usize > buf.len() {
                return Err(Error::BufferTooSmall { len });
            }
            self.read_buffer(ptr, &mut buf[..len as usize]).await?;
            let status = self.get_packet_status().await?;
            Ok(RxSettle::Frame(len, status))
        } else {
            Err(Error::Timeout)
        }
    }

    /// A header error ends the window: standby, clear, report.
    async fn end_on_header_error(&mut self, flags: u32) -> Result<RxSettle, Error> {
        self.standby_rx().await?;
        self.clear_irq(irq::ALL).await?;
        self.log("[SX_RX_HEADER_ERR] ", format_args!("flags={:#010x}", flags));
        Err(Error::HeaderError)
    }

    // CAD

    /// Channel activity detection. `true` when a LoRa preamble was
    /// detected. The chip is in standby afterwards either way.
    ///
    /// Detection thresholds are Semtech's LR11xx example values
    /// (SWSD003 `lr11xx/apps/cad/main_cad.h:66-73`: peak 50, minimum 10);
    /// 4 symbols up to SF9, 8 from SF10.
    ///
    /// A reception in the standing window is deferred or harvested into
    /// `sink` before CAD can replace it. `buf` belongs to that receive handoff.
    pub async fn cad<S>(&mut self, sf: u8, buf: &mut [u8], sink: &mut S) -> Result<bool, Error>
    where
        S: leviculum_rx_arming::FrameSink<Meta = RxStatus>,
    {
        let symbols = if sf >= 10 { 8u8 } else { 4u8 };
        self.disarm_rx_for_tx(
            RxTeardownBy::Cad,
            leviculum_rx_arming::DeferPolicy::ReleaseFalsePreamble,
            buf,
            sink,
        )
        .await?;
        self.write_command(
            op::SET_CAD_PARAMS,
            &cmd::set_cad_params(symbols, 50, 10, cmd::CAD_EXIT_STANDBY_RC, 0),
        )
        .await?;
        self.write_command(
            op::SET_DIO_IRQ_PARAMS,
            &cmd::set_dio_irq_params(irq::DIO_CAD, 0),
        )
        .await?;
        self.clear_irq(irq::ALL).await?;
        self.write_command(op::SET_CAD, &[]).await?;
        let timeout_ms = leviculum_core::sx126x::cad_timeout_ms(self.rx_ext_bw_hz, sf, symbols);
        match with_timeout(
            Duration::from_millis(timeout_ms as u64),
            self.irq.wait_for_high(),
        )
        .await
        {
            Ok(Ok(())) => {
                let flags = self.get_irq_status().await?;
                self.clear_irq(irq::ALL).await?;
                Ok(flags & irq::CAD_DETECTED != 0)
            }
            _ => {
                let _ = self.set_standby_rc().await;
                Err(Error::Timeout)
            }
        }
    }
}

/// `Fail`/`Perr` in Stat1 is an error.
fn command_ok(stat: cmd::Stat1) -> Result<(), Error> {
    match stat.command {
        cmd::CmdStatus::Fail | cmd::CmdStatus::Perr => Err(Error::Command(stat.command)),
        cmd::CmdStatus::Ok | cmd::CmdStatus::Data => Ok(()),
    }
}

fn header_error_only(flags: u32) -> bool {
    flags & irq::HEADER_ERROR != 0 && flags & (irq::RX_DONE | irq::TIMEOUT) == 0
}

fn now_ms() -> u64 {
    embassy_time::Instant::now().as_millis()
}

fn latch_of(flags: u32) -> leviculum_rx_arming::RxLatch {
    leviculum_rx_arming::RxLatch {
        raw: irq::to_sx126x(flags),
        preamble: flags & irq::PREAMBLE_DETECTED != 0,
        header: flags & irq::SYNC_WORD_HEADER_VALID != 0,
        rxdone: flags & irq::RX_DONE != 0,
    }
}

/// The receive half, as `leviculum_rx_arming` sees it: three forwarding
/// methods, no logic.
impl<SPI, NRST, BUSY, IRQ> leviculum_rx_arming::RxPort for Lr11xx<SPI, NRST, BUSY, IRQ>
where
    SPI: SpiDevice,
    NRST: OutputPin,
    BUSY: InputPin + Wait,
    IRQ: InputPin + Wait,
{
    type Window = (u32, RxSite);
    type Meta = RxStatus;
    type Error = Error;

    async fn arm(&mut self, window: Self::Window) -> Result<(), Error> {
        self.arm_rx(window.0, window.1).await
    }

    async fn await_frame(&mut self, buf: &mut [u8]) -> Result<(u8, RxStatus), Error> {
        self.await_rx(buf).await
    }

    async fn disarm(&mut self) -> Result<(), Error> {
        self.standby_rx().await
    }
}

impl<SPI, NRST, BUSY, IRQ> leviculum_rx_arming::RxWindowProbe for Lr11xx<SPI, NRST, BUSY, IRQ>
where
    SPI: SpiDevice,
    NRST: OutputPin,
    BUSY: InputPin + Wait,
    IRQ: InputPin + Wait,
{
    fn standing_window(&self) -> Option<leviculum_rx_arming::StandingWindow<(u32, RxSite)>> {
        self.rx_state
            .window()
            .map(|w| leviculum_rx_arming::StandingWindow {
                window: (w.timeout_ms, w.site),
                stood_ms: now_ms().saturating_sub(w.armed_at_ms).min(u32::MAX as u64) as u32,
            })
    }

    fn window_ends_itself(&self, window: &(u32, RxSite)) -> bool {
        window.0 != 0
    }

    async fn latched(&mut self) -> Result<leviculum_rx_arming::RxLatch, Error> {
        // A read, never a clear: on an adopted window a clear here would
        // consume the RxDone the awaiting half is about to take.
        let flags = self.get_irq_status().await?;
        Ok(latch_of(flags))
    }

    async fn take_latched_frame(
        &mut self,
        buf: &mut [u8],
    ) -> Result<Option<(u8, RxStatus)>, Error> {
        Lr11xx::take_latched_frame(self, buf).await
    }

    fn defer_ms(&self, latch: &leviculum_rx_arming::RxLatch) -> Option<u64> {
        leviculum_core::sx126x::tx_defer_ms(
            latch.raw,
            self.rx_ext_bw_hz,
            self.rx_ext_sf,
            self.rx_ext_cr_denom,
            self.preamble_len,
        )
    }

    fn false_preamble_ms(&self) -> Option<u64> {
        leviculum_core::sx126x::false_preamble_ms(
            self.rx_ext_bw_hz,
            self.rx_ext_sf,
            self.preamble_len,
        )
    }

    /// Forwards to `sx126x::latch_settle_ms` against the same cached
    /// modulation the other two bounds use. This part is where the bound was
    /// measured (925): a status read 7–10 ms after the arm, inside a
    /// back-to-back follower's preamble, reads `preamble=0` and must not be
    /// believed yet.
    fn latch_settle_ms(&self) -> Option<u64> {
        leviculum_core::sx126x::latch_settle_ms(self.rx_ext_bw_hz, self.rx_ext_sf)
    }

    async fn wait_for_frame(&mut self, wait_ms: u64) -> u64 {
        let started = Instant::now();
        self.wait_rx_irq(Duration::from_millis(wait_ms)).await;
        started.elapsed().as_millis()
    }

    fn report(&mut self, event: leviculum_rx_arming::RxEvent<'_, Error>) {
        match event {
            leviculum_rx_arming::RxEvent::Adopted(adopt) => {
                self.log("[SX_RX_ADOPT] ", format_args!("{adopt}"));
            }
            leviculum_rx_arming::RxEvent::TornDown(teardown) => {
                self.log("[SX_RX_TEARDOWN] ", format_args!("{teardown}"));
            }
            leviculum_rx_arming::RxEvent::Harvested(harvest) => {
                self.log("[SX_RX_HARVEST] ", format_args!("{harvest}"));
            }
            leviculum_rx_arming::RxEvent::Deferred(defer) => {
                self.log("[SX_TX_DEFER] ", format_args!("{defer}"));
            }
            leviculum_rx_arming::RxEvent::ProbeFailed { at, error } => {
                self.log(
                    "[SX_RX_PROBE_ERR] ",
                    format_args!("at={} error={:?}", at, error),
                );
            }
        }
    }
}
