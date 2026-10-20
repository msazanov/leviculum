//! The players of the one-button feedback grammar (#459, order 892): the
//! board's LED, its notification LED where it has one, and the T1000-E's
//! buzzer, each playing a [`leviculum_feedback::Pattern`] to its end.
//!
//! The patterns, their timings and the phase a pattern is in at a given
//! millisecond are `leviculum-feedback`'s, host-tested there. This module
//! owns the pins and the three signals that hand a pattern to a player. A
//! pattern raised while another is playing on the same sink replaces it:
//! the player starts the new one from its first step and the rest of the
//! old one is never played.
//!
//! | Board | `sink=led` | `sink=notify` | `sink=buzzer` |
//! |---|---|---|---|
//! | WisMesh Pocket V2 | green LED1, between its TX activity pulses | blue LED2, between its RX activity pulses | none |
//! | T1000-E | the green LED | none, `notify` plays on the LED | P0.25 at `BUZZER_TONE_HZ` |
//! | Heltec T114 | the green LED (active low) | none, `notify` plays on the LED | none |
//! | Solar Node | the green LED | the blue LED | none |
//!
//! Every play writes one line when the player starts it, through the
//! gated path, not the critical one:
//!
//! ```text
//! FEEDBACK pattern=<name> sink=led|notify|buzzer t=<ms>
//! ```
//!
//! A sink a board does not have is never asked, so it writes no line.
//!
//! What raises a pattern: [`status`](crate::feedback::status) (a short
//! press, after the display wake),
//! [`release_now`](crate::feedback::release_now) (a held button reaching
//! 2 s) and [`notify`](crate::feedback::notify) (the propagation role
//! storing a message for this board's own `lxmf.delivery` destination).
//! The mode blink ([`leviculum_feedback::mode`]) has no caller until
//! Tracker mode exists.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::Output;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use leviculum_feedback::{Pattern, Phase, Playback};

/// Where a pattern plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    /// The board's LED.
    Led,
    /// The notification LED (Pocket V2, Solar Node).
    Notify,
    /// The buzzer (T1000-E).
    Buzzer,
}

impl Sink {
    /// The `sink=` token of the `FEEDBACK` line.
    pub fn as_str(self) -> &'static str {
        match self {
            Sink::Led => "led",
            Sink::Notify => "notify",
            Sink::Buzzer => "buzzer",
        }
    }

    fn signal(self) -> &'static Signal<CriticalSectionRawMutex, Pattern> {
        match self {
            Sink::Led => &LED,
            Sink::Notify => &NOTIFY,
            Sink::Buzzer => &BUZZER,
        }
    }

    fn present(self) -> &'static AtomicBool {
        match self {
            Sink::Led => &HAS_LED,
            Sink::Notify => &HAS_NOTIFY,
            Sink::Buzzer => &HAS_BUZZER,
        }
    }
}

// One signal per sink. A `Signal` holds one pending value, and a newer
// pattern overwriting one the player has not taken yet is exactly the
// replacement rule.
static LED: Signal<CriticalSectionRawMutex, Pattern> = Signal::new();
static NOTIFY: Signal<CriticalSectionRawMutex, Pattern> = Signal::new();
static BUZZER: Signal<CriticalSectionRawMutex, Pattern> = Signal::new();

// Set when the sink's player is spawned, so a request to a sink the board
// does not have goes nowhere and writes no line.
static HAS_LED: AtomicBool = AtomicBool::new(false);
static HAS_NOTIFY: AtomicBool = AtomicBool::new(false);
static HAS_BUZZER: AtomicBool = AtomicBool::new(false);

fn raise(sink: Sink, pattern: Pattern) {
    if sink.present().load(Ordering::Relaxed) {
        sink.signal().signal(pattern);
    }
}

/// A short press: the battery in quarters on the LED and the buzzer. A
/// board without a percentage (no `battery` feature, or a pack outside the
/// band its cell count implies, `crate::battery`) plays the error flicker
/// instead: the user asked for the battery and the board cannot say.
pub fn status() {
    #[cfg(feature = "battery")]
    let pct = crate::baseboard::BATTERY_STATE
        .try_get()
        .and_then(|state| state.percent);
    #[cfg(not(feature = "battery"))]
    let pct: Option<u8> = None;
    let pattern = match pct {
        Some(pct) => leviculum_feedback::battery_quarters(pct),
        None => leviculum_feedback::error(),
    };
    raise(Sink::Led, pattern);
    raise(Sink::Buzzer, pattern);
}

/// A held button has reached the long press: one blip on the LED and the
/// buzzer.
pub fn release_now() {
    let pattern = leviculum_feedback::release_now();
    raise(Sink::Led, pattern);
    raise(Sink::Buzzer, pattern);
}

/// A message for the user arrived: one short on the notification LED where
/// the board has one, else on the LED, and one short buzz where there is a
/// buzzer.
pub fn notify() {
    let pattern = leviculum_feedback::notify();
    if HAS_NOTIFY.load(Ordering::Relaxed) {
        raise(Sink::Notify, pattern);
    } else {
        raise(Sink::Led, pattern);
    }
    raise(Sink::Buzzer, pattern);
}

