//! The message store's nRF52840 half (Codeberg #384): the region
//! `memory.x` reserves behind the firmware image, the SoftDevice flash as a
//! device the record log can own, and the task that runs the store.
//!
//! The requests, the counters, the mount and the operations are
//! `leviculum_firmware::record_store`, over the Flash seam (#469 step 1);
//! they are re-exported here under the names [`crate::pn`] and
//! [`crate::usb`] have always called. The cut: what stays is what touches
//! this part, the region's address, the flash device's geometry and its
//! retry loop around the shared handle, and the embassy task.
//!
//! # Where the region comes from
//!
//! Not from a constant here. `memory.x` carves `STORE` out of the top of the
//! application window and exports `__srecord_store` / `__erecord_store`;
//! [`region`](crate::record_store::region) reads those two symbols and nothing
//! else in the tree knows the
//! addresses. Moving or resizing the store is one edit in one file, and an
//! image that grows into it is a link error rather than a store quietly
//! sitting underneath the firmware (the `ASSERT`s in `memory.x`, plus
//! `scripts/check-nrf-store-gap.sh`, which prints the remaining gap for both
//! bins on every `just fast`).
//!
//! # Why the log lives in a task and is reached by channel
//!
//! Two reasons, one of them fatal.
//!
//! The fatal one: `nrf_softdevice::Flash`'s `write` and `erase` futures arm a
//! `DropBomb` (nrf-softdevice 5949a5b, `nrf-softdevice/src/flash.rs`) and
//! **panic** if they are dropped in flight. A caller that could cancel an
//! append — a `select!` with a timeout, a connection task that goes away — is
//! therefore a caller that could panic the board. Nothing that can be
//! cancelled may hold an append, so the log is owned by a task that does
//! nothing else and requests arrive by channel; the sender never waits
//! for the flash.
//!
//! The second: `Flash::take` is a singleton and the radio config (#349) and
//! the telemetry target (#236) already write through it. All three go through
//! the one [`crate::flash::SharedFlash`] mutex, which
//! [`Device`](crate::record_store::Device) below takes
//! for the duration of a single flash operation — not for a whole append. A
//! store that held the lock across an append would stall a radio-config save
//! behind its erases, and an append that waited for the whole log would be an
//! append during which nothing else could persist anything.

#[cfg(feature = "softdevice")]
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
#[cfg(feature = "softdevice")]
use leviculum_record_log::board::retry_after;

pub use leviculum_firmware::record_store::{
    board_list_landed, mounted, pn_execute, request_board_list, request_storm, PnDone, PnOp,
};

/// The store's region: `(base, len)` from the linker symbols `memory.x`
/// exports.
///
/// Absolute symbols — their *addresses* are the numbers — which is the same
/// shape `__sretained` is read with in [`crate::ble`].
pub fn region() -> (u32, u32) {
    extern "C" {
        static __srecord_store: u8;
        static __erecord_store: u8;
    }
    let base = core::ptr::addr_of!(__srecord_store) as u32;
    let end = core::ptr::addr_of!(__erecord_store) as u32;
    (base, end.saturating_sub(base))
}

/// The shared SoftDevice flash handle as a device the record log can own.
///
/// Takes the mutex per operation, which is what lets the log be mounted for
/// the lifetime of the task while the radio config and the telemetry target
/// keep writing their own pages between its operations. Safe because the three
/// stores own disjoint pages: the only thing the lock has to serialise is
/// access to the single `Flash` handle itself.
///
/// Writes and erases are retried with a doubling backoff and counted here,
/// where the kind of the operation is known. A retry is invisible to the log
/// above: by the time it sees an error, the operation has been refused
/// `FLASH_ATTEMPTS` times, which is what makes the log's page-sealing
/// response to a failure proportionate.
#[cfg(feature = "softdevice")]
pub struct Device {
    flash: &'static crate::flash::SharedFlash,
}

#[cfg(feature = "softdevice")]
type SdFlash = nrf_softdevice::Flash;

#[cfg(feature = "softdevice")]
impl Device {
    /// Borrow the shared handle.
    pub fn new(flash: &'static crate::flash::SharedFlash) -> Self {
        Self { flash }
    }
}

