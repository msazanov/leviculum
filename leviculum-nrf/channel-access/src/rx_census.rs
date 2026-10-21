//! What the receiver made of each frame it heard: decoded, or failed and
//! how (Codeberg #466).
//!
//! A frame that fails its payload CRC on an SX127x is discarded inside the
//! reference firmware's ISR with no counter and no host indication
//! (`reference/RNode_Firmware/sx127x.cpp:490-497`), so a pair of radios can
//! lose a frame that one of them provably aired and neither side can say
//! whether it arrived corrupt or never arrived. A third radio that names,
//! per frame, whether it decoded is what tells the two apart, and the
//! signal figures of a frame that failed are what tell a transmit-quality
//! fault (corrupt at -27 dBm) from range (corrupt at -110).
//!
//! This module is the board's half of that: the classification of a
//! receive window's end ([`rx_err_reason`]), the two per-frame line bodies
//! ([`RxFrameLine`], [`RxErrLine`]) and the cumulative counters the
//! once-a-second `[LORA_CHTM]` line carries ([`RxCensus`]). The firmware
//! (`leviculum-nrf/src/lora.rs`) only reads the radio and calls in here.
//!
//! # The flag word
//!
//! Every input is the SX126x's 16-bit IRQ word
//! (`leviculum_core::sx126x::IRQ_*`). The LR11xx driver maps its 32-bit
//! word onto it (`leviculum_lr11xx::cmd::irq::to_sx126x`), so one rule
//! classifies both parts.
//!
//! # What counts as a frame
//!
//! A window that ends on its timer with nothing latched is silence, and
//! silence is exactly what this census exists to tell a corrupt frame
//! apart from: it prints nothing and counts nothing. Everything else that
//! ends a window is one line and one count.

use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};

use leviculum_core::sx126x::{
    IRQ_CRC_ERR, IRQ_HEADER_ERR, IRQ_HEADER_VALID, IRQ_PREAMBLE_DETECTED, IRQ_RX_DONE,
};

/// How the driver ended a receive window that delivered no frame, before
/// the latched flags are consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxErrKind {
    /// The driver reported a payload CRC failure, with the frame's figures.
    Crc,
    /// The window's timer ran out (hardware timeout or the software wait).
    Timeout,
    /// Any other driver error: an SPI or BUSY failure, the LR11xx's
    /// header-error verdict, an unarmed wait.
    Other,
}

/// Why a frame did not decode: the `reason=` of `LORA_RX_ERR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxErrReason {
    /// `RxDone` with `CrcErr`: the header decoded, the payload did not.
    Crc,
    /// `HeaderErr`: a frame began and its explicit header failed its CRC.
    Header,
    /// A preamble (or a valid header) latched and the window ended on its
    /// timer before the frame completed: a carrier that never became a
    /// frame. A false preamble detection lands here too, which is why it
    /// is counted apart from the two decode failures.
    Timeout,
    /// An error none of the above explains, with the flag word latched at
    /// the time.
    Other(u16),
}

impl fmt::Display for RxErrReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RxErrReason::Crc => f.write_str("crc"),
            RxErrReason::Header => f.write_str("header"),
            RxErrReason::Timeout => f.write_str("timeout"),
            RxErrReason::Other(flags) => write!(f, "other-0x{flags:04x}"),
        }
    }
}

/// The reason a window that delivered no frame ended, from the driver's
/// verdict and the IRQ flags latched over the window, or `None` when the
/// window heard nothing at all.
///
/// The order is the order of evidence: a CRC failure the driver measured
/// outranks anything latched beside it; a header error is a frame even when
/// the window then ran out on its timer (the SX1262 does not end a window
/// on `HeaderErr`, `leviculum_core::sx126x::DIO1_RX`); a carrier without a
/// frame is only a reading of a timer that ran out.
pub fn rx_err_reason(kind: RxErrKind, flags: u16) -> Option<RxErrReason> {
    if kind == RxErrKind::Crc || (flags & IRQ_RX_DONE != 0 && flags & IRQ_CRC_ERR != 0) {
        return Some(RxErrReason::Crc);
    }
    if flags & IRQ_HEADER_ERR != 0 {
        return Some(RxErrReason::Header);
    }
    match kind {
        RxErrKind::Timeout if flags & (IRQ_PREAMBLE_DETECTED | IRQ_HEADER_VALID) != 0 => {
            Some(RxErrReason::Timeout)
        }
        RxErrKind::Timeout => None,
        _ => Some(RxErrReason::Other(flags)),
    }
}

/// A value or `none`.
struct Maybe<T>(Option<T>);

impl<T: fmt::Display> fmt::Display for Maybe<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(v) => v.fmt(f),
            None => f.write_str("none"),
        }
    }
}

