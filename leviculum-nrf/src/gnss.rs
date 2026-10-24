//! GNSS driver task: baud sweep, presence tri-state, NMEA fold.
//!
//! One task, four boards, three receivers. UARTE0 on P0.15/P0.16 carries
//! the WisMesh Pocket V2's u-blox ZOE-M8Q (RAK19026 VC baseboard); on
//! P1.07/P1.05 it carries the Heltec Mesh Node T114's Quectel L76K
//! (Codeberg #69); on P1.11/P1.12 it carries the SenseCAP Solar Node's
//! XIAO L76K, which is the same receiver on a different carrier and
//! therefore the same [`ModuleKind`] (#233); on P0.13/P0.14 it carries
//! the SenseCAP T1000-E's Airoha AG3335 (#456). Everything below the
//! module-init step is identical for all of them, because nothing in it is
//! module-specific: a baud sweep, a presence tri-state and an NMEA fold
//! work on sentences, and sentences are sentences. The differences are
//! all named in [`GnssWiring`] — which pins, which control lines exist,
//! and which [`ModuleKind`] the one-shot init speaks.
//!
//! The `gnss` cargo feature only says the board routes this UART; whether a
//! receiver is attached and delivering is a runtime question with three
//! answers (Codeberg #240), owned by the pure
//! [`leviculum_gnss_presence::PresenceMachine`]:
//!
//! - the machine sweeps 9600 → 38400 → 115200 until a checksum-clean
//!   sentence locks a baud (so a non-default module still works),
//! - publishes `no-hardware` / `no-fix` / `fix` through
//!   [`GNSS_PRESENCE`] with Fix→NoFix hysteresis,
//! - and forwards parsed RMC/GGA content, which this task folds into
//!   the [`GNSS_FIX`] snapshot exactly as before.
//!
//! The machine also aggregates GSV groups into satellites-in-view and
//! best C/N0 (#324); this task only prints them in the heartbeat as
//! `sv=` / `cno=`. They are the instrument for config-versus-antenna:
//! `sat=` is GGA satellites-in-USE, which most receivers report as 0
//! until a fix exists, so an
//! open-sky session that stays at `valid=false sv=0 cno=0` indicts the
//! RF path, while many SVs at 35-45 dBHz without a fix indicts
//! configuration instead.
//!
//! This task is deliberately a thin driver: reads via
//! `read_until_idle` (TIMER1 + PPI ch0/ch1 detect the line going quiet
//! between 1 Hz NMEA bursts, so chunks align with sentence boundaries
//! and nothing is lost between reads), a 1 s read timeout to carry
//! time into the machine when the line is silent, and UART
//! reconfiguration when the machine asks for a new baud. All policy —
//! window lengths, sweep order, hysteresis hold — lives host-tested in
//! the pure crate.
//!
//! One-shot module init (#324 on the V2, #69 on the T114): after the
//! first baud lock the task walks the sequence its board's
//! [`leviculum_gnss_init::ModuleInit`] hands out — UBX factory clear /
//! full power / antenna supply on the ZOE-M8Q, the `$PCAS` probe /
//! constellations / sentence selection / navigation mode on the L76K,
//! the `$PAIR` probe / constellations / sentence selection on the AG3335 —
//! so a persisted Meshtastic-era module configuration cannot survive
//! into our runtime, and acquisition never depends on a field module's
//! factory defaults. All sequences are derived from the Meshtastic
//! reference (`meshtastic/src/gps/ubx.h`, `GPS.cpp`) and live
//! host-tested in the pure crate with the citations; this task only
//! writes the frames and logs `[GNSS_INIT]` lines. No sequence
//! contains a reset or a baud command: a forced cold start would wipe
//! the module's assistance data on every boot, and a baud command would
//! desynchronise the line the driver is talking on. The UBX clear does
//! reset the module's I/O system, after which it may fall back to its
//! default baud — the presence machine's sentence-starvation re-sweep
//! recovers the line, no special-casing here.
//!
//! Two things that are not sentences and therefore live here rather
//! than in the pure crate: the module's **standby pin**, held awake for
//! the life of the task on a board that has one (the L76K parks in
//! low power otherwise — the same #324 lesson, one layer down), and the
//! **shared supply rail**, which the task waits out rather than
//! switching, because on the T114 the display sits on it too
//! ([`crate::vext`]).
//!
//! **Sleep while still** (#457 step two). On a board that publishes a
//! motion state ([`leviculum_firmware::board_state::MOTION_STATE`], the T1000-E's
//! accelerometer) and whose receiver has a power enable, the task selects
//! between its UART read and the motion watch, and the pure
//! [`leviculum_gnss_presence::PowerGate`] decides: `still` sleeps the
//! receiver, `moving` wakes it, and nothing sleeps before the first fix
//! or [`leviculum_gnss_presence::GNSS_FIRST_FIX_GRACE_S`]. Sleep is the
//! AG3335's Backup mode as the Quectel LC29H hardware design enters it:
//! `$PAIR650,0`, then the enable low while the board holds VRTC up; wake
//! is the enable high and a pulse on the RTC wake line, then the
//! `$PAIR` setup again. Each edge prints `GNSS_POWER state=sleep|wake
//! reason=still|moving t=<ms>`. While asleep the task neither reads nor
//! polls, so the `[GNSS]` heartbeat stops, and the published presence and
//! fix hold their last values: the board has not moved, so the last
//! position is still its position. Every other board, and a T1000-E whose
//! part did not answer, never gets a motion value and runs as before.
//!
//! The PPS pin (P1.04 on the T114) is configured as a pull-down input
//! but not used — reserved for a future timestamp-capture iteration.
//! Three of the four boards pass `pps: None`, and none of them means
//! "there is a pulse here we are ignoring". The T1000-E's variant names
//! no pulse line for the AG3335. The Solar
//! Node breaks no pulse line out at all: an absent output is not a quiet
//! one, and the pad that would carry it on a XIAO is that board's
//! battery ADC. The WisMesh Pocket V2 has the output but no wire — the
//! RAK19026 VC leaves the ZOE-M8Q's TIMEPULSE on an unfitted 0 ohm link,
//! and the pin the driver used to hold instead belongs to the LIS3DH's
//! interrupt (Codeberg #394; the trail is in
//! [`crate::boards::rak4631`]).

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{AnyPin, Input, Level, OutputDrive, Pull};
use embassy_nrf::peripherals;
use embassy_nrf::uarte::{self, Uarte};
use embassy_nrf::{bind_interrupts, Peri};
use embassy_time::{with_timeout, Duration, Instant, Timer};

