//! The committed Florence region: read in place, refused whole when cut
//! short or corrupted (an error, never a panic), and drawn at three zooms
//! with pinned counts, as `leviculum-mapfill/tests/fixture.rs` pins the
//! single-view fixture.

use embedded_graphics::prelude::*;
use leviculum_map::draw::{draw, draw_tiles};
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::region::{Entry, Region, RegionError, TileId, ENTRY_LEN, HEADER_LEN};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{NoClock, RenderStats, Work};
use leviculum_mapfill::style::{Profile, BACKGROUND};

const FLORENCE: &[u8] = include_bytes!("../fixtures/firenze.lmr");

/// On the seam of the region's two z15 tiles, inside its box.
const CENTRE: LatLon = LatLon {
    lat_e7: 437_750_000,
    lon_e7: 112_610_000,
};

fn render(region: &Region<&[u8]>, zoom: u8, fb: &mut [u16]) -> (usize, RenderStats) {
    let view = Viewport::new(project(CENTRE), zoom, 240, 320);
    let tiles = region.view(&view).expect("tiles");
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
        &mut frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    );
    (tiles.len(), st)
}

fn not_background(fb: &mut [u16]) -> u32 {
    let frame = Frame::new(fb, 240, 320).expect("frame");
    let mut n = 0;
    for y in 0..320 {
        for x in 0..240 {
            if frame.pixel(x, y) != Some(BACKGROUND) {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn the_fixture_opens_and_verifies() {
    let r = Region::from_bytes(FLORENCE).expect("open");
    r.verify().expect("verify");
    let h = r.header();
    assert_eq!((h.min_zoom, h.max_zoom, h.profile, h.tiles), (11, 15, 1, 6));
    assert_eq!(h.len, FLORENCE.len() as u64);
    assert_eq!(r.attribution(), "© OpenStreetMap contributors");
    assert!(r.notice().contains("Open Database License (ODbL) 1.0"));
    assert!(r.notice().contains("Protomaps Basemap 3.7.1"));
    let z15 = TileId {
        z: 15,
        x: 17408,
        y: 11943,
    };
    assert!(r.find(z15).expect("find").is_some());
    assert!(r.find(TileId { y: 11944, ..z15 }).expect("find").is_none());
    assert!(r.fixture(z15).expect("tile").is_some());
}

/// The numbers a change to the cut, the filler, the stroker or the draw
/// order moves. Background is the earth colour, so a pixel counted in the
/// last column is one some feature other than earth painted.
#[test]
fn florence_region_draws_three_zooms() {
    let r = Region::from_bytes(FLORENCE).expect("open");
    let mut fb = vec![0u16; 240 * 320];
    let mut rows = Vec::new();
    for zoom in [15, 13, 11] {
        let (tiles, st) = render(&r, zoom, &mut fb);
        assert_eq!(st.overflows, 0, "z{zoom}");
        rows.push((
            zoom,
            tiles,
            st.features,
            st.culled,
            st.vertices,
            (st.fill.edges, st.fill.spans, st.fill.pixels),
            (st.stroke.edges, st.stroke.spans, st.stroke.pixels),
            not_background(&mut fb),
        ));
    }
    assert_eq!(
        rows,
        [
            (
                15,
                2,
                617,
                282,
                4868,
                (1220, 3200, 151_645),
                (2353, 7415, 14_735),
                63_842
            ),
            (
                13,
                1,
                528,
                126,
                10_301,
                (1260, 2266, 78_167),
                (10_732, 15_986, 40_763),
                36_187
            ),
            (
                11,
                1,
                325,
                79,
                5365,
                (1832, 2809, 59_154),
                (3044, 4717, 21_777),
                22_397
            ),
        ]
    );
}

/// The same view read through `Storage` into buffers, as a file or a card
/// is read, is the same set of tiles as the in-place one.
#[test]
fn a_view_read_into_buffers_matches_the_view_in_place() {
    let r = Region::from_bytes(FLORENCE).expect("open");
    let mut read = Vec::new();
    for zoom in [11, 13, 15, 16] {
        let view = Viewport::new(project(CENTRE), zoom, 240, 320);
        r.read_view(&view, &mut read).expect("read");
        let buffered = draw_tiles(&read).expect("tiles");
        let in_place = r.view(&view).expect("tiles");
        assert_eq!(buffered.len(), in_place.len(), "z{zoom}");
        for (a, b) in buffered.iter().zip(&in_place) {
            assert_eq!(a.tile, b.tile);
            assert_eq!(a.fixture.len(), b.fixture.len());
        }
    }
}

#[test]
fn every_truncation_is_an_error() {
    for cut in 0..FLORENCE.len() {
        let r = Region::from_bytes(&FLORENCE[..cut]);
        assert!(r.is_err(), "cut at {cut} opened");
    }
}

/// Open, verify, and draw every tile at every zoom: whatever the bytes,
/// an error or a picture, never a panic.
fn survives(bytes: &[u8]) -> Result<(), RegionError> {
    let r = Region::from_bytes(bytes)?;
    let verified = r.verify();
    let mut fb = vec![0u16; 240 * 320];
    for zoom in [11, 15, 16] {
        let view = Viewport::new(project(CENTRE), zoom, 240, 320);
        if let Ok(tiles) = r.view(&view) {
            let mut edges = vec![Edge::default(); 4096];
            let mut active = vec![0u16; 4096];
            let mut heads = vec![0u16; 320];
            let mut points = vec![Point::zero(); 4096];
            let mut ring_ends = vec![0u16; 1024];
            let mut frame = Frame::new(&mut fb, 240, 320).expect("frame");
            let mut scratch = Scratch {
                edges: &mut edges,
                heads: &mut heads,
                active: &mut active,
            };
            let mut work = Work {
                points: &mut points,
                ring_ends: &mut ring_ends,
            };
            let Ok(_) = draw(
                &tiles,
                Profile::Full,
                &mut frame,
                &mut scratch,
                &mut work,
                &mut NoClock,
            );
        }
    }
    verified
}

#[test]
fn corrupted_bytes_are_an_error_not_a_panic() {
    assert_eq!(survives(FLORENCE), Ok(()));
    let h = Region::from_bytes(FLORENCE).expect("open");
    let index = h.header().index as usize;

    let mut b = FLORENCE.to_vec();
    b[0] = b'X';
    assert_eq!(survives(&b).err(), Some(RegionError::Magic));
    let mut b = FLORENCE.to_vec();
    b[4] = 2;
    assert_eq!(survives(&b).err(), Some(RegionError::Version(2)));
    let mut b = FLORENCE.to_vec();
    b[25] = 17;
    assert_eq!(survives(&b).err(), Some(RegionError::Header("zoom")));
    let mut b = FLORENCE.to_vec();
    b.push(0);
    assert_eq!(survives(&b).err(), Some(RegionError::Truncated));

    // Two entries swapped: out of order.
    let mut b = FLORENCE.to_vec();
    let (e0, e1) = (index, index + ENTRY_LEN);
    let first: Vec<u8> = b[e0..e1].to_vec();
    b.copy_within(e1..e1 + ENTRY_LEN, e0);
    b[e1..e1 + ENTRY_LEN].copy_from_slice(&first);
    assert_eq!(survives(&b).err(), Some(RegionError::Entry(1)));

    // An entry pointing past the data, and into the header.
    let entry = |b: &[u8]| {
        let mut e = [0u8; ENTRY_LEN];
        e.copy_from_slice(&b[index..index + ENTRY_LEN]);
        Entry::parse(&e).expect("entry")
    };
    for offset in [index as u64, 3] {
        let mut b = FLORENCE.to_vec();
        let mut e = entry(&b);
        e.offset = offset;
        b[index..index + ENTRY_LEN].copy_from_slice(&e.encode());
        assert_eq!(survives(&b).err(), Some(RegionError::Entry(0)));
    }

    // Every single byte flipped, header, text, tiles and index: never a
    // panic. Most flips inside a tile's geometry are still a valid picture,
    // so only the outcome being reached is asserted.
    for at in (0..FLORENCE.len()).step_by(1009).chain(0..HEADER_LEN) {
        let mut b = FLORENCE.to_vec();
        b[at] ^= 0xA5;
        let _ = survives(&b);
    }
}
