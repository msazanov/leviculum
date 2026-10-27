//! What slice (c) asked of the core (leviculum 1065): the line widths per
//! zoom band, the world outline where no region is, the region a view
//! draws when several are installed, the zooms a frontend offers, and the
//! order `Tab` steps the pins in.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use leviculum_map::choice::{choose, useful_zooms, view_zooms};
use leviculum_map::draw::{draw, Ink};
use leviculum_map::mercator::{project, LatLon, World};
use leviculum_map::outline::{self, OutlineStats, WORLD_110M};
use leviculum_map::pins::{order, Pin};
use leviculum_map::region::{Bounds, Header, Region};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{NoClock, RenderStats, Work};
use leviculum_mapfill::style::{Profile, BACKGROUND};

const FLORENCE: &[u8] = include_bytes!("../fixtures/firenze.lmr");

const CENTRE: LatLon = LatLon {
    lat_e7: 437_750_000,
    lon_e7: 112_610_000,
};

fn region_view(zoom: u8, ink: Ink, fb: &mut [u16]) -> RenderStats {
    let r = Region::from_bytes(FLORENCE).expect("open");
    let view = Viewport::new(project(CENTRE), zoom, 240, 320);
    let tiles = r.view(&view).expect("tiles");
    let mut edges = vec![Edge::default(); 4096];
    let mut active = vec![0u16; 4096];
    let mut heads = vec![0u16; 320];
    let mut points = vec![Point::zero(); 4096];
    let mut ring_ends = vec![0u16; 1024];
    let mut frame = Frame::new(fb, 240, 320).expect("frame");
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut work = Work {
        points: &mut points,
        ring_ends: &mut ring_ends,
    };
    let Ok(st) = draw(
        &tiles,
        Profile::Full,
        ink,
        &mut frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    );
    st
}

fn count(fb: &mut [u16], w: u32, h: u32, colour: Rgb565) -> u32 {
    let frame = Frame::new(fb, w, h).expect("frame");
    let mut n = 0;
    for y in 0..h {
        for x in 0..w {
            if frame.pixel(x, y) == Some(colour) {
                n += 1;
            }
        }
    }
    n
}

/// The region zooms draw thinner lines and no paths or minor roads where
/// the fixtures' widths drew a white mesh. Before 1065 the z12 view drew
/// 543 features and 35,640 stroke pixels, the z11 view 325 and 21,777.
#[test]
fn region_zooms_draw_thinner_lines() {
    let mut fb = vec![0u16; 240 * 320];
    let mut rows = Vec::new();
    for zoom in [12, 11] {
        let st = region_view(zoom, Ink::Colour, &mut fb);
        assert_eq!(st.overflows, 0);
        rows.push((zoom, st.features, st.stroke.pixels));
    }
    assert_eq!(rows, [(12, 537, 12_885), (11, 325, 9_574)]);
}

/// Lines alone: no area filled, every inked pixel the one colour.
#[test]
fn the_lines_ink_fills_nothing() {
    let ink = Rgb565::new(0, 0, 0);
    let mut fb = vec![0u16; 240 * 320];
    let st = region_view(13, Ink::Lines(ink), &mut fb);
    assert_eq!(st.fill.pixels, 0);
    let (inked, bg) = (
        count(&mut fb, 240, 320, ink),
        count(&mut fb, 240, 320, BACKGROUND),
    );
    assert_eq!(inked + bg, 240 * 320, "only the ink and the background");
    assert_eq!(
        (st.features, st.stroke.pixels, inked),
        (283, 33_138, 17_994)
    );
}

#[test]
fn the_outline_table_is_natural_earth_110m() {
    let rings: Vec<_> = outline::rings(WORLD_110M).collect();
    assert_eq!(rings.len(), 128);
    assert_eq!(rings.iter().map(|r| r.len()).sum::<usize>(), 5003);
    assert_eq!(rings.iter().filter(|r| r.hole).count(), 1);
    assert!(WORLD_110M.len() < 64 * 1024, "{}", WORLD_110M.len());
    // Cut short anywhere, it yields fewer rings and never panics.
    for cut in [0, 5, 6, 15, 1000, WORLD_110M.len() - 1] {
        assert!(outline::rings(&WORLD_110M[..cut]).count() < 128);
    }
}

fn outline_view(view: &Viewport, ink: Ink, fb: &mut [u16]) -> OutlineStats {
    let (w, h) = view.size();
    let mut frame = Frame::new(fb, w, h).expect("frame");
    let Ok(st) = outline::draw(WORLD_110M, view, ink, &mut frame);
    st
}

/// The world at z2 with no region: land in the earth colour over water,
/// and the same view as coasts alone.
#[test]
fn the_outline_draws_the_world_at_z2() {
    let view = Viewport::new(project(LatLon::from_degrees(20.0, 10.0)), 2, 240, 160);
    let mut fb = vec![0u16; 240 * 160];
    let st = outline_view(&view, Ink::Colour, &mut fb);
    assert_eq!(st.overflows, 0);
    let land = count(&mut fb, 240, 160, BACKGROUND);
    let water = count(&mut fb, 240, 160, outline::water());
    assert_eq!(land + water, 240 * 160);
    assert_eq!(
        (st.rings, st.culled, st.vertices, st.fill.pixels, land),
        (7, 121, 624, 22_183, 22_183)
    );
    let ink = Rgb565::new(0, 0, 0);
    let st = outline_view(&view, Ink::Lines(ink), &mut fb);
    assert_eq!(st.fill.pixels, 0);
    let coast = count(&mut fb, 240, 160, ink);
    assert_eq!((st.rings, st.stroke.pixels, coast), (7, 1_199, 1_199));
}

