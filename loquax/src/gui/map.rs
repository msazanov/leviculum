//! The window's map (`docs/src/concepts/loquax-map.md`, slice (d)): the
//! picture of a view as RGBA for an iced image, and the pins over it with
//! their labels set as text.
//!
//! The terminal's map counts in cells; this one counts in the screen's own
//! pixels, so the model's map functions ([`loquax::ui::map`]: the view,
//! fitting, zooming about the pointer, dragging) run here with one pixel to
//! a "cell" ([`setup`]). The picture is [`loquax::picture`]'s, the label
//! rule [`leviculum_map::pins::labels`] in pixels with the window's font
//! measuring each label ([`measure`]). Everything here is a pure function
//! of its inputs, so the window and the test below draw through the same
//! code and the test needs no display.

use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::pixelcolor::Rgb565;
use leviculum_map::draw::Ink;
use leviculum_map::pins::{self, Mark, Metrics, Pin};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::frame::rgb888;
use leviculum_ui::Rect;

use loquax::picture::Painter;
use loquax::ui::map::{MapPin, MapSetup};

/// A marker's diameter in logical pixels; times the scale on screen.
pub const MARKER: f32 = 10.0;
/// A label's text size in logical pixels.
pub const LABEL: f32 = 14.0;
/// The space between a marker and the label beside it.
pub const GAP: f32 = 4.0;
/// A label's line, as a multiple of its text size: iced's default.
pub const LINE: f32 = 1.3;
/// How far, in logical pixels either way, the pointer goes from where the
/// left button went down before the press is a drag and no longer a click.
pub const DRAG: f32 = 4.0;

/// What the model's map functions are told: one pixel a cell, and the
/// zooms and headers of the installed regions.
pub fn setup(painter: &Painter) -> MapSetup {
    MapSetup {
        cell_px: (1, 1),
        zooms: painter.zooms(),
        regions: painter.headers(),
    }
}

/// The picture's area in screen pixels, as the model's map functions take
/// it.
pub fn area(size: (u32, u32)) -> Rect {
    let clamp = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    Rect::new(0, 0, clamp(size.0), clamp(size.1))
}

/// One view's picture: RGBA rows, and the credit it carries.
pub struct Picture {
    pub view: Viewport,
    pub rgba: Vec<u8>,
    pub attribution: String,
    /// The maps directory's epoch the regions were read at.
    pub epoch: u64,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h) = self.view.size();
        write!(
            f,
            "Picture({w}x{h} z{} {})",
            self.view.zoom(),
            self.attribution
        )
    }
}

/// `view` drawn in colour with lines as wide as `scale` asks (2.0 on a
/// screen of twice the density draws them twice as wide), as RGBA.
pub fn paint(painter: &mut Painter, view: &Viewport, scale: f32) -> Picture {
    let percent = (scale * 100.0).round().max(1.0) as u32;
    let drawn = painter.draw(view, Ink::Colour, percent);
    let mut rgba = Vec::with_capacity(drawn.fb.len() * 4);
    for &px in &drawn.fb {
        let [r, g, b] = rgb888(Rgb565::from(RawU16::new(u16::from_be(px))));
        rgba.extend_from_slice(&[r, g, b, 0xff]);
    }
    Picture {
        view: *view,
        rgba,
        attribution: drawn.attribution,
        epoch: painter.epoch(),
    }
}

/// A pin as the window draws it, in screen pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub peer: [u8; 16],
    /// The marker's centre.
    pub x: f32,
    pub y: f32,
    /// The label's text and its top left corner, where it has room.
    pub label: Option<(f32, f32, String)>,
    /// The label's width, for a click on it.
    pub width: f32,
    pub stale: bool,
    pub selected: bool,
}

/// The sizes the label rule counts with at `scale`.
pub fn metrics(scale: f32) -> Metrics {
    let px = |v: f32| (v * scale).round().max(1.0) as u32;
    Metrics {
        marker: px(MARKER),
        gap: px(GAP),
        line: px(LABEL * LINE),
    }
}

