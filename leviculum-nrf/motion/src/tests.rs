//! Host tests: the init sequence against a fake register file, the sample
//! decoding against hand-computed register bytes, and the transition rule.

use super::*;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// Drive a future that never actually waits: the fake bus is ready at once.
fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("the fake bus never returns Pending"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    Read(u8, usize),
    Write(u8, u8),
    Delay(u32),
}

/// A QMA6100P as a register file: writes land, reads return what is there,
/// and the soft-reset command restores the datasheet defaults of the two
/// registers this driver touches.
struct FakeBus {
    regs: [u8; 0x40],
    ops: Vec<Op>,
    /// Fail every transaction (nothing on the bus).
    absent: bool,
    /// Fail the transaction with this index (0-based, delays excluded).
    fail_at: Option<usize>,
    /// Ignore writes to this register (a part that keeps its own value).
    sticky: Option<u8>,
    transactions: usize,
}

impl FakeBus {
    fn qma() -> Self {
        let mut regs = [0u8; 0x40];
        regs[REG_CHIP_ID as usize] = CHIP_ID;
        // Datasheet register table defaults, as a part left by another
        // firmware would NOT have them: start dirty so the reset matters.
        regs[REG_FSR as usize] = 0x0F;
        regs[REG_PM as usize] = 0x00;
        Self {
            regs,
            ops: Vec::new(),
            absent: false,
            fail_at: None,
            sticky: None,
            transactions: 0,
        }
    }

    fn check(&mut self) -> Result<(), BusError> {
        let n = self.transactions;
        self.transactions += 1;
        if self.absent || self.fail_at == Some(n) {
            return Err(BusError);
        }
        Ok(())
    }
}

impl RegisterBus for FakeBus {
    async fn write_reg(&mut self, reg: u8, value: u8) -> Result<(), BusError> {
        self.ops.push(Op::Write(reg, value));
        self.check()?;
        if reg == REG_SOFT_RESET && value == SOFT_RESET_CMD {
            self.regs[REG_FSR as usize] = 0x00;
            self.regs[REG_PM as usize] = 0x00;
        }
        if self.sticky != Some(reg) {
            self.regs[reg as usize] = value;
        }
        Ok(())
    }

    async fn read_regs(&mut self, reg: u8, buf: &mut [u8]) -> Result<(), BusError> {
        self.ops.push(Op::Read(reg, buf.len()));
        self.check()?;
        let start = reg as usize;
        buf.copy_from_slice(&self.regs[start..start + buf.len()]);
        Ok(())
    }

    async fn delay_ms(&mut self, ms: u32) {
        self.ops.push(Op::Delay(ms));
    }
}

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

#[test]
fn init_sequence_is_identify_reset_range_active_readback() {
    let mut bus = FakeBus::qma();
    assert_eq!(block_on(init(&mut bus)), Ok(()));
    assert_eq!(
        bus.ops,
        vec![
            Op::Read(0x00, 1),
            Op::Write(0x36, 0xB6),
            Op::Delay(10),
            Op::Write(0x36, 0x00),
            Op::Delay(20),
            Op::Write(0x0F, 0x01),
            Op::Write(0x11, 0x80),
            Op::Read(0x0F, 1),
            Op::Read(0x11, 1),
        ]
    );
    assert_eq!(bus.regs[0x0F], 0x01, "2 g range");
    assert_eq!(bus.regs[0x11], 0x80, "MODE_BIT set, active");
}

#[test]
fn nothing_on_the_bus_is_absent_after_one_read() {
    let mut bus = FakeBus::qma();
    bus.absent = true;
    assert_eq!(block_on(init(&mut bus)), Err(InitError::NoAnswer));
    assert_eq!(
        bus.ops,
        vec![Op::Read(0x00, 1)],
        "no write to an absent part"
    );
    assert_eq!(
        InitLine(Err(InitError::NoAnswer)).to_string(),
        "absent src=qma6100p reason=no-answer"
    );
}

#[test]
fn another_chip_at_the_address_is_absent_and_untouched() {
    let mut bus = FakeBus::qma();
    bus.regs[0x00] = 0x33;
    assert_eq!(block_on(init(&mut bus)), Err(InitError::WrongChip(0x33)));
    assert_eq!(
        bus.ops,
        vec![Op::Read(0x00, 1)],
        "never reset a foreign part"
    );
    assert_eq!(
        InitLine(Err(InitError::WrongChip(0x33))).to_string(),
        "absent src=qma6100p reason=chip-id-0x33"
    );
}

