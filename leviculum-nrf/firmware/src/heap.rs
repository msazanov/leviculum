//! The board's heap: the global allocator and the two facts the
//! orchestration reads from it, the instantaneous (used, free) pair and the
//! high-watermark since boot (Codeberg #65), with the periodic `[HEAP]` line
//! that carries both into the log (#469 961: moved from `leviculum-nrf`'s
//! `lib.rs`; `alloc` is this stratum's knowledge by the page's table).
//!
//! What stays below is where the pool lies: the bind crate owns the memory
//! the allocator manages and hands it over once, through [`init`], before
//! the first allocation. The pool's size is the boot heap budget's
//! ([`leviculum_heap_budget::HEAP_SIZE`]), the same constant the census
//! assertion sums against.
//!
//! # The allocator only on the target
//!
//! `#[global_allocator]` is attached on `target_os = "none"` alone, which is
//! every board image and never a host build. A host test binary of this
//! crate, or of any host crate that ever links it, keeps the system
//! allocator: the static here would otherwise serve the test harness's first
//! allocation before anything could call [`init`]. `target_os` names the
//! absence of an operating system, not a chip, so the orchestration stays
//! chip-blind. The static itself exists on both, which is what lets a host
//! test drive the watermark through a real `LlffHeap` over a buffer of its
//! own.

use core::sync::atomic::{AtomicUsize, Ordering};
use embedded_alloc::LlffHeap as Heap;
use leviculum_heap_budget::HEAP_SIZE;

#[cfg_attr(target_os = "none", global_allocator)]
static HEAP: Heap = Heap::empty();

/// Hand the allocator its pool.
///
/// # Safety
///
/// Call exactly once, before the first allocation, with `start..start +
/// size` a region of RAM that nothing else reads or writes for the rest of
/// the program; the same contract as `embedded_alloc::LlffHeap::init`,
/// which this forwards to.
pub unsafe fn init(start: usize, size: usize) {
    // SAFETY: forwarded from this function's contract.
    unsafe { HEAP.init(start, size) }
}

/// Return (used, free) heap bytes at this instant.
pub fn heap_stats() -> (usize, usize) {
    (HEAP.used(), HEAP.free())
}

/// High-watermark of heap usage since boot (bytes). Updated by
/// [`heap_watermark_sample`]; resets with every boot — the periodic
/// `[HEAP]` log lines (which land in the persistent tail) carry the
/// history across resets. Codeberg #65 instrumentation.
static HEAP_WATERMARK: AtomicUsize = AtomicUsize::new(0);

/// Sample the heap, update the high-watermark, and return
/// `(used, free, watermark)`.
pub fn heap_watermark_sample() -> (usize, usize, usize) {
    let (used, free) = heap_stats();
    let watermark = HEAP_WATERMARK.fetch_max(used, Ordering::Relaxed).max(used);
    (used, free, watermark)
}

/// Periodic heap telemetry (Codeberg #65): one `[HEAP]` line every 30 s
/// into the normal log path, which feeds both the USB debug capture and
/// the persistent tail in `.uninit` — so an OOM panic's post-mortem boot
/// replays the heap trajectory leading up to it. Strictly additive: the
/// cadence is coarse and the sampling is two atomic loads; the alloc
/// path itself is untouched. The bind crate's executor task runs it.
pub async fn watermark_loop() -> ! {
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(30)).await;
        let (used, free, watermark) = heap_watermark_sample();
        crate::log::log_fmt(
            "[HEAP] ",
            format_args!("used={used} free={free} watermark={watermark} size={HEAP_SIZE}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::alloc::{GlobalAlloc, Layout};

    /// One test, because the heap and the watermark are one static per test
    /// binary and the allocator takes its pool once.
    #[test]
    fn the_watermark_holds_the_peak_after_the_heap_shrinks() {
        assert_eq!(heap_stats(), (0, 0), "an uninitialised heap has no pool");

        static mut POOL: [u8; 4096] = [0; 4096];
        // SAFETY: the one init of this binary's HEAP, over a pool nothing
        // else names.
        unsafe { init(core::ptr::addr_of_mut!(POOL) as usize, 4096) };
        let (used0, free0) = heap_stats();
        // The allocator keeps a few bytes of the pool for its own hole list,
        // so the pair sums to a fixed total just under the pool.
        let total = used0 + free0;
        assert!(
            total > 4000 && total <= 4096,
            "the pool is the heap: {total}"
        );

        let layout = Layout::from_size_align(1024, 8).unwrap();
        // SAFETY: a non-zero layout, freed below with the same layout.
        let block = unsafe { HEAP.alloc(layout) };
        assert!(!block.is_null());
        let (used1, free1, mark1) = heap_watermark_sample();
        assert!(used1 >= used0 + 1024, "the block is counted as used");
        assert_eq!(used1 + free1, total);
        assert_eq!(mark1, used1, "the watermark rises with the heap");

        // SAFETY: `block` came from this allocator with this layout.
        unsafe { HEAP.dealloc(block, layout) };
        let (used2, _, mark2) = heap_watermark_sample();
        assert_eq!(used2, used0, "the block is returned");
        assert_eq!(mark2, mark1, "the watermark keeps the peak");
    }
}
