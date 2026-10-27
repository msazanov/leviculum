//! Inline pictures on a terminal: which protocol it takes, and the encoded,
//! croppable picture ratatui draws (behind the `images` feature).
//!
//! What a program does with pictures (which links are pictures, how big a
//! page lets one be drawn, the transfer queue, the cache) is its own. This is
//! the terminal half they share: ask the terminal, settle on a [`Backend`],
//! encode a decoded image to a cell size, and draw it cropped to an area.

use std::fmt::Write as _;

use ratatui::layout::{Rect, Size};
use ratatui::Frame;
use ratatui_image::protocol::sixel::Sixel;
use ratatui_image::sliced::{SignedPosition, SlicedImage};
use ratatui_image::{FilterType, Image, Resize};

pub use image::DynamicImage;
pub use ratatui_image::picker::{Picker, ProtocolType};
pub use ratatui_image::protocol::Protocol;
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
            // Sixteen colours draw no picture a reader would recognise.
            ColorDepth::Ansi16 => Backend::Text,
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

/// A picture from packed 8-bit RGB rows, as a program that draws its own
/// pixels (loquax's map) hands them over; `None` when `rgb` is not
/// `width * height * 3` bytes.
pub fn rgb_image(width: u32, height: u32, rgb: Vec<u8>) -> Option<DynamicImage> {
    image::RgbImage::from_raw(width, height, rgb).map(DynamicImage::ImageRgb8)
}

/// A picture of at most 256 exact colours as one sixel over `cells`
/// (columns, rows): `indices` are `width * height` palette entries, row by
/// row. `None` when the sizes do not match, or under tmux, whose
/// passthrough wrapping is `ratatui-image`'s; [`Pictures::encode`] is the
/// path then.
///
/// No quantiser runs: a picture drawn in a handful of known colours (the
/// map's style has under ten) is already indexed, and `ratatui-image`
/// hands every picture to `icy_sixel`'s Wu quantiser with Floyd-Steinberg
/// dithering, which is most of a frame's cost and none of its value there
/// (1075). Each band of six rows paints its commonest colour as one run,
/// then only the bits of the others, so a band of plain ground with a few
/// streets is a few hundred bytes.
pub fn sixel_indexed(
    palette: &[[u8; 3]],
    indices: &[u8],
    width: u32,
    height: u32,
    cells: (u16, u16),
) -> Option<Protocol> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || indices.len() != w * h || palette.is_empty() || in_tmux() {
        return None;
    }
    if indices.iter().any(|&i| usize::from(i) >= palette.len()) {
        return None;
    }
    let mut data = String::with_capacity(w * h / 8);
    // The erase ratatui-image writes before its own sixels: the cells under
    // the picture are blanked, so a picture with fewer rows leaves nothing.
    let (cols, rows) = cells;
    match rows {
        1 => {
            let _ = write!(data, "\x1b[{cols}X");
        }
        _ => {
            for _ in 0..rows {
                let _ = write!(data, "\x1b[{cols}X\x1b[1B");
            }
            let _ = write!(data, "\x1b[{rows}A");
        }
    }
    // Square pixels, unset ones transparent, then the raster size so the
    // terminal allocates the picture once.
    let _ = write!(data, "\x1bP9;1;0q\"1;1;{w};{h}");
    for (i, [r, g, b]) in palette.iter().enumerate() {
        let pct = |c: u8| u32::from(c) * 100 / 255;
        let _ = write!(data, "#{i};2;{};{};{}", pct(*r), pct(*g), pct(*b));
    }
    let mut counts = [0u32; 256];
    let mut masks = vec![0u8; palette.len() * w];
    for y0 in (0..h).step_by(6) {
        let band = (h - y0).min(6);
        counts.iter_mut().for_each(|c| *c = 0);
        for dy in 0..band {
            let row = &indices[(y0 + dy) * w..(y0 + dy + 1) * w];
            for (x, &i) in row.iter().enumerate() {
                counts[usize::from(i)] += 1;
                masks[usize::from(i) * w + x] |= 1 << dy;
            }
        }
        let base = (0..palette.len()).max_by_key(|&i| counts[i]).unwrap_or(0);
        let full = (1u8 << band) - 1;
        let _ = write!(data, "#{base}");
        sixel_run(&mut data, full, w);
        for i in (0..palette.len()).filter(|&i| i != base && counts[i] > 0) {
            data.push('$');
            let _ = write!(data, "#{i}");
            let mask = &masks[i * w..(i + 1) * w];
            // Trailing empty columns are never written: `$` returns anyway.
            let end = mask.iter().rposition(|&m| m != 0).map_or(0, |e| e + 1);
            let mut x = 0;
            while x < end {
                let bits = mask[x];
                let run = mask[x..end].iter().take_while(|&&m| m == bits).count();
                sixel_run(&mut data, bits, run);
                x += run;
            }
        }
        // The next band; none after the last, which would add six rows.
        if y0 + 6 < h {
            data.push('-');
        }
        // Only the colours this band used have bits to clear.
        for i in (0..palette.len()).filter(|&i| counts[i] > 0) {
            masks[i * w..(i + 1) * w].iter_mut().for_each(|m| *m = 0);
        }
    }
    data.push_str("\x1b\\");
    Some(Protocol::Sixel(Sixel {
        data,
        size: Size::new(cols, rows),
        is_tmux: false,
    }))
}