/// The pins of `pins` that land in `view`, the label of each set by the
/// core's rule where it has room and `selected`'s always; `measure(text,
/// size)` is the width of a label in pixels at that text size. The
/// selected pin comes last, so it is drawn on top.
pub fn place(
    pins: &[MapPin],
    view: &Viewport,
    selected: Option<[u8; 16]>,
    scale: f32,
    measure: impl Fn(&str, f32) -> u32,
) -> Vec<Placed> {
    let as_pins: Vec<Pin<'_>> = pins
        .iter()
        .map(|p| Pin {
            at: p.at,
            label: &p.label,
            stale: p.stale,
        })
        .collect();
    let m = metrics(scale);
    let landed: Vec<(&MapPin, u32, u32, String, u32)> = pins::place(view, &as_pins)
        .into_iter()
        .filter_map(|placed| {
            let pin = pins.get(placed.index)?;
            let words = pin.words();
            let width = measure(&words, LABEL * scale);
            Some((pin, placed.x, placed.y, words, width))
        })
        .collect();
    // The rule takes a marker's top left corner; the core places its centre.
    let marks: Vec<Mark> = landed
        .iter()
        .map(|(_, x, y, _, _)| Mark {
            x: x.saturating_sub(m.marker / 2),
            y: y.saturating_sub(m.line / 2),
        })
        .collect();
    let chosen = selected.and_then(|peer| landed.iter().position(|(p, ..)| p.peer == peer));
    let labels = pins::labels(
        &marks,
        |i| landed.get(i).map_or(0, |(.., width)| *width),
        chosen,
        view.size(),
        m,
    );
    let mut out: Vec<Placed> = landed
        .into_iter()
        .zip(labels)
        .map(|((pin, x, y, words, width), label)| Placed {
            peer: pin.peer,
            x: x as f32,
            y: y as f32,
            label: label.map(|l| (l.x as f32, l.y as f32, words)),
            width: width as f32,
            stale: pin.stale,
            selected: Some(pin.peer) == selected,
        })
        .collect();
    out.sort_by_key(|p| p.selected);
    out
}

/// The pin under the screen pixel `(x, y)`, its marker or its label, the
/// topmost where they overlap.
pub fn hit(placed: &[Placed], x: f32, y: f32, scale: f32) -> Option<[u8; 16]> {
    let radius = MARKER * scale / 2.0 + 2.0 * scale;
    let line = LABEL * LINE * scale;
    placed
        .iter()
        .rev()
        .find(|p| {
            let on_marker = (p.x - x).abs() <= radius && (p.y - y).abs() <= radius;
            let on_label = p.label.as_ref().is_some_and(|(lx, ly, _)| {
                (*lx..*lx + p.width).contains(&x) && (*ly..*ly + line).contains(&y)
            });
            on_marker || on_label
        })
        .map(|p| p.peer)
}

