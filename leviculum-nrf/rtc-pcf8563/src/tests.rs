use super::*;

// Unix fixtures from python3 calendar.timegm, independent of this crate's arithmetic.
const Y2000: u64 = 946_684_800;
const LEAP_2000: u64 = 951_825_600; // 2000-02-29T12:00:00Z
const LEAP_2024_END: u64 = 1_709_251_199; // 2024-02-29T23:59:59Z
const END_2099: u64 = 4_102_444_799; // 2099-12-31T23:59:59Z
const START_2100: u64 = 4_102_444_800; // 2100-01-01T00:00:00Z
const MARCH_2100: u64 = 4_107_542_400; // 2100-03-01T00:00:00Z
const BENCH: u64 = 1_792_417_507; // 2026-10-19T13:45:07Z, a Monday

#[test]
fn bcd_round_trips_and_rejects_bad_nibbles() {
    for v in 0..=99u8 {
        assert_eq!(bcd_decode(bcd_encode(v)), Some(v));
    }
    assert_eq!(bcd_encode(59), 0x59);
    assert_eq!(bcd_decode(0x0A), None);
    assert_eq!(bcd_decode(0xA0), None);
    assert_eq!(bcd_decode(0xFF), None);
}

#[test]
fn range_constants_are_the_calendar_edges() {
    assert_eq!(MIN_UNIX, Y2000);
    assert_eq!(
        DateTime::from_unix(MAX_UNIX),
        Ok(DateTime {
            year: 2199,
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 59
        })
    );
    assert_eq!(
        DateTime::from_unix(MIN_UNIX - 1),
        Err(OutOfRange(MIN_UNIX - 1))
    );
    assert_eq!(
        DateTime::from_unix(MAX_UNIX + 1),
        Err(OutOfRange(MAX_UNIX + 1))
    );
    assert!(write_buffer(0).is_err());
}

#[test]
fn a_known_time_encodes_to_the_datasheet_layout() {
    let regs = encode(&DateTime::from_unix(BENCH).unwrap());
    assert_eq!(regs, [0x07, 0x45, 0x13, 0x19, 1, 0x10, 0x26]);
    assert_eq!(decode_unix(&regs), Ok(BENCH));
    assert_eq!(
        write_buffer(BENCH),
        Ok([REG_SECONDS, 0x07, 0x45, 0x13, 0x19, 1, 0x10, 0x26])
    );
}

#[test]
fn every_fixture_round_trips() {
    for unix in [
        Y2000,
        LEAP_2000,
        LEAP_2024_END,
        END_2099,
        START_2100,
        MARCH_2100,
        BENCH,
        MAX_UNIX,
    ] {
        let regs = encode(&DateTime::from_unix(unix).unwrap());
        assert_eq!(decode_unix(&regs), Ok(unix), "{unix}");
    }
}

#[test]
fn round_trips_every_day_of_both_centuries() {
    let mut unix = MIN_UNIX + 3_723; // 01:02:03 each day
    while unix <= MAX_UNIX {
        let t = DateTime::from_unix(unix).unwrap();
        assert_eq!(t.to_unix(), unix);
        assert_eq!(decode(&encode(&t)), Ok(t));
        unix += 86_400;
    }
}

#[test]
fn leap_days_follow_the_gregorian_rule() {
    assert!(is_leap(2000) && is_leap(2024) && !is_leap(2100) && !is_leap(2026));
    let t = DateTime::from_unix(LEAP_2000).unwrap();
    assert_eq!((t.year, t.month, t.day, t.hour), (2000, 2, 29, 12));
    let t = DateTime::from_unix(LEAP_2024_END + 1).unwrap();
    assert_eq!((t.year, t.month, t.day), (2024, 3, 1));
    // 2100 is not a leap year: Feb 28 is followed by Mar 1.
    let t = DateTime::from_unix(MARCH_2100 - 1).unwrap();
    assert_eq!((t.year, t.month, t.day), (2100, 2, 28));
    // 29 Feb 2100 is not a date.
    let regs = [0x00, 0x00, 0x00, 0x29, 0, CENTURY_BIT | 0x02, 0x00];
    assert_eq!(decode(&regs), Err(DecodeError::OutOfRange { reg: 0x05 }));
    let regs = [0x00, 0x00, 0x00, 0x29, 0, 0x02, 0x00];
    assert_eq!(decode_unix(&regs), Ok(LEAP_2000 - 12 * 3600));
}

