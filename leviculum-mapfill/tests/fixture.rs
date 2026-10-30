//! The fixture reader against its writer, and the committed Florence z15
//! fixture drawn under both profiles with the device binary's capacities.

use embedded_graphics::prelude::*;
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::fixture::{write, Fixture, FixtureError, Kind, BRIDGE, TUNNEL, VARIANT};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{render, NoClock, RenderStats, Work};
use leviculum_mapfill::style::{Class, Profile, BACKGROUND};

/// Pixels of the stripped view not in the earth colour: a large urban
/// landuse area covers the centre, so nearly all.
const STRIPPED_NOT_EARTH: u32 = 76_737;
const FLORENCE: &[u8] = include_bytes!("../fixtures/firenze-z15.lmf");

fn draw(fx: &Fixture<'_>, profile: Profile, fb: &mut [u16]) -> RenderStats {
    let (w, h) = fx.view();
    let mut edges = vec![Edge::default(); 4096];
    let mut active = vec![0u16; 4096];
    let mut heads = vec![0u16; h as usize];
    let mut points = vec![Point::zero(); 4096];
    let mut ring_ends = vec![0u16; 1024];
    let mut frame = Frame::new(fb, w, h).expect("frame");
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut work = Work {
        points: &mut points,
        ring_ends: &mut ring_ends,
    };
    let Ok(st) = render(
        fx,
        profile,
        &mut frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    );
    st
}

#[test]
fn writer_and_reader_agree() {
    let feats = [
        write::Feature {
            kind: Kind::Area,
            class: Class::Water,
            flags: 0,
            tile: 1,
            rings: vec![vec![(0, 0), (64, 0), (64, 64)], vec![(8, 8)]],
        },
        write::Feature {
            kind: Kind::Line,
            class: Class::Path,
            flags: 0,
            tile: 0,
            rings: vec![vec![(-5, 7), (300, -2)]],
        },
    ];
    let b = write::fixture(240, 320, -1, &[(10, 20), (-4096, 0)], &feats);
    let fx = Fixture::parse(&b).expect("parse");
    assert_eq!((fx.view(), fx.shift(), fx.len()), ((240, 320), -1, 2));
    let got: Vec<_> = fx.features().collect();
    assert_eq!(
        (got[0].kind, got[0].class, got[0].offset),
        (Kind::Area, Class::Water, Point::new(-4096, 0))
    );
    assert_eq!(got[0].ring_count(), 2);
    let rings: Vec<Vec<(i16, i16)>> = got[0].rings().map(|r| r.points().collect()).collect();
    assert_eq!(rings, feats[0].rings);
    assert_eq!(
        (got[1].kind, got[1].offset),
        (Kind::Line, Point::new(10, 20))
    );
    let line: Vec<(i16, i16)> = got[1].rings().next().expect("ring").points().collect();
    assert_eq!(line, feats[1].rings[0]);

    // Every malformed variant is refused at parse, not met while drawing.
    assert_eq!(Fixture::parse(b"LMF2").err(), Some(FixtureError::Magic));
    for cut in [5, 12, 30, b.len() - 1] {
        assert!(
            matches!(Fixture::parse(&b[..cut]), Err(FixtureError::Truncated(_))),
            "cut at {cut}"
        );
    }
    let mut long = b.clone();
    long.push(0);
    assert!(matches!(
        Fixture::parse(&long),
        Err(FixtureError::Trailing(_))
    ));
    let first = 12 + 2 * 8 + 4;
    let mut bad = b.clone();
    bad[first + 1] = 99;
    assert_eq!(Fixture::parse(&bad).err(), Some(FixtureError::Class(first)));
    let mut wrong_kind = b.clone();
    wrong_kind[first] = 2;
    assert_eq!(
        Fixture::parse(&wrong_kind).err(),
        Some(FixtureError::Class(first))
    );
    let mut tile = b.clone();
    tile[first + 2] = 2;
    assert_eq!(Fixture::parse(&tile).err(), Some(FixtureError::Tile(first)));
}

