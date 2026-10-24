//! How often this board restarted while nobody was watching (Codeberg
//! #380).
//!
//! The non-volatile half of the boot instrumentation. The boot trace
//! keeps its breadcrumbs in retained RAM, which answers what the previous
//! boot was doing and is gone by definition after a power loss — and a
//! Pocket V2 that restarted twice on a field walk is exactly a power
//! loss: every boot of that walk read `reset_reason=0x00000000` with
//! `prev_magic=absent`. A count that survives the supply going away has
//! to live in flash, so this module appends one 16-byte record per boot
//! to one page of it, over the [`Flash`] seam. Which page is the bind
//! crate's to say, from its linker map and from nothing else (on the
//! nRF52840 `memory.x`'s `BOOT`, `leviculum-nrf/src/boot_count.rs`): a
//! board config carrying its own copy of the address could disagree with
//! the linker, and that disagreement is a module that erases the running
//! image's own code.
//!
//! **One page erase covers 256 boots**; the layout, the free-slot scan,
//! the wrap that carries the boot number past an erase and the wear
//! arithmetic are in [`leviculum_boot_count`], where a host test can cut
//! the power at every word boundary. This module only moves bytes.
//!
//! # Where the write sits in boot
//!
//! **Before the radio stack is up, synchronously.** Not because it is
//! simpler, but because of what it is counting. A board that boot-loops on
//! a sagging pack dies EARLY, and most of the ways it can die come before
//! the radio stack: the clock wait, the flash reads, USB. A record written
//! later would be missing from precisely the boots the operator needs
//! counted, and the count would understate a boot loop exactly in
//! proportion to how bad it was. Which flash path is legal that early is a
//! chip fact and the bind crate's: the nRF52840's is the NVMC before
//! `Softdevice::enable` (`leviculum-nrf/src/boot_count.rs`).
//!
//! What the early write gives up: a board that dies between power-on and
//! this call leaves no record, so that boot is never counted and the next
//! boot's `n=` is one lower than the truth. That window is the first few
//! milliseconds of `main` and it is not silent: the boot that dies inside
//! it is the one the boot trace reports on the NEXT boot's `BOOT_TRACE`
//! line as `prev_phase=enter-main`. The two lines are read together, and
//! between them a boot that was too short to be counted still says so.
//!
//! The erase, when the page fills, is the one expensive operation here,
//! paid on one boot in 256.
//!
//! # Why the count is not in the telemetry report yet
//!
//! It belongs there, for the reason the pack voltage does: the report is
//! the only surface that reaches an operator who is still out there,
//! while the debug line needs a cable. **The report has no field for it.**
//! `leviculum_lxmf::telemetry` carries six of Sideband's sensors —
//! time, location, battery, physical link, temperature, power production
//! — and a restart count is none of them. The format is Sideband's and
//! not ours, so the gap is stated here rather than filled by minting a
//! sensor ID: `docs/src/concepts/telemetry.md` §"The extension ladder"
//! is explicit that a field number we invent and one app understands is
//! a fork of the format with a friendlier name.
//!
//! The proposal, which is rung 1 of that ladder and therefore not an
//! extension at all: Sideband defines a **free-text information sensor**
//! among its twenty-four, and `boots=<n>` is exactly the kind of
//! statement it exists for. What is missing before it can be written is
//! its SID and the shape of its packed value, and neither can be read
//! from this tree — no Sideband checkout lives under `reference/`, which
//! holds only Reticulum, LXMF, LXST and the RNode firmware. Adding the
//! sensor is then one field, one encode arm and one decode arm in the
//! codec, plus one line here; a viewer that ignores the sensor shows
//! the report exactly as it does today, which is the "failure mode on a
//! viewer that does not participate" the same document asks for.

use leviculum_boot_count::{encode, plan, BootCountLine, BootRecord, PAGE_SIZE};

use crate::seams::Flash;

/// What this boot's record says, and whether it reached the page.
pub struct Recorded {
    /// The record as it was written (or as it would have been, had the
    /// write not failed).
    pub record: BootRecord,
    /// Slots of the page spent including this one — the `since_erase=`
    /// of the line.
    pub since_erase: u32,
    /// False if the flash refused the erase or the write. The line is
    /// emitted either way; a second line says the count did not stick.
    pub persisted: bool,
}