/// Across the antimeridian the world repeats: a view centred on 180 E draws
/// the land of both sides, and at z0 a view wider than the world (256
/// pixels) draws two copies of it. Every point stays inside the filler's limit at the deepest zoom.
#[test]
fn the_outline_wraps_and_holds_at_every_zoom() {
    let mut fb = vec![0u16; 400 * 100];
    let wide = Viewport::new(World { x: 0, y: 1 << 31 }, 0, 400, 100);
    let st = outline_view(&wide, Ink::Colour, &mut fb);
    assert_eq!(st.rings + st.culled, 2 * 128, "two copies of the world");
    let land = count(&mut fb, 400, 100, BACKGROUND);
    assert!(land > 0);
    for zoom in [8, 14, 20] {
        let view = Viewport::new(project(CENTRE), zoom, 100, 100);
        let mut fb = vec![0u16; 100 * 100];
        let st = outline_view(&view, Ink::Colour, &mut fb);
        assert_eq!(st.overflows, 0, "z{zoom}");
        // Florence is on land at every zoom.
        assert_eq!(count(&mut fb, 100, 100, BACKGROUND), 100 * 100, "z{zoom}");
    }
}

fn header(min: u8, max: u8, bounds: Bounds) -> Header {
    let mut h = *Region::from_bytes(FLORENCE).expect("open").header();
    h.min_zoom = min;
    h.max_zoom = max;
    h.bounds = bounds;
    h
}

/// Two overlapping regions: the deeper wins where both cover the centre,
/// a tie goes to the first file name, and outside both, or above both
/// zoom ranges, the view draws the outline.
#[test]
fn the_region_with_the_deepest_zoom_at_the_centre_is_drawn() {
    let florence = *Region::from_bytes(FLORENCE).expect("open").header();
    let tuscany = header(
        7,
        13,
        Bounds {
            west: 96_000_000,
            south: 424_000_000,
            east: 124_000_000,
            north: 442_000_000,
        },
    );
    let at = project(CENTRE);
    let siena = project(LatLon::from_degrees(43.318, 11.331));
    let both = [("tuscany.lmr", &tuscany), ("firenze.lmr", &florence)];
    assert_eq!(choose(both, at, 13), Some(1));
    assert_eq!(choose(both, siena, 13), Some(0), "only Tuscany holds Siena");
    // Florence starts at z11; Tuscany draws z9.
    assert_eq!(choose(both, at, 9), Some(0));
    assert_eq!(choose(both, at, 6), None, "above both: the outline");
    assert_eq!(
        choose(both, project(LatLon::from_degrees(0.0, 0.0)), 12),
        None
    );
    // Equal max zoom: the first name.
    let twin = header(11, 15, florence.bounds);
    assert_eq!(
        choose([("b.lmr", &florence), ("a.lmr", &twin)], at, 13),
        Some(1)
    );
    assert_eq!(
        choose([("a.lmr", &florence), ("b.lmr", &twin)], at, 13),
        Some(0)
    );
}

/// A box across the antimeridian holds both sides of it.
#[test]
fn a_box_across_the_antimeridian_holds_both_sides() {
    let fiji = Bounds {
        west: 1_770_000_000,
        south: -190_000_000,
        east: -1_790_000_000,
        north: -160_000_000,
    };
    assert!(fiji.contains(LatLon::from_degrees(-17.0, 178.0)));
    assert!(fiji.contains(LatLon::from_degrees(-17.0, -179.5)));
    assert!(!fiji.contains(LatLon::from_degrees(-17.0, 0.0)));
}

#[test]
fn the_zoom_range_reaches_from_the_world_to_the_deepest_overzoom() {
    let florence = *Region::from_bytes(FLORENCE).expect("open").header();
    assert_eq!(useful_zooms(&florence), 11..=20);
    let shallow = header(0, 6, florence.bounds);
    assert_eq!(useful_zooms(&shallow), 0..=14);
    assert_eq!(view_zooms([]), 0..=outline::MAX_ZOOM);
    assert_eq!(view_zooms([&shallow]), 0..=14);
    assert_eq!(view_zooms([&shallow, &florence]), 0..=20);
}

/// `Tab` goes by name, case folded, and equal names by hash.
#[test]
fn pins_are_stepped_by_name_then_hash() {
    let at = World { x: 0, y: 0 };
    let pin = |label| Pin {
        at,
        label,
        stale: false,
    };
    let pins = [pin("carol"), pin("Bob"), pin("alice"), pin("bob")];
    let hashes = [[3u8; 16], [9; 16], [1; 16], [2; 16]];
    assert_eq!(order(&pins, |i| hashes[i]), [2, 3, 1, 0]);
}
