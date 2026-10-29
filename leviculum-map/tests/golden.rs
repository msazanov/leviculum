//! Golden images per profile and the counts that gate their cost (1099,
//! `docs/drafts/map-renderer-roadmap-2026-10.md`, section 8).
//!
//! The Florence region drawn the way each device draws it: the M9 at
//! 320x240 (light, square caps, no casings), the PC at 1x in both tables
//! and at 2x (casings, round joins, dashed paths), and the terminal's text
//! techniques as cell dumps. The integer path has no anti-aliasing, so the
//! pictures are bit-exact: a PNG golden is compared pixel by pixel, zero
//! differing. The cost gate is counts, never a clock (the forge runner is
//! shared, 672, 689): the edges, spans and pixels the filler handed out
//! for fills, strokes and casings, per profile. A change that draws more
//! shows as a number here and states its delta in its commit.
//!
//! A picture that moved is written to `$TMPDIR/leviculum-golden/` under
//! its golden's name; look at it, and copy it over the golden when the
//! change is the one intended.

use std::path::PathBuf;

use embedded_graphics::prelude::*;
use leviculum_map::cells::{self, Encoding, Sets};
use leviculum_map::draw::{draw_in_scaled, Ink};
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::region::Region;
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::fill::{Edge, FillStats, Scratch};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{NoClock, RenderStats, Work};
use leviculum_mapfill::style::{MapTheme, Profile, Style};

const FLORENCE: &[u8] = include_bytes!("../fixtures/firenze.lmr");

/// The centre of the region's box.
fn centre() -> LatLon {
    let b = Region::from_bytes(FLORENCE).expect("open").header().bounds;
    LatLon {
        lat_e7: (b.north + b.south) / 2,
        lon_e7: (b.west + b.east) / 2,
    }
}

/// `view` drawn in `ink` and `style`, lines `percent` per cent wide, on a
/// frame cleared to the table's background: its pixels as RGB and what
/// the filler counted.
fn draw(view: &Viewport, ink: Ink, style: Style, percent: u32) -> (Vec<u8>, RenderStats) {
    let r = Region::from_bytes(FLORENCE).expect("open");
    let tiles = r.view(view).expect("tiles");
    let (w, h) = view.size();
    let mut fb = vec![0u16; (w * h) as usize];
    let mut edges = vec![Edge::default(); 8192];
    let mut active = vec![0u16; 8192];
    let mut heads = vec![0u16; h as usize];
    let mut points = vec![Point::zero(); 8192];
    let mut ring_ends = vec![0u16; 2048];
    let mut frame = Frame::new(&mut fb, w, h).expect("frame");
    let Ok(()) = frame.clear(style.theme.palette().background);
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut work = Work {
        points: &mut points,
        ring_ends: &mut ring_ends,
    };
    let Ok(st) = draw_in_scaled(
        &tiles,
        r.area(view),
        Profile::Full,
        ink,
        style,
        percent,
        &mut frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    );
    let mut rgb = Vec::with_capacity(fb.len() * 3);
    let frame = Frame::new(&mut fb, w, h).expect("frame");
    frame.rgb888_into(&mut rgb);
    (rgb, st)
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// Where a picture that moved is left for a look.
fn kept(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join("leviculum-golden");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write");
    path
}

fn png(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = png::Encoder::new(&mut out, w, h);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    let mut writer = e.write_header().expect("header");
    writer.write_image_data(rgb).expect("data");
    writer.finish().expect("finish");
    out
}

fn unpng(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .expect("png");
    let mut buf = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut buf).expect("frame");
    assert_eq!(
        (info.color_type, info.bit_depth),
        (png::ColorType::Rgb, png::BitDepth::Eight)
    );
    buf.truncate(info.buffer_size());
    (buf, info.width, info.height)
}

/// Hold `rgb` to the PNG golden `name`, pixel for pixel.
fn same_as(name: &str, rgb: &[u8], w: u32, h: u32) {
    let Ok(bytes) = std::fs::read(golden(name)) else {
        let path = kept(name, &png(rgb, w, h));
        panic!("no golden {name}; this run's is {}", path.display());
    };
    let (want, gw, gh) = unpng(&bytes);
    if (gw, gh) != (w, h) || want != rgb {
        let differ = want
            .chunks(3)
            .zip(rgb.chunks(3))
            .filter(|(a, b)| a != b)
            .count();
        let path = kept(name, &png(rgb, w, h));
        panic!(
            "{name} moved: {differ} pixels differ ({gw}x{gh} against {w}x{h}); this run's is {}",
            path.display()
        );
    }
}

fn counts(s: FillStats) -> (u32, u32, u32) {
    (s.edges, s.spans, s.pixels)
}

/// One profile's view: its golden, style, zoom, size and line width, and
/// the fill, stroke and casing counts (edges, spans, pixels) it costs.
type Case = (
    &'static str,
    Style,
    u8,
    (u32, u32),
    u32,
    [(u32, u32, u32); 3],
);