#[test]
fn a_failure_after_the_id_is_a_bus_error() {
    let mut bus = FakeBus::qma();
    bus.fail_at = Some(1); // the soft-reset write
    assert_eq!(block_on(init(&mut bus)), Err(InitError::Bus));
    assert_eq!(InitError::Bus.to_string(), "bus-error");
}

#[test]
fn a_register_that_does_not_keep_its_value_is_a_readback_failure() {
    let mut bus = FakeBus::qma();
    bus.sticky = Some(REG_PM);
    assert_eq!(
        block_on(init(&mut bus)),
        Err(InitError::Readback {
            reg: 0x11,
            got: 0x00
        })
    );
    assert_eq!(
        InitLine(Err(InitError::Readback {
            reg: 0x11,
            got: 0x00
        }))
        .to_string(),
        "absent src=qma6100p reason=readback-0x11-0x00"
    );
}

#[test]
fn ok_line() {
    assert_eq!(InitLine(Ok(())).to_string(), "ok src=qma6100p");
}

// ---------------------------------------------------------------------------
// Samples
// ---------------------------------------------------------------------------

#[test]
fn sample_decodes_fourteen_bit_twos_complement() {
    // x = +1 g at 2 g = 4096 LSB: 4096 << 2 = 0x4000 -> lo 0x00|NEWDATA, hi 0x40.
    // y = -1 LSB: -1 << 2 = 0xFFFC -> lo 0xFC (NEWDATA clear), hi 0xFF.
    // z = -8192 (most negative): 0x8000 -> lo 0x01 (NEWDATA), hi 0x80.
    let s = Sample::from_regs(&[0x01, 0x40, 0xFC, 0xFF, 0x01, 0x80]);
    assert_eq!(
        s,
        Sample {
            x: 4096,
            y: -1,
            z: -8192
        }
    );
}

#[test]
fn read_sample_reads_six_bytes_from_dxl() {
    let mut bus = FakeBus::qma();
    bus.regs[0x01..0x07].copy_from_slice(&[0x00, 0x40, 0x00, 0x00, 0x00, 0x00]);
    let s = block_on(read_sample(&mut bus)).expect("fake bus answers");
    assert_eq!(s.x, 4096);
    assert_eq!(bus.ops, vec![Op::Read(0x01, 6)]);
}

#[test]
fn threshold_is_thirty_milli_g_in_counts() {
    assert_eq!(MOTION_THRESHOLD_COUNTS, 122);
}

// ---------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------

const REST: Sample = Sample {
    x: 0,
    y: 0,
    z: 4096,
};

fn nudged(by: i16) -> Sample {
    Sample {
        x: REST.x + by,
        ..REST
    }
}

/// Feed `REST` every poll from `from` to `to` (exclusive), collecting lines.
fn rest(det: &mut MotionDetector, from: u64, to: u64) -> Vec<Transition> {
    (from..to)
        .step_by(POLL_INTERVAL_MS as usize)
        .filter_map(|t| det.on_sample(REST, t))
        .collect()
}

#[test]
fn one_sample_over_the_threshold_is_moving() {
    let mut det = MotionDetector::new(0);
    assert_eq!(det.on_sample(REST, 250), None, "first sample has no delta");
    let t = det.on_sample(nudged(123), 500).expect("over threshold");
    assert_eq!(
        t,
        Transition {
            state: MotionState::Moving,
            hold_ms: 500
        }
    );
    assert_eq!(t.to_string(), "state=moving hold_ms=500 src=qma6100p");
}

#[test]
fn a_change_at_the_threshold_is_not_motion() {
    let mut det = MotionDetector::new(0);
    det.on_sample(REST, 0);
    assert_eq!(det.on_sample(nudged(122), 250), None);
    assert_eq!(det.state(), MotionState::Unknown);
}

#[test]
fn deltas_sum_over_the_axes() {
    let mut det = MotionDetector::new(0);
    det.on_sample(REST, 0);
    // 41 + 41 + 41 = 123 > 122, though no single axis is near the threshold.
    let tilted = Sample {
        x: 41,
        y: -41,
        z: 4096 + 41,
    };
    assert!(det.on_sample(tilted, 250).is_some());
}

