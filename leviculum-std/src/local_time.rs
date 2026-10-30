//! The host's offset from UTC, for programs that show a person the time of
//! day: the messenger's message times and date lines.
//!
//! The C library's local time is the one source that reads the TZ database
//! the way the rest of the system does (`TZ`, else `/etc/localtime`), musl's
//! included, so a static binary agrees with `date`. It is asked per instant,
//! so a daylight saving change during a long session moves the offset with
//! it. lnflash keeps its own copy of the call (`lnflash/src/sys.rs`), being
//! outside this crate's dependents.

/// The UTC offset in force at `unix_secs`, in seconds east of UTC. `0` when
/// the conversion fails: the time is then shown as UTC, the same instant
/// written plainly.
#[cfg(unix)]
pub fn utc_offset_secs(unix_secs: i64) -> i32 {
    // The `time_t` deprecation warns about musl going 64-bit; this cast
    // already assumes the 64-bit width.
    #[allow(deprecated)]
    let t = unix_secs as libc::time_t;
    // SAFETY: `tm` is plain old data, all-zero is a valid value of it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call; `localtime_r` writes only
    // into `tm` and returns NULL on failure.
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return 0;
    }
    i32::try_from(tm.tm_gmtoff).unwrap_or(0)
}

/// Elsewhere there is no C local time to ask: UTC.
#[cfg(not(unix))]
pub fn utc_offset_secs(_unix_secs: i64) -> i32 {
    0
}

/// The wall clock in Unix seconds; `0` before 1970.
pub fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offset is a real one, within the fourteen hours either way any
    /// zone has, and the clock is past 2023.
    #[test]
    fn the_offset_is_within_the_zones_that_exist() {
        let offset = utc_offset_secs(unix_now_secs());
        assert!(offset.abs() <= 14 * 3600, "{offset}");
        assert!(unix_now_secs() > 1_700_000_000);
    }
}