/// The width of `text` at `size` pixels as the window sets it: iced's own
/// paragraph over its font system, with nothing drawn, so it works without
/// a display.
pub fn measure(text: &str, size: f32) -> u32 {
    use iced::advanced::graphics::text::Paragraph;
    use iced::advanced::text::{self, Paragraph as _};
    let paragraph = Paragraph::with_text(text::Text {
        content: text,
        bounds: iced::Size::INFINITE,
        size: iced::Pixels(size),
        line_height: text::LineHeight::default(),
        font: iced::Font::DEFAULT,
        align_x: text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: text::Shaping::default(),
        wrapping: text::Wrapping::None,
    });
    paragraph.min_bounds().width.ceil() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};
    use loquax::picture::Maps;
    use loquax::ui::map::{fit, viewport, MapState};
    use std::path::PathBuf;

    /// A painter over the committed Florence region alone.
    fn florence() -> Painter {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures");
        let maps = Maps::new(Some(dir));
        let (regions, problems) = maps.load();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(regions.len(), 1);
        Painter::new(maps)
    }

    fn pin(peer: u8, label: &str, lat: f64, lon: f64) -> MapPin {
        MapPin {
            peer: [peer; 16],
            at: project(LatLon::from_degrees(lat, lon)),
            label: label.to_string(),
            age: Some("5m".to_string()),
            stale: peer == 3,
        }
    }

    /// The map view's frame over the fixture region at three zooms through
    /// the window's own pipeline: the region's credit when the view is
    /// inside it, the outline's when zoomed out past it, every pixel set,
    /// and the pins that land counted. The window draws exactly this:
    /// `paint` for the picture, `place` for the pins, `measure` for the
    /// labels.
    #[test]
    fn the_map_frame_over_the_fixture_at_three_zooms() {
        let mut painter = florence();
        let setup = setup(&painter);
        // Three inside the region's box (11.2505,43.7728 to 11.2700,43.7785,
        // the fixture's README): the Duomo, San Lorenzo's market, east of
        // Piazza d'Azeglio's corner; and Pisa, far to the west.
        let pins = [
            pin(1, "alice", 43.7731, 11.2560),
            pin(2, "bob", 43.7760, 11.2540),
            pin(3, "carol", 43.7770, 11.2650),
            pin(4, "dave", 43.7228, 10.4017),
        ];
        let size = (800u32, 600u32);
        let scale = 1.0;
        let (centre, fitted) = fit(&pins[..3], area(size), &setup);
        let cases = [
            // (zoom, credit, pins in view)
            (fitted, "© OpenStreetMap contributors", 3),
            (11, "© OpenStreetMap contributors", 3),
            (8, "Natural Earth", 4),
        ];
        for (zoom, credit, landed) in cases {
            let state = MapState {
                centre,
                zoom,
                selected: Some([2; 16]),
            };
            let view = viewport(&state, area(size), &setup);
            let picture = paint(&mut painter, &view, scale);
            assert_eq!(picture.view.size(), size);
            assert_eq!(picture.rgba.len(), (size.0 * size.1 * 4) as usize);
            assert_eq!(picture.attribution, credit, "zoom {zoom}");
            assert!(
                picture.rgba.chunks(4).all(|px| px[3] == 0xff),
                "zoom {zoom}: every pixel opaque"
            );
            let placed = place(&pins, &view, state.selected, scale, measure);
            assert_eq!(placed.len(), landed, "zoom {zoom}: {placed:?}");
            let last = placed.last().expect("a pin");
            assert!(last.selected && last.peer == [2; 16], "selected on top");
            assert!(last.label.is_some(), "the selected keeps its label");
            assert!(placed.iter().any(|p| p.stale), "carol is stale");
            // A click on a marker finds its pin, or one drawn over it.
            for (i, p) in placed.iter().enumerate() {
                let found = hit(&placed, p.x, p.y, scale).expect("a pin under the marker");
                let at = placed.iter().position(|q| q.peer == found).expect("placed");
                assert!(at >= i, "zoom {zoom}: {:?} under {:?}", p.peer, found);
            }
            assert_eq!(hit(&placed, last.x, last.y, scale), Some(last.peer));
        }
        assert!(fitted > 11, "three pins in the old town fit deep: {fitted}");
    }

    /// The same view on a screen of twice the density: twice the pixels,
    /// the pins at twice the coordinates, and the lines drawn wider rather
    /// than the picture scaled up.
    #[test]
    fn a_dense_screen_draws_the_same_view_at_its_own_pixels() {
        let mut painter = florence();
        let at = project(LatLon::from_degrees(43.7731, 11.2560));
        let one = Viewport::new(at, 14, 400, 300);
        let two = Viewport::new(at, 15, 800, 600);
        let pins = [pin(1, "alice", 43.7700, 11.2500)];
        let a = place(&pins, &one, None, 1.0, measure);
        let b = place(&pins, &two, None, 2.0, measure);
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert!((b[0].x - 2.0 * a[0].x).abs() <= 2.0, "{a:?} {b:?}");
        let picture = paint(&mut painter, &two, 2.0);
        assert_eq!(picture.rgba.len(), 800 * 600 * 4);
        assert_eq!(metrics(2.0).marker, 2 * metrics(1.0).marker);
    }

    /// The label width comes from the font: a longer name is wider.
    #[test]
    fn a_label_is_measured_by_the_windows_font() {
        let short = measure("bob", LABEL);
        let long = measure("bob · 5m and a long name", LABEL);
        assert!(long > short, "{short} {long}");
    }
}
