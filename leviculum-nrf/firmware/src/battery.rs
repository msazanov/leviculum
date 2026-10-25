//! The battery monitor's cadences and lines (Codeberg #380; moved here in
//! #469 991).
//!
//! Once at boot the task takes one reading from the [`BatteryGauge`],
//! classifies the pack's cell count from it and says so (`[BAT] init`, and
//! `[WARN] [BAT] implausible first reading` when it is not a pack). Then it
//! samples once a second, filters ([`BatteryEwma`]) and keeps the period's
//! extremes ([`BatteryWindow`]), publishes to
//! [`BATTERY_STATE`](crate::board_state::BATTERY_STATE) every fifth sample
//! and writes the `BATTERY` line every thirtieth, and writes `BATTERY_PCT`
//! whenever the percentage starts or stops being derivable. The gauge,
//! which on the boards in this tree is the SAADC behind the board's
//! divider, stays with the bind crate (`leviculum_nrf::battery`); so does
//! the board's [`BatteryScale`](leviculum_battery_scale::BatteryScale),
//! which is the gauge's construction argument and not policy.
//!
//! What the lines mean, why a percentage can be missing and what 1 Hz can
//! catch is the bind crate's module doc and
//! `docs/src/structured-event-logs.md` (`BATTERY`, `BATTERY_PCT`); the
//! numbers are all in [`leviculum_battery_scale`].
//!
//! # One registered instance
//!
//! The loop is [`monitor`], over a gauge, a clock and a function each
//! [`Event`] goes to, so a host test holds one over
//! `fakes::FakeBatteryGauge` and keeps the events. The board's one
//! instance is the one [`run`] builds, whose function writes the lines and
//! publishes the state.

use leviculum_battery_scale::{
    classify_cell_count, pack_percent, BatteryEwma, BatteryLine, BatteryPercentLine, BatteryWindow,
};

use crate::board_state::{BatteryState, BATTERY_STATE};
use crate::seams::{BatteryGauge, Clock};

/// How often the gauge is read.
const SAMPLE_PERIOD_MS: u64 = 1_000;

/// Samples between two [`BATTERY_STATE`] updates — 5 s, the cadence the
/// display has always been fed at. Deliberately unchanged: the status
/// screen quantises the pack voltage to 100 mV and the charge to 1 %
/// (`leviculum_screen::FrameKey`), and a transmit sag is larger than
/// 100 mV, so publishing every sample would have put a redraw on the
/// screen for every packet the node sends.
const SAMPLES_PER_PUBLISH: u32 = 5;

/// Samples between two `BATTERY` lines.
const SAMPLES_PER_REPORT: u32 = 30;

/// What the gauge says about itself on the `[BAT] init` line: a value the
/// bind crate hands down, so this crate names no ADC. For the SAADC it is
/// the board's whole measurable range at the battery terminal and the
/// acquisition window its divider needs, both off the board's
/// `BatteryScale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GaugeFacts {
    /// The pack voltage a full-scale reading means, in millivolts.
    pub full_scale_mv: u32,
    /// The acquisition window, in microseconds.
    pub acquisition_us: u32,
}

/// What the monitor does, in order: a line to write or a state to
/// publish. [`run`] turns each into its log line or its
/// [`BATTERY_STATE`] update; a host test keeps them.
pub enum Event {
    /// The boot's first reading and the cell count classified from it.
    Init {
        pack_mv: u16,
        cells: u8,
        facts: GaugeFacts,
    },
    /// The first reading is outside anything a pack can be; the cell count
    /// fell back to 1S.
    Implausible { pack_mv: u16, full_scale_mv: u32 },
    /// A new snapshot for the display and the telemetry.
    Publish(BatteryState),
    /// The `BATTERY` line; `at_boot` for the one at boot, which bypasses
    /// the runtime-drain gate.
    Report { line: BatteryLine, at_boot: bool },
    /// The `BATTERY_PCT` line: the percentage just started or stopped
    /// being derivable.
    Gate(BatteryPercentLine),
}