/// The classes and flags of 1099 in the byte a reader before them never
/// looked at: forest and the secondary road come back as written, and the
/// class byte alone, all such a reader reads, is the park's and the major
/// road's it drew them as before.
#[test]
fn the_flags_byte_carries_the_new_classes_past_an_old_reader() {
    let feats = [
        (Kind::Area, Class::Forest, 0),
        (Kind::Area, Class::Park, 0),
        (Kind::Line, Class::SecondaryRoad, BRIDGE),
        (Kind::Line, Class::MajorRoad, TUNNEL),
    ]
    .map(|(kind, class, flags)| write::Feature {
        kind,
        class,
        flags,
        tile: 0,
        rings: vec![vec![(0, 0), (64, 0), (64, 64)]],
    });
    let b = write::fixture(256, 256, 0, &[(0, 0)], &feats);
    let fx = Fixture::parse(&b).expect("parse");
    let got: Vec<_> = fx
        .features()
        .map(|f| (f.class, f.bridge(), f.tunnel()))
        .collect();
    assert_eq!(
        got,
        [
            (Class::Forest, false, false),
            (Class::Park, false, false),
            (Class::SecondaryRoad, true, false),
            (Class::MajorRoad, false, true),
        ]
    );
    // Each feature is 6 header bytes and one ring of three vertices.
    let first = 12 + 8 + 4;
    let header = |i: usize| &b[first + i * (6 + 2 + 12)..][..4];
    assert_eq!(header(0), [1, 2, 0, VARIANT]);
    assert_eq!(header(1), [1, 2, 0, 0]);
    assert_eq!(header(2), [2, 19, 0, VARIANT | BRIDGE]);
    assert_eq!(header(3), [2, 19, 0, TUNNEL]);
    // A bit no reader knows yet is ignored, as these were.
    let mut later = b.clone();
    later[first + 3] |= 0x80;
    let fx = Fixture::parse(&later).expect("parse");
    assert_eq!(fx.features().next().map(|f| f.class), Some(Class::Forest));
}

/// The numbers the draft's "Device measurement" section quotes for the
/// host; a change to the filler, the stroker or the fixture moves them.
#[test]
fn florence_z15_draws_both_profiles() {
    let fx = Fixture::parse(FLORENCE).expect("committed fixture parses");
    assert_eq!((fx.view(), fx.len()), ((240, 320), 6600));
    let mut fb = vec![0u16; 240 * 320];

    let full = draw(&fx, Profile::Full, &mut fb);
    assert_eq!(full.overflows, 0);
    assert_eq!(
        (
            full.features,
            full.culled,
            full.vertices,
            full.max_feature_vertices
        ),
        (6600, 3851, 55_183, 1051)
    );
    assert_eq!(
        (full.fill.edges, full.fill.spans, full.fill.pixels),
        (13_747, 16_778, 232_728)
    );
    assert_eq!(
        (full.stroke.edges, full.stroke.spans, full.stroke.pixels),
        (5919, 10_922, 21_422)
    );

    let stripped = draw(&fx, Profile::Stripped, &mut fb);
    assert_eq!(stripped.overflows, 0);
    assert_eq!(
        (stripped.features, stripped.culled, stripped.vertices),
        (1280, 762, 13_470)
    );
    assert_eq!(
        (
            stripped.fill.edges,
            stripped.fill.spans,
            stripped.fill.pixels
        ),
        (831, 2581, 174_195)
    );
    assert_eq!(stripped.stroke, full.stroke);

    // The stripped picture, pixel by pixel: how many differ from the
    // background (the earth colour).
    let frame = Frame::new(&mut fb, 240, 320).expect("frame");
    let mut drawn = 0;
    for y in 0..320 {
        for x in 0..240 {
            if frame.pixel(x, y) != Some(BACKGROUND) {
                drawn += 1;
            }
        }
    }
    assert_eq!(drawn, STRIPPED_NOT_EARTH);
}
