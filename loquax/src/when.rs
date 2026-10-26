//! Wall-clock instants as the screen says them: `14:05` today, `Fri` this
//! week, `22 Oct` before that, and the date lines between days
//! (`docs/src/concepts/loquax-keys-and-layout.md`, section 5).
//!
//! The core has no clock and no time zone. The shell hands the model a
//! [`WallTime`], an instant with the UTC offset in force at it, and everything
//! here is arithmetic on that pair, so a test or a key script pins the time
//! of day exactly. The civil date is Howard Hinnant's `civil_from_days`, as in
//! `lnflash` and `lblogd`, which keep their own copies outside this crate's
//! graph.

use alloc::format;
use alloc::string::String;

/// An instant and the UTC offset in force at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallTime {
    /// Seconds since 1970-01-01 00:00 UTC.
    pub unix_secs: i64,
    /// Seconds east of UTC.
    pub offset_secs: i32,
}

/// A local date and time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    /// 1 to 12.
    pub month: u32,
    /// 1 to 31.
    pub day: u32,
    /// 0 is Monday, 6 Sunday.
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

impl WallTime {
    /// The local day this instant falls on, counted from 1970-01-01.
    pub fn day(&self) -> i64 {
        self.local_secs().div_euclid(86_400)
    }

    fn local_secs(&self) -> i64 {
        self.unix_secs + i64::from(self.offset_secs)
    }

    /// The local date and time of day.
    pub fn civil(&self) -> Civil {
        let local = self.local_secs();
        let days = local.div_euclid(86_400);
        let secs = local.rem_euclid(86_400) as u32;
        let (year, month, day) = civil_from_days(days);
        Civil {
            year,
            month,
            day,
            // 1970-01-01 was a Thursday.
            weekday: (days + 3).rem_euclid(7) as u32,
            hour: secs / 3600,
            minute: secs / 60 % 60,
            second: secs % 60,
        }
    }

    /// The instant at a local date and time of day, at `offset_secs`: what a
    /// key script's `clock` step names.
    pub fn from_civil(
        year: i64,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        offset_secs: i32,
    ) -> Self {
        let days = days_from_civil(year, month, day);
        let local = days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60;
        Self {
            unix_secs: local - i64::from(offset_secs),
            offset_secs,
        }
    }

    /// `14:05`.
    pub fn hh_mm(&self) -> String {
        let c = self.civil();
        format!("{:02}:{:02}", c.hour, c.minute)
    }

    /// `14:05:09`, the detail view's ledger.
    pub fn hh_mm_ss(&self) -> String {
        let c = self.civil();
        format!("{:02}:{:02}:{:02}", c.hour, c.minute, c.second)
    }
}

/// The conversation list's time of a message: `14:05` today, the weekday
/// (`Fri`) within the six days before, else the date (`22 Oct`), with the
/// year when it is not this one.
pub fn list_time(at: &WallTime, now: &WallTime) -> String {
    let ago = now.day() - at.day();
    let c = at.civil();
    match ago {
        0 => at.hh_mm(),
        1..=6 => WEEKDAYS[c.weekday as usize][..3].into(),
        _ if c.year == now.civil().year => {
            format!("{} {}", c.day, &MONTHS[c.month as usize - 1][..3])
        }
        _ => format!(
            "{} {} {}",
            c.day,
            &MONTHS[c.month as usize - 1][..3],
            c.year
        ),
    }
}

/// The label of the date line before a day's first message: `Today`, else
/// the weekday and the date (`Saturday 24 October`), with the year when it is
/// not this one.
pub fn date_label(at: &WallTime, now: &WallTime) -> String {
    if at.day() == now.day() {
        return "Today".into();
    }
    let c = at.civil();
    let date = format!(
        "{} {} {}",
        WEEKDAYS[c.weekday as usize],
        c.day,
        MONTHS[c.month as usize - 1]
    );
    match c.year == now.civil().year {
        true => date,
        false => format!("{date} {}", c.year),
    }
}

/// Year, month (1 to 12) and day (1 to 31) of the `days`th day after
/// 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The inverse of [`civil_from_days`].
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u32, hour: u32, minute: u32) -> WallTime {
        WallTime::from_civil(2026, 10, day, hour, minute, 3600)
    }

    #[test]
    fn civil_dates_round_trip_and_know_their_weekday() {
        let t = WallTime::from_civil(2026, 10, 24, 13, 58, 7200);
        let c = t.civil();
        assert_eq!(
            (c.year, c.month, c.day, c.hour, c.minute),
            (2026, 10, 24, 13, 58)
        );
        assert_eq!(WEEKDAYS[c.weekday as usize], "Saturday");
        // 2026-10-24 11:58 UTC.
        assert_eq!(t.unix_secs, 1_792_843_080);
        assert_eq!(t.hh_mm(), "13:58");
        let leap = WallTime::from_civil(2024, 2, 29, 0, 0, 0).civil();
        assert_eq!((leap.month, leap.day), (2, 29));
        let epoch = WallTime::from_civil(1970, 1, 1, 0, 0, 0);
        assert_eq!(epoch.unix_secs, 0);
    }

    #[test]
    fn the_offset_moves_the_day_not_the_instant() {
        // 23:30 UTC on the 25th is the 26th an hour east.
        let utc = WallTime::from_civil(2026, 10, 25, 23, 30, 0);
        let east = WallTime {
            unix_secs: utc.unix_secs,
            offset_secs: 3600,
        };
        assert_eq!(utc.day() + 1, east.day());
        assert_eq!(east.hh_mm(), "00:30");
    }

    #[test]
    fn the_list_says_the_time_today_the_weekday_this_week_else_the_date() {
        let now = at(26, 9, 0);
        assert_eq!(list_time(&at(26, 8, 5), &now), "08:05");
        assert_eq!(list_time(&at(23, 19, 40), &now), "Fri");
        assert_eq!(list_time(&at(20, 12, 0), &now), "Tue");
        assert_eq!(list_time(&at(19, 12, 0), &now), "19 Oct");
        let last_year = WallTime::from_civil(2025, 12, 31, 12, 0, 3600);
        assert_eq!(list_time(&last_year, &now), "31 Dec 2025");
    }

    #[test]
    fn the_date_line_says_today_else_the_weekday_and_the_date() {
        let now = at(26, 9, 0);
        assert_eq!(date_label(&at(26, 0, 1), &now), "Today");
        assert_eq!(date_label(&at(24, 13, 58), &now), "Saturday 24 October");
        let last_year = WallTime::from_civil(2025, 12, 31, 12, 0, 3600);
        assert_eq!(date_label(&last_year, &now), "Wednesday 31 December 2025");
    }
}