use leviculum_gnss_init::{AirohaInit, L76kInit, ModuleInit, UbxInit};
use leviculum_gnss_presence::{Output, Power, PowerEdge, PowerGate, Presence, PresenceMachine};

use leviculum_firmware::board_state::{GnssFix, GnssPresenceState, GNSS_FIX, GNSS_PRESENCE};

/// Which receiver is on the other end of the UART, and therefore which
/// command language the one-shot boot init speaks. The board knows; the
/// rest of the driver does not care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleKind {
    /// u-blox ZOE-M8Q (WisMesh Pocket V2), UBX binary protocol.
    UbloxM8,
    /// Quectel L76K, CASIC `$PCAS` sentences. On the Heltec Mesh Node
    /// T114 as a bare module, on the SenseCAP Solar Node as Seeed's
    /// XIAO L76K carrier — the same part, the same command language,
    /// the same probe answer, so one variant serves both.
    QuectelL76k,
    /// Airoha AG3335 (SenseCAP T1000-E), `$PAIR` sentences (#456).
    AirohaAg3335,
}

/// The board-specific half of the driver: which peripherals and pins
/// carry the receiver, and which module language it speaks.
pub struct GnssWiring {
    pub uarte: Peri<'static, peripherals::UARTE0>,
    /// Idle-line detection for `read_until_idle`.
    pub timer: Peri<'static, peripherals::TIMER1>,
    /// RXDRDY → timer clear/start.
    pub ppi_a: Peri<'static, peripherals::PPI_CH0>,
    /// Timer compare → RX stop.
    pub ppi_b: Peri<'static, peripherals::PPI_CH1>,
    /// RX at the MCU: the module's TX line.
    pub rx: Peri<'static, AnyPin>,
    /// TX at the MCU: the module's RX line.
    pub tx: Peri<'static, AnyPin>,
    /// PPS, configured and held but not used. `None` on a board that
    /// breaks no pulse line out — the SenseCAP Solar Node exposes the
    /// L76K's wakeup, reset and enable only, which makes it an
    /// NMEA-only time source (#166, #233).
    pub pps: Option<Peri<'static, AnyPin>>,
    /// Standby control, driven HIGH for the life of the task to keep the
    /// module awake (L76K: LOW would allow sleep). `None` on a board
    /// whose receiver has no such pin.
    pub standby: Option<Peri<'static, AnyPin>>,
    /// The receiver's own power enable, driven HIGH for the life of the
    /// task. `None` where the module has no switch of its own — either
    /// because it is permanently powered (RAK19026, whose 3V3-S rail the
    /// binary raises for four peripherals at once) or because its supply
    /// is a shared rail the binary owns (T114: VEXT, see
    /// [`crate::vext`]). The SenseCAP Solar Node is the board where the
    /// switch belongs to the receiver alone, so it belongs here rather
    /// than at board level: nothing else on that carrier goes dark with
    /// it.
    pub power_enable: Option<Peri<'static, AnyPin>>,
    /// The receiver's wake-from-Backup line, already driven LOW by the
    /// board, pulsed HIGH for 20 ms (`WAKE_PULSE_MS`) each time the task powers
    /// the receiver up after a sleep. Only the T1000-E has one (the
    /// AG3335's `GPS_RTC_INT`, "wake by HIGH"); `None` elsewhere.
    pub wake: Option<embassy_nrf::gpio::Output<'static>>,
    pub module: ModuleKind,
}

