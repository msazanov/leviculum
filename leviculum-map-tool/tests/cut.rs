//! The cut over synthetic vector tiles: deterministic whatever the thread
//! count or the order the source lists its tiles, the profile applied, and
//! the output a region the core reads.

use std::collections::BTreeMap;
use std::io::Cursor;

use leviculum_map::region::{Bounds, Region, TileId};
use leviculum_map_tool::cut::{cut, cut_tile, in_bounds, CutOptions, DEVICE_POINTS};
use leviculum_map_tool::{Res, TileSource};
use leviculum_mapfill::fixture::Kind;
use leviculum_mapfill::style::Class;
use prost::Message;
use tinymvt::geometry::GeometryEncoder;
use tinymvt::vector_tile::tile::{Feature, Layer, Value};
use tinymvt::vector_tile::Tile;

const POLYGON: i32 = 3;
const LINESTRING: i32 = 2;
const POINT: i32 = 1;

fn square(x: i32, y: i32, side: i32) -> Vec<[i32; 2]> {
    // Clockwise on screen, positive area: an outer ring.
    vec![[x, y], [x + side, y], [x + side, y + side], [x, y + side]]
}

fn layer(name: &str, features: Vec<(i32, &str, Vec<u32>)>) -> Layer {
    let mut values = Vec::new();
    let mut fs = Vec::new();
    for (t, kind, geometry) in features {
        let tags = if kind.is_empty() {
            vec![]
        } else {
            values.push(Value {
                string_value: Some(kind.to_owned()),
                ..Value::default()
            });
            vec![0, values.len() as u32 - 1]
        };
        fs.push(Feature {
            id: None,
            tags,
            r#type: Some(t),
            geometry,
        });
    }
    Layer {
        version: 2,
        name: name.to_owned(),
        features: fs,
        keys: vec!["kind".to_owned()],
        values,
        extent: Some(4096),
    }
}

fn polygon(rings: &[Vec<[i32; 2]>]) -> Vec<u32> {
    let mut e = GeometryEncoder::new();
    for r in rings {
        e.add_ring(r.iter().copied());
    }
    e.into_vec()
}

fn line(pts: &[[i32; 2]]) -> Vec<u32> {
    let mut e = GeometryEncoder::new();
    e.add_linestring(pts.iter().copied());
    e.into_vec()
}

/// A tile with one of each thing the profile acts on.
fn busy_tile() -> Vec<u8> {
    let mut hole = square(1000, 1000, 500);
    hole.reverse();
    // A straight road with 50 collinear vertices.
    let road: Vec<[i32; 2]> = (0..50).map(|i| [i * 80, 2000]).collect();
    // A long wiggly path over the device's capacity.
    let wiggle: Vec<[i32; 2]> = (0..(DEVICE_POINTS as i32 + 500))
        .map(|i| [i % 4000, (i / 4000) * 100 + (i % 2) * 40])
        .collect();
    Tile {
        layers: vec![
            layer("earth", vec![(POLYGON, "", polygon(&[square(0, 0, 4096)]))]),
            layer(
                "landuse",
                vec![
                    (POLYGON, "park", polygon(&[square(500, 500, 2000), hole])),
                    // Three pixels square at z15: thinned only at region zooms.
                    (POLYGON, "park", polygon(&[square(3000, 3000, 48)])),
                ],
            ),
            layer(
                "buildings",
                vec![(POLYGON, "building", polygon(&[square(100, 100, 50)]))],
            ),
            layer(
                "roads",
                vec![
                    (LINESTRING, "major_road", line(&road)),
                    (LINESTRING, "path", line(&wiggle)),
                ],
            ),
            layer(
                "water",
                vec![(LINESTRING, "river", line(&[[0, 10], [4096, 30]]))],
            ),
            layer(
                "pois",
                vec![(POINT, "cafe", {
                    let mut e = GeometryEncoder::new();
                    e.add_points([[5, 5]]);
                    e.into_vec()
                })],
            ),
        ],
    }
    .encode_to_vec()
}

fn sea_tile() -> Vec<u8> {
    Tile {
        layers: vec![layer(
            "water",
            vec![(POLYGON, "ocean", polygon(&[square(0, 0, 4096)]))],
        )],
    }
    .encode_to_vec()
}

struct Synthetic {
    tiles: BTreeMap<TileId, Vec<u8>>,
    reversed: bool,
}

impl TileSource for Synthetic {
    fn tiles(&self) -> Vec<TileId> {
        let mut t: Vec<TileId> = self.tiles.keys().copied().collect();
        if self.reversed {
            t.reverse();
        }
        t
    }

    fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        Ok(self.tiles.get(&t).cloned())
    }
}

/// Around Santa Maria del Fiore: the z15 tiles 17408..=17409 x 11943.
const BOX: Bounds = Bounds {
    west: 112_505_000,
    south: 437_728_000,
    east: 112_700_000,
    north: 437_785_000,
};

