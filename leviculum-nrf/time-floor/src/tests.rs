//! Host tests for the checkpoint ring.
//!
//! [`Ring`] simulates the `TIME` region with the two NOR properties the
//! format leans on: programming only clears bits, and an erase is the only
//! way to raise one. Programming bytes that are not erased panics, because
//! doing so is the defect the torn-slot rule exists to prevent.

use super::*;

struct Ring {
    bytes: Vec<u8>,
    erases: Vec<u32>,
}

impl Ring {
    fn erased(pages: usize) -> Self {
        Ring {
            bytes: vec![ERASED; pages * PAGE_SIZE],
            erases: vec![0; pages],
        }
    }

    fn erase(&mut self, page: usize) {
        self.bytes[page * PAGE_SIZE..(page + 1) * PAGE_SIZE].fill(ERASED);
        self.erases[page] += 1;
    }

    fn program(&mut self, offset: usize, data: &[u8]) {
        let target = &mut self.bytes[offset..offset + data.len()];
        assert!(
            target.iter().all(|b| *b == ERASED),
            "programmed over bytes that were not erased, at offset {offset}"
        );
        target.copy_from_slice(data);
    }

    /// One checkpoint, carried out as `src/time_floor.rs` does.
    fn checkpoint(&mut self, value: u64) -> Plan {
        let plan = plan(&self.bytes, value).expect("a ring of at least two pages");
        if let Some(page) = plan.erase_page {
            self.erase(page);
        }
        self.program(plan.offset, &encode(&plan.record));
        plan
    }

    /// A checkpoint cut off after `words` of its five words landed.
    fn torn_checkpoint(&mut self, value: u64, words: usize) {
        let plan = plan(&self.bytes, value).expect("a ring of at least two pages");
        if let Some(page) = plan.erase_page {
            self.erase(page);
        }
        let bytes = encode(&plan.record);
        self.program(plan.offset, &bytes[..words * 4]);
    }

    fn newest_value(&self) -> Option<u64> {
        newest(&self.bytes).map(|n| n.record.value)
    }
}

const T0: u64 = 1_790_000_000;

#[test]
fn crc_matches_the_standard_check_value() {
    assert_eq!(crc16(b"123456789"), 0x29B1);
}

#[test]
fn a_record_round_trips() {
    let r = Record { seq: 7, value: T0 };
    assert_eq!(decode(&encode(&r)), Some(r));
    assert_eq!(decode(&[ERASED; RECORD_SIZE]), None);
}

#[test]
fn an_erased_ring_holds_no_floor_and_starts_at_sequence_one() {
    let mut ring = Ring::erased(RING_PAGES);
    assert_eq!(newest(&ring.bytes), None);
    let plan = ring.checkpoint(T0);
    assert_eq!(plan.record.seq, 1);
    assert_eq!(plan.offset, 0);
    assert_eq!(plan.erase_page, None, "an erased page needs no erase");
    assert_eq!(ring.newest_value(), Some(T0));
}

#[test]
fn the_newest_valid_record_wins() {
    let mut ring = Ring::erased(RING_PAGES);
    for i in 0..10 {
        ring.checkpoint(T0 + i * 3_600);
    }
    let n = newest(&ring.bytes).expect("ten records");
    assert_eq!(n.record.seq, 10);
    assert_eq!(n.record.value, T0 + 9 * 3_600);
}

#[test]
fn the_newest_record_wins_across_a_page_boundary() {
    // Fill page 0 and spill into page 1, then lap back to page 0: the
    // newest is wherever the highest sequence number is, not the
    // highest address.
    let mut ring = Ring::erased(RING_PAGES);
    let total = 2 * RECORDS_PER_PAGE as u64 + 3;
    for i in 0..total {
        ring.checkpoint(T0 + i);
    }
    let n = newest(&ring.bytes).expect("records");
    assert_eq!(n.page, 0, "the third lap is back on page 0");
    assert_eq!(n.record.seq as u64, total);
    assert_eq!(n.record.value, T0 + total - 1);
}

#[test]
fn a_torn_record_is_skipped_at_every_word_boundary() {
    for words in 0..RECORD_SIZE / 4 {
        let mut ring = Ring::erased(RING_PAGES);
        ring.checkpoint(T0);
        ring.torn_checkpoint(T0 + 7_200, words);
        assert_eq!(
            ring.newest_value(),
            Some(T0),
            "a write cut after {words} words must leave the previous floor"
        );
        // The next checkpoint lands after the torn slot, never on it
        // (the simulated flash panics on a program over written bytes),
        // and is the one a restore then picks.
        let plan = ring.checkpoint(T0 + 10_800);
        assert_eq!(plan.record.seq, 2);
        assert_eq!(ring.newest_value(), Some(T0 + 10_800));
    }
}