/// `run` columns of the sixel `bits`, run-length coded when that is shorter.
fn sixel_run(out: &mut String, bits: u8, run: usize) {
    let c = char::from(63 + bits);
    if run > 3 {
        let _ = write!(out, "!{run}{c}");
    } else {
        (0..run).for_each(|_| out.push(c));
    }
}

/// Inside tmux, by the rule `ratatui-image`'s picker applies.
fn in_tmux() -> bool {
    std::env::var("TERM").is_ok_and(|t| t.starts_with("tmux"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|t| t == "tmux")
}

/// Draw a whole picture into `area`, from its top-left cell; nothing when it
/// does not fit.
pub fn render_whole(frame: &mut Frame, area: Rect, picture: &Protocol) {
    frame.render_widget(Image::new(picture), area);
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

    /// What a terminal reads back from [`sixel_indexed`] is the picture it
    /// was given, pixel for pixel: a last band of fewer than six rows, a
    /// colour only in one corner, runs long and short.
    #[test]
    fn an_indexed_sixel_reads_back_exactly() {
        let palette = [[255, 255, 255], [0, 0, 0], [255, 0, 0], [0, 0, 255]];
        let (w, h) = (23u32, 14u32);
        let indices: Vec<u8> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                match (x, y) {
                    (22, 13) => 3,
                    _ if y == x / 2 => 1,
                    _ if x < 5 && y > 8 => 2,
                    _ => 0,
                }
            })
            .collect();
        let Some(Protocol::Sixel(sixel)) = sixel_indexed(&palette, &indices, w, h, (3, 2)) else {
            panic!("a sixel");
        };
        assert!(sixel
            .data
            .starts_with("\x1b[3X\x1b[1B\x1b[3X\x1b[1B\x1b[2A\x1bP"));
        let dcs = &sixel.data[sixel.data.find("\x1bP").expect("dcs")..];
        let image = icy_sixel::SixelImage::decode(dcs.as_bytes()).expect("decode");
        // The decoder's canvas grows by whole bands of six; the rows past
        // the picture are never painted, as with ratatui-image's sixels.
        assert_eq!(image.width, w as usize);
        assert!(image.height >= h as usize && image.height < h as usize + 6);
        for (n, (px, &i)) in image.pixels.chunks_exact(4).zip(&indices).enumerate() {
            let [r, g, b] = palette[usize::from(i)];
            assert_eq!(px, [r, g, b, 255], "pixel {n}");
        }
        // An index past the palette, or a size that does not match, is no
        // picture rather than a wrong one.
        assert!(sixel_indexed(&palette, &[4; 4], 2, 2, (1, 1)).is_none());
        assert!(sixel_indexed(&palette, &[0; 3], 2, 2, (1, 1)).is_none());
    }

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
        assert_eq!(
            choose_backend(ProtocolType::Halfblocks, ColorDepth::Ansi16, false),
            Backend::Text
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
