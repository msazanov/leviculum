//! One-shot Airoha AG3335 boot init: probe, constellations, sentence
//! selection (Codeberg #456).
//!
//! The SenseCAP T1000-E carries an Airoha (MediaTek lineage) AG3335 on
//! P0.14/P0.13 at 115200. It speaks neither UBX nor `$PCAS`; it is
//! configured with Airoha's proprietary `$PAIR` sentences. The *shape*
//! is the one [`ModuleInit`] already describes for the other two.
//!
//! The reference is Meshtastic's `GNSS_MODEL_AG3335` branch
//! (`meshtastic/src/gps/GPS.cpp:642-664`), reached on the T1000-E
//! because its variant defines `GNSS_AIROHA`
//! (`variants/nrf52840/tracker-t1000-e/variant.h:108`), and its probe
//! (`GPS.cpp:1353-1356`, `PROBE_FAMILY("Airoha Family", "$PAIR021*39",
//! …)`, answer `$PAIR021,AG3335`). We send the reference's sentences,
//! one per step, with one deviation and two omissions:
//!
//! 1. **`$PAIR021`** (first): the version query. Its answer,
//!    `$PAIR021,AG3335…`, is the positive evidence that the module hears
//!    our TX line, exactly as the `$PCAS06,0` probe is on the L76K. A
//!    timeout is reported and the sequence continues, for the same
//!    reason as there.
//! 2. **`$PAIR066,1,1,1,1,0,0`**: GPS + GLONASS + Galileo + BeiDou.
//!    Verbatim from the reference's default arm. The reference switches
//!    to GPS + Galileo + NavIC when its LoRa region is India; this
//!    firmware has no region setting to key that on, so it sends the
//!    arm every other region gets.
//! 3. **`$PAIR062,<type>,<rate>`** for GGA, GLL, GSA, GSV, RMC, VTG,
//!    ZDA in the reference's order: GGA and RMC every fix, GLL, GSA,
//!    VTG and ZDA off. The deviation is **GSV on**, for the reason the
//!    L76K sequence keeps it: `leviculum-gnss-presence` folds GSV into
//!    the `sv=` / `cno=` heartbeat, the instrument that tells a deaf
//!    antenna from a misconfigured module while presence reads `no-fix`
//!    (#324). Every rate is sent, the off ones included, because a
//!    T1000-E arrives from the vendor or from Meshtastic with the
//!    reference's selection saved in the module (`$PAIR513`, below),
//!    and an unsent sentence would keep that saved value.
//!
//! **Omitted: `$PAIR513`**, the reference's "save configuration". The
//! sequence runs on every boot, so a saved copy buys nothing, and a
//! write to the module's flash on every boot is wear for no gain.
//!
//! **Omitted: a baud command**, for the reason [`crate::l76k`] gives:
//! the driver's rate is whatever the presence machine locked. The
//! sweep's 115200 is the variant's `GPS_BAUDRATE 115200`.
//!
//! The `$PAIR` commands do answer, with `$PAIR001,<cmd>,<result>`; the
//! reference never reads those answers and neither does this sequence.
//! The probe is the one answer it waits for, so the wiring question has
//! one line, and every command step stays fire-and-forget behind the
//! settle-plus-fresh-sentence gate the other sequencers use.
//!
//! **Control lines.** The AG3335 has more than a power enable: VRTC,
//! RESET, SLEEP_INT and RTC_INT. Each is held at the level the
//! reference's `initVariant` sets for the life of the board, which is a
//! GPIO act and therefore the board file's, not this crate's
//! (`leviculum-nrf/src/boards/t1000e.rs`).

use crate::{nmea_frame, AckOutcome, LiteralScanner, ModuleInit, Output};

/// `$PAIR021` — version query (reference: `GPS.cpp:1356`).
pub const PROBE_FRAME: [u8; 13] = nmea_frame(b"PAIR021");

/// `$PAIR066,…` — constellation enables in fixed field order: GPS,
/// GLONASS, Galileo, BeiDou, QZSS, NavIC (reference: `GPS.cpp:650`).
pub const CONSTELLATIONS_FRAME: [u8; 25] = nmea_frame(b"PAIR066,1,1,1,1,0,0");

