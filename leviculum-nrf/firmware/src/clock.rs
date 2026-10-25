//! Embassy-backed monotonic clock for leviculum-core, and the calendar
//! source the board reports beside it (#469, 958): the Clock seam's
//! calendar-source rank, mirrored for the banner and the telemetry report.

use core::sync::atomic::{AtomicU8, Ordering};

use leviculum_core::traits::Clock;

/// Monotonic clock using Embassy's RTC1 timer driver.
pub struct EmbassyClock;

impl Clock for EmbassyClock {
    fn now_ms(&self) -> u64 {
        embassy_time::Instant::now().as_millis()
    }
}

/// The Clock seam's monotonic half (976), the same reading as core's.
impl crate::seams::Clock for EmbassyClock {
    fn now_ms(&self) -> u64 {
        embassy_time::Instant::now().as_millis()
    }

    fn wait_until(&self, at_ms: u64) -> impl core::future::Future<Output = ()> {
        embassy_time::Timer::at(embassy_time::Instant::from_millis(at_ms))
    }
}

/// Current time source of the emission timebase (Codeberg #166 item 3),
/// mirrored out of the main loop so the periodic banner task can state it
/// beside `[FW_BUILD]` without owning the node. Encodes
/// `leviculum_core::transport::TimeSource`; starts at the build floor, the
/// birth state of every boot (#247).
static TIME_SOURCE_STATE: AtomicU8 = AtomicU8::new(TIME_SOURCE_BUILD_FLOOR);

const TIME_SOURCE_GNSS: u8 = 0;
const TIME_SOURCE_HOST: u8 = 1;
const TIME_SOURCE_OVERHEARD: u8 = 2;
const TIME_SOURCE_BUILD_FLOOR: u8 = 3;
const TIME_SOURCE_PLATFORM_CLOCK: u8 = 4;
const TIME_SOURCE_RESTORED_FLOOR: u8 = 5;

/// Record the time source for the status banner. Returns whether the
/// recorded source CHANGED, so a caller that runs on every pass can log the
/// transition once instead of on every pass.
///
/// Called at each seeding site, and — since Codeberg #398 — from the main
/// loop as an unconditional mirror of `node.time_source()`. The seeding
/// sites are not enough on their own: `Transport::learn_emission_timebase`
/// seats an anchor of its own when an overheard announce carries a
/// plausible emission stamp, and nothing at this layer is called when it
/// does, so a board healed from traffic kept reporting the birth state and
/// `[TIME_SOURCE]` said the same word for a board announcing on cadence as
/// for one withholding its announce for want of a clock.
pub fn set_time_source(source: leviculum_core::transport::TimeSource) -> bool {
    use leviculum_core::transport::TimeSource;
    let v = match source {
        TimeSource::Gnss => TIME_SOURCE_GNSS,
        TimeSource::Host => TIME_SOURCE_HOST,
        TimeSource::Overheard => TIME_SOURCE_OVERHEARD,
        TimeSource::BuildFloor => TIME_SOURCE_BUILD_FLOOR,
        TimeSource::RestoredFloor => TIME_SOURCE_RESTORED_FLOOR,
        // A board reports a platform wall clock only if its bind crate gives
        // `EmbassyClock` one; today none does (the trait default of None),
        // so this arm is unreachable. It is spelled out rather than
        // wildcarded so a board that grows an RTC fails to compile until it
        // decides what to report.
        TimeSource::PlatformClock => TIME_SOURCE_PLATFORM_CLOCK,
    };
    TIME_SOURCE_STATE.swap(v, Ordering::Relaxed) != v
}

/// The recorded time source as its stable event-log token
/// (`TimeSource::as_str`).
pub fn time_source_str() -> &'static str {
    use leviculum_core::transport::TimeSource;
    match TIME_SOURCE_STATE.load(Ordering::Relaxed) {
        TIME_SOURCE_GNSS => TimeSource::Gnss.as_str(),
        TIME_SOURCE_HOST => TimeSource::Host.as_str(),
        TIME_SOURCE_OVERHEARD => TimeSource::Overheard.as_str(),
        TIME_SOURCE_PLATFORM_CLOCK => TimeSource::PlatformClock.as_str(),
        TIME_SOURCE_RESTORED_FLOOR => TimeSource::RestoredFloor.as_str(),
        _ => TimeSource::BuildFloor.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::transport::TimeSource;

    /// The `[TIME_SOURCE]` line and the telemetry report print
    /// `time_source_str()`, so every source the store accepts has to come
    /// back as its own `TimeSource::as_str` token, the birth state included,
    /// and the store has to say when it moved (the main loop's mirror logs
    /// on that). One test, because the store is one static.
    #[test]
    fn every_source_round_trips_to_its_token_and_reports_the_change() {
        assert_eq!(time_source_str(), TimeSource::BuildFloor.as_str());
        let all = [
            TimeSource::Gnss,
            TimeSource::Host,
            TimeSource::PlatformClock,
            TimeSource::Overheard,
            TimeSource::BuildFloor,
            TimeSource::RestoredFloor,
        ];
        for source in all {
            set_time_source(source);
            assert_eq!(time_source_str(), source.as_str());
            assert!(!set_time_source(source), "{source:?} again is no change");
        }
        assert!(set_time_source(TimeSource::Gnss));
    }
}
