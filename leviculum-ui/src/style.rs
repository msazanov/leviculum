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

use crate::color::{rgb_to_ansi256, ColorDepth};

/// A resolved terminal style: colours already reduced to RGB, attributes as
/// flags. `None` colours mean "use the terminal default" (no SGR emitted).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RStyle {
    /// Foreground colour, or `None` for the terminal default.
    pub fg: Option<(u8, u8, u8)>,
    /// Background colour, or `None` for the terminal default.
    pub bg: Option<(u8, u8, u8)>,
    /// Bold attribute.
    pub bold: bool,
    /// Underline attribute.
    pub underline: bool,
    /// Italic attribute.
    pub italic: bool,
}

impl RStyle {
    /// Whether this style sets nothing: no colour, no attribute.
    pub fn is_plain(&self) -> bool {
        self.fg.is_none() && self.bg.is_none() && !self.bold && !self.underline && !self.italic
    }

    /// The self-contained SGR prefix for this style (starts by resetting). Under
    /// [`ColorDepth::Truecolor`] colours emit as 24-bit `38;2;r;g;b` / `48;2;...`;
    /// under [`ColorDepth::Ansi256`] they are downgraded to the nearest palette
    /// index as `38;5;idx` / `48;5;idx`.
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
        if let Some((r, g, b)) = self.fg {
            match depth {
                ColorDepth::Truecolor => s.push_str(&format!(";38;2;{r};{g};{b}")),
                ColorDepth::Ansi256 => s.push_str(&format!(";38;5;{}", rgb_to_ansi256(r, g, b))),
            }
        }
        if let Some((r, g, b)) = self.bg {
            match depth {
                ColorDepth::Truecolor => s.push_str(&format!(";48;2;{r};{g};{b}")),
                ColorDepth::Ansi256 => s.push_str(&format!(";48;5;{}", rgb_to_ansi256(r, g, b))),
            }
        }
        s.push('m');
        s
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
            fg: Some((0, 175, 255)),
            bg: Some((0, 0, 0)),
            bold: true,
            underline: true,
            italic: true,
        };
        assert!(!st.is_plain());
        assert!(RStyle::default().is_plain());
        assert_eq!(
            st.sgr(ColorDepth::Truecolor),
            "\x1b[0;1;3;4;38;2;0;175;255;48;2;0;0;0m"
        );
        assert_eq!(
            st.sgr(ColorDepth::Ansi256),
            format!(
                "\x1b[0;1;3;4;38;5;{};48;5;{}m",
                rgb_to_ansi256(0, 175, 255),
                rgb_to_ansi256(0, 0, 0)
            )
        );
    }
}