/// Play `pattern` on `sink` through `drive` (`true` = on) until it ends,
/// starting over with any pattern raised on the same sink meanwhile. One
/// `FEEDBACK` line per pattern started. Returns with the sink off.
async fn play(mut pattern: Pattern, sink: Sink, mut drive: impl FnMut(bool)) {
    let signal = sink.signal();
    loop {
        crate::log::log_fmt(
            "",
            format_args!("FEEDBACK pattern={} sink={}", pattern.name, sink.as_str()),
        );
        let playback = Playback::new(pattern, Instant::now().as_millis());
        let replaced = loop {
            let until_ms = match playback.phase(Instant::now().as_millis()) {
                Phase::On { until_ms } => {
                    drive(true);
                    until_ms
                }
                Phase::Off { until_ms } => {
                    drive(false);
                    until_ms
                }
                Phase::Done => {
                    drive(false);
                    break None;
                }
            };
            if let Either::Second(next) =
                select(Timer::at(Instant::from_millis(until_ms)), signal.wait()).await
            {
                break Some(next);
            }
        };
        match replaced {
            Some(next) => pattern = next,
            None => return,
        }
    }
}

/// The Pocket V2's activity pulse, unchanged from `crate::led` before it
/// shared the LEDs: 80 ms on, then 30 ms forced dark so a burst still
/// reads as separate flashes.
const ACTIVITY_ON_MS: u64 = 80;
const ACTIVITY_OFF_MS: u64 = 30;

/// An LED player. `activity`, where given, is the Pocket V2's LoRa TX or
/// RX pulse signal: the LED pulses on it while no pattern plays, and the
/// pulses raised during a pattern are dropped, so a pattern is never cut
/// into by traffic and never followed by a stale flash.
#[embassy_executor::task(pool_size = 2)]
pub async fn led_task(
    mut led: Output<'static>,
    active_high: bool,
    sink: Sink,
    activity: Option<&'static Signal<CriticalSectionRawMutex, ()>>,
) {
    let mut set = move |on: bool| {
        if on == active_high {
            led.set_high();
        } else {
            led.set_low();
        }
    };
    set(false);
    let signal = sink.signal();
    loop {
        let pattern = match activity {
            Some(pulse) => match select(signal.wait(), pulse.wait()).await {
                Either::First(pattern) => pattern,
                Either::Second(()) => {
                    set(true);
                    Timer::after(Duration::from_millis(ACTIVITY_ON_MS)).await;
                    set(false);
                    Timer::after(Duration::from_millis(ACTIVITY_OFF_MS)).await;
                    continue;
                }
            },
            None => signal.wait().await,
        };
        play(pattern, sink, &mut set).await;
        if let Some(pulse) = activity {
            pulse.reset();
        }
    }
}

/// Spawn an LED player for `sink`. `active_high` is the board file's
/// polarity (the T114's LED is the one active low).
pub fn spawn_led(
    spawner: &Spawner,
    led: Output<'static>,
    active_high: bool,
    sink: Sink,
    activity: Option<&'static Signal<CriticalSectionRawMutex, ()>>,
) {
    spawner.must_spawn(led_task(led, active_high, sink, activity));
    sink.present().store(true, Ordering::Relaxed);
}

/// The T1000-E buzzer's tone: B3, 247 Hz, the note Meshtastic's own beep
/// plays on this board (`playBeep`, `NOTE_B3`, `src/buzz/buzz.cpp`; its
/// T1000-E variant drives `PIN_BUZZER` P0.25 as "pwm output" with
/// `tone()`). Neither Seeed nor Meshtastic publishes the transducer's
/// resonance, so the tone is the one this part is known to have sounded in
/// the field; a louder one is a rig measurement, not a guess.
#[cfg(feature = "bsp-t1000e")]
pub const BUZZER_TONE_HZ: u32 = 247;

/// The buzzer player (T1000-E). A square wave at [`BUZZER_TONE_HZ`], 50 %
/// duty, while a step is on. The enable line is raised for the length of a
/// pattern and dropped after it, so the buzzer is unpowered whenever it is
/// silent, as it was before it had a player.
#[cfg(feature = "bsp-t1000e")]
#[embassy_executor::task]
pub async fn buzzer_task(
    mut pwm: embassy_nrf::pwm::SimplePwm<'static>,
    mut enable: Output<'static>,
) {
    use embassy_nrf::pwm::DutyCycle;
    pwm.set_period(BUZZER_TONE_HZ);
    let half = DutyCycle::normal(pwm.max_duty() / 2);
    pwm.disable();
    loop {
        let pattern = BUZZER.wait().await;
        enable.set_high();
        play(pattern, Sink::Buzzer, |on| {
            if on {
                // Enabling alone does not restart the waveform; loading the
                // duty starts the sequence (and waits one period for it).
                pwm.enable();
                pwm.set_duty(0, half);
            } else {
                // Disabled, the pin falls back to its idle level, low.
                pwm.disable();
            }
        })
        .await;
        enable.set_low();
    }
}

/// Spawn the buzzer player (T1000-E).
#[cfg(feature = "bsp-t1000e")]
pub fn spawn_buzzer(
    spawner: &Spawner,
    pwm: embassy_nrf::pwm::SimplePwm<'static>,
    enable: Output<'static>,
) {
    spawner.must_spawn(buzzer_task(pwm, enable));
    HAS_BUZZER.store(true, Ordering::Relaxed);
}