#[test]
fn the_century_bit_carries_2099_into_2100() {
    let end = encode(&DateTime::from_unix(END_2099).unwrap());
    assert_eq!(end[5] & CENTURY_BIT, 0);
    assert_eq!(end[6], 0x99);
    let start = encode(&DateTime::from_unix(START_2100).unwrap());
    assert_eq!(start[5], CENTURY_BIT | 0x01);
    assert_eq!(start[6], 0x00);
    assert_eq!(start[4], 5); // 2100-01-01 is a Friday
    assert_eq!(decode_unix(&start), Ok(START_2100));
    // Same registers without C are a century earlier.
    let mut no_c = start;
    no_c[5] &= !CENTURY_BIT;
    assert_eq!(decode_unix(&no_c), Ok(Y2000));
}

#[test]
fn voltage_low_is_invalid_whatever_the_time() {
    let mut regs = encode(&DateTime::from_unix(BENCH).unwrap());
    regs[0] |= VL_BIT;
    assert_eq!(decode(&regs), Err(DecodeError::VoltageLow));
    assert_eq!(decode_unix(&regs), Err(DecodeError::VoltageLow));
    assert_eq!(DecodeError::VoltageLow.to_string(), "vl=1");
}

#[test]
fn a_write_clears_voltage_low() {
    for unix in [MIN_UNIX, BENCH, MAX_UNIX] {
        assert_eq!(write_buffer(unix).unwrap()[1] & VL_BIT, 0);
    }
}

#[test]
fn unused_bits_are_ignored_on_read() {
    // Unused bits read as undefined per the datasheet.
    let mut regs = encode(&DateTime::from_unix(BENCH).unwrap());
    regs[1] |= 0x80;
    regs[2] |= 0xC0;
    regs[3] |= 0xC0;
    regs[4] = 0xFF;
    regs[5] |= 0x60;
    assert_eq!(decode_unix(&regs), Ok(BENCH));
}

#[test]
fn bad_registers_name_their_address() {
    let good = encode(&DateTime::from_unix(BENCH).unwrap());
    let cases: [(usize, u8, DecodeError); 7] = [
        (0, 0x5A, DecodeError::BadBcd { reg: 0x02 }),
        (0, 0x60, DecodeError::OutOfRange { reg: 0x02 }),
        (1, 0x60, DecodeError::OutOfRange { reg: 0x03 }),
        (2, 0x24, DecodeError::OutOfRange { reg: 0x04 }),
        (3, 0x00, DecodeError::OutOfRange { reg: 0x05 }),
        (5, 0x13, DecodeError::OutOfRange { reg: 0x07 }),
        (6, 0x9A, DecodeError::BadBcd { reg: 0x08 }),
    ];
    for (i, v, err) in cases {
        let mut regs = good;
        regs[i] = v;
        assert_eq!(decode(&regs), Err(err), "reg {i} = {v:#04x}");
    }
    let mut regs = good;
    regs[3] = 0x31; // 31 October is fine, 31 September is not
    regs[5] = 0x09;
    assert_eq!(decode(&regs), Err(DecodeError::OutOfRange { reg: 0x05 }));
    assert_eq!(
        DecodeError::BadBcd { reg: 0x08 }.to_string(),
        "bcd reg=0x08"
    );
}

#[test]
fn weekdays_count_from_sunday() {
    assert_eq!(DateTime::from_unix(Y2000).unwrap().weekday(), 6);
    assert_eq!(DateTime::from_unix(BENCH).unwrap().weekday(), 1);
    assert_eq!(DateTime::from_unix(START_2100).unwrap().weekday(), 5);
}
