//! The user button, on every board that has one (Codeberg #459, the
//! gestures of `docs/src/concepts/one-button.md`).
//!
//! What counts as a press, which gesture it is and how often one may
//! announce is decided in [`leviculum_announce_policy::ButtonGate`]; this
//! task only times the edges, writes the `BTN_PRESS` and `BTN_HOLD` lines,
//! raises [`ANNOUNCE_PRESS`](crate::button::ANNOUNCE_PRESS) for a double
//! press, wakes the display where there is one and asks
//! [`crate::feedback`] for the status and "release now" patterns. The announces
//! themselves go out from the node loop, which owns the node
//! ([`crate::announce::on_button_press`]).
//!
//! | Gesture | `action=` | Does |
//! |---|---|---|
//! | short | `status` | wakes the display (Pocket V2), then the battery in quarters on the LED and the buzzer (`crate::feedback`) |
//! | double | `announce` / `rate-limited` | both announces, at most one set per 30 s, then one position report if a recipient is set (`crate::telemetry::on_button_press`) |
//! | long, 2 s | `long` | nothing yet: the mode switch, once Tracker mode exists; the 2 s mark blips "release now" |
//! | very long, 8 s | `hold8` | nothing yet: the power off, once the board can sleep |
//!
//! | Board | Pin | Wiring |
//! |---|---|---|
//! | WisMesh Pocket V2 | P0.09 (NFC1, freed by the RAK bootloader's UICR patch) | active low, internal pull-up |
//! | T1000-E | P0.06 | active high, internal pull-down (`boards/t1000e.rs`) |
//! | Heltec T114 | P1.10 (`boards/t114.rs`) | active low, internal pull-up |
//! | Solar Node | P1.01, the XIAO's program button (`boards/solarnode.rs`) | active low, internal pull-up |
//!
//! # The Pocket V2's display
//!
//! Any press lights a dark display, and the display goes dark on its own
//! a minute after the last press; no press turns it off
//! (`leviculum_screen::power::DisplayWake`). This task raises
//! `DISPLAY_WAKE` on every press; `display::display_task` owns the state
//! and the timeout. The T114's ST7789 has no power control yet and stays
//! lit, as before.

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{AnyPin, Input, Pin, Pull};
use embassy_nrf::Peri;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Instant, Timer};
use leviculum_announce_policy::{
    ButtonEvent, ButtonGate, PressAction, BUTTON_DEBOUNCE_MS, LONG_PRESS_MIN_MS,
};

/// Raised once per double press that announces; the node loop waits on it beside
/// its other inputs. A `Signal` rather than a channel because the rate
/// limit already ran: two raises the loop has not consumed yet would be
/// two presses inside one window, which the gate never lets through.
pub static ANNOUNCE_PRESS: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// How a board's button is wired.
pub struct ButtonWiring {
    /// The pin.
    pub pin: Peri<'static, AnyPin>,
    /// `true` when a press pulls the pin high (the T1000-E), `false` when it
    /// pulls it low (the others). The internal pull goes the other way.
    pub active_high: bool,
    /// The `board=` token of the `BTN_PRESS` and `BTN_HOLD` lines.
    pub board: &'static str,
}

#[embassy_executor::task]
pub async fn button_task(wiring: ButtonWiring) {
    let ButtonWiring {
        pin,
        active_high,
        board,
    } = wiring;
    let (port, pin_no) = (pin.port() as u8, pin.pin());
    let pull = if active_high { Pull::Down } else { Pull::Up };
    let mut button = Input::new(pin, pull);
    // The bring-up line goes through the gate-free path: the task starts
    // before the host attaches, and a gated line written then is dropped,
    // not delayed (Codeberg #296).
    crate::log::log_fmt_critical(
        "[BTN] ",
        format_args!(
            "init pin=P{}.{:02} pull={} active={} debounce={}ms",
            port,
            pin_no,
            if active_high { "down" } else { "up" },
            if active_high { "high" } else { "low" },
            BUTTON_DEBOUNCE_MS
        ),
    );
    let mut gate = ButtonGate::new();
    // The level the gate last heard about. The GPIOTE wait senses the
    // opposite of the level at the moment it arms, so a change between the
    // last read and the arming would only wake us on its way back; reading
    // again right before arming, with no await between, hands that change
    // to the gate as the edge it is.
    let mut seen = false;
    loop {
        let level = button.is_high() == active_high;
        if level != seen {
            gate.edge(Instant::now().as_millis(), level);
        }
        let quiet = async {
            match gate.deadline_ms() {
                Some(at) => Timer::at(Instant::from_millis(at)).await,
                None => core::future::pending::<()>().await,
            }
        };
        let woke = select(button.wait_for_any_edge(), quiet).await;
        let now_ms = Instant::now().as_millis();
        let pressed = button.is_high() == active_high;
        seen = pressed;
        let event = match woke {
            Either::First(()) => {
                gate.edge(now_ms, pressed);
                continue;
            }
            Either::Second(()) => gate.settle(now_ms, pressed),
        };
        let press = match event {
            Some(ButtonEvent::Press(press)) => press,
            // The moment a held button reaches 2 s and 8 s, so the rig can
            // see it. At 2 s the board blips "release now" (order 892); the
            // 8 s mark has no signal until it powers the board off. The
            // gesture itself is the `BTN_PRESS` line at the release. The
            // formatter appends the line's `t=`.
            Some(ButtonEvent::Held { ms }) => {
                crate::log::log_fmt_critical(
                    "",
                    format_args!("BTN_HOLD board={} ms={}", board, ms),
                );
                if ms == LONG_PRESS_MIN_MS {
                    crate::feedback::release_now();
                }
                continue;
            }
            None => continue,
        };
        crate::log::log_fmt_critical(
            "",
            format_args!(
                "BTN_PRESS board={} action={} held_ms={}",
                board,
                press.action.as_str(),
                press.held_ms
            ),
        );
        // Any press lights a dark display and restarts its minute; a
        // debounced glitch was never a press.
        #[cfg(feature = "display")]
        if press.action != PressAction::Debounced {
            crate::baseboard::DISPLAY_WAKE.signal(());
        }

        match press.action {
            // A double press outside the 30 s window: both announces.
            PressAction::Announce => ANNOUNCE_PRESS.signal(()),
            // A short press is status: after the display wake above, the
            // battery in quarters on the LED and the buzzer (order 892).
            // The mode blink joins it once there is a mode.
            PressAction::Status => crate::feedback::status(),
            // Recognised and reported, nothing else: the long press becomes
            // the mode switch in the pass that builds Tracker mode, the very
            // long one the power off in the pass that builds the sleep.
            PressAction::Long | PressAction::Hold8 => {}
            PressAction::RateLimited | PressAction::Debounced => {}
        }
    }
}

/// Spawn the button task for a board that has one.
pub fn init(spawner: &Spawner, wiring: ButtonWiring) {
    spawner.must_spawn(button_task(wiring));
}

/// Wait for a press that announces. For the node loop's select.
pub async fn announce_press() {
    ANNOUNCE_PRESS.wait().await;
}
