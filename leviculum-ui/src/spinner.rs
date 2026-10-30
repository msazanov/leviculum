//! The loading spinner: six braille frames, a single dot circling, in a
//! colour that glides through the HSV hue wheel.
//!
//! Terminal-free: a frame is a glyph and an RGB triple, which
//! `leviculum-ui-ratatui` turns into a styled span. The delivery glyph table
//! ([`crate::delivery`]) animates with these same frames.

/// The six braille glyphs of the loading spinner: a single dot circling once per
/// full cycle. `spin % 6` picks the current frame.
pub const SPIN_FRAMES: [&str; 6] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴"];

/// Degrees of hue the spinner colour advances per animation tick. The colour
/// glides continuously through the HSV wheel rather than snapping between a few
/// fixed rainbow stops. At lnomad's 120 ms redraw cadence (its
/// `SPINNER_TICK_MS`) a full 360 degree rainbow takes `360 / HUE_STEP` ticks =
/// 40 ticks = 4.8 s, and each tick shifts the hue by only 9 degrees, so the change reads as a smooth glide
/// with no flicker.
pub const HUE_STEP: f32 = 9.0;

/// Convert an HSV colour to 8-bit RGB. Pure function. `h_deg` is the hue in
/// degrees (wrapped into `[0, 360)`), `s` and `v` are the saturation and value in
/// `[0, 1]`. Uses the standard sextant formulation; the spinner calls it with
/// `s = v = 1.0` for maximally vivid, full-brightness colours. With `std`
/// only: core carries no float rounding.
#[cfg(feature = "std")]
pub fn hsv_to_rgb(h_deg: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = h_deg.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let sextant = h / 60.0;
    let x = c * (1.0 - (sextant.rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r1, g1, b1) = match sextant as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to_u8 = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
}

/// The glyph for animation step `spin`: the frames cycle once per six steps.
pub fn glyph(spin: usize) -> &'static str {
    SPIN_FRAMES[spin % SPIN_FRAMES.len()]
}

/// The colour for animation step `spin`, fully saturated and at full
/// brightness: the hue advances [`HUE_STEP`] degrees a step, so the colour
/// glides much more slowly than the dot circles.
#[cfg(feature = "std")]
pub fn color(spin: usize) -> (u8, u8, u8) {
    let hue = (spin as f32 * HUE_STEP).rem_euclid(360.0);
    hsv_to_rgb(hue, 1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_to_rgb_known_conversions() {
        // The primary and secondary hues at full saturation and value.
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), (255, 0, 0), "h=0 -> red");
        assert_eq!(hsv_to_rgb(60.0, 1.0, 1.0), (255, 255, 0), "h=60 -> yellow");
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), (0, 255, 0), "h=120 -> green");
        assert_eq!(hsv_to_rgb(180.0, 1.0, 1.0), (0, 255, 255), "h=180 -> cyan");
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), (0, 0, 255), "h=240 -> blue");
        assert_eq!(
            hsv_to_rgb(300.0, 1.0, 1.0),
            (255, 0, 255),
            "h=300 -> magenta"
        );
        // Hue wraps: 360 == 0, and negatives fold back into range.
        assert_eq!(
            hsv_to_rgb(360.0, 1.0, 1.0),
            (255, 0, 0),
            "h=360 wraps to red"
        );
        assert_eq!(
            hsv_to_rgb(-60.0, 1.0, 1.0),
            (255, 0, 255),
            "h=-60 -> magenta"
        );
        // s=1, v=1 always yields a fully saturated, full-brightness colour: one
        // channel at 255 and at least one at 0.
        for deg in (0..360).step_by(7) {
            let (r, g, b) = hsv_to_rgb(deg as f32, 1.0, 1.0);
            assert_eq!(r.max(g).max(b), 255, "not full brightness at h={deg}");
            assert_eq!(r.min(g).min(b), 0, "not fully saturated at h={deg}");
        }
    }
}
