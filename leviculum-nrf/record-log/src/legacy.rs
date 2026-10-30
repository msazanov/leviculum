//! Every record of a region, oldest first, one at a time.
//!
//! [`RecordLog`](crate::RecordLog) is the writer: it mounts the active page,
//! appends and purges. This module is the reader a migration needs — the
//! one-time copy of a board's internal flash into the microSD store
//! (Codeberg #384) — and its two shapes are decided by that job:
//!
//! * **The page order is collected before anything is walked.** Physical
//!   page order is *not* append order: reclaim is round-robin, so after one
//!   lap the oldest records sit in a page that comes late in the region.
//!   [`crate::legacy::PageOrder::collect`] reads the 12-byte header of every page, keeps
//!   the ones this log wrote, and sorts them by sequence — which is what
//!   [`RecordLog::for_each_seq`](crate::RecordLog::for_each_seq) gets for
//!   free from the live mount state and a reader of a bare region does not.
//! * **One record at a time, without re-walking.** A migration compares one
//!   legacy record against one record of the destination store, which is a
//!   stream; holding either side in memory is what this cursor exists to
//!   avoid, and the order it yields is exactly the order the destination
//!   appends in.
//!
//! # The walk rules, verbatim from the log
//!
//! The same rules `leviculum_pn_store::scan` states, and for the same
//! reason — a page counts when its magic, its format version and its header
//! CRC all agree; a record exists when its flags byte reads [`FLAG_LIVE`] or
//! [`FLAG_PURGED`] *and* its CRC over header and body agrees; the walk of a
//! page stops at the first gap, and the walk of the region resumes at the
//! next page in sequence order. So a page whose erase was interrupted is
//! skipped whole, an append that a power cut left uncommitted stops its page
//! (the bytes after it are not records), a record that lost a bit costs
//! itself and not the records behind it (its stride still steps the walk
//! over it), and purged records are visible with `live = false` because the
//! caller's merge is the thing that decides what to do about them.
//!
//! Reading is a plain copy here: this walks a memory-mapped region, where the
//! log walks a device.

use alloc::vec::Vec;
use core::borrow::Borrow;

use crate::{
    crc16_update, record_stride, CRC_INIT, FLAG_LIVE, FLAG_PURGED, HEADER_LEN, KEY_LEN, MAGIC,
    MIN_STRIDE, SECTOR_HEADER_LEN, SECTOR_SIZE, VERSION,
};

/// One valid page: the sequence its header carries, and where it lives.
///
/// Ordered by sequence first and page index second, so the whole order is a
/// total one even if a region somehow carries the same sequence twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Page {
    seq: u32,
    index: u32,
}

/// Every valid page of a record-log region, in append order.
///
/// Built once per region: [`LegacyCursor`] needs it to walk, and it costs a
/// read of one page header per page — `pages` × 12 bytes, nothing else.
#[derive(Debug, Clone)]
pub struct PageOrder {
    pages: Vec<Page>,
}

impl PageOrder {
    /// Read the page headers of `region` and order the valid ones.
    ///
    /// Touches the 12-byte header of every page and no record bytes; a page
    /// whose magic, version or header CRC does not agree is left out
    /// entirely, exactly as the walkers leave it out.
    pub fn collect(region: &[u8]) -> Self {
        let pages = region.len() / SECTOR_SIZE as usize;
        let mut valid = Vec::with_capacity(pages);
        for index in 0..pages {
            let at = index * SECTOR_SIZE as usize;
            let header = &region[at..at + SECTOR_HEADER_LEN as usize];
            let magic = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
            if magic != MAGIC || header[8] != VERSION {
                continue;
            }
            let stored = u16::from_le_bytes([header[10], header[11]]);
            if crc16_update(CRC_INIT, &header[0..10]) != stored {
                continue;
            }
            valid.push(Page {
                seq: u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
                index: index as u32,
            });
        }
        valid.sort_unstable();
        Self { pages: valid }
    }

