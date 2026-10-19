use super::*;
use crate::MAX_FRAME;

// Fixtures written out as literal wire text, INDEPENDENT of
// `nmea_frame`. All but one are the reference's own literals
// (`meshtastic/src/gps/GPS.cpp:650`, `:655-661`, `:1356`), copied
// character for character; the GSV-on line is the deviation, its
// checksum computed with a separate XOR (python3). A checksum bug in the
// const fn therefore cannot confirm itself.
const FIXTURE_PROBE: &[u8] = b"$PAIR021*39\r\n";
const FIXTURE_CONSTELLATIONS: &[u8] = b"$PAIR066,1,1,1,1,0,0*3A\r\n";
const FIXTURE_GGA: &[u8] = b"$PAIR062,0,1*3F\r\n";
const FIXTURE_GLL: &[u8] = b"$PAIR062,1,0*3F\r\n";
const FIXTURE_GSA: &[u8] = b"$PAIR062,2,0*3C\r\n";
const FIXTURE_GSV: &[u8] = b"$PAIR062,3,1*3C\r\n";
const FIXTURE_RMC: &[u8] = b"$PAIR062,4,1*3B\r\n";
const FIXTURE_VTG: &[u8] = b"$PAIR062,5,0*3B\r\n";
const FIXTURE_ZDA: &[u8] = b"$PAIR062,6,0*38\r\n";

/// The reference's GSV line, off. Kept because the one documented
/// deviation is measured against it.
const REFERENCE_GSV: &[u8] = b"$PAIR062,3,0*3D\r\n";

/// A plausible answer to `$PAIR021` from an AG3335 (the shape of the
/// line the module prints; the build fields after the part name are not
/// parsed).
const PROBE_ANSWER_LINE: &[u8] =
    b"$PAIR021,AG3335MN,S,N,9b52d01,2310181624,2a5,3,,,ba9d3227*5E\r\n";

/// The whole boot sequence in the order it must go out.
fn expected_sequence() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("pair-probe", FIXTURE_PROBE),
        ("pair-constellations", FIXTURE_CONSTELLATIONS),
        ("pair-gga", FIXTURE_GGA),
        ("pair-gll", FIXTURE_GLL),
        ("pair-gsa", FIXTURE_GSA),
        ("pair-gsv", FIXTURE_GSV),
        ("pair-rmc", FIXTURE_RMC),
        ("pair-vtg", FIXTURE_VTG),
        ("pair-zda", FIXTURE_ZDA),
    ]
}

/// The module side of the line: a fake UART that records every frame the
/// sequencer writes, answers the probe the way the AG3335 does when
/// `answers` is set, and otherwise emits one clean sentence per 100 ms.
struct FakeUart {
    answers: bool,
    written: Vec<(&'static str, &'static [u8])>,
    acks: Vec<(&'static str, AckOutcome)>,
}

impl FakeUart {
    fn new(answers: bool) -> Self {
        Self {
            answers,
            written: Vec::new(),
            acks: Vec::new(),
        }
    }