/// One operation with its retry loop.
///
/// A macro rather than a function taking a closure: each of the two operations
/// borrows the mutex guard for the duration of its own future, and expressing
/// that as a `FnMut` returning a future that borrows its argument needs
/// lending closures. The loop is the interesting part and it exists once.
#[cfg(feature = "softdevice")]
macro_rules! retried {
    ($self:expr, $op:literal, |$guard:ident| $call:expr) => {{
        let mut attempt = 1u8;
        loop {
            let outcome = {
                let mut $guard = $self.flash.lock().await;
                $call.await
            };
            if outcome.is_ok() {
                break outcome;
            }
            leviculum_firmware::record_store::note_op_fail($op, attempt);
            match retry_after(attempt) {
                Some(ms) => {
                    embassy_time::Timer::after(embassy_time::Duration::from_millis(ms)).await
                }
                None => break outcome,
            }
            attempt += 1;
        }
    }};
}

#[cfg(feature = "softdevice")]
impl ErrorType for Device {
    type Error = <SdFlash as ErrorType>::Error;
}

#[cfg(feature = "softdevice")]
impl ReadNorFlash for Device {
    const READ_SIZE: usize = <SdFlash as ReadNorFlash>::READ_SIZE;

    /// Not retried: internal flash is memory-mapped and the SoftDevice's own
    /// implementation is a `memcpy` that cannot fail or be refused.
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let mut guard = self.flash.lock().await;
        <SdFlash as ReadNorFlash>::read(&mut guard, offset, bytes).await
    }

    fn capacity(&self) -> usize {
        // 1 MiB, the nRF52840's flash. `nrf_softdevice::Flash::capacity`
        // returns this same compile-time constant under its `nrf52840`
        // feature (nrf-softdevice 5949a5b, `nrf-softdevice/src/flash.rs`);
        // the trait method is synchronous, so the async mutex cannot be taken
        // to ask it, and restating the constant is the lesser evil. Getting it
        // wrong is a safe failure: the record log compares `base + len`
        // against it and refuses the region with `OutOfBounds`, which the
        // mount line reports.
        256 * 4096
    }
}

#[cfg(feature = "softdevice")]
impl NorFlash for Device {
    const WRITE_SIZE: usize = <SdFlash as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <SdFlash as NorFlash>::ERASE_SIZE;

    /// The record log erases exactly one page per call, so a retry of a
    /// refused erase cannot re-erase a page that already succeeded — which
    /// would spend an erase cycle the wear budget counts.
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        debug_assert_eq!(to - from, Self::ERASE_SIZE as u32);
        retried!(self, "erase", |guard| <SdFlash as NorFlash>::erase(
            &mut *guard,
            from,
            to
        ))
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        retried!(self, "write", |guard| <SdFlash as NorFlash>::write(
            &mut *guard,
            offset,
            bytes
        ))
    }
}

/// `nrf_softdevice::Flash` declares it (a word takes a second write as long as
/// it only clears bits), and [`RecordLog::purge`](leviculum_record_log::RecordLog::purge) needs it.
#[cfg(feature = "softdevice")]
impl MultiwriteNorFlash for Device {}

/// Mount the store and serve requests for the lifetime of the board
/// (`leviculum_firmware::record_store::run`), on the shared SoftDevice
/// flash handle.
#[cfg(feature = "softdevice")]
#[embassy_executor::task]
pub async fn store_task(flash: &'static crate::flash::SharedFlash) {
    let (base, len) = region();
    leviculum_firmware::record_store::run(
        Device::new(flash),
        base,
        len,
        crate::pn::FlashRegion::store_region(),
    )
    .await
}

/// Spawn the store task, borrowing the shared SoftDevice flash handle.
///
/// Call after `Softdevice::enable`, beside the radio-config and telemetry
/// store tasks: writing internal flash with the SoftDevice live is only legal
/// through its own syscalls.
#[cfg(feature = "softdevice")]
pub fn spawn_store_task(
    spawner: &embassy_executor::Spawner,
    flash: &'static crate::flash::SharedFlash,
) {
    spawner.must_spawn(store_task(flash));
}