/// The body of one `LORA_RX_FRAME` line: a frame the radio decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxFrameLine {
    /// The frame's bytes as the radio reported them, split header included.
    pub len: usize,
    /// `RssiPkt`, dBm.
    pub rssi: i16,
    /// `SnrPkt`, dB.
    pub snr: i16,
    /// The first 4 bytes of the packet hash this frame completed — a prefix
    /// of the 16-hex `ph=` every journey line carries — or `None` for a
    /// split part that completed nothing.
    pub ph: Option<[u8; 4]>,
}

/// A 4-byte hash prefix as 8 hex digits.
struct Hex4([u8; 4]);

impl fmt::Display for Hex4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Display for RxFrameLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ok=1 len={} rssi={} snr={} ph={}",
            self.len,
            self.rssi,
            self.snr,
            Maybe(self.ph.map(Hex4)),
        )
    }
}

/// The body of one `LORA_RX_ERR` line: a frame the radio heard and did not
/// decode.
///
/// The figures are the chip's packet status and the length out of the
/// header, which only a CRC failure has: the header passed its own CRC, so
/// `len` is what the sender keyed, and `RssiPkt`/`SnrPkt` were latched at
/// the end of a reception. A header error or a carrier that never became a
/// frame ended no reception, and the packet status after one is not
/// documented to describe it, so those print `none` rather than the last
/// good frame's figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxErrLine {
    pub reason: RxErrReason,
    pub rssi: Option<i16>,
    pub snr: Option<i16>,
    pub len: Option<u8>,
}

impl fmt::Display for RxErrLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "reason={} rssi={} snr={} len={}",
            self.reason,
            Maybe(self.rssi),
            Maybe(self.snr),
            Maybe(self.len),
        )
    }
}

/// The counters since boot, as one reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RxCounts {
    pub ok: u32,
    pub crc: u32,
    pub hdr: u32,
    pub tmo: u32,
    pub other: u32,
}

/// The keys `[LORA_CHTM]` appends, after the ledger's own.
impl fmt::Display for RxCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rx_ok={} rx_crc={} rx_hdr={} rx_tmo={} rx_other={}",
            self.ok, self.crc, self.hdr, self.tmo, self.other
        )
    }
}

/// Cumulative per-frame counts since boot, one per line kind, so a long
/// capture can be summed from the 1 Hz line without the per-frame lines
/// (which the gate drops before a reader attaches).
///
/// Atomics, so the LoRa task increments and the line reads without a lock;
/// wrapping, like every other counter on the board.
pub struct RxCensus {
    ok: AtomicU32,
    crc: AtomicU32,
    hdr: AtomicU32,
    tmo: AtomicU32,
    other: AtomicU32,
}

impl Default for RxCensus {
    fn default() -> Self {
        Self::new()
    }
}

impl RxCensus {
    /// All zero.
    pub const fn new() -> Self {
        Self {
            ok: AtomicU32::new(0),
            crc: AtomicU32::new(0),
            hdr: AtomicU32::new(0),
            tmo: AtomicU32::new(0),
            other: AtomicU32::new(0),
        }
    }

    /// One frame decoded.
    pub fn note_frame(&self) {
        self.ok.fetch_add(1, Ordering::Relaxed);
    }

