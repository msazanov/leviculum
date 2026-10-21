//! LR11xx commands: opcodes, argument encoders, interrupt flags, status
//! bytes.
//!
//! Every encoder here is a pure function from parameters to the bytes that
//! go out after the two opcode bytes, and each is pinned by a test against
//! the layout in Semtech's SWDR001 v3.0.0 (line references are into that
//! tree). The driver does nothing with a byte that is not built here.
//!
//! The LR11xx transaction shape (`src/lr11xx_hal.h:95-140`): a write is
//! one transaction of opcode plus arguments; a read is two, the command
//! transaction, then after BUSY falls a second one that clocks out a
//! status byte followed by the response while the host sends NOPs. A
//! direct read is one transaction of NOPs with no command, and answers
//! Stat1, Stat2 and the 32-bit interrupt status.

/// System commands (`src/lr11xx_system.c:95-121`).
pub mod op {
    pub const GET_STATUS: u16 = 0x0100;
    pub const GET_VERSION: u16 = 0x0101;
    pub const GET_ERRORS: u16 = 0x010D;
    pub const CLEAR_ERRORS: u16 = 0x010E;
    pub const CALIBRATE: u16 = 0x010F;
    pub const SET_REG_MODE: u16 = 0x0110;
    pub const CALIBRATE_IMAGE: u16 = 0x0111;
    pub const SET_DIO_AS_RF_SWITCH: u16 = 0x0112;
    pub const SET_DIO_IRQ_PARAMS: u16 = 0x0113;
    pub const CLEAR_IRQ: u16 = 0x0114;
    pub const CFG_LF_CLK: u16 = 0x0116;
    pub const SET_TCXO_MODE: u16 = 0x0117;
    pub const SET_STANDBY: u16 = 0x011C;
    // Register and buffer access (`src/lr11xx_regmem.c:72-77`).
    pub const WRITE_BUFFER8: u16 = 0x0109;
    pub const READ_BUFFER8: u16 = 0x010A;
    // Radio commands (`src/lr11xx_radio.c:135-169`).
    pub const GET_RX_BUFFER_STATUS: u16 = 0x0203;
    pub const GET_PKT_STATUS: u16 = 0x0204;
    pub const GET_RSSI_INST: u16 = 0x0205;
    pub const SET_LORA_PUBLIC_NETWORK: u16 = 0x0208;
    pub const SET_RX: u16 = 0x0209;
    pub const SET_TX: u16 = 0x020A;
    pub const SET_RF_FREQUENCY: u16 = 0x020B;
    pub const SET_CAD_PARAMS: u16 = 0x020D;
    pub const SET_PKT_TYPE: u16 = 0x020E;
    pub const SET_MODULATION_PARAMS: u16 = 0x020F;
    pub const SET_PKT_PARAMS: u16 = 0x0210;
    pub const SET_TX_PARAMS: u16 = 0x0211;
    pub const SET_RX_TX_FALLBACK_MODE: u16 = 0x0213;
    pub const SET_PA_CFG: u16 = 0x0215;
    pub const STOP_TIMEOUT_ON_PREAMBLE: u16 = 0x0217;
    pub const SET_CAD: u16 = 0x0218;
    pub const SET_RX_BOOSTED: u16 = 0x0227;
    pub const SET_RSSI_CALIBRATION: u16 = 0x0229;
}

/// Interrupt flags, the 32-bit word `GetStatus` and `SetDioIrqParams`
/// carry (`src/lr11xx_system_types.h:100-128`).
pub mod irq {
    pub const TX_DONE: u32 = 1 << 2;
    pub const RX_DONE: u32 = 1 << 3;
    pub const PREAMBLE_DETECTED: u32 = 1 << 4;
    pub const SYNC_WORD_HEADER_VALID: u32 = 1 << 5;
    pub const HEADER_ERROR: u32 = 1 << 6;
    pub const CRC_ERROR: u32 = 1 << 7;
    pub const CAD_DONE: u32 = 1 << 8;
    pub const CAD_DETECTED: u32 = 1 << 9;
    pub const TIMEOUT: u32 = 1 << 10;
    pub const CMD_ERROR: u32 = 1 << 22;
    pub const ERROR: u32 = 1 << 23;
    /// Every flag: what `ClearIrq` is given between operations.
    pub const ALL: u32 = 0xFFFF_FFFF;