/// The board's battery monitor, forever: [`monitor`] over `gauge` and
/// `clock`, writing the lines and publishing to [`BATTERY_STATE`].
pub async fn run<G: BatteryGauge, C: Clock>(gauge: G, clock: C, facts: GaugeFacts) -> ! {
    let sender = BATTERY_STATE.sender();
    monitor(gauge, &clock, facts, |event| match event {
        // Critical, i.e. past the `RUNTIME_DRAIN_OPEN` gate, and the boot
        // lines are where that is right: the gate stays shut until
        // DTR-assert or 30 s of uptime, whichever comes first, and a board
        // on battery in a field has neither a host nor, if it resets
        // inside that window, a second chance. The boot statement is
        // exactly the one #380 went looking for and did not find.
        Event::Init {
            pack_mv,
            cells,
            facts,
        } => crate::log::log_fmt_critical(
            "[BAT] ",
            format_args!(
                "init pack_mv={} cells={}S full_scale_mv={} acq_us={}",
                pack_mv, cells, facts.full_scale_mv, facts.acquisition_us
            ),
        ),
        Event::Implausible {
            pack_mv,
            full_scale_mv,
        } => crate::log::log_fmt_critical(
            "[WARN] ",
            format_args!(
                "[BAT] implausible first reading pack_mv={} (full scale {} mV) — assuming 1S",
                pack_mv, full_scale_mv
            ),
        ),
        Event::Publish(state) => sender.send(state),
        // The boot one bypasses the gate, the every-30-s ones do not.
        Event::Report {
            line,
            at_boot: true,
        } => crate::log::log_fmt_critical("[BAT] ", format_args!("{}", line)),
        Event::Report {
            line,
            at_boot: false,
        } => crate::log::log_fmt("[BAT] ", format_args!("{}", line)),
        // Critical rather than gated: these lines are rare by
        // construction — one per transition, not one per sample — and each
        // of them is what explains a gap in the `pct=` series that a
        // capture would otherwise have to guess at. A field board has no
        // host to open the gate, and this is precisely the run in which
        // the gap appears.
        Event::Gate(line) => crate::log::log_fmt_critical("[BAT] ", format_args!("{}", line)),
    })
    .await
}

