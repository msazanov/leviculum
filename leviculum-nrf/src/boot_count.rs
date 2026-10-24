//! The boot count's nRF52840 half (Codeberg #380): the page, and the flash
//! path legal at the point of boot the count is written.
//!
//! What a record says, where it goes and the `BOOT_COUNT` line are
//! `leviculum_firmware::boot_count`, over the Flash seam (#469 step 1).
//! What stays here is what only this chip decides: the page is `memory.x`'s
//! `BOOT` (`0xD9000`), read from the linker through [`page`] and named by no
//! constant anywhere else, the same terms [`crate::record_store`] holds its
//! region on; and the write goes straight to the NVMC.
//!
//! # Which side of the SoftDevice line this write sits on
//!
//! [`crate::radio_store`] draws that line: loading at boot is a plain
//! read of memory-mapped flash, while writing under a live SoftDevice
//! must go through `sd_flash_page_erase` / `sd_flash_write`, because the
//! SD owns the flash timing and a direct NVMC erase stalls the CPU for
//! milliseconds and breaks the BLE radio schedule.
//!
//! **The boot write sits on the other side: before `Softdevice::enable`,
//! straight to the NVMC.** Not because it is simpler — though it is, with
//! no channel, no task and no retry loop — but because of what it is
//! counting. A board that boot-loops on a sagging pack dies EARLY, and
//! most of the ways it can die are before the SoftDevice is up: the HFXO
//! wait, the flash reads, USB. A record written after `Softdevice::enable`
//! would be missing from precisely the boots the operator needs counted,
//! and the count would understate a boot loop exactly in proportion to
//! how bad it was. Writing at the same point the media profile and the
//! node name are read costs those boots nothing.
//!
//! What the early write gives up: a board that dies between power-on and
//! this call — inside `embassy_nrf::init`'s HFXO wait, say — leaves no
//! record, so that boot is never counted and the next boot's `n=` is one
//! lower than the truth. That window is the first few milliseconds of
//! `main` and it is not silent: the boot that dies inside it is the one
//! [`crate::boot_trace`] reports on the NEXT boot's `BOOT_TRACE` line as
//! `prev_phase=enter-main`. The two lines are read together, and between
//! them a boot that was too short to be counted still says so.
//!
//! The erase, when the page fills, is the one expensive operation here —
//! a page erase costs milliseconds, during which the CPU stalls on every
//! flash fetch. It is paid before `usb::init`, so it delays enumeration
//! rather than interrupting it, on one boot in 256.

use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_nrf::nvmc::Nvmc;
use leviculum_boot_count::PAGE_SIZE;

pub use leviculum_firmware::boot_count::{log_banner, Recorded};

/// The page this log owns, from `memory.x`'s `BOOT` region.
///
/// One symbol, no Rust constant beside it: the map is the single place
/// the address is decided, and the `ASSERT`s next to it are what refuse
/// a link in which the page overlaps the image below or the record log
/// above. Absolute symbol (value, not content), so what is taken is its
/// ADDRESS — the same shape [`crate::record_store::region`] reads.
pub fn page() -> u32 {
    extern "C" {
        static __sboot_record: u8;
    }
    core::ptr::addr_of!(__sboot_record) as u32
}

/// Append this boot's record to the page.
///
/// Call once, from `main`, **before `Softdevice::enable`** (see the
/// module comment) and before `usb::init`, next to the other
/// boot-time persistence reads. Blocks: the NVMC write is synchronous,
/// which is the whole reason it belongs this early. The NVMC is the Flash
/// seam's implementer here through [`BlockingAsync`], whose futures finish
/// on their first poll, so `block_on` returns as soon as the NVMC does.
///
/// `boot` is the capture [`crate::boot_trace::capture`] took as the first
/// statement of `main` — this reads its already-cleared `RESETREAS` and
/// whether the retained record survived, and writes to nothing of its.
pub fn record_at_boot(nvmc: Nvmc<'_>, boot: &crate::boot_trace::Captured) -> Recorded {
    let retained = matches!(boot.prev, leviculum_boot_trace::PrevBoot::Present { .. });
    let page = page();

    // SAFETY: `page` is the base of `memory.x`'s BOOT region, a whole
    // 4 KiB page inside the 1 MiB flash map that the linker hands to no
    // section and that the ASSERTs there keep clear of both the image
    // below and the record log above. Internal flash is readable as
    // normal memory on this part — the same read `radio_store::load`
    // does, legal before `Softdevice::enable`. The slice is dropped
    // before anything writes.
    let mapped = unsafe { core::slice::from_raw_parts(page as *const u8, PAGE_SIZE) };
    let mut flash = BlockingAsync::new(nvmc);
    embassy_futures::block_on(leviculum_firmware::boot_count::record(
        &mut flash,
        page,
        mapped,
        boot.reset_reason,
        retained,
    ))
}
