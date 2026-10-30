//! NXP PCF8563 real-time clock: the time-register codec, without I/O.
//!
//! The ThinkNode M9 carries a PCF8563 on I2C at [`ADDRESS`]. The seven time
//! registers start at [`REG_SECONDS`] and are read or written in one burst:
//!
//! ```text
//! 0x02 VL | seconds   (BCD 00-59, bit 7 = voltage low)
//! 0x03      minutes   (BCD 00-59)
//! 0x04      hours     (BCD 00-23)
//! 0x05      days      (BCD 01-31)
//! 0x06      weekday   (0-6, 0 = Sunday here)
//! 0x07 C  | months    (BCD 01-12, bit 7 = century)
//! 0x08      years     (BCD 00-99)
//! ```
//!
//! The voltage-low flag says the oscillator stopped since it was last
//! cleared, so the time is not trustworthy; [`decode_unix`] refuses it as
//! [`DecodeError::VoltageLow`]. Writing the seconds register clears it.
//!
//! The century bit toggles when the years register wraps from 99 to 00, so
//! this crate reads C = 0 as 20xx and C = 1 as 21xx, giving 2000-2199. The
//! weekday register is written for completeness and ignored on read.

#![cfg_attr(not(test), no_std)]

use core::fmt;

/// 7-bit I2C address.
pub const ADDRESS: u8 = 0x51;
/// Control/status 1; [`STOP_BIT`] halts the clock.
pub const REG_CONTROL_1: u8 = 0x00;
/// The STOP bit of [`REG_CONTROL_1`].
pub const STOP_BIT: u8 = 0x20;
/// First of the seven time registers.
pub const REG_SECONDS: u8 = 0x02;
/// Number of time registers.
pub const TIME_LEN: usize = 7;
/// Voltage-low flag, bit 7 of the seconds register.
pub const VL_BIT: u8 = 0x80;
/// Century flag, bit 7 of the months register.
pub const CENTURY_BIT: u8 = 0x80;

/// First second this codec represents: 2000-01-01T00:00:00Z.
pub const MIN_UNIX: u64 = 946_684_800;
/// Last second this codec represents: 2199-12-31T23:59:59Z.
pub const MAX_UNIX: u64 = 7_258_118_399;

/// Why a register burst does not yield a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// VL is set: the oscillator stopped, the time is invalid.
    VoltageLow,
    /// The register at this address holds a nibble above 9.
    BadBcd { reg: u8 },
    /// The register at this address decodes outside its calendar range.
    OutOfRange { reg: u8 },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VoltageLow => write!(f, "vl=1"),
            Self::BadBcd { reg } => write!(f, "bcd reg=0x{:02x}", reg),
            Self::OutOfRange { reg } => write!(f, "range reg=0x{:02x}", reg),
        }
    }
}

/// A unix time outside [`MIN_UNIX`]..=[`MAX_UNIX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfRange(pub u64);

/// A UTC calendar time in the codec's range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    /// 2000-2199.
    pub year: u16,
    /// 1-12.
    pub month: u8,
    /// 1-31.
    pub day: u8,
    /// 0-23.
    pub hour: u8,
    /// 0-59.
    pub minute: u8,
    /// 0-59.
    pub second: u8,
}

