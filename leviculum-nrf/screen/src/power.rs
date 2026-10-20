//! The display's power decisions: what a button press asks for, and what
//! the display task does with a request (leviculum#413, rows 369 and 374 of
//! `docs/src/firmware/host-test-seams.md`).
//!
//! `leviculum-nrf/src/button.rs` times the press and `display.rs` drives the
//! panel; what they decide is here, so the press rule, the one-minute
//! timeout and the I2C error rule are host tests instead of a thumb on a
//! board.

/// How long a press keeps the display lit (`docs/src/concepts/one-button.md`:
/// the status press wakes it for one minute). Long enough to read the
/// status frame, which changes once a second, a few times over; short
/// enough that a display nobody looks at stops burning the battery within
/// the minute. There is no press that turns the display off: it goes dark
/// on its own this long after the last press.
pub const DISPLAY_WAKE_MS: u64 = 60_000;

/// The display state a press asks for, or `None` when it asks for no
/// change: any press turns a dark display on, and a lit one stays lit
/// (its timeout restarts, [`DisplayWake::press`]).
pub const fn press_target(currently_on: bool) -> Option<bool> {
    if currently_on {
        None
    } else {
        Some(true)
    }
}

/// When the display goes dark on its own: [`DISPLAY_WAKE_MS`] after the
/// last press, or after boot when nobody has pressed yet (the display
/// boots lit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayWake {
    dark_at_ms: u64,
}

impl DisplayWake {
    /// A display lit at `boot_ms`.
    #[must_use]
    pub const fn new(boot_ms: u64) -> Self {
        Self {
            dark_at_ms: boot_ms.saturating_add(DISPLAY_WAKE_MS),
        }
    }

    /// A press at `now_ms` on a display that is `currently_on`: the
    /// timeout restarts, and the request is [`press_target`]'s.
    pub fn press(&mut self, now_ms: u64, currently_on: bool) -> Option<bool> {
        self.dark_at_ms = now_ms.saturating_add(DISPLAY_WAKE_MS);
        press_target(currently_on)
    }

    /// `Some(false)` once a lit display has gone [`DISPLAY_WAKE_MS`]
    /// without a press, `None` otherwise.
    #[must_use]
    pub const fn timeout(&self, now_ms: u64, currently_on: bool) -> Option<bool> {
        if currently_on && now_ms >= self.dark_at_ms {
            Some(false)
        } else {
            None
        }
    }
}

/// Apply a power `request` to a display whose state is `*on`: `None` without
/// a change, so the panel is not commanded at all; otherwise the result of
/// `set`, with `*on` following the request only when `set` succeeded. A
/// failed write keeps the old state, so the next frame tick still knows
/// what the panel shows.
pub fn apply_power<E>(
    request: bool,
    on: &mut bool,
    set: impl FnOnce(bool) -> Result<(), E>,
) -> Option<Result<(), E>> {
    if request == *on {
        return None;
    }
    let result = set(request);
    if result.is_ok() {
        *on = request;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_press_turns_a_dark_display_on_and_none_turns_a_lit_one_off() {
        assert_eq!(press_target(false), Some(true));
        assert_eq!(press_target(true), None);
    }

    #[test]
    fn a_lit_display_goes_dark_a_minute_after_boot_without_a_press() {
        let wake = DisplayWake::new(500);
        assert_eq!(wake.timeout(500 + DISPLAY_WAKE_MS - 1, true), None);
        assert_eq!(wake.timeout(500 + DISPLAY_WAKE_MS, true), Some(false));
        assert_eq!(
            wake.timeout(500 + DISPLAY_WAKE_MS, false),
            None,
            "a dark display is not asked to go dark again"
        );
    }

    #[test]
    fn a_press_restarts_the_minute() {
        let mut wake = DisplayWake::new(0);
        assert_eq!(wake.press(50_000, true), None, "lit stays lit");
        assert_eq!(wake.timeout(DISPLAY_WAKE_MS, true), None);
        assert_eq!(wake.timeout(50_000 + DISPLAY_WAKE_MS - 1, true), None);
        assert_eq!(wake.timeout(50_000 + DISPLAY_WAKE_MS, true), Some(false));
    }

    #[test]
    fn a_press_on_a_dark_display_lights_it_for_a_minute() {
        let mut wake = DisplayWake::new(0);
        assert_eq!(wake.timeout(DISPLAY_WAKE_MS, true), Some(false));
        let at = 200_000;
        assert_eq!(wake.press(at, false), Some(true));
        assert_eq!(wake.timeout(at + DISPLAY_WAKE_MS - 1, true), None);
        assert_eq!(wake.timeout(at + DISPLAY_WAKE_MS, true), Some(false));
    }

    #[test]
    fn an_unchanged_request_does_not_command_the_panel() {
        let mut on = true;
        let r = apply_power(true, &mut on, |_| -> Result<(), ()> {
            panic!("an unchanged request reached the panel")
        });
        assert_eq!(r, None);
        assert!(on);
    }

    #[test]
    fn a_changed_request_follows_the_panel_write() {
        let mut on = true;
        let mut asked = None;
        let r = apply_power(false, &mut on, |v| -> Result<(), ()> {
            asked = Some(v);
            Ok(())
        });
        assert_eq!(r, Some(Ok(())));
        assert_eq!(asked, Some(false));
        assert!(!on);
    }

    #[test]
    fn a_failed_panel_write_keeps_the_old_state() {
        let mut on = false;
        let r = apply_power(true, &mut on, |_| Err("i2c nack"));
        assert_eq!(r, Some(Err("i2c nack")));
        assert!(!on, "the panel still shows what it showed");
    }
}
