//! The learned calendar's flash checkpoint (Codeberg #166 item 4).
//!
//! # What it is for
//!
//! A clockless board, a solar relay with neither GNSS nor a host, learns
//! real time from the first credible announce it overhears and forgets it
//! at the next power cycle: until it overhears another, it stamps from its
//! build timestamp. This crate keeps the learned value across the cycle.
//! The firmware writes the calendar to flash at most hourly, and at boot
//! the newest valid record is restored through
//! `Transport::restore_emission_floor`, which seats it at the BIRTH rank
//! (`docs/src/concepts/time-and-clocks.md`, "A restored floor is a birth
//! anchor with a better value"). The stored value is a floor, never a
//! claim about now.
//!
//! This crate is the pure half: the record layout, the scan that finds the
//! newest valid record, the plan that says where the next one goes, and
//! the gate that says when one is due. The flash I/O stays in
//! `leviculum-nrf/src/time_floor.rs`, the split `leviculum-boot-count` uses
//! and for the same reason: a power cut at every word boundary is only
//! provable over a simulated page on the host.
//!
//! # The ring
//!
//! The region is [`RING_PAGES`] pages that `memory.x` reserves as `TIME`.
//! Records are APPENDED into the current page, never rewritten in place:
//! NOR programming only clears bits, so an erased slot costs no erase.
//! When the current page is spent, the NEXT page of the ring is erased
//! and the record goes to its first slot. The newest record therefore
//! always sits in a page that is not being erased, and the page being
//! erased holds only records older than one still on flash: a power cut
//! during the erase, or during any write, loses at most the record being
//! written. Every page is erased once per lap, so wear spreads evenly.
//!
//! The issue's sketch was one record per page, erased on every write:
//! [`CHECKPOINTS_PER_YEAR`] writes a year against [`ENDURANCE_CYCLES`]
//! makes a four-page ring of that kind last [`years_one_record_per_page`]
//! (4) = 4.6 years. Appending [`RECORDS_PER_PAGE`] records per erase over
//! the same scheme gives [`years_appending`] (2) = 465 years with two
//! pages, which is why the region is two pages and not four: the image
//! window had 41 KiB of gap left at the time and two more pages would buy
//! nothing a board outlives. The arithmetic is pinned in the tests.
//!
//! # On-flash layout
//!
//! [`RECORD_SIZE`] = 20 bytes, five NVMC words, [`RECORDS_PER_PAGE`] = 204
//! to a page (the last 16 bytes of each page stay erased):
//!
//! | Offset | Bytes | Field |
//! |---|---|---|
//! | 0 | 4 | magic [`MAGIC`], u32 LE |
//! | 4 | 4 | sequence number, u32 LE, one more than the newest on flash |
//! | 8 | 8 | calendar value, unix seconds, u64 LE |
//! | 16 | 1 | format version [`FORMAT_VERSION`] |
//! | 17 | 1 | reserved, 0 |
//! | 18 | 2 | CRC-16/CCITT-FALSE over bytes 0..18, u16 LE |
//!
//! The CRC is in the last word, so a write cut before it lands leaves
//! `0xFFFF` there and cannot match: a torn record is never read as a
//! floor. Its slot is not reused either (programming over it would produce
//! bytes neither record contained); the next record goes after the last
//! slot that is not erased.

#![cfg_attr(not(test), no_std)]

/// Erase granularity of the nRF52840's internal flash.
pub const PAGE_SIZE: usize = 4096;

/// One record, five NVMC words.
pub const RECORD_SIZE: usize = 20;

/// Records one page holds before the ring moves to the next page.
pub const RECORDS_PER_PAGE: usize = PAGE_SIZE / RECORD_SIZE;

// The record is whole NVMC words and 204 of them fit a page.
const _: () = assert!(RECORD_SIZE.is_multiple_of(4) && RECORDS_PER_PAGE == 204);

/// Pages in the `TIME` region `memory.x` reserves. The firmware reads the
/// region's bounds from the linker and asserts its length against this.
pub const RING_PAGES: usize = 2;

/// Record-valid marker. Bump [`FORMAT_VERSION`] when a field moves, so a
/// ring written by an older image reads as empty instead of misparsed.
pub const MAGIC: u32 = 0x71E3_F10A;

/// Layout version stored in every record.
pub const FORMAT_VERSION: u8 = 1;

/// Erase cycles per page the nRF52840's flash is rated for, the same
/// stated assumption `leviculum-boot-count` carries.
pub const ENDURANCE_CYCLES: u32 = 10_000;

/// The gate's minimum spacing between two checkpoints, in uptime.
pub const CHECKPOINT_INTERVAL_MS: u64 = 3_600_000;

/// The gate's minimum advance of the calendar since the last record.
pub const CHECKPOINT_MIN_ADVANCE_SECS: u64 = 3_600;

