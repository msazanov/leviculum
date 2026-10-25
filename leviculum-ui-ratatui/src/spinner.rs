//! The loading spinner as a ratatui span, over [`leviculum_ui::spinner`].

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span as RtSpan;

use leviculum_ui::spinner;

/// Render one frame of the loading spinner: the current braille glyph as a bold,
/// full-brightness span whose colour rotates slowly and continuously through the
/// HSV hue wheel. The glyph advances one step per `spin` (the dot circles) while
/// the hue advances a small [`spinner::HUE_STEP`] per tick, so the colour glides
/// smoothly and much more slowly than the dot position.
pub fn spinner_span(spin: usize) -> RtSpan<'static> {
    let (r, g, b) = spinner::color(spin);
    RtSpan::styled(
        spinner::glyph(spin),
        Style::default()
            .fg(Color::Rgb(r, g, b))
            .add_modifier(Modifier::BOLD),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_ui::spinner::{hsv_to_rgb, HUE_STEP, SPIN_FRAMES};

    #[test]
    fn spinner_span_glides_smoothly_through_the_spectrum() {
        // The glyph steps through the six braille frames as `spin` advances.
        for spin in 0..12 {
            assert_eq!(
                &*spinner_span(spin).content,
                SPIN_FRAMES[spin % SPIN_FRAMES.len()],
                "glyph out of cycle at spin={spin}"
            );
        }
        // Every frame is bold and carries the HSV colour for its hue.
        let cycle = (360.0 / HUE_STEP).ceil() as usize;
        for spin in 0..cycle {
            let style = spinner_span(spin).style;
            assert!(
                style.add_modifier.contains(Modifier::BOLD),
                "spinner not bold at spin={spin}"
            );
            let hue = (spin as f32 * HUE_STEP).rem_euclid(360.0);
            let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
            assert_eq!(
                style.fg,
                Some(Color::Rgb(r, g, b)),
                "wrong hsv fg at spin={spin}"
            );
        }
        // The colour glides smoothly: consecutive ticks shift the hue by only the
        // small HUE_STEP, so each RGB channel moves by a bounded amount (no hard
        // jumps). A single HUE_STEP spans at most one 60-degree sextant, over which
        // a channel changes by at most 255.
        let max_delta = (HUE_STEP / 60.0 * 255.0).ceil() as i32 + 1;
        for spin in 0..cycle {
            let a = spinner_span(spin).style.fg;
            let b = spinner_span(spin + 1).style.fg;
            let (Some(Color::Rgb(ar, ag, ab)), Some(Color::Rgb(br, bg, bb))) = (a, b) else {
                panic!("spinner fg not an Rgb colour at spin={spin}");
            };
            assert_ne!(a, b, "hue did not change between spin={spin} and next");
            for (x, y, ch) in [(ar, br, "r"), (ag, bg, "g"), (ab, bb, "b")] {
                let d = (x as i32 - y as i32).abs();
                assert!(
                    d <= max_delta,
                    "channel {ch} jumped by {d} (> {max_delta}) at spin={spin}"
                );
            }
        }
        // A full cycle spans the whole spectrum: the hue reaches both ends.
        let hues: Vec<f32> = (0..cycle)
            .map(|s| (s as f32 * HUE_STEP).rem_euclid(360.0))
            .collect();
        let min = hues.iter().cloned().fold(f32::MAX, f32::min);
        let max = hues.iter().cloned().fold(f32::MIN, f32::max);
        assert!(min < 30.0, "cycle does not start near hue 0 (min={min})");
        assert!(max > 330.0, "cycle does not reach the far end (max={max})");
    }
}