/// Each profile's picture, and its fill, stroke and casing counts.
#[test]
fn each_profile_draws_its_golden_at_its_counts() {
    let at = project(centre());
    let cases: [Case; 4] = [
        (
            "m9-light-320x240-z15.png",
            Style::M9,
            15,
            (320, 240),
            100,
            [(831, 2384, 138_593), (2105, 6738, 12_544), (0, 0, 0)],
        ),
        (
            "pc-light-480x240-z15.png",
            Style::PC,
            15,
            (480, 240),
            100,
            [
                (1038, 2725, 191_173),
                (10_435, 7998, 16_419),
                (282, 279, 1613),
            ],
        ),
        (
            "pc-dark-480x240-z15.png",
            Style::PC.in_theme(MapTheme::Dark),
            15,
            (480, 240),
            100,
            [
                (1038, 2725, 191_173),
                (10_435, 7998, 16_419),
                (282, 279, 1613),
            ],
        ),
        (
            "pc-light-2x-640x320-z16.png",
            Style::PC,
            16,
            (640, 320),
            200,
            [(722, 3969, 479_497), (7037, 9310, 34_552), (1, 0, 0)],
        ),
    ];
    let mut moved = Vec::new();
    for (name, style, zoom, (w, h), percent, want) in cases {
        let view = Viewport::new(at, zoom, w, h);
        let (rgb, st) = draw(&view, Ink::Colour, style, percent);
        assert_eq!(st.overflows, 0, "{name}");
        let got = [counts(st.fill), counts(st.stroke), counts(st.casing)];
        if got != want {
            moved.push(format!("{name}: {got:?}"));
        }
        same_as(name, &rgb, w, h);
    }
    assert!(moved.is_empty(), "the counts moved:\n{}", moved.join("\n"));
}

/// The M9's finish strokes no casing, and the PC's finish draws the same
/// fills: casings and joints cost strokes, never areas.
#[test]
fn casings_cost_strokes_only_and_never_on_the_m9() {
    let view = Viewport::new(project(centre()), 15, 480, 240);
    let (_, m9) = draw(&view, Ink::Colour, Style::M9, 100);
    let (_, pc) = draw(&view, Ink::Colour, Style::PC, 100);
    assert_eq!(m9.casing, FillStats::default());
    assert!(pc.casing.pixels > 0);
    assert_eq!(m9.fill, pc.fill);
    assert_eq!((m9.features, m9.culled), (pc.features, pc.culled));
}

/// The terminal's techniques as the frames a terminal shows: each cell's
/// glyph, and its two colours as the sixteen-colour index of the table
/// (`.` for none, `*` for a mean of two table colours).
fn dump(cells: &[cells::Cell], cols: usize, theme: MapTheme) -> String {
    let code = |c: Option<cells::Rgb>| match c {
        None => '.',
        Some(rgb) => match cells::ansi16(rgb, theme) {
            Some(i) => char::from_digit(u32::from(i), 16).unwrap_or('?'),
            None => '*',
        },
    };
    let mut out = String::new();
    for row in cells.chunks(cols) {
        out.extend(row.iter().map(|c| c.ch));
        out.push('\n');
        out.extend(row.iter().map(|c| code(c.fg)));
        out.push('\n');
        out.extend(row.iter().map(|c| code(c.bg)));
        out.push('\n');
    }
    out
}

/// The text techniques' frames, drawn the way the terminal draws them: at
/// four by eight pixels a cell, one zoom deeper and twice as wide.
#[test]
fn the_terminal_techniques_dump_their_golden_frames() {
    let (cols, rows) = (60usize, 16usize);
    let (w, h) = ((cols * cells::SX) as u32, (rows * cells::SY) as u32);
    let view = Viewport::new(project(centre()), 16, w, h);
    let cases = [
        ("octant-pc-light.cells", Encoding::Octant, Style::PC),
        (
            "octant-pc-dark.cells",
            Encoding::Octant,
            Style::PC.in_theme(MapTheme::Dark),
        ),
        (
            "braille-thin-m9-light.cells",
            Encoding::BrailleThin,
            Style::M9,
        ),
    ];
    let sets = Sets::default();
    let mut moved = Vec::new();
    for (name, encoding, style) in cases {
        let to_rgb = |v: Vec<u8>| -> Vec<cells::Rgb> {
            v.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
        };
        let ground = cells::rgb_of(style.theme.palette().background);
        let mask = |percent| -> Vec<bool> {
            let ink = Ink::Lines(embedded_graphics::pixelcolor::Rgb565::new(0, 0, 0));
            to_rgb(draw(&view, ink, style, percent).0)
                .into_iter()
                .map(|c| c != ground)
                .collect()
        };
        let source = cells::Source {
            cols,
            rows,
            rgb: to_rgb(draw(&view, Ink::Colour, style, 200).0),
            line: mask(200),
            thin: mask(100),
            theme: style.theme,
        };
        let text = dump(&cells::encode(&source, encoding, &sets), cols, style.theme);
        let want = std::fs::read_to_string(golden(name)).unwrap_or_default();
        if want != text {
            moved.push(kept(name, text.as_bytes()).display().to_string());
        }
    }
    assert!(moved.is_empty(), "frames moved, this run's: {moved:?}");
}