fn source(reversed: bool) -> Synthetic {
    let mut tiles = BTreeMap::new();
    let t = |z, x, y| TileId { z, x, y };
    tiles.insert(t(15, 17408, 11943), busy_tile());
    // Two sea tiles with the same bytes: stored once.
    tiles.insert(t(15, 17409, 11943), sea_tile());
    tiles.insert(t(14, 8704, 5971), sea_tile());
    tiles.insert(t(12, 2176, 1492), busy_tile());
    // Outside the box, and outside the zoom range.
    tiles.insert(t(15, 17500, 11943), busy_tile());
    tiles.insert(t(9, 272, 186), busy_tile());
    Synthetic { tiles, reversed }
}

fn options(threads: usize) -> CutOptions {
    CutOptions {
        bounds: BOX,
        zooms: 10..=15,
        profile: 1,
        attribution: "© OpenStreetMap contributors".to_owned(),
        notice: "test notice, ODbL".to_owned(),
        threads,
    }
}

fn run(src: &Synthetic, threads: usize) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    cut(src, &options(threads), &mut out).expect("cut");
    out.into_inner()
}

#[test]
fn the_cut_is_deterministic() {
    let a = run(&source(false), 1);
    let b = run(&source(true), 4);
    let c = run(&source(false), 7);
    assert_eq!(a, b);
    assert_eq!(a, c);
}

#[test]
fn the_cut_applies_profile_one_and_reads_back() {
    let bytes = run(&source(false), 3);
    let r = Region::from_bytes(&bytes).expect("open");
    r.verify().expect("verify");
    let ids: Vec<TileId> = (0..r.header().tiles)
        .map(|n| r.entry(n).expect("entry").id)
        .collect();
    assert_eq!(
        ids,
        [
            (12, 2176, 1492),
            (14, 8704, 5971),
            (15, 17408, 11943),
            (15, 17409, 11943)
        ]
        .map(|(z, x, y)| TileId { z, x, y })
    );
    assert_eq!(r.attribution(), "© OpenStreetMap contributors");
    // The two sea tiles share one blob.
    let sea14 = r.entry(1).expect("entry");
    let sea15 = r.entry(3).expect("entry");
    assert_eq!((sea14.offset, sea14.len), (sea15.offset, sea15.len));

    let classes = |id: TileId| -> Vec<(Class, usize)> {
        let fx = r.fixture(id).expect("tile").expect("present");
        fx.features()
            .map(|f| (f.class, f.rings().map(|r| r.len()).sum()))
            .collect()
    };
    // z15: no building, no point; the park keeps its hole and the small
    // park stays; the straight road is two vertices; the long path is cut
    // into two features the device holds, sharing an end point; the river
    // is a line.
    let z15 = classes(TileId {
        z: 15,
        x: 17408,
        y: 11943,
    });
    let names: Vec<Class> = z15.iter().map(|c| c.0).collect();
    assert_eq!(
        names,
        [
            Class::Earth,
            Class::Park,
            Class::Park,
            Class::Waterway,
            Class::Path,
            Class::Path,
            Class::MajorRoad
        ]
    );
    assert_eq!(z15[1].1, 8, "outer ring and hole");
    assert_eq!(z15[6].1, 2, "the straight road");
    assert_eq!(z15[4].1, DEVICE_POINTS);
    assert!((1..DEVICE_POINTS).contains(&z15[5].1));
    let fx = r
        .fixture(TileId {
            z: 15,
            x: 17408,
            y: 11943,
        })
        .expect("tile")
        .expect("present");
    assert_eq!(fx.shift(), 0);
    assert!(fx.features().all(|f| f.class != Class::Building));
    let path = fx
        .features()
        .find(|f| f.class == Class::Path)
        .expect("path");
    assert_eq!(path.kind, Kind::Line);

    // z12, a region zoom: extent 512, the small park thinned away.
    let z12 = r
        .fixture(TileId {
            z: 12,
            x: 2176,
            y: 1492,
        })
        .expect("tile")
        .expect("present");
    assert_eq!(z12.shift(), 3);
    let parks = z12.features().filter(|f| f.class == Class::Park).count();
    assert_eq!(parks, 1);
}

#[test]
fn tiles_outside_the_box_or_the_zooms_are_left_out() {
    let t = |z, x, y| TileId { z, x, y };
    assert!(in_bounds(t(15, 17408, 11943), &BOX));
    assert!(!in_bounds(t(15, 17500, 11943), &BOX));
    assert!(!in_bounds(t(15, 17408, 11000), &BOX));
    assert!(in_bounds(t(0, 0, 0), &BOX));
    // A box across the antimeridian holds both edge columns.
    let across = Bounds {
        west: 1_790_000_000,
        south: -10_000_000,
        east: -1_790_000_000,
        north: 10_000_000,
    };
    assert!(in_bounds(t(5, 31, 15), &across));
    assert!(in_bounds(t(5, 0, 16), &across));
    assert!(!in_bounds(t(5, 16, 15), &across));
}

#[test]
fn a_tile_with_nothing_left_is_not_written() {
    let only_buildings = Tile {
        layers: vec![layer(
            "buildings",
            vec![(POLYGON, "building", polygon(&[square(10, 10, 100)]))],
        )],
    }
    .encode_to_vec();
    let layers = leviculum_map::profile::layers(1, 15).expect("profile");
    let (blob, st) = cut_tile(&only_buildings, layers).expect("cut");
    assert!(blob.is_none());
    assert_eq!((st.empty, st.features_in, st.features_out), (1, 1, 0));
}
