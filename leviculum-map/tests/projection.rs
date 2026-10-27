//! Web Mercator and the viewport: round trips, the antimeridian, the
//! view's edges, fit, pan and the tiles a view selects.

use leviculum_map::mercator::{
    lat_of_y, lon_of_x, project, unproject, x_of_lon, y_of_lat, LatLon, World, MAX_LAT_E7,
};
use leviculum_map::pins::{hit, place, Pin};
use leviculum_map::region::TileId;
use leviculum_map::viewport::{Viewport, MAX_OVERZOOM};

/// Santa Maria del Fiore.
const DUOMO: LatLon = LatLon {
    lat_e7: 437_731_000,
    lon_e7: 112_560_000,
};

#[test]
fn longitude_round_trips_exactly() {
    for lon in (-1_800_000_000i32..1_800_000_000).step_by(7_777_777) {
        assert_eq!(lon_of_x(x_of_lon(lon)), lon, "lon_e7 {lon}");
    }
    for lon in [-1_800_000_000, -1, 0, 1, 112_560_000, 1_799_999_999] {
        assert_eq!(lon_of_x(x_of_lon(lon)), lon, "lon_e7 {lon}");
    }
    assert_eq!(x_of_lon(0), 1 << 31);
    assert_eq!(x_of_lon(-1_800_000_000), 0);
}

#[test]
fn latitude_round_trips_to_the_e7_step() {
    // A world unit is finer than 10^-7 degrees of latitude everywhere, so
    // the round trip lands on the same e7 step.
    for lat in (-MAX_LAT_E7..=MAX_LAT_E7).step_by(3_333_333) {
        assert_eq!(lat_of_y(y_of_lat(lat)), lat, "lat_e7 {lat}");
    }
    assert_eq!(y_of_lat(0), 1 << 31);
    assert_eq!(y_of_lat(MAX_LAT_E7), 0);
    assert_eq!(y_of_lat(-MAX_LAT_E7), u32::MAX);
    // Beyond the projection's edge is the edge.
    assert_eq!(y_of_lat(900_000_000), 0);
    assert_eq!(unproject(project(DUOMO)), DUOMO);
}

#[test]
fn the_antimeridian_is_one_line() {
    assert_eq!(x_of_lon(1_800_000_000), 0);
    assert_eq!(x_of_lon(1_800_000_000), x_of_lon(-1_800_000_000));
    // 180 E reads back as 180 W.
    assert_eq!(lon_of_x(x_of_lon(1_800_000_000)), -1_800_000_000);
    // The last e7 step west of it is the last x.
    assert_eq!(x_of_lon(1_799_999_999), u32::MAX);

    // A view centred just west of the line shows a point just east of it
    // to the right of the centre, not a world away.
    let west = project(LatLon {
        lat_e7: 0,
        lon_e7: 1_799_900_000,
    });
    let east = project(LatLon {
        lat_e7: 0,
        lon_e7: -1_799_900_000,
    });
    let v = Viewport::new(west, 15, 240, 320);
    let (x, y) = v.to_screen(east);
    assert_eq!(y, 160);
    // 0.02 degrees at z15: 0.02 / 360 * 2^15 * 256 pixels.
    assert_eq!(x, 120 + 466);
    let v = Viewport::new(west, 10, 240, 320);
    assert!(v.contains(east));
    assert_eq!(v.to_screen(east).0, 120 + 14);

    // Panning east across the line wraps the centre.
    // 100 pixels at z10 are 100 << 14 units; the line was 0.01 degrees,
    // 119,305 units, away.
    let mut v = Viewport::new(west, 10, 240, 320);
    v.pan(100, 0);
    assert_eq!(v.centre.x, west.x.wrapping_add(100 << 14));
    assert!(v.centre.x < 1 << 21, "wrapped to the west edge of x");

    // Its tiles wrap too: the last column, then the first.
    let v = Viewport::new(
        project(LatLon {
            lat_e7: 0,
            lon_e7: 1_800_000_000,
        }),
        3,
        240,
        320,
    );
    let cols: Vec<u32> = v.tiles(0..=15).iter().map(|t| t.id.x).collect();
    assert!(cols.contains(&7) && cols.contains(&0), "{cols:?}");
}

#[test]
fn a_pin_on_the_left_edge_is_in_and_on_the_right_edge_out() {
    let v = Viewport::new(project(DUOMO), 15, 240, 320);
    // One pixel at z15 is 512 world units.
    let left = World {
        x: v.centre.x - 120 * 512,
        y: v.centre.y,
    };
    let right = World {
        x: v.centre.x + 120 * 512,
        y: v.centre.y,
    };
    let top = World {
        x: v.centre.x,
        y: v.centre.y - 160 * 512,
    };
    let bottom = World {
        x: v.centre.x,
        y: v.centre.y + 160 * 512,
    };
    assert_eq!(v.to_screen(left), (0, 160));
    assert!(v.contains(left));
    assert_eq!(v.to_screen(right), (240, 160));
    assert!(!v.contains(right));
    assert!(v.contains(top));
    assert!(!v.contains(bottom));
    // One unit inside the right edge is the last pixel.
    let inside = World {
        x: right.x - 512,
        ..right
    };
    assert_eq!(v.to_screen(inside).0, 239);

    let pins = [
        Pin {
            at: left,
            label: "left",
            stale: false,
        },
        Pin {
            at: right,
            label: "right",
            stale: false,
        },
        Pin {
            at: v.centre,
            label: "centre",
            stale: true,
        },
    ];
    let placed = place(&v, &pins);
    assert_eq!(placed.len(), 2);
    assert_eq!((placed[0].index, placed[0].x, placed[0].y), (0, 0, 160));
    assert_eq!((placed[1].index, placed[1].x, placed[1].y), (2, 120, 160));
    assert_eq!(hit(&placed, 3, 158, 3), Some(0));
    assert_eq!(hit(&placed, 118, 160, 3), Some(2));
    assert_eq!(hit(&placed, 60, 160, 3), None);
}