/// `$PAIR062,0,1` — GGA every fix.
pub const GGA_FRAME: [u8; 17] = nmea_frame(b"PAIR062,0,1");
/// `$PAIR062,1,0` — GLL off.
pub const GLL_FRAME: [u8; 17] = nmea_frame(b"PAIR062,1,0");
/// `$PAIR062,2,0` — GSA off.
pub const GSA_FRAME: [u8; 17] = nmea_frame(b"PAIR062,2,0");
/// `$PAIR062,3,1` — GSV every fix. The reference sends `3,0`; the module
/// doc carries the reason for the difference.
pub const GSV_FRAME: [u8; 17] = nmea_frame(b"PAIR062,3,1");
/// `$PAIR062,4,1` — RMC every fix.
pub const RMC_FRAME: [u8; 17] = nmea_frame(b"PAIR062,4,1");
/// `$PAIR062,5,0` — VTG off.
pub const VTG_FRAME: [u8; 17] = nmea_frame(b"PAIR062,5,0");
/// `$PAIR062,6,0` — ZDA off. The reference's comment says "ZDA ON"; its
/// bytes say off, and the bytes are what the module obeys.
pub const ZDA_FRAME: [u8; 17] = nmea_frame(b"PAIR062,6,0");

const _: () = assert!(crate::MAX_FRAME >= PROBE_FRAME.len());
const _: () = assert!(crate::MAX_FRAME >= CONSTELLATIONS_FRAME.len());
const _: () = assert!(crate::MAX_FRAME >= GGA_FRAME.len());

/// The prefix of the `$PAIR021` answer, matched literally in the RX
/// stream: the reference's own token for this part (`GPS.cpp:1353`).
/// The firmware build string follows it and is not parsed.
pub const PROBE_ANSWER: &[u8] = b"$PAIR021,AG3335";

/// Wait for the probe answer, in ms. The reference allows 1000 ms
/// (`GPS.cpp:1356`); tripled for the reason [`crate::l76k`] gives — the
/// driver reads in burst-aligned ~1 s chunks.
pub const PROBE_TIMEOUT_MS: u64 = 3_000;

/// Settle between two configuration sentences, in ms. The reference
/// writes its `$PAIR062` block back to back and holds 250 ms before the
/// save; 250 ms per step here, with the fresh-sentence gate carrying the
/// real weight as on the L76K.
pub const POST_STEP_SETTLE_MS: u64 = 250;

/// One init step. `as_str` is the stable `[GNSS_INIT] step=<...>` log
/// token, `pair-` prefixed so it is disjoint from the L76K's and the
/// UBX module's tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// `$PAIR021`: version query, the one step with an answer.
    Probe,
    /// `$PAIR066`: GPS + GLONASS + Galileo + BeiDou.
    Constellations,
    Gga,
    Gll,
    Gsa,
    Gsv,
    Rmc,
    Vtg,
    Zda,
}

impl Step {
    pub fn as_str(self) -> &'static str {
        match self {
            Step::Probe => "pair-probe",
            Step::Constellations => "pair-constellations",
            Step::Gga => "pair-gga",
            Step::Gll => "pair-gll",
            Step::Gsa => "pair-gsa",
            Step::Gsv => "pair-gsv",
            Step::Rmc => "pair-rmc",
            Step::Vtg => "pair-vtg",
            Step::Zda => "pair-zda",
        }
    }

    /// The frame this step transmits, `&'static` in flash; the driver
    /// stages it into RAM for EasyDMA.
    pub fn frame(self) -> &'static [u8] {
        match self {
            Step::Probe => &PROBE_FRAME,
            Step::Constellations => &CONSTELLATIONS_FRAME,
            Step::Gga => &GGA_FRAME,
            Step::Gll => &GLL_FRAME,
            Step::Gsa => &GSA_FRAME,
            Step::Gsv => &GSV_FRAME,
            Step::Rmc => &RMC_FRAME,
            Step::Vtg => &VTG_FRAME,
            Step::Zda => &ZDA_FRAME,
        }
    }

    /// The step after this one, `None` after the last.
    fn next(self) -> Option<Step> {
        match self {
            Step::Probe => Some(Step::Constellations),
            Step::Constellations => Some(Step::Gga),
            Step::Gga => Some(Step::Gll),
            Step::Gll => Some(Step::Gsa),
            Step::Gsa => Some(Step::Gsv),
            Step::Gsv => Some(Step::Rmc),
            Step::Rmc => Some(Step::Vtg),
            Step::Vtg => Some(Step::Zda),
            Step::Zda => None,
        }
    }
}