    /// The flags a transmit routes to the IRQ pin.
    pub const DIO_TX: u32 = TX_DONE | TIMEOUT;
    /// RX window terminators; header error as in SWSD003 v2.4.0 `main_ping_pong.c:100-102`.
    pub const DIO_RX: u32 = RX_DONE | CRC_ERROR | TIMEOUT | HEADER_ERROR;
    /// The flags a channel-activity detection routes to the IRQ pin.
    pub const DIO_CAD: u32 = CAD_DONE | CAD_DETECTED;

    /// The same events in the SX126x's 16-bit flag positions
    /// (`leviculum_core::sx126x::IRQ_*`), so the PHY helpers that decide
    /// on them (`rx_extend_ms`, `tx_defer_ms`) and the `RxLatch` the
    /// arming crate carries keep their one definition. Bits with no SX126x
    /// counterpart are dropped; nothing downstream reads them.
    pub fn to_sx126x(flags: u32) -> u16 {
        let mut out = 0u16;
        if flags & TX_DONE != 0 {
            out |= leviculum_core::sx126x::IRQ_TX_DONE;
        }
        if flags & RX_DONE != 0 {
            out |= leviculum_core::sx126x::IRQ_RX_DONE;
        }
        if flags & PREAMBLE_DETECTED != 0 {
            out |= leviculum_core::sx126x::IRQ_PREAMBLE_DETECTED;
        }
        if flags & SYNC_WORD_HEADER_VALID != 0 {
            out |= leviculum_core::sx126x::IRQ_HEADER_VALID;
        }
        if flags & HEADER_ERROR != 0 {
            out |= leviculum_core::sx126x::IRQ_HEADER_ERR;
        }
        if flags & CRC_ERROR != 0 {
            out |= leviculum_core::sx126x::IRQ_CRC_ERR;
        }
        if flags & CAD_DONE != 0 {
            out |= leviculum_core::sx126x::IRQ_CAD_DONE;
        }
        if flags & CAD_DETECTED != 0 {
            out |= leviculum_core::sx126x::IRQ_CAD_DETECTED;
        }
        if flags & TIMEOUT != 0 {
            out |= leviculum_core::sx126x::IRQ_TIMEOUT;
        }
        out
    }
}

/// `GetErrors` bits (`src/lr11xx_system_types.h:160-167`).
pub mod errors {
    pub const LF_RC_CALIB: u16 = 1 << 0;
    pub const HF_RC_CALIB: u16 = 1 << 1;
    pub const ADC_CALIB: u16 = 1 << 2;
    pub const PLL_CALIB: u16 = 1 << 3;
    pub const IMG_CALIB: u16 = 1 << 4;
    /// 32 MHz XOSC/TCXO did not start (wrong TCXO voltage or start-up time).
    pub const HF_XOSC_START: u16 = 1 << 5;
    /// 32.768 kHz crystal did not start (`LfClk::Xtal` on a board without one).
    pub const LF_XOSC_START: u16 = 1 << 6;
    pub const PLL_LOCK: u16 = 1 << 7;
}

/// `GetErrors` response, big-endian (`src/lr11xx_system.c:214-230`).
pub fn decode_errors(bytes: &[u8; 2]) -> u16 {
    u16::from_be_bytes(*bytes)
}

/// Command status out of Stat1 bits 3:1 (`src/lr11xx_system.c:649-650`,
/// `src/lr11xx_system_types.h:205-208`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdStatus {
    /// The previous command could not be executed.
    Fail,
    /// The previous command had an invalid parameter.
    Perr,
    /// The previous command executed.
    Ok,
    /// The previous command executed and data is waiting.
    Data,
}

/// Stat1 decoded (`src/lr11xx_system.c:649-650`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat1 {
    pub raw: u8,
    pub interrupt_active: bool,
    pub command: CmdStatus,
}