#[test]
fn the_world_under_a_pixel_lands_back_on_it() {
    let v = Viewport::new(project(DUOMO), 14, 241, 319);
    for (x, y) in [(0, 0), (120, 159), (240, 318), (17, 301)] {
        let w = v.to_world(x, y);
        assert_eq!(v.to_screen(w), (i64::from(x), i64::from(y)));
    }
}

#[test]
fn fit_shows_every_point_at_the_deepest_zoom_that_does() {
    let pts: Vec<World> = [
        (437_731_000, 112_560_000),
        (437_700_000, 112_400_000),
        (437_900_000, 112_700_000),
    ]
    .iter()
    .map(|&(lat_e7, lon_e7)| project(LatLon { lat_e7, lon_e7 }))
    .collect();
    let v = Viewport::fit(&pts, 240, 320, 8, 0..=18).expect("points");
    assert!(pts.iter().all(|p| v.contains(*p)));
    for p in &pts {
        let (x, y) = v.to_screen(*p);
        assert!((8..232).contains(&x) && (8..312).contains(&y), "{x},{y}");
    }
    // One zoom deeper and they would not fit.
    let mut deeper = v;
    deeper.set_zoom(v.zoom() + 1);
    assert!(!pts.iter().all(|p| deeper.contains(*p)));
    assert_eq!(v.zoom(), 13);

    // One point: the deepest zoom allowed, centred on it.
    let one = Viewport::fit(&pts[..1], 240, 320, 8, 0..=16).expect("point");
    assert_eq!((one.zoom(), one.centre), (16, pts[0]));
    assert!(Viewport::fit(&[], 240, 320, 8, 0..=16).is_none());

    // Points on both sides of the antimeridian fit across it, not across
    // the whole world the other way.
    let across = [
        project(LatLon {
            lat_e7: -170_000_000,
            lon_e7: 1_790_000_000,
        }),
        project(LatLon {
            lat_e7: -180_000_000,
            lon_e7: -1_790_000_000,
        }),
    ];
    let v = Viewport::fit(&across, 240, 320, 8, 0..=18).expect("points");
    assert!(across.iter().all(|p| v.contains(*p)));
    assert!(v.zoom() >= 6, "zoom {}", v.zoom());
    assert!(lon_of_x(v.centre.x).abs() > 1_790_000_000);
}

#[test]
fn tile_selection_at_the_zoom_boundaries() {
    let centre = project(DUOMO);
    // At the region's deepest zoom: its own tiles, not overzoomed.
    let v = Viewport::new(centre, 15, 240, 320);
    let at = v.tiles(11..=15);
    assert!(at.iter().all(|t| t.id.z == 15 && t.overzoom == 0));
    assert_eq!(at.len(), 4, "the measurement's view touches four z15 tiles");
    let mut ids: Vec<TileId> = at.iter().map(|t| t.id).collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            (17408, 11943),
            (17408, 11944),
            (17409, 11943),
            (17409, 11944)
        ]
        .map(|(x, y)| TileId { z: 15, x, y })
    );
    // One past it: the deepest tiles at twice the size.
    let v = Viewport::new(centre, 16, 240, 320);
    let past = v.tiles(11..=15);
    assert!(past.iter().all(|t| t.id.z == 15 && t.overzoom == 1));
    assert!(past.len() <= 4);
    // Shallower than the region's shallowest, and too far past its
    // deepest: nothing to draw.
    assert!(Viewport::new(centre, 10, 240, 320)
        .tiles(11..=15)
        .is_empty());
    let far = 10 + MAX_OVERZOOM + 1;
    assert!(Viewport::new(centre, far, 240, 320)
        .tiles(5..=10)
        .is_empty());
    let deepest = Viewport::new(centre, far - 1, 240, 320).tiles(5..=10);
    assert!(!deepest.is_empty());
    assert!(deepest.iter().all(|t| t.overzoom == MAX_OVERZOOM));

    // A view whose left edge lies exactly on a tile edge does not select
    // the tile to its left; one sub-pixel further and it does.
    let edge = World {
        x: (17409u32 << 17) + 120 * 512,
        y: (11943u32 << 17) + 160 * 512,
    };
    let v = Viewport::new(edge, 15, 240, 320);
    let t = v.tiles(15..=15);
    assert_eq!(t.len(), 2);
    assert!(t.iter().all(|t| t.id.x == 17409 && t.origin.x == 0));
    let v = Viewport::new(
        World {
            x: edge.x - 32,
            ..edge
        },
        15,
        240,
        320,
    );
    let t = v.tiles(15..=15);
    assert_eq!(t.len(), 4);
    assert!(t.iter().any(|t| t.id.x == 17408 && t.origin.x == 1 - 4096));
    // The tiles are placed where the projection puts their corners.
    for t in v.tiles(15..=15) {
        let corner = World {
            x: t.id.x << 17,
            y: t.id.y << 17,
        };
        let (x, y) = v.to_screen_sub(corner);
        assert_eq!((x, y), (i64::from(t.origin.x), i64::from(t.origin.y)));
    }
}

#[test]
fn rows_outside_the_projection_are_not_selected() {
    let north = World {
        x: 1 << 31,
        y: 1000,
    };
    let v = Viewport::new(north, 5, 240, 320);
    assert!(v.tiles(0..=15).iter().all(|t| t.id.y < 32));
    assert!(v.tiles(0..=15).iter().all(|t| t.id.y == 0 || t.id.y == 1));
}
