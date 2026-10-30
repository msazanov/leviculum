//! The learned calendar, checkpointed to flash across a power cycle
//! (Codeberg #166 item 4).
//!
//! A clockless board with neither GNSS nor a host learns real time from
//! the first credible announce it overhears. Without this module it
//! forgets it at every power cycle and stamps from its build timestamp
//! until it overhears another. With it, the calendar is written at most
//! hourly to a ring of two or more flash pages, and at boot the newest
//! valid record is restored through `NodeCore::restore_emission_floor`.
//! That seam seats it at the birth rank: a floor, never a claim about now,
//! so the first credible announce still heals the calendar in one step
//! (`docs/src/concepts/time-and-clocks.md`, "A restored floor is a birth
//! anchor with a better value").
//!
//! The ring's layout, its scan, where the next record goes and when one is
//! due live in [`leviculum_time_floor`], where a host test cuts the power
//! at every word boundary. This module moves bytes over the [`Flash`] seam
//! (#469 step 1); where the ring lies and how it is mapped for reading are
//! the bind crate's (on the nRF52840 `memory.x`'s `TIME` region, two pages
//! at `0xD7000`, `leviculum-nrf/src/time_floor.rs`).
//!
//! # Who waits for flash
//!
//! The restore at boot is a plain read of the mapped ring. The checkpoint
//! writes go through the one shared flash handle the bind crate passes
//! [`serve`], from a task that nothing cancels, because a backend may
//! panic on a write dropped in flight (`nrf_softdevice::Flash` does). The
//! main loop never waits for flash: it drops the value into a one-slot
//! channel and moves on.
//!
//! # What is logged
//!
//! ```text
//! [TIME_RESTORE] value=<unix> seq=<n>
//! [TIME_RESTORE_REFUSED] value=<unix> seq=<n> reason=seam|empty
//! [TIME_CHECKPOINT] value=<unix> seq=<n> erased=<page|->
//! [TIME_CHECKPOINT_FAILED] value=<unix> seq=<n>
//! ```
//!
//! The restore line is ungated and printed once, beside `[TIME_SOURCE]`.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use leviculum_time_floor::{newest, CheckpointGate, RestoreLine, RING_PAGES};

use crate::seams::Flash;

/// The checkpoint state the main loop owns.
pub struct Checkpoint {
    gate: CheckpointGate,
}

impl Checkpoint {
    /// Restore the newest valid record through `restore` (the node's
    /// `restore_emission_floor`) and print the outcome. Call once at boot,
    /// after the node is built and BEFORE any interface can deliver an
    /// announce, so the restored floor is in place when the first one is
    /// judged. `ring` is the ring as the part maps it.
    pub fn restore_at_boot(ring: &[u8], restore: impl FnOnce(u64) -> bool) -> Self {
        // The linker ASSERT holds the region at two pages or more; this
        // states the number the crate's wear arithmetic was done for.
        if ring.len() != RING_PAGES * leviculum_time_floor::PAGE_SIZE {
            crate::log::log_fmt_critical(
                "",
                format_args!("[TIME_RING] unexpected length={}", ring.len()),
            );
        }
        let mut gate = CheckpointGate::new();
        let line = match newest(ring) {
            None => RestoreLine::Empty,
            Some(n) => {
                if restore(n.record.value) {
                    gate.seed(n.record.value);
                    RestoreLine::Restored(n.record)
                } else {
                    RestoreLine::Refused(n.record)
                }
            }
        };
        crate::log::log_fmt_critical("", format_args!("{}", line));
        Self { gate }
    }

    /// Hand the calendar to the store task when a checkpoint is due (at
    /// most hourly, rank 4 or better, advanced an hour since the last).
    /// Cheap; call on every main-loop pass.
    pub fn poll(&mut self, rank: u8, value: u64, now_ms: u64) {
        if self.gate.due(rank, value, now_ms) {
            request(value);
        }
    }
}

/// Pending checkpoint. Depth 1 and `try_send`: the main loop never blocks
/// on flash, and a superseded value is worthless.
static PENDING: Channel<CriticalSectionRawMutex, u64, 1> = Channel::new();

fn request(value: u64) {
    if PENDING.try_send(value).is_err() {
        let _ = PENDING.try_receive();
        let _ = PENDING.try_send(value);
    }
}

/// 4-byte-aligned record buffer: a NOR part programs whole words, and
/// `sd_flash_write` rejects an unaligned source.
#[repr(align(4))]
struct Aligned([u8; leviculum_time_floor::RECORD_SIZE]);

/// Write each checkpoint [`Checkpoint::poll`] hands over, for the lifetime
/// of the board.
///
/// For the bind crate's store task: `flash` is the shared handle it locks
/// per operation, `start` the ring's first byte on it, and `mapped` maps
/// the ring for reading. `mapped` is called afresh before every plan,
/// because the bytes it returns change under each write.
pub async fn serve<F: Flash>(
    flash: &Mutex<CriticalSectionRawMutex, F>,
    start: u32,
    mapped: impl Fn() -> &'static [u8],
) -> ! {
    use leviculum_time_floor::{encode, plan, PAGE_SIZE};

    loop {
        let value = PENDING.receive().await;
        let Some(plan) = plan(mapped(), value) else {
            crate::log::log_fmt_critical("", format_args!("[TIME_RING] too small"));
            continue;
        };
        let record = Aligned(encode(&plan.record));
        let result = async {
            let mut flash = flash.lock().await;
            if let Some(page) = plan.erase_page {
                let base = start + (page * PAGE_SIZE) as u32;
                flash.erase(base, base + PAGE_SIZE as u32).await?;
            }
            flash.write(start + plan.offset as u32, &record.0).await
        }
        .await;
        // A failed write is not retried here: the gate counted it, so the
        // next checkpoint is an hour away, and the ring's newest valid
        // record is still the previous one.
        match result {
            Ok(()) => match plan.erase_page {
                Some(page) => crate::log::log_fmt(
                    "",
                    format_args!(
                        "[TIME_CHECKPOINT] value={} seq={} erased={}",
                        plan.record.value, plan.record.seq, page
                    ),
                ),
                None => crate::log::log_fmt(
                    "",
                    format_args!(
                        "[TIME_CHECKPOINT] value={} seq={} erased=-",
                        plan.record.value, plan.record.seq
                    ),
                ),
            },
            Err(_) => crate::log::log_fmt_critical(
                "",
                format_args!(
                    "[TIME_CHECKPOINT_FAILED] value={} seq={}",
                    plan.record.value, plan.record.seq
                ),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use leviculum_time_floor::{encode, Record, PAGE_SIZE};

    fn ring_with(record: Option<Record>) -> alloc::vec::Vec<u8> {
        let mut ring = vec![0xFF; RING_PAGES * PAGE_SIZE];
        if let Some(r) = record {
            ring[..leviculum_time_floor::RECORD_SIZE].copy_from_slice(&encode(&r));
        }
        ring
    }

    #[test]
    fn an_empty_ring_restores_nothing() {
        let mut asked = false;
        Checkpoint::restore_at_boot(&ring_with(None), |_| {
            asked = true;
            true
        });
        assert!(!asked, "an erased ring has no floor to offer");
    }

    #[test]
    fn the_newest_record_is_offered_to_the_node() {
        let record = Record {
            seq: 3,
            value: 1_790_000_000,
        };
        let mut offered = None;
        Checkpoint::restore_at_boot(&ring_with(Some(record)), |v| {
            offered = Some(v);
            false
        });
        assert_eq!(offered, Some(1_790_000_000));
    }
}