/// The worst rank a calendar may have for its value to be written: 4,
/// anchored from traffic. A birth anchor (rank 5, the build floor or a
/// restored floor) is never written back, so the ring only ever holds a
/// value some live source vouched for.
pub const MAX_CHECKPOINT_RANK: u8 = 4;

/// The most checkpoints the gate lets through in a year: one an hour.
pub const CHECKPOINTS_PER_YEAR: u32 = 365 * 24;

/// An erased byte of NOR flash.
pub const ERASED: u8 = 0xFF;

/// CRC-16/CCITT-FALSE, as in `leviculum-boot-count`.
const CRC_POLY: u16 = 0x1021;
const CRC_INIT: u16 = 0xFFFF;

/// One checkpoint as it sits in the ring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Record {
    /// Strictly increasing across the ring; the newest record is the one
    /// with the highest. Starts at 1 on a ring holding no readable record.
    pub seq: u32,
    /// The calendar, unix seconds, when the record was written.
    pub value: u64,
}

/// Serialise a record into the bytes the flash is programmed with.
pub fn encode(record: &Record) -> [u8; RECORD_SIZE] {
    let mut buf = [0u8; RECORD_SIZE];
    buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&record.seq.to_le_bytes());
    buf[8..16].copy_from_slice(&record.value.to_le_bytes());
    buf[16] = FORMAT_VERSION;
    buf[17] = 0;
    let crc = crc16(&buf[0..18]);
    buf[18..20].copy_from_slice(&crc.to_le_bytes());
    buf
}

/// Read one slot back. `None` for an erased slot, a torn write, foreign
/// bytes, or a layout version this image does not own.
pub fn decode(slot: &[u8]) -> Option<Record> {
    if slot.len() < RECORD_SIZE {
        return None;
    }
    if u32::from_le_bytes([slot[0], slot[1], slot[2], slot[3]]) != MAGIC {
        return None;
    }
    if slot[16] != FORMAT_VERSION {
        return None;
    }
    if crc16(&slot[0..18]) != u16::from_le_bytes([slot[18], slot[19]]) {
        return None;
    }
    let mut value = [0u8; 8];
    value.copy_from_slice(&slot[8..16]);
    Some(Record {
        seq: u32::from_le_bytes([slot[4], slot[5], slot[6], slot[7]]),
        value: u64::from_le_bytes(value),
    })
}

/// What one page says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct PageScan {
    /// The readable record with the highest sequence number.
    newest: Option<Record>,
    /// Offset within the page of the slot after the last one that is not
    /// erased, or `None` when that is past the last slot.
    free: Option<usize>,
    /// Whether every byte of the page is erased.
    blank: bool,
}

fn scan_page(page: &[u8]) -> PageScan {
    let mut newest: Option<Record> = None;
    let mut last_used: Option<usize> = None;
    for i in 0..RECORDS_PER_PAGE {
        let slot = &page[i * RECORD_SIZE..(i + 1) * RECORD_SIZE];
        if let Some(record) = decode(slot) {
            if newest.is_none_or(|n| record.seq > n.seq) {
                newest = Some(record);
            }
        }
        if !is_erased(slot) {
            last_used = Some(i);
        }
    }
    let free = match last_used {
        None => Some(0),
        Some(i) if i + 1 < RECORDS_PER_PAGE => Some((i + 1) * RECORD_SIZE),
        Some(_) => None,
    };
    PageScan {
        newest,
        free,
        blank: is_erased(page),
    }
}

/// The newest readable record in the ring and the page it sits in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Newest {
    pub record: Record,
    pub page: usize,
}

/// Find the newest readable record in `region` (a whole number of pages):
/// the highest sequence number wins, wherever it sits. Torn records,
/// erased slots and foreign bytes are skipped.
pub fn newest(region: &[u8]) -> Option<Newest> {
    let mut best: Option<Newest> = None;
    for (page, bytes) in region.chunks_exact(PAGE_SIZE).enumerate() {
        if let Some(record) = scan_page(bytes).newest {
            if best.is_none_or(|b| record.seq > b.record.seq) {
                best = Some(Newest { record, page });
            }
        }
    }
    best
}

/// What a checkpoint must do to the region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    /// Erase this page (index into the region) before writing.
    pub erase_page: Option<usize>,
    /// Byte offset within the region for the record.
    pub offset: usize,
    /// The record to write there.
    pub record: Record,
}