impl Stat1 {
    pub const fn from_raw(raw: u8) -> Self {
        Self {
            raw,
            interrupt_active: raw & 0x01 != 0,
            command: match (raw >> 1) & 0x07 {
                0 => CmdStatus::Fail,
                1 => CmdStatus::Perr,
                2 => CmdStatus::Ok,
                _ => CmdStatus::Data,
            },
        }
    }
}

/// Stat2 decoded (`src/lr11xx_system.c:658-660`): chip mode in bits 3:1,
/// the reset cause in bits 7:4, bit 0 set when running from flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat2 {
    pub raw: u8,
    /// 0 sleep, 1 standby RC, 2 standby XOSC, 3 FS, 4 RX, 5 TX, 6 WiFi/GNSS.
    pub chip_mode: u8,
    /// `src/lr11xx_system_types.h:191-197`: 0 cleared, 1 analog, 2 external,
    /// 3 system, 4 watchdog, 5 IOCD restart, 6 RTC restart.
    pub reset_status: u8,
    pub running_from_flash: bool,
}

impl Stat2 {
    pub const fn from_raw(raw: u8) -> Self {
        Self {
            raw,
            chip_mode: (raw & 0x0F) >> 1,
            reset_status: (raw & 0xF0) >> 4,
            running_from_flash: raw & 0x01 != 0,
        }
    }
}

/// The six bytes a direct read answers: Stat1, Stat2, the interrupt word
/// (`src/lr11xx_system.c:160-176`).
pub fn decode_status(bytes: &[u8; 6]) -> (Stat1, Stat2, u32) {
    (
        Stat1::from_raw(bytes[0]),
        Stat2::from_raw(bytes[1]),
        u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]),
    )
}

/// `GetVersion`'s four response bytes (`src/lr11xx_system.c:193-205`).
pub fn decode_version(bytes: &[u8; 4]) -> (u8, u8, u16) {
    (bytes[0], bytes[1], u16::from_be_bytes([bytes[2], bytes[3]]))
}

/// `GetVersion` type while the bootloader runs (LR11xx user manual, GetVersion "Use Case").
pub const VERSION_TYPE_BOOTLOADER: u8 = 0xDF;

/// Longest LoRa payload (one-byte length field).
pub const MAX_PAYLOAD: usize = 255;

/// LoRa packet status, three bytes (`src/lr11xx_radio.c:318-330`): RSSI
/// in -dBm/2, SNR in dB/4 with rounding, signal RSSI.
pub fn decode_lora_pkt_status(bytes: &[u8; 3]) -> (i16, i16) {
    let rssi = -i16::from(bytes[0] >> 1);
    let snr = (i16::from(bytes[1] as i8) + 2) >> 2;
    (rssi, snr)
}

/// `GetRssiInst`'s response (`src/lr11xx_radio.c:339-355`), truncated to
/// whole dBm exactly as SWDR001 v3.0.0 does.
pub const fn decode_rssi_inst(raw: u8) -> i16 {
    -((raw >> 1) as i16)
}

/// `GetRxBufferStatus`'s two bytes: payload length, buffer start
/// (`src/lr11xx_radio.c:270-282`).
pub fn decode_rx_buffer_status(bytes: &[u8; 2]) -> (u8, u8) {
    (bytes[0], bytes[1])
}

/// Milliseconds to the 32.768 kHz RTC steps `SetRx`/`SetTx` take
/// (`src/lr11xx_radio.c:108`), saturated to 0xFFFFFE (0xFFFFFF is continuous RX).
pub const fn ms_to_rtc_steps(ms: u32) -> u32 {
    let steps = (ms as u64 * 32_768) / 1_000;
    if steps > 0x00FF_FFFE {
        0x00FF_FFFE
    } else {
        steps as u32
    }
}