/// Binary 0-99 to packed BCD.
pub const fn bcd_encode(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

/// Packed BCD to binary, or `None` for a nibble above 9.
pub const fn bcd_decode(b: u8) -> Option<u8> {
    let (hi, lo) = (b >> 4, b & 0x0F);
    if hi > 9 || lo > 9 {
        None
    } else {
        Some(hi * 10 + lo)
    }
}

/// Whether `year` is a Gregorian leap year.
pub const fn is_leap(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// Days in `month` (1-12) of `year`.
pub const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a civil date (proleptic Gregorian, year >= 1).
const fn days_from_civil(year: u16, month: u8, day: u8) -> u64 {
    let y = if month <= 2 {
        year as u64 - 1
    } else {
        year as u64
    };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (month as u64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date for days since 1970-01-01.
const fn civil_from_days(days: u64) -> (u16, u8, u8) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + (month <= 2) as u64) as u16;
    (year, month, day)
}

impl DateTime {
    /// The calendar time of a unix second in range.
    pub const fn from_unix(unix: u64) -> Result<Self, OutOfRange> {
        if unix < MIN_UNIX || unix > MAX_UNIX {
            return Err(OutOfRange(unix));
        }
        let (year, month, day) = civil_from_days(unix / 86_400);
        let secs = unix % 86_400;
        Ok(Self {
            year,
            month,
            day,
            hour: (secs / 3600) as u8,
            minute: (secs / 60 % 60) as u8,
            second: (secs % 60) as u8,
        })
    }

    /// Unix seconds; the fields must be in range (as [`decode`] and [`Self::from_unix`] produce).
    pub const fn to_unix(&self) -> u64 {
        days_from_civil(self.year, self.month, self.day) * 86_400
            + self.hour as u64 * 3600
            + self.minute as u64 * 60
            + self.second as u64
    }

    /// Day of week, 0 = Sunday (1970-01-01 was a Thursday).
    pub const fn weekday(&self) -> u8 {
        ((days_from_civil(self.year, self.month, self.day) + 4) % 7) as u8
    }
}

fn field(regs: &[u8; TIME_LEN], i: usize, mask: u8, lo: u8, hi: u8) -> Result<u8, DecodeError> {
    let reg = REG_SECONDS + i as u8;
    let v = bcd_decode(regs[i] & mask).ok_or(DecodeError::BadBcd { reg })?;
    if v < lo || v > hi {
        return Err(DecodeError::OutOfRange { reg });
    }
    Ok(v)
}

/// Decode a burst read of the seven time registers; VL set is [`DecodeError::VoltageLow`].
pub fn decode(regs: &[u8; TIME_LEN]) -> Result<DateTime, DecodeError> {
    if regs[0] & VL_BIT != 0 {
        return Err(DecodeError::VoltageLow);
    }
    let second = field(regs, 0, 0x7F, 0, 59)?;
    let minute = field(regs, 1, 0x7F, 0, 59)?;
    let hour = field(regs, 2, 0x3F, 0, 23)?;
    let day = field(regs, 3, 0x3F, 1, 31)?;
    let month = field(regs, 5, 0x1F, 1, 12)?;
    let yy = field(regs, 6, 0xFF, 0, 99)?;
    let century: u16 = if regs[5] & CENTURY_BIT != 0 {
        2100
    } else {
        2000
    };
    let year = century + yy as u16;
    if day > days_in_month(year, month) {
        return Err(DecodeError::OutOfRange {
            reg: REG_SECONDS + 3,
        });
    }
    Ok(DateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

/// [`decode`] as unix seconds.
pub fn decode_unix(regs: &[u8; TIME_LEN]) -> Result<u64, DecodeError> {
    decode(regs).map(|t| t.to_unix())
}

/// The seven time registers for `t`, VL clear; `t` must be in range.
pub const fn encode(t: &DateTime) -> [u8; TIME_LEN] {
    let century = if t.year >= 2100 { CENTURY_BIT } else { 0 };
    [
        bcd_encode(t.second),
        bcd_encode(t.minute),
        bcd_encode(t.hour),
        bcd_encode(t.day),
        t.weekday(),
        century | bcd_encode(t.month),
        bcd_encode((t.year % 100) as u8),
    ]
}

/// The I2C write for `unix`: [`REG_SECONDS`] followed by the seven time registers.
pub const fn write_buffer(unix: u64) -> Result<[u8; TIME_LEN + 1], OutOfRange> {
    let t = match DateTime::from_unix(unix) {
        Ok(t) => t,
        Err(e) => return Err(e),
    };
    let regs = encode(&t);
    let mut out = [REG_SECONDS; TIME_LEN + 1];
    let mut i = 0;
    while i < TIME_LEN {
        out[i + 1] = regs[i];
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