#[test]
fn a_cut_between_the_erase_and_the_write_keeps_the_previous_page() {
    let mut ring = Ring::erased(RING_PAGES);
    for i in 0..RECORDS_PER_PAGE as u64 {
        ring.checkpoint(T0 + i);
    }
    let before = ring.newest_value();
    // The next checkpoint erases page 1 and is cut before any word lands.
    ring.torn_checkpoint(T0 + 99_999, 0);
    assert_eq!(
        ring.erases,
        vec![0, 0],
        "page 1 was blank and needs no erase"
    );
    assert_eq!(ring.newest_value(), before);

    // Same cut on the second lap, where page 1 does hold old records and
    // is erased: page 0 still holds the newest.
    let mut ring = Ring::erased(RING_PAGES);
    for i in 0..(2 * RECORDS_PER_PAGE) as u64 {
        ring.checkpoint(T0 + i);
    }
    let before = ring.newest_value();
    ring.torn_checkpoint(T0 + 99_999, 0);
    assert_eq!(ring.erases, vec![1, 0]);
    assert_eq!(
        ring.newest_value(),
        before,
        "the erase took only older records"
    );
}

#[test]
fn foreign_bytes_are_never_a_floor_and_are_erased_before_use() {
    // The TIME pages were application flash before they were carved out,
    // so a board may carry code bytes there from an older image.
    let mut ring = Ring::erased(RING_PAGES);
    for (i, b) in ring.bytes.iter_mut().enumerate() {
        *b = (i * 31 % 251) as u8;
    }
    assert_eq!(newest(&ring.bytes), None);
    let plan = ring.checkpoint(T0);
    assert_eq!(plan.record.seq, 1);
    assert_eq!(ring.newest_value(), Some(T0));
}

#[test]
fn wear_spreads_over_the_ring_pages() {
    for pages in [2usize, 4] {
        let mut ring = Ring::erased(pages);
        let laps = 5;
        let writes = laps * pages * RECORDS_PER_PAGE;
        for i in 0..writes as u64 {
            ring.checkpoint(T0 + i * 3_600);
        }
        // Every page was erased once per lap after the first (the first
        // lap finds them blank), and no page more than any other.
        let max = *ring.erases.iter().max().unwrap();
        let min = *ring.erases.iter().min().unwrap();
        assert!(max - min <= 1, "{pages} pages: {:?}", ring.erases);
        assert_eq!(ring.erases.iter().sum::<u32>() as usize, (laps - 1) * pages);
        assert_eq!(ring.newest_value(), Some(T0 + (writes as u64 - 1) * 3_600));
    }
}

#[test]
fn the_wear_arithmetic() {
    // The issue's sketch: one record per page, erased on every write.
    let sketch = years_one_record_per_page(4);
    assert!((4.5..4.6).contains(&sketch), "{sketch}");
    // What ships: appended records over two pages.
    let shipped = years_appending(RING_PAGES as u32);
    assert!((465.0..466.0).contains(&shipped), "{shipped}");
}

#[test]
fn a_ring_below_two_pages_is_refused() {
    assert_eq!(plan(&[ERASED; PAGE_SIZE], T0), None);
    assert_eq!(plan(&[], T0), None);
}

#[test]
fn the_gate_waits_an_hour_of_uptime_from_boot() {
    let mut gate = CheckpointGate::new();
    assert!(!gate.due(4, T0, CHECKPOINT_INTERVAL_MS - 1));
    assert!(gate.due(4, T0, CHECKPOINT_INTERVAL_MS));
}

#[test]
fn the_gate_refuses_a_birth_rank() {
    let mut gate = CheckpointGate::new();
    assert!(!gate.due(5, T0, CHECKPOINT_INTERVAL_MS));
    for rank in 1..=MAX_CHECKPOINT_RANK {
        let mut gate = CheckpointGate::new();
        assert!(gate.due(rank, T0, CHECKPOINT_INTERVAL_MS));
    }
}

#[test]
fn the_gate_is_hourly_and_needs_an_hour_of_advance() {
    let mut gate = CheckpointGate::new();
    let h = CHECKPOINT_INTERVAL_MS;
    assert!(gate.due(4, T0, h));
    // Not an hour of uptime later.
    assert!(!gate.due(4, T0 + 7_200, 2 * h - 1));
    // An hour of uptime later, but the calendar has not moved an hour.
    assert!(!gate.due(4, T0 + 3_599, 2 * h));
    assert!(gate.due(4, T0 + 3_600, 2 * h));
}

#[test]
fn a_seeded_gate_writes_only_past_the_value_on_flash() {
    let mut gate = CheckpointGate::new();
    gate.seed(T0);
    let h = CHECKPOINT_INTERVAL_MS;
    assert!(!gate.due(4, T0 + 100, h));
    assert!(gate.due(4, T0 + 3_600, h));
}

#[test]
fn the_restore_lines() {
    let r = Record { seq: 42, value: T0 };
    assert_eq!(
        RestoreLine::Restored(r).to_string(),
        "[TIME_RESTORE] value=1790000000 seq=42"
    );
    assert_eq!(
        RestoreLine::Refused(r).to_string(),
        "[TIME_RESTORE_REFUSED] value=1790000000 seq=42 reason=seam"
    );
    assert_eq!(
        RestoreLine::Empty.to_string(),
        "[TIME_RESTORE_REFUSED] value=0 seq=0 reason=empty"
    );
}