/// Decide where a checkpoint of `value` goes. `None` when `region` is not
/// at least two whole pages: a one-page ring would have to erase the page
/// holding the newest record, which is the loss the ring exists to avoid.
pub fn plan(region: &[u8], value: u64) -> Option<Plan> {
    let pages = region.len() / PAGE_SIZE;
    if pages < 2 || !region.len().is_multiple_of(PAGE_SIZE) {
        return None;
    }
    let page_of = |p: usize| &region[p * PAGE_SIZE..(p + 1) * PAGE_SIZE];
    let newest = newest(region);
    let record = Record {
        seq: newest.map_or(1, |n| n.record.seq.wrapping_add(1)),
        value,
    };
    let current = newest.map_or(0, |n| n.page);
    if let Some(free) = scan_page(page_of(current)).free {
        return Some(Plan {
            erase_page: None,
            offset: current * PAGE_SIZE + free,
            record,
        });
    }
    // The current page is spent: move on round the ring. Its successor
    // holds only records older than the newest, so erasing it loses
    // nothing a restore would pick. A successor that is already blank (an
    // erase whose write was cut off) needs no second erase.
    let next = (current + 1) % pages;
    let erase_page = if scan_page(page_of(next)).blank {
        None
    } else {
        Some(next)
    };
    Some(Plan {
        erase_page,
        offset: next * PAGE_SIZE,
        record,
    })
}

/// When a checkpoint is due: at most once per [`CHECKPOINT_INTERVAL_MS`]
/// of uptime (counted from boot, so a board that reboots often writes no
/// more often), only while the calendar ranks [`MAX_CHECKPOINT_RANK`] or
/// better, and only when it advanced by [`CHECKPOINT_MIN_ADVANCE_SECS`]
/// since the last record.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CheckpointGate {
    last_value: Option<u64>,
    last_ms: u64,
}

impl CheckpointGate {
    pub const fn new() -> Self {
        Self {
            last_value: None,
            last_ms: 0,
        }
    }

    /// The value already on flash: the restored floor. The next record
    /// must be an hour past it.
    pub fn seed(&mut self, on_flash: u64) {
        self.last_value = Some(on_flash);
    }

    /// Whether a checkpoint of `value` is due now; when it is, the gate
    /// counts it as written, so a failed write is retried an hour later
    /// rather than on every pass.
    pub fn due(&mut self, rank: u8, value: u64, now_ms: u64) -> bool {
        if rank > MAX_CHECKPOINT_RANK {
            return false;
        }
        if now_ms.saturating_sub(self.last_ms) < CHECKPOINT_INTERVAL_MS {
            return false;
        }
        if let Some(last) = self.last_value {
            if value < last.saturating_add(CHECKPOINT_MIN_ADVANCE_SECS) {
                return false;
            }
        }
        self.last_value = Some(value);
        self.last_ms = now_ms;
        true
    }
}

/// Years a ring of `pages` lasts at [`CHECKPOINTS_PER_YEAR`] when every
/// write erases its page: the issue's sketch.
pub fn years_one_record_per_page(pages: u32) -> f64 {
    (ENDURANCE_CYCLES as f64 * pages as f64) / CHECKPOINTS_PER_YEAR as f64
}

/// Years a ring of `pages` lasts at [`CHECKPOINTS_PER_YEAR`] with records
/// appended: one erase per page per lap of `pages * RECORDS_PER_PAGE`.
pub fn years_appending(pages: u32) -> f64 {
    let erases_per_page_per_year =
        CHECKPOINTS_PER_YEAR as f64 / (pages as f64 * RECORDS_PER_PAGE as f64);
    ENDURANCE_CYCLES as f64 / erases_per_page_per_year
}

/// The restore outcome the firmware prints beside `[TIME_SOURCE]`:
///
/// ```text
/// [TIME_RESTORE] value=1790000000 seq=42
/// [TIME_RESTORE_REFUSED] value=1790000000 seq=42 reason=seam
/// [TIME_RESTORE_REFUSED] value=0 seq=0 reason=empty
/// ```
///
/// `reason=seam` is the core seam's refusal (outside the sanity window,
/// a better source already seated, or not above the current calendar);
/// `reason=empty` is a ring holding no readable record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RestoreLine {
    Restored(Record),
    Refused(Record),
    Empty,
}

impl core::fmt::Display for RestoreLine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RestoreLine::Restored(r) => {
                write!(f, "[TIME_RESTORE] value={} seq={}", r.value, r.seq)
            }
            RestoreLine::Refused(r) => write!(
                f,
                "[TIME_RESTORE_REFUSED] value={} seq={} reason=seam",
                r.value, r.seq
            ),
            RestoreLine::Empty => write!(f, "[TIME_RESTORE_REFUSED] value=0 seq=0 reason=empty"),
        }
    }
}

fn is_erased(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| *b == ERASED)
}

/// CRC-16/CCITT-FALSE over `data`; the check value (`"123456789"` →
/// `0x29B1`) is asserted in the tests, as in `leviculum-boot-count`.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = CRC_INIT;
    for byte in data {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ CRC_POLY;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests;
