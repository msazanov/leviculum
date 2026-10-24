//! The learned calendar's nRF52840 half (Codeberg #166 item 4): where the
//! checkpoint ring lies, and the store task that writes it under the
//! SoftDevice.
//!
//! The checkpoint itself, the restore at boot and the write loop are
//! `leviculum_firmware::time_floor`, over the Flash seam (#469 step 1).
//! What stays here is this chip's: the ring is the `TIME` region
//! `memory.x` reserves (two pages at `0xD7000`), read from the linker
//! through [`region`](crate::time_floor::region) and named by no constant
//! anywhere else, the terms [`crate::boot_count`] and
//! [`crate::record_store`] hold their regions on.
//!
//! # Which side of the SoftDevice line
//!
//! The same split as [`crate::radio_store`]: the restore at boot is a plain
//! read of memory-mapped flash, legal before and after
//! `Softdevice::enable`; the checkpoint writes run while the SoftDevice is
//! live, so they go through its flash API on the one shared handle
//! ([`crate::flash::SharedFlash`]), from a task that nothing cancels,
//! because `nrf_softdevice::Flash` panics on a write dropped in flight.

pub use leviculum_firmware::time_floor::Checkpoint;

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

/// Restore the newest valid checkpoint through `restore` (the node's
/// `restore_emission_floor`) and print the outcome; see
/// [`Checkpoint::restore_at_boot`] for when to call it.
pub fn restore_at_boot(restore: impl FnOnce(u64) -> bool) -> Checkpoint {
    Checkpoint::restore_at_boot(mapped(), restore)
}

#[cfg(feature = "softdevice")]
#[embassy_executor::task]
async fn store_task(flash: &'static crate::flash::SharedFlash) {
    let (start, _) = region();
    leviculum_firmware::time_floor::serve(flash, start, mapped).await
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
