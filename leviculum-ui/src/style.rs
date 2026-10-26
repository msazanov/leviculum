//! The terminal-independent styled text model: a laid-out row is a run of
//! [`StyledChar`]s, each carrying its resolved [`RStyle`].
//!
//! It names no terminal crate. lnomad's micron layout produces it, its ANSI
//! sink turns it into escape sequences through [`RStyle::sgr`], and the
//! ratatui bridge in `leviculum-ui-ratatui` turns it into ratatui text; a GUI
//! frontend would read the same cells.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::color::{rgb_to_ansi16, rgb_to_ansi256, ColorDepth, Ink};

/// A resolved terminal style: colours as [`Ink`]s, attributes as flags.
/// `None` colours mean "use the terminal default" (no SGR emitted).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RStyle {
    /// Foreground colour, or `None` for the terminal default.
    pub fg: Option<Ink>,
    /// Background colour, or `None` for the terminal default.
    pub bg: Option<Ink>,
    /// Bold attribute.
    pub bold: bool,
    /// Underline attribute.
    pub underline: bool,
    /// Italic attribute.
    pub italic: bool,
    /// Reverse video: foreground and background swapped. What a monochrome
    /// screen draws a selection in, where a colour screen would tint it.
    pub reverse: bool,
}

impl RStyle {
    /// Whether this style sets nothing: no colour, no attribute.
    pub fn is_plain(&self) -> bool {
        self.fg.is_none()
            && self.bg.is_none()
            && !self.bold
            && !self.underline
            && !self.italic
            && !self.reverse
    }

    /// The self-contained SGR prefix for this style (starts by resetting). An
    /// RGB colour emits as 24-bit `38;2;r;g;b` / `48;2;...` under
    /// [`ColorDepth::Truecolor`], as its nearest xterm-256 index under
    /// [`ColorDepth::Ansi256`] and as its nearest ANSI colour under
    /// [`ColorDepth::Ansi16`]; an indexed one emits as itself at every depth.
    pub fn sgr(&self, depth: ColorDepth) -> String {
        let mut s = String::from("\x1b[0");
        if self.bold {
            s.push_str(";1");
        }
        if self.italic {
            s.push_str(";3");
        }
        if self.underline {
            s.push_str(";4");
        }
        if self.reverse {
            s.push_str(";7");
        }
        if let Some(ink) = self.fg {
            s.push_str(&sgr_ink(ink, depth, 30, 90, 38));
        }
        if let Some(ink) = self.bg {
            s.push_str(&sgr_ink(ink, depth, 40, 100, 48));
        }
        s.push('m');
        s
    }
}

/// One colour's SGR parameters, `;` first: `base` + n for the eight
/// colours, `bright` + n for their bright halves, `extended` for the rest.
fn sgr_ink(ink: Ink, depth: ColorDepth, base: u8, bright: u8, extended: u8) -> String {
    let index = match (ink, depth) {
        (Ink::Rgb(r, g, b), ColorDepth::Truecolor) => {
            return format!(";{extended};2;{r};{g};{b}");
        }
        (Ink::Rgb(r, g, b), ColorDepth::Ansi256) => rgb_to_ansi256(r, g, b),
        (Ink::Rgb(r, g, b), ColorDepth::Ansi16) => rgb_to_ansi16(r, g, b),
        (Ink::Indexed(n), _) => n,
    };
    match index {
        0..=7 => format!(";{}", base + index),
        8..=15 => format!(";{}", bright + index - 8),
        _ => format!(";{extended};5;{index}"),
    }
}

/// One styled character, the unit the layout engine works in.
#[derive(Clone, Copy, Debug)]
pub struct StyledChar {
    /// The visible character.
    pub ch: char,
    /// Its resolved style.
    pub st: RStyle,
    /// The 1-based index of the link this cell is a clickable part of, if any.
    /// Leading/trailing whitespace inside a link label is untagged so it stays
    /// unstyled and outside the hit-test range.
    pub link: Option<usize>,
    /// The 1-based index of the form field this cell is part of, if any (the
    /// whole rendered widget is tagged, including its brackets), for hit-testing
    /// and the input-box overlay.
    pub field: Option<usize>,
}

/// One laid-out output row: a sequence of styled cells, already wrapped,
/// aligned and indented. The target-agnostic unit both sinks consume.
#[derive(Clone, Debug, Default)]
pub struct RLine {
    /// The row's cells, left to right.
    pub cells: Vec<StyledChar>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_style_emits_one_self_contained_sgr_at_either_depth() {
        let st = RStyle {
            fg: Some(Ink::Rgb(0, 175, 255)),
            bg: Some(Ink::Rgb(0, 0, 0)),
            bold: true,
            underline: true,
            italic: true,
            reverse: true,
        };
        assert!(!st.is_plain());
        assert!(RStyle::default().is_plain());
        assert_eq!(
            st.sgr(ColorDepth::Truecolor),
            "\x1b[0;1;3;4;7;38;2;0;175;255;48;2;0;0;0m"
        );
        assert_eq!(
            st.sgr(ColorDepth::Ansi256),
            format!(
                "\x1b[0;1;3;4;7;38;5;{};48;5;{}m",
                rgb_to_ansi256(0, 175, 255),
                rgb_to_ansi256(0, 0, 0)
            )
        );
        assert_eq!(st.sgr(ColorDepth::Ansi16), "\x1b[0;1;3;4;7;36;40m");
    }

    #[test]
    fn an_indexed_colour_emits_as_itself_at_every_depth() {
        let st = RStyle {
            fg: Some(Ink::Indexed(15)),
            bg: Some(Ink::Indexed(4)),
            ..RStyle::default()
        };
        let wide = RStyle {
            fg: Some(Ink::Indexed(255)),
            bg: Some(Ink::Indexed(24)),
            ..RStyle::default()
        };
        for depth in [
            ColorDepth::Truecolor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
        ] {
            assert_eq!(st.sgr(depth), "\x1b[0;97;44m");
            assert_eq!(wide.sgr(depth), "\x1b[0;38;5;255;48;5;24m");
        }
    }
}