/// Append this boot's record to the page at `page` on `flash`.
///
/// `mapped` is the same page as the part maps it for reading, which is how
/// the free slot is found without a page-sized buffer on the stack. The bind
/// crate owns both the address and the mapping, and calls this once at the
/// point of boot its chip allows (see the module comment).
///
/// `reset_reason` and `retained` are this boot's reset cause and whether the
/// retained boot-trace record survived, both from the capture the bind crate
/// took as the first statement of `main`.
pub async fn record<F: Flash>(
    flash: &mut F,
    page: u32,
    mapped: &[u8],
    reset_reason: u32,
    retained: bool,
) -> Recorded {
    let plan = plan(mapped, reset_reason, retained);

    let encoded = Aligned(encode(&plan.record));
    let result = async {
        if plan.erase_first {
            flash.erase(page, page + PAGE_SIZE as u32).await?;
        }
        flash.write(page + plan.offset as u32, &encoded.0).await
    }
    .await;

    Recorded {
        record: plan.record,
        since_erase: plan.since_erase,
        persisted: result.is_ok(),
    }
}

/// 4-byte-aligned record buffer. A NOR part programs whole words, and the
/// nRF52840's NVMC reads the source through a `*const u32`
/// (`embassy_nrf::nvmc`), so the buffer a backend is handed is aligned.
#[repr(align(4))]
struct Aligned([u8; leviculum_boot_count::RECORD_SIZE]);

/// Emit the `BOOT_COUNT` line:
///
/// ```text
/// BOOT_COUNT n=42 reset_reason=0x00000004 retained=1 since_erase=17 t=118
/// ```
///
/// Ungated (`log_fmt_critical`, like `[QSPI]` and `[MEDIA]`): the drain
/// gate stays shut until DTR-assert or 30 s of uptime, and a field board
/// on battery has no host to open it — which is precisely the run whose
/// restart count is worth keeping. Shape frozen in
/// `docs/src/structured-event-logs.md` and byte-pinned by
/// [`leviculum_boot_count`]'s host tests.
///
/// Call after `usb::init`, beside `BOOT_TRACE` and `[RESET_REASON]`; the
/// three are read together.
pub fn log_banner(recorded: &Recorded) {
    crate::log::log_fmt_critical(
        "",
        format_args!(
            "{}",
            BootCountLine {
                record: recorded.record,
                since_erase: recorded.since_erase,
            }
        ),
    );
    if !recorded.persisted {
        // The number on the line above is what this boot IS; it is just
        // not what the next boot will read, so say so rather than let a
        // repeated `n=` look like a board that stopped counting.
        crate::log::log_fmt_critical(
            "",
            format_args!("BOOT_COUNT_WRITE_FAILED n={}", recorded.record.boot),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{ready, FakeFlash};

    const PAGE: u32 = 0;

    fn boot(flash: &mut FakeFlash, reset_reason: u32) -> Recorded {
        let mapped = flash.bytes[..PAGE_SIZE].to_vec();
        ready(record(flash, PAGE, &mapped, reset_reason, true))
    }

    #[test]
    fn an_erased_page_counts_the_first_boot_and_the_next_one_follows() {
        let mut flash = FakeFlash::new(1);
        let first = boot(&mut flash, 0x4);
        assert!(first.persisted);
        assert_eq!(first.record.boot, 1);
        assert_eq!(first.since_erase, 1);
        let second = boot(&mut flash, 0x1);
        assert_eq!(second.record.boot, 2);
        assert_eq!(second.record.reset_reason, 0x1);
        assert_eq!(second.since_erase, 2);
    }

    #[test]
    fn a_full_page_is_erased_and_the_number_carries_over() {
        let mut flash = FakeFlash::new(1);
        let slots = (PAGE_SIZE / leviculum_boot_count::RECORD_SIZE) as u32;
        for n in 1..=slots {
            assert_eq!(boot(&mut flash, 0).record.boot, n);
        }
        let wrapped = boot(&mut flash, 0);
        assert!(wrapped.persisted);
        assert_eq!(wrapped.record.boot, slots + 1);
        assert_eq!(wrapped.since_erase, 1);
    }
}