const fn u24(v: u32) -> [u8; 3] {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

// System command arguments.

/// `SetRegMode` (`src/lr11xx_system.c:254-262`): 0 LDO, 1 DC-DC.
pub const fn set_reg_mode(mode: u8) -> [u8; 1] {
    [mode]
}

/// `SetDioAsRfSwitch` (`src/lr11xx_system.c:290-306`): which RFSW lines
/// are used, then the pattern for standby, RX, TX, TX high power, TX high
/// frequency, GNSS and WiFi.
#[allow(clippy::too_many_arguments)]
pub const fn set_dio_as_rf_switch(
    enable: u8,
    standby: u8,
    rx: u8,
    tx: u8,
    tx_hp: u8,
    tx_hf: u8,
    gnss: u8,
    wifi: u8,
) -> [u8; 8] {
    [enable, standby, rx, tx, tx_hp, tx_hf, gnss, wifi]
}

/// `SetTcxoMode` (`src/lr11xx_system.c:374-386`): the supply code
/// (`src/lr11xx_system_types.h:280-287`, 0x06 = 3.0 V) and the startup
/// delay in RTC steps.
pub const fn set_tcxo_mode(supply: u8, startup_steps: u32) -> [u8; 4] {
    let t = u24(startup_steps);
    [supply, t[0], t[1], t[2]]
}

/// `Calibrate` (`src/lr11xx_system.c:243-251`): the block mask
/// (`src/lr11xx_system_types.h:145-150`).
pub const fn calibrate(mask: u8) -> [u8; 1] {
    [mask]
}

/// All six calibration blocks: LF RC, HF RC, PLL, ADC, image, PLL TX.
pub const CALIB_ALL: u8 = 0x3F;

/// `CalibImage` (`src/lr11xx_system.c:265-278`): the band edges in 4 MHz
/// steps, the lower rounded down and the upper up.
pub const fn calibrate_image(freq1_mhz: u16, freq2_mhz: u16) -> [u8; 2] {
    [(freq1_mhz / 4) as u8, freq2_mhz.div_ceil(4) as u8]
}

/// The image-calibration band that contains `freq_hz`, in MHz, the same
/// five bands `leviculum-nrf/src/sx1262.rs` selects for the SX1262.
pub const fn image_band_mhz(freq_hz: u32) -> (u16, u16) {
    if freq_hz > 900_000_000 {
        (902, 928)
    } else if freq_hz > 850_000_000 {
        (863, 870)
    } else if freq_hz > 770_000_000 {
        (779, 787)
    } else if freq_hz > 460_000_000 {
        (470, 510)
    } else {
        (430, 440)
    }
}

/// `CfgLfClk` (`src/lr11xx_system.c:362-370`): the clock source in bits
/// 1:0 (`src/lr11xx_system_types.h:216-218`, 0 RC, 1 crystal, 2 external)
/// and "wait for the 32 kHz to be ready" in bit 2.
pub const fn cfg_lf_clk(source: u8, wait_32k_ready: bool) -> [u8; 1] {
    [source | ((wait_32k_ready as u8) << 2)]
}

/// `SetDioIrqParams` (`src/lr11xx_system.c:310-326`): the flags routed
/// to DIO9 (IRQ) and to DIO11.
pub const fn set_dio_irq_params(dio1: u32, dio2: u32) -> [u8; 8] {
    let a = dio1.to_be_bytes();
    let b = dio2.to_be_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3]]
}

/// `ClearIrq` (`src/lr11xx_system.c:330-340`).
pub const fn clear_irq(mask: u32) -> [u8; 4] {
    mask.to_be_bytes()
}

/// `SetStandby` (`src/lr11xx_system.c:446-452`): 0 RC, 1 XOSC.
pub const fn set_standby(cfg: u8) -> [u8; 1] {
    [cfg]
}

// Radio command arguments.

/// `SetRfFrequency` (`src/lr11xx_radio.c:519-529`): the frequency in Hz,
/// no PLL arithmetic on this part.
pub const fn set_rf_frequency(freq_hz: u32) -> [u8; 4] {
    freq_hz.to_be_bytes()
}

/// `SetPacketType` (`src/lr11xx_radio.c:569-575`): 2 = LoRa
/// (`src/lr11xx_radio_types.h:308`).
pub const fn set_pkt_type(kind: u8) -> [u8; 1] {
    [kind]
}