    /// One frame failed, for `reason`.
    pub fn note_err(&self, reason: RxErrReason) {
        let counter = match reason {
            RxErrReason::Crc => &self.crc,
            RxErrReason::Header => &self.hdr,
            RxErrReason::Timeout => &self.tmo,
            RxErrReason::Other(_) => &self.other,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// The counts as of now.
    pub fn counts(&self) -> RxCounts {
        RxCounts {
            ok: self.ok.load(Ordering::Relaxed),
            crc: self.crc.load(Ordering::Relaxed),
            hdr: self.hdr.load(Ordering::Relaxed),
            tmo: self.tmo.load(Ordering::Relaxed),
            other: self.other.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use leviculum_core::sx126x::IRQ_TIMEOUT;
    use std::format;

    #[test]
    fn a_crc_verdict_is_crc_whatever_else_latched() {
        for flags in [
            0,
            IRQ_RX_DONE | IRQ_CRC_ERR,
            IRQ_HEADER_ERR,
            IRQ_PREAMBLE_DETECTED | IRQ_HEADER_VALID | IRQ_RX_DONE | IRQ_CRC_ERR,
        ] {
            assert_eq!(
                rx_err_reason(RxErrKind::Crc, flags),
                Some(RxErrReason::Crc),
                "flags {flags:#06x}"
            );
        }
    }

    #[test]
    fn rx_done_with_crc_err_latched_is_crc_under_any_verdict() {
        let flags = IRQ_PREAMBLE_DETECTED | IRQ_HEADER_VALID | IRQ_RX_DONE | IRQ_CRC_ERR;
        assert_eq!(
            rx_err_reason(RxErrKind::Other, flags),
            Some(RxErrReason::Crc)
        );
        assert_eq!(
            rx_err_reason(RxErrKind::Timeout, flags),
            Some(RxErrReason::Crc)
        );
    }

    /// The SX1262 shape: `HeaderErr` latched, the window then ended on its
    /// timer. The LR11xx shape: the driver ended the window on it.
    #[test]
    fn a_latched_header_error_is_header_on_either_part() {
        let sx1262 = IRQ_PREAMBLE_DETECTED | IRQ_HEADER_ERR | IRQ_TIMEOUT;
        assert_eq!(
            rx_err_reason(RxErrKind::Timeout, sx1262),
            Some(RxErrReason::Header)
        );
        let lr11xx = IRQ_PREAMBLE_DETECTED | IRQ_HEADER_ERR;
        assert_eq!(
            rx_err_reason(RxErrKind::Other, lr11xx),
            Some(RxErrReason::Header)
        );
    }

    #[test]
    fn a_timer_that_ran_out_on_a_carrier_is_timeout() {
        for flags in [
            IRQ_PREAMBLE_DETECTED,
            IRQ_PREAMBLE_DETECTED | IRQ_TIMEOUT,
            IRQ_PREAMBLE_DETECTED | IRQ_HEADER_VALID,
        ] {
            assert_eq!(
                rx_err_reason(RxErrKind::Timeout, flags),
                Some(RxErrReason::Timeout),
                "flags {flags:#06x}"
            );
        }
    }

    #[test]
    fn a_timer_that_ran_out_on_nothing_is_silence_not_a_frame() {
        assert_eq!(rx_err_reason(RxErrKind::Timeout, 0), None);
        assert_eq!(rx_err_reason(RxErrKind::Timeout, IRQ_TIMEOUT), None);
    }

    #[test]
    fn any_other_error_carries_the_word_it_latched() {
        assert_eq!(
            rx_err_reason(RxErrKind::Other, 0),
            Some(RxErrReason::Other(0))
        );
        assert_eq!(
            rx_err_reason(RxErrKind::Other, IRQ_PREAMBLE_DETECTED),
            Some(RxErrReason::Other(IRQ_PREAMBLE_DETECTED))
        );
    }

    #[test]
    fn the_reason_tokens_are_the_documented_ones() {
        assert_eq!(format!("{}", RxErrReason::Crc), "crc");
        assert_eq!(format!("{}", RxErrReason::Header), "header");
        assert_eq!(format!("{}", RxErrReason::Timeout), "timeout");
        assert_eq!(format!("{}", RxErrReason::Other(0x0204)), "other-0x0204");
    }

    #[test]
    fn the_frame_line_prints_its_figures_and_the_hash_prefix() {
        let line = RxFrameLine {
            len: 42,
            rssi: -27,
            snr: 13,
            ph: Some([0x5c, 0x7c, 0x4f, 0x48]),
        };
        assert_eq!(format!("{line}"), "ok=1 len=42 rssi=-27 snr=13 ph=5c7c4f48");
        let part = RxFrameLine { ph: None, ..line };
        assert_eq!(format!("{part}"), "ok=1 len=42 rssi=-27 snr=13 ph=none");
    }

    #[test]
    fn the_error_line_prints_none_for_figures_the_chip_did_not_give() {
        let crc = RxErrLine {
            reason: RxErrReason::Crc,
            rssi: Some(-27),
            snr: Some(-3),
            len: Some(255),
        };
        assert_eq!(format!("{crc}"), "reason=crc rssi=-27 snr=-3 len=255");
        let header = RxErrLine {
            reason: RxErrReason::Header,
            rssi: None,
            snr: None,
            len: None,
        };
        assert_eq!(
            format!("{header}"),
            "reason=header rssi=none snr=none len=none"
        );
    }

    #[test]
    fn each_reason_counts_in_its_own_column() {
        let census = RxCensus::new();
        census.note_frame();
        census.note_frame();
        census.note_err(RxErrReason::Crc);
        census.note_err(RxErrReason::Header);
        census.note_err(RxErrReason::Header);
        census.note_err(RxErrReason::Header);
        census.note_err(RxErrReason::Timeout);
        census.note_err(RxErrReason::Other(0x0001));
        assert_eq!(
            census.counts(),
            RxCounts {
                ok: 2,
                crc: 1,
                hdr: 3,
                tmo: 1,
                other: 1,
            }
        );
    }

    #[test]
    fn the_stats_keys_are_cumulative_and_in_order() {
        let census = RxCensus::new();
        assert_eq!(
            format!("{}", census.counts()),
            "rx_ok=0 rx_crc=0 rx_hdr=0 rx_tmo=0 rx_other=0"
        );
        census.note_frame();
        census.note_err(RxErrReason::Crc);
        census.note_frame();
        assert_eq!(
            format!("{}", census.counts()),
            "rx_ok=2 rx_crc=1 rx_hdr=0 rx_tmo=0 rx_other=0"
        );
    }
}
