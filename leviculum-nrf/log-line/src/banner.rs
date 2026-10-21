//! When the build banner repeats (911).
//!
//! The banner group is `[FW_BUILD]`, `[TIME_SOURCE]`, `[MEDIA]`, `[NAME ]`
//! and `[IDENTITY]`, printed together by each board binary's
//! `fw_build_banner` task. Until 911 it repeated every 5 s for as long as a
//! board ran: 116 627 `[FW_BUILD]` lines in feld-t114's capture, 110 455 in
//! feld-pocket's, 25 143 in the T1000-E's (2026-10-21), each on the
//! ungated critical path and so in the persistent crash tail too.
//!
//! # Who reads it, and when
//!
//! Every reader that waits for the NEXT line reads right after a reset it
//! caused: lnflash's post-flash verification (`FRESH_BANNER_BUDGET`, 15 s),
//! the flash runners' read-back (`tools/fw-readback.sh`, three 8 s
//! windows), `flash-lnodes-from-head.sh` (three 8 s windows), the media
//! proof (6.5 s after the cell's reset). The reviewer's boot proof reads
//! the last banner off the running capture 45 s after the flash. All of
//! them land inside [`BANNER_FAST_S`], where the cadence is unchanged.
//!
//! The one reader that asks a board nobody reset is periculum's discovery
//! inventory, which gives each board 6.5 s to state its build. A slow
//! period cannot serve that, so the debug port takes a `b` byte that asks
//! for the group at once (`leviculum-usb-policy`'s `debug_commands`), and a
//! reader of a long-running board sends it instead of waiting.
//!
//! # The rule
//!
//! Every [`BANNER_FAST_PERIOD_S`] for the first [`BANNER_FAST_S`] of
//! uptime, then every [`BANNER_SLOW_S`]: 24 groups in the first two
//! minutes and 287 in the rest of a day, 311 a day where there were 17 280.

/// How long after boot the banner keeps its old 5 s cadence, in seconds.
/// Every reader that waits for the next banner after a reset reads inside
/// this: the longest is the boot proof, 45 s after its flash.
pub const BANNER_FAST_S: u64 = 120;

/// The banner period inside [`BANNER_FAST_S`], in seconds: the cadence the
/// post-flash readers' budgets are sized against (lnflash waits three
/// periods, `fw-readback.sh` reads 8 s windows).
pub const BANNER_FAST_PERIOD_S: u64 = 5;

/// The banner period after [`BANNER_FAST_S`], in seconds. Long enough that
/// the banner is a few hundred lines a day instead of seventeen thousand,
/// short enough that a capture attached to a running board without asking
/// (`b`) still learns the build within five minutes.
pub const BANNER_SLOW_S: u64 = 300;

/// How long to wait, from `uptime_ms`, before the next banner. A pure
/// function of uptime: the task asks it after every banner and at boot.
pub fn banner_delay_ms(uptime_ms: u64) -> u64 {
    if uptime_ms < BANNER_FAST_S * 1000 {
        BANNER_FAST_PERIOD_S * 1000
    } else {
        BANNER_SLOW_S * 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The banner instants a board prints over `horizon_ms` of uptime when
    /// every banner lands exactly when its delay runs out.
    fn schedule(horizon_ms: u64) -> std::vec::Vec<u64> {
        let mut at = std::vec::Vec::new();
        let mut now = 0;
        loop {
            now += banner_delay_ms(now);
            if now > horizon_ms {
                return at;
            }
            at.push(now);
        }
    }

    #[test]
    fn the_first_two_minutes_keep_the_five_second_cadence() {
        let fast: std::vec::Vec<u64> = schedule(BANNER_FAST_S * 1000);
        assert_eq!(fast.first(), Some(&5_000), "first banner 5 s after boot");
        assert_eq!(fast.len(), 24, "5, 10, … 120 s");
        assert!(fast.windows(2).all(|w| w[1] - w[0] == 5_000));
    }

    #[test]
    fn after_the_fast_window_the_banner_comes_every_five_minutes() {
        let all = schedule(24 * 3600 * 1000);
        let slow: std::vec::Vec<&u64> = all.iter().filter(|&&t| t > 120_000).collect();
        assert_eq!(
            *slow[0], 420_000,
            "the first slow banner, 300 s after the last fast one"
        );
        assert!(slow.windows(2).all(|w| w[1] - w[0] == 300_000));
    }

    #[test]
    fn the_boundary_belongs_to_the_slow_period() {
        assert_eq!(banner_delay_ms(0), 5_000);
        assert_eq!(banner_delay_ms(BANNER_FAST_S * 1000 - 1), 5_000);
        assert_eq!(banner_delay_ms(BANNER_FAST_S * 1000), 300_000);
        assert_eq!(banner_delay_ms(u64::MAX), 300_000);
    }

    /// The line count the report predicts: 311 banner groups a day for a
    /// board that stays up all day, against 17 280 at a flat 5 s.
    #[test]
    fn a_day_of_uptime_prints_three_hundred_and_eleven_banners() {
        assert_eq!(schedule(24 * 3600 * 1000).len(), 311);
        assert_eq!(24 * 3600 / BANNER_FAST_PERIOD_S, 17_280);
    }
}
