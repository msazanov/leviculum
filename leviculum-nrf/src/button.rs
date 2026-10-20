//! The user button, on every board that has one (Codeberg #459).
//!
//! A short press sends the board's own announces; what counts as a press
//! and how often one may announce is decided in
//! [`leviculum_announce_policy::ButtonGate`], and this task only times the
//! edges, writes the `BTN_PRESS` line and raises
//! [`ANNOUNCE_PRESS`](crate::button::ANNOUNCE_PRESS). The announces
//! themselves go out from the node loop, which owns the node
//! ([`crate::announce::on_button_press`]).
//!
//! | Board | Pin | Wiring | Long press |
//! |---|---|---|---|
//! | WisMesh Pocket V2 | P0.09 (NFC1, freed by the RAK bootloader's UICR patch) | active low, internal pull-up | display on |
//! | T1000-E | P0.06 | active high, internal pull-down (`boards/t1000e.rs`) | unassigned (the power button) |
//!
//! The T114 and the Solar Node bring up no button and say `[BTN] none`.
//!
//! # The Pocket V2's display
//!
//! The same press also drives the display, through the same debouncer:
//!
//! | Display state | Press duration | Display |
//! |---|---|---|
//! | on  | any           | off |
//! | off | < 2 s         | stays off |
//! | off | ≥ 2 s         | on |
//!
//! A short press in the user's pocket can't accidentally wake the screen
//! and burn battery; turning it off is one quick tap. The short press
//! announces either way, the long one never does
//! ([`leviculum_announce_policy::LONG_PRESS_MIN_MS`]), so on this board one
//! press means one thing. The power-cycling itself happens in
//! `display::display_task`, which subscribes to `DISPLAY_ON_REQ`.

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{AnyPin, Input, Pin, Pull};
use embassy_nrf::Peri;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Instant, Timer};
use leviculum_announce_policy::{ButtonGate, PressAction, BUTTON_DEBOUNCE_MS};

/// Raised once per press that announces; the node loop waits on it beside
/// its other inputs. A `Signal` rather than a channel because the rate
/// limit already ran: two raises the loop has not consumed yet would be
/// two presses inside one window, which the gate never lets through.
pub static ANNOUNCE_PRESS: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// One display rule for the Pocket: the display turns on at the length a
/// press stops announcing at.
#[cfg(feature = "display")]
const _: () = assert!(
    leviculum_screen::power::LONG_PRESS_MIN_MS == leviculum_announce_policy::LONG_PRESS_MIN_MS
);

/// How a board's button is wired.
pub struct ButtonWiring {
    /// The pin.
    pub pin: Peri<'static, AnyPin>,
    /// `true` when a press pulls the pin high (the T1000-E), `false` when it
    /// pulls it low (the Pocket V2). The internal pull goes the other way.
    pub active_high: bool,
    /// The `board=` token of the `BTN_PRESS` line.
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
    #[cfg(feature = "display")]
    let display_tx = crate::baseboard::DISPLAY_ON_REQ.sender();
    // Receiver here is just for the "what's the current state?" lookup
    // when classifying a press. We never block on changes from this side.
    #[cfg(feature = "display")]
    let Some(mut display_rx) = crate::baseboard::DISPLAY_ON_REQ.receiver() else {
        crate::log::log_fmt_critical("[BTN] ", format_args!("no DISPLAY_ON_REQ receiver left"));
        return;
    };

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
        let press = match woke {
            Either::First(()) => {
                gate.edge(now_ms, pressed);
                continue;
            }
            Either::Second(()) => gate.settle(now_ms, pressed),
        };
        let Some(press) = press else {
            continue;
        };
        crate::log::log_fmt_critical(
            "",
            format_args!(
                "BTN_PRESS board={} action={} held_ms={} t={}",
                board,
                press.action.as_str(),
                press.held_ms,
                now_ms
            ),
        );
        if press.action == PressAction::Announce {
            ANNOUNCE_PRESS.signal(());
        }

        // Which presses switch the display: `leviculum_screen::power::press_target`.
        // A debounced glitch was never a press.
        #[cfg(feature = "display")]
        if press.action != PressAction::Debounced {
            // Treat first-ever press (Watch never sent yet) as if the
            // display is on — that's the boot state we expect.
            let currently_on = display_rx.try_get().unwrap_or(true);
            match leviculum_screen::power::press_target(currently_on, press.held_ms) {
                Some(target) => {
                    crate::log::log_fmt(
                        "[BTN] ",
                        format_args!(
                            "press {} ms → display {}",
                            press.held_ms,
                            if target { "on" } else { "off" }
                        ),
                    );
                    display_tx.send(target);
                }
                None => crate::log::log_fmt(
                    "[BTN] ",
                    format_args!("short press leaves the display off ({} ms)", press.held_ms),
                ),
            }
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