#[test]
fn still_only_after_the_full_hold_with_no_sample_over() {
    let mut det = MotionDetector::new(0);
    det.on_sample(REST, 0);
    det.on_sample(nudged(500), 1_000).expect("moving");
    // Back at rest: the sample at 1250 differs from the nudged one by 500,
    // which is motion too, so the quiet period starts there.
    assert_eq!(det.on_sample(REST, 1_250), None);
    let quiet_from = 1_250;
    assert!(
        rest(&mut det, 1_500, quiet_from + STILL_HOLD_MS).is_empty(),
        "nothing before the hold has elapsed"
    );
    let t = det
        .on_sample(REST, quiet_from + STILL_HOLD_MS)
        .expect("hold elapsed");
    assert_eq!(
        t,
        Transition {
            state: MotionState::Still,
            hold_ms: STILL_HOLD_MS
        }
    );
    assert_eq!(t.to_string(), "state=still hold_ms=30000 src=qma6100p");
}

#[test]
fn motion_inside_the_hold_restarts_it() {
    let mut det = MotionDetector::new(0);
    det.on_sample(REST, 0);
    det.on_sample(nudged(500), 250).expect("moving");
    det.on_sample(REST, 500); // the way back is motion too
                              // A second nudge 20 s later, still moving: no line, but the hold restarts.
    assert_eq!(det.on_sample(nudged(500), 20_500), None);
    assert_eq!(det.on_sample(REST, 20_750), None);
    assert!(rest(&mut det, 21_000, 20_750 + STILL_HOLD_MS).is_empty());
    assert_eq!(
        det.on_sample(REST, 20_750 + STILL_HOLD_MS).map(|t| t.state),
        Some(MotionState::Still)
    );
}

#[test]
fn no_repeated_lines_while_a_state_holds() {
    let mut det = MotionDetector::new(0);
    det.on_sample(REST, 0);
    let mut lines = Vec::new();
    // Ten seconds of shaking: alternate samples 500 LSB apart.
    for i in 1..=40u64 {
        let s = if i % 2 == 0 { REST } else { nudged(500) };
        lines.extend(det.on_sample(s, i * POLL_INTERVAL_MS));
    }
    assert_eq!(lines.len(), 1, "one moving line for ten seconds of motion");
    // Then five minutes at rest: exactly one still line.
    let after = rest(&mut det, 10_250, 10_250 + 300_000);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].state, MotionState::Still);
}

#[test]
fn a_board_that_boots_at_rest_says_still_after_the_hold() {
    let mut det = MotionDetector::new(1_000);
    assert!(rest(&mut det, 1_000, 1_000 + STILL_HOLD_MS).is_empty());
    let t = det.on_sample(REST, 1_000 + STILL_HOLD_MS).expect("still");
    assert_eq!(t.state, MotionState::Still);
    assert_eq!(t.hold_ms, STILL_HOLD_MS);
    // Lifted a minute later: moving, and hold_ms says how long it was still.
    let lifted_at = 1_000 + STILL_HOLD_MS + 60_000;
    det.on_sample(REST, lifted_at - 250);
    let m = det.on_sample(nudged(200), lifted_at).expect("moving");
    assert_eq!(m.state, MotionState::Moving);
    assert_eq!(m.hold_ms, 60_000);
}

#[test]
fn read_health_reports_edges_only() {
    let mut h = ReadHealth::default();
    assert_eq!(h.on_read(true), None);
    assert_eq!(h.on_read(false), Some("state=failed src=qma6100p"));
    assert_eq!(h.on_read(false), None);
    assert_eq!(h.on_read(true), Some("state=ok src=qma6100p"));
    assert_eq!(h.on_read(true), None);
}

// ---------------------------------------------------------------------------
// MOTION_STAT
// ---------------------------------------------------------------------------

#[test]
fn no_stat_before_the_interval() {
    let mut det = MotionDetector::new(1_000);
    det.on_sample(REST, 1_250);
    assert_eq!(det.stat_due(1_000 + STAT_INTERVAL_MS - 1), None);
    assert!(det.stat_due(1_000 + STAT_INTERVAL_MS).is_some());
}