/// After the Backup-mode command, before the enable drops, in ms. The
/// LC29H hardware design (1.3, §3.3.3, figure 9) cuts VCC after the
/// command and names no interval; the module answers `$PAIR001,650,0`
/// within a sentence time at 115200. 100 ms covers that answer many
/// times over and is nothing against a sleep of at least 30 s.
const BACKUP_ENTRY_MS: u64 = 100;

/// After the enable rises, before the wake pulse, in ms. The same
/// document's power-up sequence (§3.4, figure 10) has VCC rise in under
/// 50 ms, and its Backup exit pulls WAKEUP once VCC is back.
const SUPPLY_RISE_MS: u64 = 50;

/// The wake pulse, in ms: WAKEUP high "for at least 10 ms" (§3.3.3,
/// note 2), doubled.
const WAKE_PULSE_MS: u64 = 20;

impl ModuleKind {
    /// The command that parks the receiver before its power enable drops,
    /// if its language has one this driver sends. Only the AG3335's
    /// (`leviculum_gnss_init::airoha::BACKUP_FRAME` says why); the other
    /// receivers sit on boards without an accelerometer and never sleep.
    fn backup_frame(self) -> Option<&'static [u8]> {
        match self {
            ModuleKind::AirohaAg3335 => Some(&leviculum_gnss_init::airoha::BACKUP_FRAME),
            ModuleKind::UbloxM8 | ModuleKind::QuectelL76k => None,
        }
    }
}

/// Print a power edge: `GNSS_POWER state=sleep|wake reason=still|moving
/// t=<ms>`, the renderer's stamp. Critical: an edge is rare, nothing repeats it.
fn log_power(edge: PowerEdge) {
    crate::log::log_fmt_critical("GNSS_POWER ", format_args!("{edge}"));
}

/// The board's boot init sequencer. An enum rather than a `dyn` object
/// so the state lives in the task's own frame; the delegation is what
/// makes [`ModuleInit`] the single surface the loop below drives.
enum Init {
    Ubx(UbxInit),
    L76k(L76kInit),
    Airoha(AirohaInit),
}

impl Init {
    fn new(module: ModuleKind) -> Self {
        match module {
            ModuleKind::UbloxM8 => Init::Ubx(UbxInit::new()),
            ModuleKind::QuectelL76k => Init::L76k(L76kInit::new()),
            ModuleKind::AirohaAg3335 => Init::Airoha(AirohaInit::new()),
        }
    }

    fn as_mut(&mut self) -> &mut dyn ModuleInit {
        match self {
            Init::Ubx(i) => i,
            Init::L76k(i) => i,
            Init::Airoha(i) => i,
        }
    }
}

/// Act on the init sequencer's outputs: stage the frame into RAM (UBX
/// frames are flash consts, EasyDMA reads RAM only) and write it, then
/// log the banner-class `[GNSS_INIT]` line the board smoke check greps
/// for. TX errors are logged, never retried — the sequencer's ACK
/// handling makes a lost frame visible as `ack=timeout`.
async fn apply_init_output(output: leviculum_gnss_init::Output, uart_tx: &mut uarte::UarteTx<'_>) {
    match output {
        leviculum_gnss_init::Output::Send { step, frame } => {
            let mut staged = [0u8; leviculum_gnss_init::MAX_FRAME];
            staged[..frame.len()].copy_from_slice(frame);
            let verb = if uart_tx.write(&staged[..frame.len()]).await.is_ok() {
                "sent"
            } else {
                "tx-error"
            };
            crate::log::log_fmt_critical(
                "[INFO!] ",
                format_args!("[GNSS_INIT] step={} {}", step, verb),
            );
        }
        leviculum_gnss_init::Output::AckResult { step, outcome } => {
            crate::log::log_fmt_critical(
                "[INFO!] ",
                format_args!("[GNSS_INIT] step={} ack={}", step, outcome.as_str()),
            );
        }
    }
}