/// The LoRa packet type code.
pub const PKT_TYPE_LORA: u8 = 0x02;

/// `SetModulationParams` for LoRa (`src/lr11xx_radio.c:619-628`): SF,
/// BW code, CR code, LDRO. The codes are the SX126x's
/// (`src/lr11xx_radio_types.h:155-191`).
pub const fn set_lora_modulation_params(sf: u8, bw: u8, cr: u8, ldro: bool) -> [u8; 4] {
    [sf, bw, cr, ldro as u8]
}

/// `SetPacketParams` for LoRa (`src/lr11xx_radio.c:690-701`): preamble
/// symbols, header type (0 explicit), payload length, CRC (1 on), IQ
/// (0 standard).
pub const fn set_lora_pkt_params(
    preamble: u16,
    explicit_header: bool,
    payload_len: u8,
    crc_on: bool,
    invert_iq: bool,
) -> [u8; 6] {
    let p = preamble.to_be_bytes();
    [
        p[0],
        p[1],
        if explicit_header { 0x00 } else { 0x01 },
        payload_len,
        crc_on as u8,
        invert_iq as u8,
    ]
}

/// `SetTxParams` (`src/lr11xx_radio.c:707-714`): the power in dBm as a
/// signed byte and the ramp code (`src/lr11xx_radio_types.h:123`, 0x02 =
/// 48 us).
pub const fn set_tx_params(power_dbm: i8, ramp: u8) -> [u8; 2] {
    [power_dbm as u8, ramp]
}

/// The 48 us ramp, Semtech's default.
pub const RAMP_48_US: u8 = 0x02;

/// `SetPaCfg` (`src/lr11xx_radio.c:778-788`): PA select, regulator
/// supply, duty cycle, HP select.
pub const fn set_pa_cfg(pa_sel: u8, reg_supply: u8, duty_cycle: u8, hp_sel: u8) -> [u8; 4] {
    [pa_sel, reg_supply, duty_cycle, hp_sel]
}

/// `SetRxTxFallbackMode` (`src/lr11xx_radio.c:733-740`): 1 = standby RC
/// (`src/lr11xx_radio_types.h:109`).
pub const fn set_rx_tx_fallback_mode(mode: u8) -> [u8; 1] {
    [mode]
}

pub const FALLBACK_STDBY_RC: u8 = 0x01;

/// `SetLoRaPublicNetwork` (`src/lr11xx_radio.c:398-406`): 0 = private
/// (`src/lr11xx_radio_types.h:144`), the RNode network's sync word.
pub const fn set_lora_public_network(kind: u8) -> [u8; 1] {
    [kind]
}

pub const NETWORK_PRIVATE: u8 = 0x00;

/// `StopTimeoutOnPreamble` (`src/lr11xx_radio.c:792-800`).
pub const fn stop_timeout_on_preamble(stop: bool) -> [u8; 1] {
    [stop as u8]
}

/// `SetRxBoosted` (`src/lr11xx_radio.c:940-948`).
pub const fn set_rx_boosted(on: bool) -> [u8; 1] {
    [on as u8]
}

/// `SetRssiCalibration` (`src/lr11xx_radio.c:951-975`): gains g4..g13, g13hp1..g13hp7.
pub const fn set_rssi_calibration(gain_tune: &[u8; 17], gain_offset: i16) -> [u8; 11] {
    const fn pair(lo: u8, hi: u8) -> u8 {
        ((hi & 0x0F) << 4) | (lo & 0x0F)
    }
    let g = gain_tune;
    let o = gain_offset.to_be_bytes();
    [
        pair(g[0], g[1]),
        pair(g[2], g[3]),
        pair(g[4], g[5]),
        pair(g[6], g[7]),
        pair(g[8], g[9]),
        pair(g[10], g[11]),
        pair(g[12], g[13]),
        pair(g[14], g[15]),
        g[16] & 0x0F,
        o[0],
        o[1],
    ]
}

/// `SetRx` (`src/lr11xx_radio.c:437-448`): timeout in RTC steps, 0 single,
/// 0xFFFFFF continuous.
pub const fn set_rx(timeout_steps: u32) -> [u8; 3] {
    u24(timeout_steps)
}