    /// Run the sequencer against this module for `ticks` × 100 ms with
    /// the line locked from the start.
    fn run(&mut self, m: &mut AirohaInit, ticks: u64) {
        for i in 0..ticks {
            let t = i * 100;
            let mut out = Vec::new();
            m.poll(true, i as u32 + 1, t, &mut |o| out.push(o));
            let mut reply = Vec::new();
            for o in out {
                match o {
                    Output::Send { step, frame } => {
                        self.written.push((step, frame));
                        if self.answers && frame == FIXTURE_PROBE {
                            reply.extend_from_slice(PROBE_ANSWER_LINE);
                        }
                    }
                    Output::AckResult { step, outcome } => self.acks.push((step, outcome)),
                }
            }
            let mut out = Vec::new();
            m.on_bytes(&reply, t + 10, &mut |o| out.push(o));
            for o in out {
                if let Output::AckResult { step, outcome } = o {
                    self.acks.push((step, outcome));
                }
            }
        }
    }
}

fn poll(m: &mut AirohaInit, locked: bool, sentences: u32, now_ms: u64) -> Vec<Output> {
    let mut out = Vec::new();
    m.poll(locked, sentences, now_ms, &mut |o| out.push(o));
    out
}

fn on_bytes(m: &mut AirohaInit, bytes: &[u8], now_ms: u64) -> Vec<Output> {
    let mut out = Vec::new();
    m.on_bytes(bytes, now_ms, &mut |o| out.push(o));
    out
}

// ---- Frame bytes ----

#[test]
fn frames_match_independent_fixtures() {
    for (step, fixture) in expected_sequence() {
        let s = [
            Step::Probe,
            Step::Constellations,
            Step::Gga,
            Step::Gll,
            Step::Gsa,
            Step::Gsv,
            Step::Rmc,
            Step::Vtg,
            Step::Zda,
        ]
        .into_iter()
        .find(|s| s.as_str() == step)
        .unwrap();
        assert_eq!(s.frame(), fixture, "{step}");
        assert!(s.frame().len() <= MAX_FRAME);
    }
}

// The one documented deviation: GSV on, against the reference's off, and
// the two lines differ in the rate digit (and so the checksum) only.
#[test]
fn gsv_differs_from_reference_only_in_the_rate() {
    let rate = b"$PAIR062,3,".len();
    assert_eq!(GSV_FRAME[..rate], REFERENCE_GSV[..rate]);
    assert_eq!(GSV_FRAME[rate], b'1');
    assert_eq!(REFERENCE_GSV[rate], b'0');
}

// No baud command and no save: neither `$PAIR864` (baud) nor `$PAIR513`
// (save configuration to the module's flash) ever goes out.
#[test]
fn no_frame_sets_baud_or_saves() {
    let mut m = AirohaInit::new();
    let mut uart = FakeUart::new(true);
    uart.run(&mut m, 600);
    for (step, frame) in uart.written {
        assert!(!frame.starts_with(b"$PAIR864"), "{step} sets the baud");
        assert!(!frame.starts_with(b"$PAIR513"), "{step} saves to flash");
    }
}

// ---- The boot path against the fake UART ----

#[test]
fn boot_sequence_on_an_answering_module() {
    let mut m = AirohaInit::new();
    let mut uart = FakeUart::new(true);
    uart.run(&mut m, 600);
    assert_eq!(uart.written, expected_sequence());
    assert_eq!(uart.acks, vec![("pair-probe", AckOutcome::Ack)]);
}

// A module that does not answer the query is still configured: the
// timeout is reported once and the commands follow.
#[test]
fn boot_sequence_on_a_silent_module() {
    let mut m = AirohaInit::new();
    let mut uart = FakeUart::new(false);
    uart.run(&mut m, 600);
    assert_eq!(uart.written, expected_sequence());
    assert_eq!(uart.acks, vec![("pair-probe", AckOutcome::Timeout)]);
}

// One-shot: once the last step is out, nothing ever again.
#[test]
fn silent_after_the_last_step() {
    let mut m = AirohaInit::new();
    let mut uart = FakeUart::new(true);
    uart.run(&mut m, 600);
    for i in 0..30u64 {
        let at = 100_000 + i * 1_000;
        assert!(poll(&mut m, true, 10_000 + i as u32, at).is_empty());
        assert!(on_bytes(&mut m, PROBE_ANSWER_LINE, at).is_empty());
    }
}

// ---- Ordering and the gate ----

#[test]
fn no_tx_before_lock() {
    let mut m = AirohaInit::new();
    for i in 0..20u64 {
        assert!(poll(&mut m, false, i as u32, i * 1_000).is_empty());
    }
    let out = poll(&mut m, true, 20, 20_000);
    assert_eq!(
        out,
        vec![Output::Send {
            step: "pair-probe",
            frame: &PROBE_FRAME,
        }]
    );
}

#[test]
fn probe_timeout_runs_its_full_length() {
    let mut m = AirohaInit::new();
    poll(&mut m, true, 3, 1_000);
    assert!(poll(&mut m, true, 3, 999 + PROBE_TIMEOUT_MS).is_empty());
    assert_eq!(
        poll(&mut m, true, 3, 1_000 + PROBE_TIMEOUT_MS),
        vec![Output::AckResult {
            step: "pair-probe",
            outcome: AckOutcome::Timeout,
        }]
    );
}

// The reference's other Airoha parts answer `$PAIR021,AG3352` and
// `$PAIR021,REYAX_…`; neither is this board's module.
#[test]
fn another_airoha_part_is_not_the_answer() {
    let mut m = AirohaInit::new();
    poll(&mut m, true, 3, 1_000);
    assert!(on_bytes(&mut m, b"$PAIR021,AG3352Q,S*00\r\n", 1_100).is_empty());
}

// The answer split across two read chunks, behind a `$` that breaks a
// false start, is still found.
#[test]
fn answer_found_across_chunks_and_after_a_false_start() {
    let mut m = AirohaInit::new();
    poll(&mut m, true, 3, 1_000);
    let mut chunk1 = b"$GNRMC,,V,,,,,,,,,,N*4D\r\n$PAIR02".to_vec();
    chunk1.extend_from_slice(&PROBE_ANSWER_LINE[..10]);
    assert!(on_bytes(&mut m, &chunk1, 1_100).is_empty());
    let out = on_bytes(&mut m, &PROBE_ANSWER_LINE[10..], 1_200);
    assert_eq!(
        out,
        vec![Output::AckResult {
            step: "pair-probe",
            outcome: AckOutcome::Ack,
        }]
    );
}

// A command step needs the settle, a fresh sentence and the lock.
#[test]
fn config_gate_needs_settle_and_fresh_sentence_and_lock() {
    let mut m = AirohaInit::new();
    poll(&mut m, true, 3, 1_000);
    on_bytes(&mut m, PROBE_ANSWER_LINE, 1_100);
    let gate_t = 1_100 + POST_STEP_SETTLE_MS;

    assert!(poll(&mut m, true, 9, 1_150).is_empty(), "pre-settle");
    poll(&mut m, true, 9, gate_t);
    assert!(
        poll(&mut m, true, 9, gate_t + 5_000).is_empty(),
        "frozen counter"
    );
    assert!(
        poll(&mut m, false, 10, gate_t + 6_000).is_empty(),
        "lock lost"
    );
    assert_eq!(
        poll(&mut m, true, 11, gate_t + 7_000),
        vec![Output::Send {
            step: "pair-constellations",
            frame: &CONSTELLATIONS_FRAME,
        }]
    );
}

#[test]
fn constants_hold_their_justifications() {
    const { assert!(POST_STEP_SETTLE_MS >= 250) };
    // The reference's probe wait (`GPS.cpp:1356`).
    const { assert!(PROBE_TIMEOUT_MS >= 1_000) };
}
