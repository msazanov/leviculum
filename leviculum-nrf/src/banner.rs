//! The build banner group and its cadence (911).
//!
//! `[FW_BUILD]`, `[TIME_SOURCE]`, `[MEDIA]`, `[NAME ]` and `[IDENTITY]`,
//! printed together so a debug-serial reader can learn the running
//! git_sha, the clock source, the carriers, the names and the identity
//! from one place. Every 5 s for the first two minutes after boot, where
//! every post-flash reader reads, then every 5 minutes; the rule and the
//! reader table are `leviculum-log-line`'s `banner` module, host-tested
//! there. A `b` byte on the debug port prints the group at once
//! (`usb::debug_reader_task`), so a reader of a long-running board never
//! has to wait for the slow period.
//!
//! The group reads its state live, so a runtime media or name change shows
//! up on the next banner.

use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use leviculum_log_line::banner::banner_delay_ms;

/// Set by the debug-port byte `b`; the banner task prints on it.
static BANNER_NOW: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Ask for the banner group now (debug port `b`).
pub fn request() {
    BANNER_NOW.signal(());
}

/// Print the group once.
pub fn emit(media_src: crate::media::Source) {
    crate::log_critical!("[FW_BUILD] {}", crate::FW_BUILD_STAMP);
    crate::log_critical!("[TIME_SOURCE] source={}", crate::time_source_str());
    crate::media::log_banner(media_src);
    crate::name::log_banner();
    leviculum_firmware::identity::log_banner();
}

/// The banner loop each board binary's `fw_build_banner` task runs: wait
/// out the delay the uptime calls for, or a `b` request, whichever comes
/// first, print, repeat. Uses the embassy time driver like the other
/// periodic tasks; no SD-reserved peripheral is touched.
pub async fn run(media_src: crate::media::Source) -> ! {
    loop {
        let delay = banner_delay_ms(Instant::now().as_millis());
        select(
            Timer::after(Duration::from_millis(delay)),
            BANNER_NOW.wait(),
        )
        .await;
        emit(media_src);
    }
}
