//! The window's map (`docs/src/concepts/loquax-map.md`, slice (d)): the
//! picture of a view as RGBA for an iced image, and the pins over it with
//! their labels set as text. The picture, the pins' places and the names
//! are [`loquax::screenmap`]'s, which the Android app draws through too;
//! what is the window's own is its font measuring a label ([`measure`]),
//! as iced sets the label on the canvas, and its one typesetter.

use std::sync::{Mutex, OnceLock, PoisonError};

use leviculum_map::labels::Rect as Box2;
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::style::Style;

use loquax::mapnames::Typesetter;
use loquax::picture::Painter;
pub use loquax::screenmap::{
    area, hit, place, setup, taken, Picture, Placed, DRAG, LABEL, LINE, MARKER,
};

/// The window's one typesetter, shared by its paints.
fn typesetter() -> &'static Mutex<Typesetter> {
    static TYPE: OnceLock<Mutex<Typesetter>> = OnceLock::new();
    TYPE.get_or_init(|| Mutex::new(Typesetter::new()))
}

/// [`loquax::screenmap::paint`] with the window's typesetter.
pub fn paint(
    painter: &mut Painter,
    view: &Viewport,
    style: Style,
    scale: f32,
    names_after: Option<Vec<Box2>>,
) -> Picture {
    let mut type_ = typesetter().lock().unwrap_or_else(PoisonError::into_inner);
    loquax::screenmap::paint(painter, view, style, scale, names_after, &mut type_)
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
    use loquax::screenmap::metrics;
    use loquax::ui::map::{fit, viewport, MapPin, MapState};
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
            let picture = paint(&mut painter, &view, Style::PC, scale, None);
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

    /// The smallest shift by which every pixel column of `rgba`, `w`
    /// wide, repeats: the copies of the world side by side.
    fn period(rgba: &[u8], w: usize) -> Option<usize> {
        let h = rgba.len() / 4 / w;
        let px = |x: usize, y: usize| &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
        (1..w).find(|&p| (0..w - p).all(|x| (0..h).all(|y| px(x, y) == px(x + p, y))))
    }

    /// The window shows one world at most (1131): a window of 1280 x 800
    /// asked for z0 is drawn at z3, the world 2048 pixels wide, and no
    /// column of its picture repeats. The view as asked, the control,
    /// repeats every 256.
    #[test]
    fn the_window_never_shows_the_world_twice() {
        let mut painter = Painter::new(Maps::new(None));
        let setup = setup(&painter);
        let size = (1280u32, 800u32);
        let state = MapState {
            centre: project(LatLon::from_degrees(30.0, 10.0)),
            zoom: 0,
            selected: None,
        };
        let asked = Viewport::new(state.centre, 0, size.0, size.1);
        let control = paint(&mut painter, &asked, Style::PC, 1.0, None);
        assert_eq!(period(&control.rgba, 1280), Some(256), "the control");
        let view = viewport(&state, area(size), &setup);
        assert_eq!(view.zoom(), 3);
        let picture = paint(&mut painter, &view, Style::PC, 1.0, None);
        assert_eq!(period(&picture.rgba, 1280), None);
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
        let picture = paint(&mut painter, &two, Style::PC, 2.0, None);
        assert_eq!(picture.rgba.len(), 800 * 600 * 4);
        assert_eq!(metrics(2.0).marker, 2 * metrics(1.0).marker);
    }

    /// The map's names set into the window's picture (1100): the Florence
    /// fixture at z16 with one pin, the parks and the streets the zoom
    /// shows, each name with its halo, none under the pin's name; the
    /// picture the same on a second paint and pixel for pixel the golden.
    /// A picture drawn while the view moves carries none.
    #[test]
    fn the_map_names_are_set_into_the_picture() {
        let mut painter = florence();
        let at = project(LatLon::from_degrees(43.7752, 11.2660));
        let view = Viewport::new(at, 16, 480, 320);
        let pins = [pin(1, "alice", 43.7752, 11.2640)];
        let placed = place(&pins, &view, None, 1.0, measure);
        let boxes = taken(&placed, 1.0);
        let named = paint(&mut painter, &view, Style::PC, 1.0, Some(boxes.clone()));
        let plain = paint(&mut painter, &view, Style::PC, 1.0, None);
        assert!(plain.names.is_empty());
        assert_ne!(named.rgba, plain.rgba, "the names changed the picture");
        assert!(
            named.names.iter().any(|n| n == "Piazza D'Azeglio"),
            "{:?}",
            named.names
        );
        assert!(
            named.names.iter().filter(|n| n.starts_with("Via ")).count() >= 2,
            "{:?}",
            named.names
        );
        let again = paint(&mut painter, &view, Style::PC, 1.0, Some(boxes));
        assert_eq!(again.rgba, named.rgba, "a second paint is the same");
        assert_eq!(again.names, named.names);
        let rgb: Vec<u8> = named
            .rgba
            .chunks(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        same_as("gui-names-florence-z16.png", &rgb, 480, 320);
    }

    /// Hold `rgb` to the PNG golden `name` under `loquax/tests/golden`,
    /// pixel for pixel; a picture that moved is left in the temp dir.
    fn same_as(name: &str, rgb: &[u8], w: u32, h: u32) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        let encode = || {
            let mut out = Vec::new();
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            let mut writer = e.write_header().expect("header");
            writer.write_image_data(rgb).expect("data");
            writer.finish().expect("finish");
            out
        };
        let keep = || {
            let dir = std::env::temp_dir().join("loquax-golden");
            std::fs::create_dir_all(&dir).expect("temp dir");
            let kept = dir.join(name);
            std::fs::write(&kept, encode()).expect("write");
            kept
        };
        let Ok(bytes) = std::fs::read(&path) else {
            panic!("no golden {name}; this run's is {}", keep().display());
        };
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .expect("png");
        let mut want = vec![0; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut want).expect("frame");
        want.truncate(info.buffer_size());
        if (info.width, info.height) != (w, h) || want != rgb {
            let differ = want
                .chunks(3)
                .zip(rgb.chunks(3))
                .filter(|(a, b)| a != b)
                .count();
            panic!(
                "{name} moved: {differ} pixels differ; this run's is {}",
                keep().display()
            );
        }
    }

    /// The label width comes from the font: a longer name is wider.
    #[test]
    fn a_label_is_measured_by_the_windows_font() {
        let short = measure("bob", LABEL);
        let long = measure("bob · 5m and a long name", LABEL);
        assert!(long > short, "{short} {long}");
    }
}