/// `SetTx` (`src/lr11xx_radio.c:484-495`): the timeout in RTC steps.
pub const fn set_tx(timeout_steps: u32) -> [u8; 3] {
    u24(timeout_steps)
}

/// `SetCadParams` (`src/lr11xx_radio.c:552-566`): symbol count (plain, not
/// the SX126x code), detection peak, minimum, exit mode, timeout in RTC steps.
pub const fn set_cad_params(
    symb_nb: u8,
    det_peak: u8,
    det_min: u8,
    exit_mode: u8,
    timeout_steps: u32,
) -> [u8; 7] {
    let t = u24(timeout_steps);
    [symb_nb, det_peak, det_min, exit_mode, t[0], t[1], t[2]]
}

pub const CAD_EXIT_STANDBY_RC: u8 = 0x00;

/// `ReadBuffer8` (`src/lr11xx_regmem.c:198-208`): offset and length; the
/// response is `length` bytes after the status byte.
pub const fn read_buffer8(offset: u8, len: u8) -> [u8; 2] {
    [offset, len]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcodes_are_the_reference_drivers() {
        // `src/lr11xx_system.c:95-121`, `src/lr11xx_regmem.c:72-77`,
        // `src/lr11xx_radio.c:135-169`.
        assert_eq!(op::GET_STATUS, 0x0100);
        assert_eq!(op::GET_VERSION, 0x0101);
        assert_eq!(op::SET_DIO_IRQ_PARAMS, 0x0113);
        assert_eq!(op::SET_TCXO_MODE, 0x0117);
        assert_eq!(op::WRITE_BUFFER8, 0x0109);
        assert_eq!(op::READ_BUFFER8, 0x010A);
        assert_eq!(op::SET_RX, 0x0209);
        assert_eq!(op::SET_TX, 0x020A);
        assert_eq!(op::SET_RF_FREQUENCY, 0x020B);
        assert_eq!(op::SET_PA_CFG, 0x0215);
        assert_eq!(op::SET_CAD, 0x0218);
        assert_eq!(op::SET_LORA_PUBLIC_NETWORK, 0x0208);
    }

    #[test]
    fn irq_bits_are_the_reference_drivers() {
        // `src/lr11xx_system_types.h:105-113`.
        assert_eq!(irq::TX_DONE, 0x0004);
        assert_eq!(irq::RX_DONE, 0x0008);
        assert_eq!(irq::PREAMBLE_DETECTED, 0x0010);
        assert_eq!(irq::SYNC_WORD_HEADER_VALID, 0x0020);
        assert_eq!(irq::HEADER_ERROR, 0x0040);
        assert_eq!(irq::CRC_ERROR, 0x0080);
        assert_eq!(irq::CAD_DONE, 0x0100);
        assert_eq!(irq::CAD_DETECTED, 0x0200);
        assert_eq!(irq::TIMEOUT, 0x0400);
    }

    #[test]
    fn irq_translation_lands_on_the_sx126x_positions() {
        use leviculum_core::sx126x as sx;
        assert_eq!(irq::to_sx126x(irq::TX_DONE), sx::IRQ_TX_DONE);
        assert_eq!(
            irq::to_sx126x(irq::RX_DONE | irq::CRC_ERROR),
            sx::IRQ_RX_DONE | sx::IRQ_CRC_ERR
        );
        assert_eq!(
            irq::to_sx126x(irq::PREAMBLE_DETECTED | irq::SYNC_WORD_HEADER_VALID),
            sx::IRQ_PREAMBLE_DETECTED | sx::IRQ_HEADER_VALID
        );
        assert_eq!(
            irq::to_sx126x(irq::CAD_DONE | irq::CAD_DETECTED),
            sx::IRQ_CAD_DONE | sx::IRQ_CAD_DETECTED
        );
        assert_eq!(irq::to_sx126x(irq::TIMEOUT), sx::IRQ_TIMEOUT);
        // Negative control: a bit with no counterpart contributes nothing.
        assert_eq!(irq::to_sx126x(irq::CMD_ERROR | irq::ERROR), 0);
        // The translation is what makes core's RX-extend decision usable
        // on this part: preamble seen, nothing terminated, extend.
        assert!(
            sx::rx_extend_ms(irq::to_sx126x(irq::PREAMBLE_DETECTED), 125_000, 8, 5, 18).is_some()
        );
        assert!(sx::rx_extend_ms(
            irq::to_sx126x(irq::PREAMBLE_DETECTED | irq::RX_DONE),
            125_000,
            8,
            5,
            18
        )
        .is_none());
    }

    #[test]
    fn status_bytes_decode_as_the_reference_driver_does() {
        // Stat1: bit 0 interrupt, bits 3:1 command status (`lr11xx_system.c:649-650`).
        let s = Stat1::from_raw(0b0000_0101);
        assert!(s.interrupt_active);
        assert_eq!(s.command, CmdStatus::Ok);
        assert_eq!(Stat1::from_raw(0b0000_0010).command, CmdStatus::Perr);
        assert_eq!(Stat1::from_raw(0b0000_0110).command, CmdStatus::Data);
        assert_eq!(Stat1::from_raw(0).command, CmdStatus::Fail);
        // Stat2: chip mode bits 3:1, reset bits 7:4, flash bit 0 (`:658-660`).
        let s = Stat2::from_raw(0b0011_1001);
        assert_eq!(s.chip_mode, 4);
        assert_eq!(s.reset_status, 3);
        assert!(s.running_from_flash);
        let (s1, s2, irq) = decode_status(&[0x04, 0x02, 0x00, 0x00, 0x00, 0x08]);
        assert_eq!(s1.command, CmdStatus::Ok);
        assert_eq!(s2.chip_mode, 1);
        assert_eq!(irq, irq::RX_DONE);
    }

    #[test]
    fn version_and_packet_status_decode() {
        assert_eq!(
            decode_version(&[0x22, 0x01, 0x03, 0x08]),
            (0x22, 0x01, 0x0308)
        );
        // RSSI -(raw>>1), SNR (raw+2)>>2 (`lr11xx_radio.c:318-330`).
        assert_eq!(decode_lora_pkt_status(&[200, 40, 0]), (-100, 10));
        assert_eq!(decode_lora_pkt_status(&[100, 0xF8, 0]), (-50, -2)); // -8 dB/4
        assert_eq!(decode_rx_buffer_status(&[123, 7]), (123, 7));
    }

    #[test]
    fn snr_rounding_does_not_wrap_at_the_top_of_the_byte() {
        assert_eq!(decode_lora_pkt_status(&[0, 126, 0]).1, 32);
        assert_eq!(decode_lora_pkt_status(&[0, 127, 0]).1, 32);
        assert_eq!(decode_lora_pkt_status(&[0, 0x80, 0]).1, -32);
    }

    #[test]
    fn errors_decode_big_endian() {
        assert_eq!(decode_errors(&[0x00, 0x20]), errors::HF_XOSC_START);
        assert_eq!(decode_errors(&[0x00, 0x40]), errors::LF_XOSC_START);
        assert_eq!(decode_errors(&[0x00, 0x00]), 0);
    }

    #[test]
    fn instant_rssi_truncates_before_negating() {
        for (raw, expected) in [(0, 0), (1, 0), (159, -79), (160, -80), (255, -127)] {
            assert_eq!(decode_rssi_inst(raw), expected);
        }
    }

    #[test]
    fn rtc_steps_follow_the_32k_clock_and_saturate() {
        assert_eq!(ms_to_rtc_steps(0), 0);
        assert_eq!(ms_to_rtc_steps(1000), 32_768);
        assert_eq!(ms_to_rtc_steps(500), 16_384);
        // Saturates below 0xFFFFFF, which `SetRx` reads as continuous.
        assert_eq!(ms_to_rtc_steps(600_000), 0x00FF_FFFE);
    }

    #[test]
    fn system_arguments_match_the_reference_layouts() {
        assert_eq!(set_reg_mode(1), [0x01]);
        assert_eq!(
            set_dio_as_rf_switch(0x07, 0x00, 0x01, 0x03, 0x02, 0x00, 0x04, 0x00),
            [0x07, 0x00, 0x01, 0x03, 0x02, 0x00, 0x04, 0x00]
        );
        assert_eq!(set_tcxo_mode(0x06, 300), [0x06, 0x00, 0x01, 0x2C]);
        assert_eq!(calibrate(CALIB_ALL), [0x3F]);
        assert_eq!(calibrate_image(863, 870), [215, 218]);
        assert_eq!(image_band_mhz(869_463_000), (863, 870));
        assert_eq!(image_band_mhz(915_000_000), (902, 928));
        assert_eq!(cfg_lf_clk(0x01, true), [0x05]);
        assert_eq!(cfg_lf_clk(0x01, false), [0x01]);
        assert_eq!(
            set_dio_irq_params(irq::DIO_RX, 0),
            [0x00, 0x00, 0x04, 0xC8, 0x00, 0x00, 0x00, 0x00]
        );
        assert_eq!(clear_irq(irq::ALL), [0xFF; 4]);
        assert_eq!(set_standby(0), [0x00]);
    }

    #[test]
    fn radio_arguments_match_the_reference_layouts() {
        assert_eq!(set_rf_frequency(869_463_000), [0x33, 0xD2, 0xF3, 0xD8]);
        assert_eq!(set_pkt_type(PKT_TYPE_LORA), [0x02]);
        assert_eq!(
            set_lora_modulation_params(8, 0x04, 0x01, false),
            [0x08, 0x04, 0x01, 0x00]
        );
        assert_eq!(
            set_lora_modulation_params(12, 0x04, 0x04, true),
            [0x0C, 0x04, 0x04, 0x01]
        );
        assert_eq!(
            set_lora_pkt_params(18, true, 0xFF, true, false),
            [0x00, 0x12, 0x00, 0xFF, 0x01, 0x00]
        );
        assert_eq!(set_tx_params(22, RAMP_48_US), [22, 0x02]);
        assert_eq!(set_tx_params(-9, RAMP_48_US), [0xF7, 0x02]);
        assert_eq!(set_pa_cfg(1, 1, 0x04, 0x07), [0x01, 0x01, 0x04, 0x07]);
        assert_eq!(set_rx_tx_fallback_mode(FALLBACK_STDBY_RC), [0x01]);
        assert_eq!(set_lora_public_network(NETWORK_PRIVATE), [0x00]);
        assert_eq!(stop_timeout_on_preamble(true), [0x01]);
        assert_eq!(set_rx(0), [0, 0, 0]);
        assert_eq!(set_rx(0x00FF_FFFE), [0xFF, 0xFF, 0xFE]);
        assert_eq!(set_tx(ms_to_rtc_steps(1000)), [0x00, 0x80, 0x00]);
        assert_eq!(
            set_cad_params(4, 50, 10, CAD_EXIT_STANDBY_RC, 0),
            [0x04, 50, 10, 0x00, 0, 0, 0]
        );
        assert_eq!(read_buffer8(7, 200), [7, 200]);
        assert_eq!(set_rx_boosted(false), [0x00]);
        assert_eq!(set_rx_boosted(true), [0x01]);
    }

    #[test]
    fn rssi_calibration_packs_semtechs_table_as_the_reference_driver_does() {
        // SWSD003 v2.4.0 600 MHz-2 GHz table.
        let gains = [2, 2, 2, 3, 3, 4, 5, 4, 4, 6, 5, 5, 6, 6, 6, 7, 6];
        assert_eq!(
            set_rssi_calibration(&gains, 0),
            [0x22, 0x32, 0x43, 0x45, 0x64, 0x55, 0x66, 0x76, 0x06, 0x00, 0x00]
        );
        // The offset is big-endian; stray high nibbles are masked.
        let r = set_rssi_calibration(&[0xF0; 17], 2030);
        assert_eq!(r[..9], [0x00; 9]);
        assert_eq!(r[9..], [0x07, 0xEE]);
    }
}
