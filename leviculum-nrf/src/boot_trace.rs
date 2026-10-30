//! The reset-cause half of the boot breadcrumbs (#469): the record, its
//! phases and its line are `leviculum_firmware::boot_trace`; the register
//! that says why the core reset is the chip's, and is read here.

use leviculum_firmware::boot_trace::Captured;

/// POWER.RESETREAS (0x40000000 + 0x400). embassy-nrf 0.9 keeps its pac
/// crate-private, so raw volatile access, as in the rest of this crate.
const POWER_RESETREAS: *mut u32 = 0x4000_0400 as *mut u32;

/// [`leviculum_firmware::boot_trace::capture`] with POWER.RESETREAS as the
/// reset cause, cleared in the register after the read (write-1-to-clear:
/// writing back the read value clears exactly the latched bits).
///
/// Call once, as the FIRST statement of `main`, for the reasons at the
/// generic `capture`: POWER is still this core's, not the SoftDevice's.
pub fn capture() -> Captured {
    leviculum_firmware::boot_trace::capture(|| {
        // SAFETY: a pre-SD read and write-back of one POWER register,
        // before any concurrent task exists.
        unsafe {
            let reset_reason = core::ptr::read_volatile(POWER_RESETREAS);
            core::ptr::write_volatile(POWER_RESETREAS, reset_reason);
            reset_reason
        }
    })
}