    /// Whether the region carries no page this log wrote.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// How many pages the walk will visit.
    pub fn len(&self) -> usize {
        self.pages.len()
    }
}

/// A one-record-at-a-time cursor over a region, oldest record first.
///
/// Yields records in append order: `page_seq` ascending, offset ascending
/// inside a page. Nothing is buffered and nothing is walked twice, so the
/// caller can compare this against a stream without holding either side.
#[derive(Debug, Clone)]
pub struct LegacyCursor<O: Borrow<PageOrder> = PageOrder> {
    order: O,
    /// Position in the order of the page being walked.
    at: usize,
    /// Offset within that page of the next record header to read.
    offset: u32,
}

impl<O: Borrow<PageOrder>> LegacyCursor<O> {
    /// A cursor at the first record of `order`.
    pub fn new(order: O) -> Self {
        Self {
            order,
            at: 0,
            offset: SECTOR_HEADER_LEN,
        }
    }

    /// Restart the same ordered region without copying or allocating its page
    /// table. The original cursor remains at its current position.
    pub fn rewalk(&self) -> LegacyCursor<&PageOrder> {
        LegacyCursor::new(self.order.borrow())
    }

    /// The next intact record, or `None` at the end of the region.
    ///
    /// `region` is the same bytes [`PageOrder::collect`] was given; passing
    /// a different one is a caller error, and a shorter one is skipped
    /// rather than read out of bounds.
    pub fn next<'a>(&mut self, region: &'a [u8]) -> Option<LegacyRecord<'a>> {
        while let Some(page) = self.order.borrow().pages.get(self.at) {
            let base = page.index as usize * SECTOR_SIZE as usize;
            if base + SECTOR_SIZE as usize > region.len() {
                self.at += 1;
                self.offset = SECTOR_HEADER_LEN;
                continue;
            }
            while SECTOR_SIZE - self.offset >= MIN_STRIDE {
                let at = base + self.offset as usize;
                let head = &region[at..at + HEADER_LEN];
                let flags = head[39];
                if flags != FLAG_LIVE && flags != FLAG_PURGED {
                    break;
                }
                let len = u16::from_le_bytes([head[0], head[1]]) as usize;
                let stride = record_stride(len);
                if stride > (SECTOR_SIZE - self.offset) as usize {
                    break;
                }
                // The record owns its bytes whether or not its CRC still
                // agrees, so the step happens before the check: a record
                // that lost a bit costs itself and not the ones behind it.
                self.offset += stride as u32;
                let body = &region[at + HEADER_LEN..at + HEADER_LEN + len];
                let stored = u16::from_le_bytes([head[40], head[41]]);
                if crc16_update(crc16_update(CRC_INIT, &head[0..39]), body) == stored {
                    return Some(LegacyRecord {
                        key: region[at + 2..at + 2 + KEY_LEN]
                            .try_into()
                            .expect("slice length is KEY_LEN"),
                        time: u32::from_le_bytes([head[34], head[35], head[36], head[37]]),
                        tag: head[38],
                        live: flags == FLAG_LIVE,
                        body,
                        page_seq: page.seq,
                        offset: at as u32,
                    });
                }
            }
            self.at += 1;
            self.offset = SECTOR_HEADER_LEN;
        }
        None
    }
}