bind_interrupts!(pub struct GnssIrqs {
    UARTE0 => uarte::InterruptHandler<peripherals::UARTE0>;
});

/// Map a sweep baud to the UARTE register value. The machine only emits
/// values from `BAUD_SWEEP`; the catch-all keeps this total without an
/// `unwrap`.
fn baudrate_of(baud: u32) -> uarte::Baudrate {
    match baud {
        38_400 => uarte::Baudrate::BAUD38400,
        115_200 => uarte::Baudrate::BAUD115200,
        _ => uarte::Baudrate::BAUD9600,
    }
}

/// Act on one machine output: publish a transition (watch + debug
/// event), record a requested baud change, or fold RMC/GGA content into
/// the `GnssFix` snapshot (`GnssFix::fold_rmc` and `fold_gga`,
/// host-tested in `leviculum-gnss-time`: `unix_secs` exists only while a
/// valid RMC does, so the #166 seed gate stays keyed to valid RMC only).
fn apply_output(output: Output, latest: &mut GnssFix, pending_baud: &mut Option<u32>) {
    let sender = GNSS_FIX.sender();
    match output {
        Output::Transition { state, baud } => {
            GNSS_PRESENCE
                .sender()
                .send(GnssPresenceState { state, baud });
            // Banner-class state event, same replay semantics as
            // `[TIME_SOURCE]`: bypasses the runtime-drain gate, last
            // line wins. Rate is bounded by the hysteresis hold and
            // duplicate-suppression in the machine.
            crate::log::log_fmt_critical(
                "[INFO!] ",
                format_args!("[GNSS_PRESENCE] state={} baud={}", state.as_str(), baud),
            );
        }
        Output::SetBaud(baud) => *pending_baud = Some(baud),
        Output::Rmc(rmc) => {
            latest.fold_rmc(&rmc);
            sender.send(*latest);
        }
        Output::Gga(gga) => {
            latest.fold_gga(&gga);
            sender.send(*latest);
        }
    }
}