/// The monitor's loop over `gauge` against `clock`, handing each [`Event`]
/// to `out` in the order it happens.
pub async fn monitor<G: BatteryGauge, C: Clock>(
    mut gauge: G,
    clock: &C,
    facts: GaugeFacts,
    mut out: impl FnMut(Event),
) -> ! {
    // One sample decides the cell count for the lifetime of the task.
    // Persisting it across boots waits for the flash store; redoing it
    // costs one reading.
    let first_mv = gauge.sample_mv().await;
    let class = classify_cell_count(first_mv);
    out(Event::Init {
        pack_mv: first_mv,
        cells: class.cells,
        facts,
    });
    if !class.plausible {
        // Not a pack voltage: a floating input, a divider whose enable
        // pin is not doing what the board file says, or a multiplier
        // that does not match the board this image is running on. 1S is
        // the fallback because it cannot make an empty pack look full.
        out(Event::Implausible {
            pack_mv: first_mv,
            full_scale_mv: facts.full_scale_mv,
        });
    }
    let cell_count = class.cells;

    let mut ewma = BatteryEwma::new(first_mv);
    let mut window = BatteryWindow::new(first_mv);

    let state = |mv: u16, percent: Option<u8>| {
        Event::Publish(BatteryState {
            voltage_mv: mv,
            percent,
            cell_count,
        })
    };
    let report =
        |mv: u16, percent: Option<u8>, window: &BatteryWindow, at_boot: bool| Event::Report {
            line: BatteryLine {
                mv,
                min_mv: window.min_mv(),
                max_mv: window.max_mv(),
                percent,
                cells: cell_count,
            },
            at_boot,
        };

    // Say it once at boot rather than 30 s in: a board that resets
    // before the first period is up would otherwise report nothing at
    // all, which is exactly the boot worth hearing about.
    let mut percent = pack_percent(first_mv, cell_count);
    let mut reportable = percent.is_some();
    out(state(first_mv, percent));
    out(report(first_mv, percent, &window, true));
    if !reportable {
        // Only the withheld state earns a line at boot. A boot that CAN
        // derive a percentage has already said so, with `pct=` on the
        // `BATTERY` line immediately above; a boot that cannot is the
        // anomaly.
        out(Event::Gate(BatteryPercentLine::new(
            first_mv, cell_count, false,
        )));
    }

    let mut samples: u32 = 0;
    loop {
        clock.wait_until(clock.now_ms() + SAMPLE_PERIOD_MS).await;
        let pack_mv = gauge.sample_mv().await;
        let filtered = ewma.step(pack_mv);
        window.observe(pack_mv);
        samples = samples.wrapping_add(1);

        // Judged on the same number the percentage would be derived from,
        // in the same iteration it is derived in, so the line and the
        // published state can never disagree about which side of the band
        // the pack is on. Every sample, not every report: the transition
        // is the event, and waiting up to 30 s to mention it would put it
        // in the wrong place on a merged timeline.
        percent = pack_percent(filtered, cell_count);
        if percent.is_some() != reportable {
            reportable = percent.is_some();
            out(Event::Gate(BatteryPercentLine::new(
                filtered, cell_count, reportable,
            )));
        }

        if samples.is_multiple_of(SAMPLES_PER_PUBLISH) {
            out(state(filtered, percent));
        }
        if samples.is_multiple_of(SAMPLES_PER_REPORT) {
            out(report(filtered, percent, &window, false));
            // Seed the next window with the sample that closed this one,
            // so no sample falls between two windows.
            window.restart(pack_mv);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{drive, FakeBatteryGauge, FakeClock};
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    const FACTS: GaugeFacts = GaugeFacts {
        full_scale_mv: 6228,
        acquisition_us: 10,
    };

    /// One event as a test compares it: when, and what, rendered.
    #[derive(Debug, PartialEq, Eq)]
    enum Seen {
        Init { cells: u8, plausible: bool },
        Publish { mv: u16, percent: Option<u8> },
        Report(String),
        Gate(String),
    }

    /// Run the monitor over a gauge that reads `script` (its last value
    /// from then on) until `until_ms`, and keep what it said with when.
    fn watch(script: &[u16], until_ms: u64) -> Vec<(u64, Seen)> {
        let clock = FakeClock::new();
        let gauge = FakeBatteryGauge::new(script);
        let mut seen = Vec::new();
        let mut plausible = true;
        let mut cells = 0;
        drive(
            monitor(&gauge, &clock, FACTS, |event| {
                let at = clock.now_ms();
                match event {
                    Event::Init { cells: c, .. } => cells = c,
                    Event::Implausible { .. } => plausible = false,
                    Event::Publish(s) => seen.push((
                        at,
                        Seen::Publish {
                            mv: s.voltage_mv,
                            percent: s.percent,
                        },
                    )),
                    Event::Report { line, .. } => seen.push((at, Seen::Report(format!("{line}")))),
                    Event::Gate(line) => seen.push((at, Seen::Gate(format!("{line}")))),
                }
            }),
            &clock,
            until_ms,
            |_| None,
        );
        seen.insert(0, (0, Seen::Init { cells, plausible }));
        seen
    }

    // The boot reading is published and reported at 0; the next publish
    // is the fifth sample's, at 5 s, with the filter's value after five
    // steps of 4000 from a 3900 seed, as `BatteryEwma` computes it.
    #[test]
    fn the_first_publish_after_boot_comes_after_five_samples() {
        let seen = watch(&[3_900, 4_000], 9_999);
        let mut ewma = BatteryEwma::new(3_900);
        let fifth = (0..5).map(|_| ewma.step(4_000)).last();
        let publishes: Vec<_> = seen
            .iter()
            .filter(|(_, s)| matches!(s, Seen::Publish { .. }))
            .collect();
        assert_eq!(publishes.len(), 2);
        assert_eq!(
            publishes[0],
            &(
                0,
                Seen::Publish {
                    mv: 3_900,
                    percent: pack_percent(3_900, 1)
                }
            )
        );
        let fifth = fifth.unwrap_or(0);
        assert_eq!(
            publishes[1],
            &(
                5_000,
                Seen::Publish {
                    mv: fifth,
                    percent: pack_percent(fifth, 1)
                }
            )
        );
    }

    // The boot line at 0, the next at the thirtieth sample, 30 s, with
    // the filter after thirty steps and the window's extremes over the
    // boot reading and the thirty samples.
    #[test]
    fn the_report_line_comes_after_thirty_samples() {
        let seen = watch(&[3_900, 3_800, 4_100, 4_000], 59_999);
        let reports: Vec<_> = seen
            .iter()
            .filter_map(|(at, s)| match s {
                Seen::Report(l) => Some((*at, l.as_str())),
                _ => None,
            })
            .collect();
        let mut ewma = BatteryEwma::new(3_900);
        let mut mv = 0;
        for sample in [3_800, 4_100].into_iter().chain([4_000; 28]) {
            mv = ewma.step(sample);
        }
        let thirtieth = format!(
            "{}",
            BatteryLine {
                mv,
                min_mv: 3_800,
                max_mv: 4_100,
                percent: pack_percent(mv, 1),
                cells: 1,
            }
        );
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].0, 0);
        assert_eq!(
            reports[0].1,
            format!(
                "BATTERY mv=3900 min_mv=3900 max_mv=3900 pct={} cells=1S",
                pack_percent(3_900, 1).unwrap_or(0)
            )
        );
        assert_eq!(reports[1], (30_000, thirtieth.as_str()));
    }

    // A 1S pack at 3.9 V whose readings fall to 2.0 V: the filtered value
    // leaves the band on the sample `BatteryEwma` says it does, and the
    // gate line is said then, once, with that value. Back at 3.9 V it
    // comes back in, once.
    #[test]
    fn the_gate_line_is_said_when_the_percentage_leaves_and_reenters_its_band() {
        let mut script = alloc::vec![3_900];
        script.extend([2_000; 20]);
        script.push(3_900);
        let seen = watch(&script, 40_000);

        let mut ewma = BatteryEwma::new(3_900);
        let (mut out_at, mut out_mv) = (None, 0);
        for n in 1..=20u64 {
            let mv = ewma.step(2_000);
            if out_at.is_none() && pack_percent(mv, 1).is_none() {
                (out_at, out_mv) = (Some(n * 1_000), mv);
            }
        }
        let mut back = None;
        for n in 21..=40u64 {
            let mv = ewma.step(3_900);
            if back.is_none() && pack_percent(mv, 1).is_some() {
                back = Some((n * 1_000, mv));
            }
        }
        let gates: Vec<_> = seen
            .iter()
            .filter_map(|(at, s)| match s {
                Seen::Gate(l) => Some((*at, l.clone())),
                _ => None,
            })
            .collect();
        let back = back.unwrap_or((0, 0));
        assert_eq!(
            gates,
            [
                (
                    out_at.unwrap_or(0),
                    format!("{}", BatteryPercentLine::new(out_mv, 1, false))
                ),
                (
                    back.0,
                    format!("{}", BatteryPercentLine::new(back.1, 1, true))
                ),
            ]
        );
    }

    // Classification at boot: 3.9 V is 1S, 7.6 V is 2S, both plausible
    // and neither withheld, so no gate line at boot; 12 V is not a pack,
    // falls back to 1S, is withheld and says so at 0.
    #[test]
    fn the_cell_count_is_classified_from_the_first_reading() {
        let one = watch(&[3_900], 0);
        assert_eq!(
            one[0].1,
            Seen::Init {
                cells: 1,
                plausible: true
            }
        );
        let two = watch(&[7_600], 0);
        assert_eq!(
            two[0].1,
            Seen::Init {
                cells: 2,
                plausible: true
            }
        );
        assert!(two
            .iter()
            .any(|(_, s)| matches!(s, Seen::Report(l) if l.ends_with("cells=2S"))));
        assert!(!one
            .iter()
            .chain(&two)
            .any(|(_, s)| matches!(s, Seen::Gate(_))));

        let none = watch(&[12_000], 0);
        assert_eq!(
            none[0].1,
            Seen::Init {
                cells: 1,
                plausible: false
            }
        );
        assert_eq!(
            none.last().map(|(at, s)| (*at, s)),
            Some((
                0,
                &Seen::Gate(format!("{}", BatteryPercentLine::new(12_000, 1, false)))
            ))
        );
    }
}
