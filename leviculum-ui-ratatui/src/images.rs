//! Inline pictures on a terminal: which protocol it takes, and the encoded,
//! croppable picture ratatui draws (behind the `images` feature).
//!
//! What a program does with pictures (which links are pictures, how big a
//! page lets one be drawn, the transfer queue, the cache) is its own. This is
//! the terminal half they share: ask the terminal, settle on a [`Backend`],
//! encode a decoded image to a cell size, and draw it cropped to an area.

use ratatui::layout::{Rect, Size};
use ratatui::Frame;
use ratatui_image::sliced::{SignedPosition, SlicedImage};
use ratatui_image::{FilterType, Resize};

pub use image::DynamicImage;
pub use ratatui_image::picker::{Picker, ProtocolType};
pub use ratatui_image::sliced::SlicedProtocol;
pub use ratatui_image::FontSize;

use leviculum_ui::color::ColorDepth;

/// How the terminal will be asked to draw pictures.
///
/// Not `Eq`: `ProtocolType` is only `PartialEq` upstream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Backend {
    /// A real graphics protocol: Kitty, iTerm2 or Sixel.
    Graphics(ProtocolType),
    /// Unicode half-blocks with foreground and background colour. Works on any
    /// colour terminal, at two pixels per cell.
    Halfblocks,
    /// No picture at all: a text box naming the file. What a monochrome
    /// terminal, `--no-color`, or an undecodable payload gets.
    Text,
}

impl Backend {
    /// How many image pixels one character cell carries, which is what
    /// "natural size" has to be measured in.
    ///
    /// A graphics protocol paints the cell's full pixel box, so the terminal's
    /// font size is the answer. Half-blocks carry exactly one pixel across and
    /// two down — the upper and lower half of the cell — whatever the font
    /// size is. Measuring half-blocks against the font size was the bug this
    /// distinction exists for: a 300x300 portrait came out as 38x38 pixels on
    /// an 8x16 font, when the same 78-column page had room for 78x44.
    pub fn cell_pixels(self, font: FontSize) -> (u16, u16) {
        match self {
            Backend::Graphics(_) => (font.width.max(1), font.height.max(1)),
            Backend::Halfblocks => (1, 2),
            // Nothing is drawn; the value is never used.
            Backend::Text => (font.width.max(1), font.height.max(1)),
        }
    }
}

/// Choose how to draw, from what the terminal answered and how much colour it
/// has.
///
/// The ladder is: a graphics protocol if the terminal claimed one; else
/// half-blocks if there is colour to draw them with; else text. `no_color`
/// overrides everything — a reader who asked for no colour did not ask for a
/// picture painted out of coloured blocks either, and the graphics protocols
/// are excluded with it so `--no-color` output stays plain.
pub fn choose_backend(detected: ProtocolType, depth: ColorDepth, no_color: bool) -> Backend {
    if no_color {
        return Backend::Text;
    }
    match detected {
        ProtocolType::Halfblocks => match depth {
            ColorDepth::Truecolor | ColorDepth::Ansi256 => Backend::Halfblocks,
        },
        graphics => Backend::Graphics(graphics),
    }
}

/// What this terminal can draw: the picker the query settled on, and the
/// backend chosen from it.
pub struct Pictures {
    picker: Option<Picker>,
    backend: Backend,
}

impl Default for Pictures {
    /// Draws nothing: no terminal was asked, so there is no picture to draw.
    fn default() -> Self {
        Pictures {
            picker: None,
            backend: Backend::Text,
        }
    }
}

impl Pictures {
    /// A pinned picker and backend, for a test that must not depend on which
    /// terminal runs it.
    pub fn new(picker: Option<Picker>, backend: Backend) -> Self {
        Pictures { picker, backend }
    }

    /// Ask the terminal what it can draw, and settle on a backend.
    ///
    /// The query talks to the terminal directly and wants raw mode, so this
    /// runs after the terminal is set up. Any failure — a terminal that
    /// answers nothing, a pipe, a query that times out — falls back to
    /// half-blocks rather than propagating: a program that refuses to start
    /// because it could not identify the terminal would be absurd.
    pub fn detect(no_color: bool, depth: ColorDepth) -> Self {
        let picker = match no_color {
            // Nothing will be drawn, so do not write query sequences at all.
            true => None,
            false => Some(Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())),
        };
        let detected = picker
            .as_ref()
            .map(|p| p.protocol_type())
            .unwrap_or(ProtocolType::Halfblocks);
        Pictures {
            picker,
            backend: choose_backend(detected, depth, no_color),
        }
    }

    /// The backend pictures are drawn with.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The terminal's font size, or `None` when no terminal was asked (then
    /// nothing is drawn).
    pub fn font_size(&self) -> Option<FontSize> {
        self.picker.as_ref().map(Picker::font_size)
    }

    /// Encode `image` to occupy `cells` (columns, rows).
    ///
    /// `Fit` never enlarges, and it measures "large enough" in font-size
    /// pixels. On the half-block path that silently undoes a sizing done in
    /// half-block pixels: a 300x300 portrait is only 38x19 cells of font box,
    /// so the protocol would shrink back to that and paint 38x38 half-block
    /// pixels. `Scale` honours the area asked for, which is where the
    /// resolution lives when a cell carries one pixel across and two down.
    pub fn encode(&self, image: DynamicImage, cells: (u16, u16)) -> Result<SlicedProtocol, String> {
        let Some(picker) = self.picker.as_ref() else {
            return Err("this terminal shows no images".to_string());
        };
        let size = Size::new(cells.0, cells.1);
        let resize = match self.backend {
            Backend::Halfblocks => Resize::Scale(Some(FilterType::Triangle)),
            _ => Resize::Fit(None),
        };
        SlicedProtocol::new_with_resize(picker, image, size, resize)
            .map_err(|e| format!("cannot draw: {e}"))
    }
}

/// Draw an encoded picture into `area` with its top-left cell at `(x, y)`
/// relative to it, cropped to the area: a picture half-scrolled off the top is
/// drawn from its middle rather than jumping or spilling over what is above.
/// The position is signed for exactly that reason.
pub fn render(frame: &mut Frame, area: Rect, picture: &SlicedProtocol, x: i16, y: i16) {
    frame.render_widget(
        SlicedImage::new(picture, SignedPosition::from((x, y))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backend_ladder_ends_in_text() {
        // A terminal that answered for a graphics protocol gets it.
        assert_eq!(
            choose_backend(ProtocolType::Kitty, ColorDepth::Truecolor, false),
            Backend::Graphics(ProtocolType::Kitty)
        );
        assert_eq!(
            choose_backend(ProtocolType::Sixel, ColorDepth::Ansi256, false),
            Backend::Graphics(ProtocolType::Sixel)
        );
        // One that did not falls back to coloured blocks.
        assert_eq!(
            choose_backend(ProtocolType::Halfblocks, ColorDepth::Truecolor, false),
            Backend::Halfblocks
        );
        assert_eq!(
            choose_backend(ProtocolType::Halfblocks, ColorDepth::Ansi256, false),
            Backend::Halfblocks
        );
        // --no-color means no picture, not a picture in fewer colours. It
        // overrides even a working graphics protocol.
        assert_eq!(
            choose_backend(ProtocolType::Halfblocks, ColorDepth::Truecolor, true),
            Backend::Text
        );
        assert_eq!(
            choose_backend(ProtocolType::Kitty, ColorDepth::Truecolor, true),
            Backend::Text
        );
    }
}
