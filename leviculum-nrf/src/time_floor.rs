//! The learned calendar, checkpointed to flash across a power cycle
//! (Codeberg #166 item 4).
//!
//! A clockless board with neither GNSS nor a host learns real time from
//! the first credible announce it overhears. Without this module it
//! forgets it at every power cycle and stamps from its build timestamp
//! until it overhears another. With it, the calendar is written at most
//! hourly to the `TIME` ring `memory.x` reserves (two pages at `0xD7000`,
//! read from the linker through [`region`](crate::time_floor::region) and named by no constant
//! anywhere else, the terms [`crate::boot_count`] and
//! [`crate::record_store`] hold their regions on), and at boot the newest
//! valid record is restored through `NodeCore::restore_emission_floor`.
//! That seam seats it at the birth rank: a floor, never a claim about now,
//! so the first credible announce still heals the calendar in one step
//! (`docs/src/concepts/time-and-clocks.md`, "A restored floor is a birth
//! anchor with a better value").
//!
//! The ring's layout, its scan, where the next record goes and when one is
//! due live in [`leviculum_time_floor`], where a host test cuts the power
//! at every word boundary. This module only moves bytes.
//!
//! # Which side of the SoftDevice line
//!
//! The same split as [`crate::radio_store`]: the restore at boot is a plain
//! read of memory-mapped flash, legal before and after
//! `Softdevice::enable`; the checkpoint writes run while the SoftDevice is
//! live, so they go through its flash API on the one shared handle
//! ([`crate::flash::SharedFlash`]), from a task that nothing cancels,
//! because `nrf_softdevice::Flash` panics on a write dropped in flight.
//! The main loop never waits for flash: it drops the value into a one-slot
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
use leviculum_time_floor::{newest, CheckpointGate, RestoreLine, RING_PAGES};

/// The ring's bounds, from `memory.x`'s `TIME` region.
///
/// Absolute symbols (value, not content), so what is taken is their
/// ADDRESSES, the shape [`crate::boot_count::page`] reads.
pub fn region() -> (u32, u32) {
    extern "C" {
        static __stime_floor: u8;
        static __etime_floor: u8;
    }
    (
        core::ptr::addr_of!(__stime_floor) as u32,
        core::ptr::addr_of!(__etime_floor) as u32,
    )
}

/// The ring as memory-mapped flash.
fn mapped() -> &'static [u8] {
    let (start, end) = region();
    let len = end.saturating_sub(start) as usize;
    // SAFETY: `start..end` is `memory.x`'s TIME region, whole 4 KiB pages
    // inside the 1 MiB flash map that the linker hands to no section, with
    // ASSERTs there keeping it clear of the image below and the boot page
    // above. Internal flash is readable as normal memory on this part, the
    // read `radio_store::load` does. Only the store task below writes it,
    // through the SoftDevice, and every reader re-maps after a write.
    unsafe { core::slice::from_raw_parts(start as *const u8, len) }
}

/// The checkpoint state the main loop owns.
pub struct Checkpoint {
    gate: CheckpointGate,
}

impl Checkpoint {
    /// Restore the newest valid record through `restore` (the node's
    /// `restore_emission_floor`) and print the outcome. Call once at boot,
    /// after the node is built and BEFORE any interface can deliver an
    /// announce, so the restored floor is in place when the first one is
    /// judged.
    pub fn restore_at_boot(restore: impl FnOnce(u64) -> bool) -> Self {
        let ring = mapped();
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

/// 4-byte-aligned record buffer: `sd_flash_write` writes whole words and
/// rejects an unaligned source.
#[repr(align(4))]
struct Aligned([u8; leviculum_time_floor::RECORD_SIZE]);

#[cfg(feature = "softdevice")]
#[embassy_executor::task]
async fn store_task(flash: &'static crate::flash::SharedFlash) {
    use embedded_storage_async::nor_flash::NorFlash;
    use leviculum_time_floor::{encode, plan, PAGE_SIZE};

    let (start, _) = region();
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

/// Spawn the checkpoint store task, borrowing the shared SoftDevice flash
/// handle. Call after `Softdevice::enable`, beside the other store tasks.
#[cfg(feature = "softdevice")]
pub fn spawn_store_task(
    spawner: &embassy_executor::Spawner,
    flash: &'static crate::flash::SharedFlash,
) {
    spawner.must_spawn(store_task(flash));
}