/// Sequencer state, the L76K's: `Gate` holds the send of `step` until
/// the settle has passed AND a clean sentence arrived after it.
#[derive(Clone, Copy)]
enum State {
    AwaitLock,
    AwaitAnswer {
        deadline_ms: u64,
    },
    Gate {
        step: Step,
        earliest_ms: u64,
        snap: Option<u32>,
    },
    Done,
}

/// The one-shot init sequencer. Construct once per boot; feed RX bytes
/// via [`ModuleInit::on_bytes`] and drive time, lock state and the
/// clean-sentence counter via [`ModuleInit::poll`]. After the final step
/// it stays silent forever.
pub struct AirohaInit {
    state: State,
    scanner: LiteralScanner,
}

impl Default for AirohaInit {
    fn default() -> Self {
        Self::new()
    }
}

impl AirohaInit {
    pub fn new() -> Self {
        Self {
            state: State::AwaitLock,
            scanner: LiteralScanner::new(PROBE_ANSWER),
        }
    }

    /// Emit the Send for `step` and move to its follow-up state: the
    /// probe waits for its answer, every other step gates the next.
    fn send(&mut self, step: Step, now_ms: u64, emit: &mut dyn FnMut(Output)) {
        emit(Output::Send {
            step: step.as_str(),
            frame: step.frame(),
        });
        self.state = match (step, step.next()) {
            (Step::Probe, _) => State::AwaitAnswer {
                deadline_ms: now_ms + PROBE_TIMEOUT_MS,
            },
            (_, Some(next)) => gate(next, now_ms),
            (_, None) => State::Done,
        };
    }
}

/// The settle-plus-fresh-sentence gate in front of `step`.
fn gate(step: Step, now_ms: u64) -> State {
    State::Gate {
        step,
        earliest_ms: now_ms + POST_STEP_SETTLE_MS,
        snap: None,
    }
}

impl ModuleInit for AirohaInit {
    fn on_bytes(&mut self, bytes: &[u8], now_ms: u64, emit: &mut dyn FnMut(Output)) {
        for &b in bytes {
            if !self.scanner.push(b) {
                continue;
            }
            if !matches!(self.state, State::AwaitAnswer { .. }) {
                continue;
            }
            emit(Output::AckResult {
                step: Step::Probe.as_str(),
                outcome: AckOutcome::Ack,
            });
            self.state = gate(Step::Constellations, now_ms);
        }
    }

    fn poll(&mut self, locked: bool, sentences: u32, now_ms: u64, emit: &mut dyn FnMut(Output)) {
        match self.state {
            State::AwaitLock => {
                if locked {
                    self.send(Step::Probe, now_ms, emit);
                }
            }
            State::AwaitAnswer { deadline_ms } => {
                if now_ms >= deadline_ms {
                    emit(Output::AckResult {
                        step: Step::Probe.as_str(),
                        outcome: AckOutcome::Timeout,
                    });
                    self.state = gate(Step::Constellations, now_ms);
                }
            }
            State::Gate {
                step,
                earliest_ms,
                snap,
            } => {
                if now_ms < earliest_ms {
                    return;
                }
                match snap {
                    None => {
                        self.state = State::Gate {
                            step,
                            earliest_ms,
                            snap: Some(sentences),
                        }
                    }
                    Some(s) if sentences > s && locked => {
                        self.send(step, now_ms, emit);
                    }
                    Some(_) => {}
                }
            }
            State::Done => {}
        }
    }
}

#[cfg(test)]
mod tests;