/// Pump UART bytes through the presence machine; publish presence
/// transitions via `GNSS_PRESENCE` and fix snapshots via `GNSS_FIX`.
#[embassy_executor::task]
pub async fn gnss_task(wiring: GnssWiring) {
    let GnssWiring {
        mut uarte,
        mut timer,
        mut ppi_a,
        mut ppi_b,
        mut rx,
        mut tx,
        pps,
        standby,
        power_enable,
        mut wake,
        module,
    } = wiring;

    // Hold the PPS pin low-impedance enough that no spurious capture fires
    // before we wire it up. Drop returns it to its reset state on task exit
    // (which never happens for this task, but the convention is clear).
    // Nothing is configured at all on a board that has no such line: an
    // absent pulse output is not the same as a quiet one, and a pin this
    // board gave to something else must not be driven from here.
    let _pps = pps.map(|pin| Input::new(pin, Pull::Down));

    // Power first. Held for the life of the task for the same reason the
    // standby line below is: dropping the output returns the pin to its
    // reset state, which cuts the receiver's supply. Lowered and raised
    // again only by the motion gate below.
    let mut power_enable = power_enable.map(|pin| {
        crate::log::log_fmt("[GNSS] ", format_args!("power enable held high"));
        embassy_nrf::gpio::Output::new(pin, Level::High, OutputDrive::Standard)
    });

    // Force the module awake and keep it that way. A receiver parked in
    // standby by whatever firmware ran before ours streams nothing, and
    // the presence machine would report `no-hardware` on perfectly good
    // wiring — the #324 failure mode with a different cause. Held for
    // the life of the task: dropping it returns the pin to its reset
    // state, which on the T114 is "sleep allowed".
    let _standby = standby.map(|pin| {
        crate::log::log_fmt("[GNSS] ", format_args!("standby pin held high (wake)"));
        embassy_nrf::gpio::Output::new(pin, Level::High, OutputDrive::Standard)
    });

    // The receiver's supply may be a rail the binary raised for several
    // peripherals at once (T114: VEXT, shared with the display). Waiting
    // for its warmup is free on a board without one.
    crate::vext::wait_ready().await;

    let mut machine = PresenceMachine::new(Instant::now().as_millis());

    // The motion gate (#457 step two): asleep while the board is still,
    // awake while it moves, never asleep before the first fix or
    // `GNSS_FIRST_FIX_GRACE_S`. Only a receiver this task can switch off
    // listens to the accelerometer, and on a board without one (or whose
    // part did not answer) the watch never gets a value, the gate never
    // leaves `Awake`, and this task runs exactly as it did before.
    let mut motion_rx = if power_enable.is_some() {
        leviculum_firmware::board_state::MOTION_STATE.receiver()
    } else {
        None
    };
    let mut gate = PowerGate::new(Instant::now().as_millis());

    // One-shot module init (#324, #69): constructed once per boot,
    // silent until the machine locks a baud, silent again forever once
    // done.
    let mut init = Init::new(module);

    // Rolling GnssFix snapshot across sentences. RMC owns "is the
    // receiver happy" (mode is_valid()); GGA owns "how many sats".
    let mut latest = GnssFix::empty();
    let mut pending_baud: Option<u32> = None;

    let mut bytes_total: u32 = 0;
    let mut uart_errors: u32 = 0;
    let mut last_health_log = Instant::now();

    let mut configured_baud = machine.current_baud();
    crate::log::log_fmt(
        "[GNSS] ",
        format_args!("UARTE0 up, sweep start @ {} 8N1", configured_baud),
    );

    // Outer loop: one iteration per UART configuration. The machine
    // requests baud changes through `Output::SetBaud`; recreating the
    // Uarte from reborrowed peripherals is the supported embassy way to
    // change the baud rate (and re-derives the idle timeout from it).
    loop {
        let mut config = uarte::Config::default();
        config.baudrate = baudrate_of(configured_baud);
        let uart = Uarte::new(
            uarte.reborrow(),
            rx.reborrow(),
            tx.reborrow(),
            GnssIrqs,
            config,
        );
        let (mut uart_tx, mut uart_rx) =
            uart.split_with_idle(timer.reborrow(), ppi_a.reborrow(), ppi_b.reborrow());

        // 256-byte chunk: a full 1 Hz NMEA burst (RMC+GGA+GSA — the
        // GSV tail may split) fits, and the idle detector ends each
        // read at the burst boundary anyway. The 1 s timeout exists to
        // carry time into the machine on a silent line (detection
        // windows, fix hold); when data flows, idle completes the read
        // long before it fires, so the cancel-loses-bytes race is
        // confined to near-silent lines where there is nothing to lose.
        let mut buf = [0u8; 256];
        let new_baud = loop {
            let now_ms = Instant::now().as_millis();
            let mut chunk_len = 0usize;
            let read = with_timeout(Duration::from_secs(1), uart_rx.read_until_idle(&mut buf));
            // A motion edge ends the read early. The bytes a cancelled
            // read held are lost, as on a timeout; the edge is rare.
            let (read, mut edge) = match motion_rx.as_mut() {
                Some(rx) => match select(read, rx.changed()).await {
                    Either::First(read) => (Some(read), None),
                    Either::Second(motion) => {
                        (None, gate.on_motion(motion, Instant::now().as_millis()))
                    }
                },
                None => (Some(read.await), None),
            };
            match read {
                None => {}
                Some(Ok(Ok(n))) => {
                    bytes_total = bytes_total.saturating_add(n as u32);
                    chunk_len = n;
                    machine.on_bytes(&buf[..n], now_ms, &mut |o| {
                        apply_output(o, &mut latest, &mut pending_baud)
                    });
                }
                Some(Ok(Err(_e))) => {
                    // Framing/overrun. At a wrong sweep baud this can be
                    // every chunk — count it (the heartbeat reports it)
                    // instead of logging per error, and give the EasyDMA
                    // a moment instead of hot-looping.
                    uart_errors = uart_errors.saturating_add(1);
                    machine.on_uart_error(now_ms, &mut |o| {
                        apply_output(o, &mut latest, &mut pending_baud)
                    });
                    Timer::after(Duration::from_millis(50)).await;
                }
                Some(Err(_timeout)) => {
                    machine.poll(now_ms, &mut |o| {
                        apply_output(o, &mut latest, &mut pending_baud)
                    });
                }
            }

            // Heartbeat log every 5 s with cumulative counters. Keeps
            // the debug log readable but proves the GNSS pipe is alive.
            // `sat=` is GGA satellites-in-USE, 0 on most receivers without a fix;
            // `sv=`/`cno=` are the GSV in-VIEW count and best C/N0 in
            // dBHz (#324) and move as soon as the antenna hears anything
            // — that is what separates a config problem from a weak RF
            // path while `valid=false`. `cno=0` means "no C/N0 reported
            // yet", which a tracked satellite never is.
            if last_health_log.elapsed().as_secs() >= 5 {
                crate::log::log_fmt(
                    "[GNSS] ",
                    format_args!(
                        "bytes={} sentences={} errs={} valid={} sat={} sv={} cno={} baud={}",
                        bytes_total,
                        machine.sentences_seen(),
                        uart_errors,
                        latest.valid,
                        latest.sat_in_use,
                        machine.sv_in_view(),
                        machine.cno_best().unwrap_or(0),
                        configured_baud,
                    ),
                );
                last_health_log = Instant::now();
            }

            // The first fix ends the grace early; time alone ends it too.
            let gate_now = Instant::now().as_millis();
            if machine.published() == Some(Presence::Fix) {
                edge = edge.or(gate.on_fix(gate_now));
            }
            edge = edge.or(gate.poll(gate_now));
            if let (Some(asleep), Some(rx)) = (edge, motion_rx.as_mut()) {
                if asleep.power == Power::Asleep {
                    log_power(asleep);
                    // Park, then cut: the LC29H hardware design's way into
                    // Backup mode, VRTC (the board's) staying up so the
                    // wake is a warm start.
                    if let Some(frame) = module.backup_frame() {
                        let mut staged = [0u8; leviculum_gnss_init::MAX_FRAME];
                        staged[..frame.len()].copy_from_slice(frame);
                        let _ = uart_tx.write(&staged[..frame.len()]).await;
                        Timer::after_millis(BACKUP_ENTRY_MS).await;
                    }
                    if let Some(pin) = power_enable.as_mut() {
                        pin.set_low();
                    }
                    // Nothing to read and nothing to poll until the board
                    // moves: the heartbeat stops with the receiver.
                    loop {
                        let motion = rx.changed().await;
                        let now = Instant::now().as_millis();
                        if let Some(awake) = gate.on_motion(motion, now) {
                            log_power(awake);
                            break;
                        }
                    }
                    if let Some(pin) = power_enable.as_mut() {
                        pin.set_high();
                    }
                    if let Some(pin) = wake.as_mut() {
                        Timer::after_millis(SUPPLY_RISE_MS).await;
                        pin.set_high();
                        Timer::after_millis(WAKE_PULSE_MS).await;
                        pin.set_low();
                    }
                    // The receiver wakes at the baud it slept at, so the
                    // lock stands; the dark time is neither starvation nor
                    // a lost fix. The `$PAIR` setup goes out again: the
                    // module's answers to it are not read, so whether it
                    // kept the settings across the cut cannot be told, and
                    // the sequence is the same one every boot sends.
                    machine.resume(Instant::now().as_millis());
                    init = Init::new(module);
                    continue;
                }
            }

            if let Some(b) = pending_baud.take() {
                if b != configured_baud {
                    break b;
                }
            }

            // Init runs strictly after the baud decision above, so a
            // frame is only ever written at the rate the machine locked
            // (a UBX frame at the wrong baud is garbage into the
            // module). The emit callback cannot await, so outputs are
            // staged and applied afterwards; the sequencer emits at
            // most one Send and one AckResult per iteration.
            let mut staged: [Option<leviculum_gnss_init::Output>; 4] = [None; 4];
            let mut staged_n = 0usize;
            {
                let mut emit = |o: leviculum_gnss_init::Output| {
                    if staged_n < staged.len() {
                        staged[staged_n] = Some(o);
                        staged_n += 1;
                    }
                };
                let init_now = Instant::now().as_millis();
                let init = init.as_mut();
                init.on_bytes(&buf[..chunk_len], init_now, &mut emit);
                init.poll(
                    machine.locked(),
                    machine.sentences_seen(),
                    init_now,
                    &mut emit,
                );
            }
            for slot in staged.iter_mut().take(staged_n) {
                if let Some(o) = slot.take() {
                    apply_init_output(o, &mut uart_tx).await;
                }
            }
        };
        configured_baud = new_baud;
    }
}

/// Convenience wrapper invoked from the bin file.
pub fn init(spawner: &Spawner, wiring: GnssWiring) {
    spawner.must_spawn(gnss_task(wiring));
}