#[test]
fn a_window_at_rest_reports_its_noise_floor() {
    let mut det = MotionDetector::new(0);
    // Noise 0, 7, 0, 7 ... LSB on x: a delta of 7 every sample after the
    // first, which has nothing to compare with.
    let mut t = 0;
    for i in 0..(STAT_INTERVAL_MS / POLL_INTERVAL_MS) {
        t = (i + 1) * POLL_INTERVAL_MS;
        det.on_sample(nudged(if i % 2 == 0 { 0 } else { 7 }), t);
    }
    let stat = det.stat_due(t).expect("due");
    assert_eq!(
        stat,
        Stat {
            samples: 120,
            max_delta: 7,
            over: 0,
            state: MotionState::Still,
            quiet_ms: 30_000,
        }
    );
    assert_eq!(
        stat.to_string(),
        "samples=120 max_delta=7 over=0 state=still quiet_ms=30000 src=qma6100p"
    );
}

#[test]
fn max_delta_and_over_count_the_whole_window_then_reset() {
    let mut det = MotionDetector::new(0);
    det.on_sample(REST, 250);
    det.on_sample(nudged(200), 500); // over: 200
    det.on_sample(nudged(150), 750); // 50, under
    det.on_sample(nudged(400), 1_000); // over: 250, the largest
    det.on_sample(nudged(400), 1_250); // 0
    let stat = det.stat_due(STAT_INTERVAL_MS).expect("due");
    assert_eq!(stat.samples, 5);
    assert_eq!(stat.max_delta, 250);
    assert_eq!(stat.over, 2);
    assert_eq!(stat.state, MotionState::Moving);
    assert_eq!(stat.quiet_ms, STAT_INTERVAL_MS - 1_000);

    // The next window starts empty, and its first sample has a delta: the
    // detector keeps the previous sample across windows.
    det.on_sample(nudged(410), STAT_INTERVAL_MS + 250);
    let next = det.stat_due(2 * STAT_INTERVAL_MS).expect("due");
    assert_eq!(next.samples, 1);
    assert_eq!(next.max_delta, 10);
    assert_eq!(next.over, 0);
    assert_eq!(det.stat_due(2 * STAT_INTERVAL_MS + 250), None);
}

#[test]
fn quiet_ms_resets_on_an_over_sample() {
    let mut det = MotionDetector::new(0);
    det.on_sample(REST, 250);
    det.on_sample(nudged(123), 20_000);
    det.on_sample(nudged(123), 20_250);
    let stat = det.stat_due(STAT_INTERVAL_MS).expect("due");
    assert_eq!(stat.quiet_ms, STAT_INTERVAL_MS - 20_000);
    assert_eq!(stat.over, 1);
}

#[test]
fn a_recurring_glitch_keeps_moving_and_the_stat_shows_it() {
    // Reading (a) of the 881 rig finding: one sample over the threshold in
    // every window keeps the hold from ever completing. The stat line is
    // what tells it from a missing still edge.
    let mut det = MotionDetector::new(0);
    det.on_sample(REST, 0);
    let mut lines = Vec::new();
    let mut stats = Vec::new();
    let mut t = 0;
    while t < 4 * STAT_INTERVAL_MS {
        t += POLL_INTERVAL_MS;
        let glitch = t % 20_000 == 0;
        lines.extend(det.on_sample(if glitch { nudged(130) } else { REST }, t));
        stats.extend(det.stat_due(t));
    }
    assert_eq!(lines.len(), 1, "one moving line, never still");
    assert_eq!(stats.len(), 4);
    for s in &stats {
        assert_eq!(s.state, MotionState::Moving);
        assert!(s.over >= 2, "the glitch and the way back: {s}");
        assert!(s.quiet_ms < STILL_HOLD_MS, "{s}");
        assert_eq!(s.max_delta, 130);
    }
}

#[test]
fn an_unknown_state_is_printed_on_the_stat_line() {
    let mut det = MotionDetector::new(0);
    let stat = det.stat_due(STAT_INTERVAL_MS).expect("due");
    assert_eq!(
        stat.to_string(),
        "samples=0 max_delta=0 over=0 state=unknown quiet_ms=30000 src=qma6100p"
    );
}
