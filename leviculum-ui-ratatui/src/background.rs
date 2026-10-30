//! Terminal background detection for `--theme auto`: the OSC 11 query, with
//! the `COLORFGBG` fallback, reduced to the [`Bg`] the pure
//! [`leviculum_ui::theme::resolve_theme`] decides on.

use std::time::Duration;

use leviculum_ui::theme::{Bg, ThemeFlag};

/// How long to wait for the terminal's OSC 11 background reply before giving up
/// and falling back to the dark theme. Kept short so a terminal that never
/// answers does not stall startup.
const BG_QUERY_TIMEOUT: Duration = Duration::from_millis(100);

/// Query the terminal background for `--theme auto`, returning `None` (→ dark)
/// when the flag is explicit, the query fails, or the terminal does not answer.
///
/// The `termbg` crate sends the OSC 11 query (falling back to `COLORFGBG`),
/// parses the reply, and handles the timeout, tmux/screen wrapping and raw-mode
/// save/restore itself. Its 16-bit-per-channel result is reduced to 8-bit; the
/// foreground, when `COLORFGBG` provides one, is a luminance tiebreaker only.
pub fn detect_background(flag: ThemeFlag) -> Option<Bg> {
    if !matches!(flag, ThemeFlag::Auto) {
        return None;
    }
    let bg = termbg::rgb(BG_QUERY_TIMEOUT).ok()?;
    Some(Bg {
        bg: rgb16_to_rgb8(bg),
        fg: colorfgbg_foreground(),
    })
}

/// Reduce a `termbg` 16-bit-per-channel colour to 8-bit by taking the high byte.
fn rgb16_to_rgb8(c: termbg::Rgb) -> (u8, u8, u8) {
    ((c.r >> 8) as u8, (c.g >> 8) as u8, (c.b >> 8) as u8)
}

/// The foreground colour implied by `COLORFGBG` (`"<fg>;<bg>"` of palette
/// indices), as a coarse grey used only as a mid-tone tiebreaker. `None` when
/// the variable is absent or unparsable.
fn colorfgbg_foreground() -> Option<(u8, u8, u8)> {
    let raw = std::env::var("COLORFGBG").ok()?;
    let fg_index: u8 = raw.split(';').next()?.trim().parse().ok()?;
    Some(ansi_index_grey(fg_index))
}

/// A grey whose luminance approximates a 16-colour palette index: the normal
/// set (0..8) reads dark, the bright set (8..16) light. Only the relative
/// luminance matters, so an exact palette is unnecessary.
fn ansi_index_grey(index: u8) -> (u8, u8, u8) {
    let v = match index {
        0 => 0,
        7 => 192,
        8 => 96,
        15 => 255,
        i if i < 8 => 128,
        _ => 224,
    };
    (v, v, v)
}