/// One committed record as the cursor sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyRecord<'a> {
    /// The record's key. Opaque here.
    pub key: &'a [u8; KEY_LEN],
    /// The record's timestamp. Opaque here.
    pub time: u32,
    /// The record's tag byte. Opaque here.
    pub tag: u8,
    /// Whether the record is still current; a purged record is walked too.
    pub live: bool,
    /// The record's body.
    pub body: &'a [u8],
    /// Sequence of the page the record sits in.
    pub page_seq: u32,
    /// Absolute device offset of the record header.
    pub offset: u32,
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use crate::sim::{block_on, SimNor};
    use crate::RecordLog;

    use super::*;

    const PAGES: u32 = 4;
    const REGION: u32 = PAGES * SECTOR_SIZE;
    /// Three records to a page.
    const SMALL: usize = 1_000;
    /// Two records to a page.
    const MEDIUM: usize = 1_900;
    /// One record to a page.
    const LARGE: usize = 4_000;

    fn key(n: u32) -> [u8; KEY_LEN] {
        let mut k = [0u8; KEY_LEN];
        k[0..4].copy_from_slice(&n.to_le_bytes());
        k[28..32].copy_from_slice(&n.to_be_bytes());
        k
    }

    fn body(n: u32, len: usize) -> Vec<u8> {
        (0..len).map(|i| (n as u8).wrapping_add(i as u8)).collect()
    }

    fn number(key: &[u8; KEY_LEN]) -> u32 {
        u32::from_le_bytes(key[0..4].try_into().expect("four bytes"))
    }

    /// One record as this module's *second* implementation sees it.
    #[derive(Debug, PartialEq, Eq)]
    struct Seen {
        key: [u8; KEY_LEN],
        time: u32,
        tag: u8,
        live: bool,
        body: Vec<u8>,
        page_seq: u32,
        offset: u32,
    }

    /// The rule set of `leviculum_pn_store::scan`, written out again here so
    /// the cursor is checked against an independent walk of the same bytes
    /// rather than against itself. It walks pages in physical order on
    /// purpose: append order is then [`in_append_order`]'s job.
    fn scan(region: &[u8]) -> Vec<Seen> {
        let mut out = Vec::new();
        for page in 0..region.len() / SECTOR_SIZE as usize {
            let base = page * SECTOR_SIZE as usize;
            let header = &region[base..base + SECTOR_HEADER_LEN as usize];
            if u32::from_le_bytes([header[0], header[1], header[2], header[3]]) != MAGIC
                || header[8] != VERSION
            {
                continue;
            }
            let stored = u16::from_le_bytes([header[10], header[11]]);
            if crc16_update(CRC_INIT, &header[0..10]) != stored {
                continue;
            }
            let page_seq = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);

            let mut off = SECTOR_HEADER_LEN as usize;
            while SECTOR_SIZE as usize - off >= MIN_STRIDE as usize {
                let at = base + off;
                let head = &region[at..at + HEADER_LEN];
                let flags = head[39];
                if flags != FLAG_LIVE && flags != FLAG_PURGED {
                    break;
                }
                let len = u16::from_le_bytes([head[0], head[1]]) as usize;
                let stride = record_stride(len);
                if stride > SECTOR_SIZE as usize - off {
                    break;
                }
                let body = &region[at + HEADER_LEN..at + HEADER_LEN + len];
                let stored = u16::from_le_bytes([head[40], head[41]]);
                if crc16_update(crc16_update(CRC_INIT, &head[0..39]), body) == stored {
                    out.push(Seen {
                        key: region[at + 2..at + 2 + KEY_LEN]
                            .try_into()
                            .expect("slice length is KEY_LEN"),
                        time: u32::from_le_bytes([head[34], head[35], head[36], head[37]]),
                        tag: head[38],
                        live: flags == FLAG_LIVE,
                        body: body.to_vec(),
                        page_seq,
                        offset: at as u32,
                    });
                }
                off += stride;
            }
        }
        out
    }

    /// What the reference walker sees, in the append order the cursor
    /// promises: page sequence first, offset inside the page second.
    fn in_append_order(region: &[u8]) -> Vec<Seen> {
        let mut records = scan(region);
        records.sort_by_key(|record| (record.page_seq, record.offset));
        records
    }

    /// Everything the cursor yields, as the reference's own shape.
    fn walked(region: &[u8]) -> Vec<Seen> {
        let mut cursor = LegacyCursor::new(PageOrder::collect(region));
        let mut out = Vec::new();
        while let Some(record) = cursor.next(region) {
            out.push(Seen {
                key: *record.key,
                time: record.time,
                tag: record.tag,
                live: record.live,
                body: record.body.to_vec(),
                page_seq: record.page_seq,
                offset: record.offset,
            });
        }
        out
    }

    fn numbers(records: &[Seen]) -> Vec<u32> {
        records.iter().map(|record| number(&record.key)).collect()
    }

    /// Clear every bit of one byte that the simulation refuses to leave
    /// alone, so the corruption is real.
    fn corrupt(device: &mut SimNor, at: u32) {
        assert_ne!(device.bytes()[at as usize], 0, "no bit to clear at {at:#x}");
        device.corrupt(at, 0x00);
    }

    /// The first byte of a page's header CRC with a bit set.
    fn header_crc_byte(device: &SimNor, page: u32) -> u32 {
        let first = page * SECTOR_SIZE + 10;
        (first..first + 2)
            .find(|at| device.bytes()[*at as usize] != 0)
            .expect("a page header CRC is not zero")
    }

    #[test]
    fn the_cursor_yields_the_region_in_append_order_after_a_reclaim() {
        const TOTAL: u32 = 14; // one full lap of four pages, then two more

        let device = block_on(async {
            let mut log = RecordLog::open(SimNor::new(PAGES), 0, REGION)
                .await
                .unwrap();
            for n in 0..TOTAL {
                log.append(&key(n), 1_000 + n, n as u8, &body(n, SMALL))
                    .await
                    .unwrap();
            }
            log.into_flash()
        });
        let region = device.bytes();

        let records = walked(region);
        assert_eq!(records, in_append_order(region));

        // The reclaim dropped the oldest page, so the survivors are the
        // newest contiguous run and the region is physically out of append
        // order — which is what makes the ordering in the cursor do work.
        let physical: Vec<u32> = scan(region).iter().map(|r| r.page_seq).collect();
        assert!(
            physical.windows(2).any(|pair| pair[0] > pair[1]),
            "the region must be out of physical append order: {physical:?}"
        );
        let live = numbers(&records);
        assert_eq!(live, (TOTAL - live.len() as u32..TOTAL).collect::<Vec<_>>());
        assert!(
            !live.contains(&0),
            "record 0's page was reclaimed, so it must be gone"
        );

        // No record twice, whatever the cursor does internally.
        let mut sorted = records.iter().map(|r| r.offset).collect::<Vec<_>>();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), records.len());

        // A second cursor over the same region sees the same stream.
        assert_eq!(walked(device.bytes()), records);
    }

    #[test]
    fn purged_records_are_walked_in_place_with_live_false() {
        let device = block_on(async {
            let mut log = RecordLog::open(SimNor::new(PAGES), 0, REGION)
                .await
                .unwrap();
            for n in 0..5 {
                log.append(&key(n), n, 7, &body(n, SMALL)).await.unwrap();
            }
            let mut target = None;
            log.for_each(|record| {
                if record.key == key(2) {
                    target = Some(*record);
                }
            })
            .await
            .unwrap();
            log.purge(&target.expect("record 2 is on the part"))
                .await
                .unwrap();
            log.into_flash()
        });
        let region = device.bytes();

        let records = walked(region);
        assert_eq!(numbers(&records), alloc::vec![0, 1, 2, 3, 4]);
        assert!(records[..2].iter().all(|record| record.live));
        assert!(
            !records[2].live,
            "a purged record is still walked, with live false"
        );
        assert!(records[3..].iter().all(|record| record.live));
        assert_eq!(records, in_append_order(region));
    }

    #[test]
    fn a_torn_tail_ends_its_page_and_the_walk_resumes_at_the_next() {
        let device = block_on(async {
            let mut log = RecordLog::open(SimNor::new(PAGES), 0, REGION)
                .await
                .unwrap();
            log.append(&key(0), 0, 1, &body(0, SMALL)).await.unwrap();
            log.append(&key(1), 1, 1, &body(1, SMALL)).await.unwrap();
            // The append is three program runs: nine words of header, then
            // the CRC and the body from offset 40, then the one-word commit.
            // Cutting ten words into the second run leaves a header whose
            // flags byte is still erased — a gap, not a record.
            log.flash_mut().arm_power_cut(9 + 10);
            assert!(log.append(&key(2), 2, 1, &body(2, SMALL)).await.is_err());
            assert!(log.flash_mut().is_dark());
            log.flash_mut().power_on();
            // The failed append sealed its page, so this lands in the next
            // one, with the torn record still sitting ahead of it.
            log.append(&key(3), 3, 1, &body(3, SMALL)).await.unwrap();
            log.into_flash()
        });
        let region = device.bytes();

        let records = walked(region);
        assert_eq!(numbers(&records), alloc::vec![0, 1, 3]);
        assert_eq!(records, in_append_order(region));
        // The torn page's bytes are not a record, and its successor page is
        // still reached: the walk skipped the gap rather than stopping.
        assert_eq!(records.len(), 3);
        // The torn record's header is on the part with its commit byte still
        // erased — which is what makes it a gap rather than a record.
        let torn = SECTOR_HEADER_LEN as usize + 2 * record_stride(SMALL) + 39;
        assert_eq!(
            region[torn], 0xFF,
            "the cut must leave the commit unwritten"
        );
    }

    #[test]
    fn a_page_whose_header_does_not_agree_is_skipped_whole() {
        let mut device = block_on(async {
            let mut log = RecordLog::open(SimNor::new(PAGES), 0, REGION)
                .await
                .unwrap();
            for n in 0..3 {
                log.append(&key(n), n, 1, &body(n, LARGE)).await.unwrap();
            }
            log.into_flash()
        });

        // Every page header still agrees, so the page order holds all three
        // even though one record is about to stop counting.
        assert_eq!(PageOrder::collect(device.bytes()).len(), 3);

        let at = header_crc_byte(&device, 1);
        device.corrupt(at, 0x00);
        let region = device.bytes();

        let order = PageOrder::collect(region);
        assert_eq!(order.len(), 2);
        assert!(!order.is_empty());
        assert_eq!(numbers(&walked(region)), alloc::vec![0, 2]);
        assert_eq!(walked(region), in_append_order(region));
    }

    #[test]
    fn a_record_with_a_broken_crc_costs_itself_and_not_the_ones_behind_it() {
        let mut device = block_on(async {
            let mut log = RecordLog::open(SimNor::new(PAGES), 0, REGION)
                .await
                .unwrap();
            for n in 0..4 {
                log.append(&key(n), n, 1, &body(n, MEDIUM)).await.unwrap();
            }
            log.into_flash()
        });

        // Two records to a page, so page 0 holds records 0 and 1 and the
        // broken first one has a live record behind it.
        let first_body = SECTOR_HEADER_LEN + HEADER_LEN as u32 + 7;
        corrupt(&mut device, first_body);
        let region = device.bytes();

        let records = walked(region);
        assert_eq!(numbers(&records), alloc::vec![1, 2, 3]);
        assert_eq!(records, in_append_order(region));
        // Both page headers still agree; one record inside a page is what
        // went missing, and the page order never noticed.
        assert_eq!(PageOrder::collect(region).len(), 2);
    }

    #[test]
    fn a_blank_region_has_no_pages_and_no_records() {
        let blank = alloc::vec![0xFFu8; REGION as usize];
        let order = PageOrder::collect(&blank);
        assert!(order.is_empty());
        assert_eq!(order.len(), 0);
        let mut cursor = LegacyCursor::new(order);
        assert!(cursor.next(&blank).is_none());

        // A region the caller hands the cursor that is shorter than the one
        // the order was collected from yields nothing rather than panicking.
        let shorter = &blank[..SECTOR_SIZE as usize];
        let mut cursor = LegacyCursor::new(PageOrder::collect(&blank));
        assert!(cursor.next(shorter).is_none());
    }
}
