//! Mapbox Vector Tiles to classed geometry: the one MVT decoder of the
//! tree. The cut uses it, and so does the measurement harness
//! (`leviculum-mapfill/measure`, `extract`), which is where it came from.
//!
//! Layer and kind names are the Protomaps Basemap's, both schemas the tree
//! meets: 3.x (the Florence sample, kind under `pmap:kind`, waterways in
//! `physical_line`, `natural`) and 4.x (the daily builds, kind under
//! `kind`, waterways as lines of `water`, `landcover`).

use leviculum_mapfill::fixture::{Kind, BRIDGE, TUNNEL};
use leviculum_mapfill::style::Class;
use prost::Message;
use tinymvt::geometry::GeometryDecoder;
use tinymvt::vector_tile::tile::{Feature, Layer};
use tinymvt::vector_tile::Tile;

use crate::Res;

/// The only extent the decoder takes, Protomaps' and the fixture's.
pub const EXTENT: u32 = 4096;

const POLYGON: i32 = 3;
const LINESTRING: i32 = 2;

/// One feature: its kind, class and parts. An area's part is a polygon,
/// outer ring first, then its holes; a line's part is one line string.
pub struct SourceFeature {
    pub kind: Kind,
    pub class: Class,
    /// [`BRIDGE`] or [`TUNNEL`] for a line that runs on one or in one.
    pub flags: u8,
    pub parts: Vec<Vec<Vec<[i32; 2]>>>,
}

/// A decoded tile: features in tile order (layer by layer, feature by
/// feature) and how many features had no class or the wrong geometry.
pub struct Decoded {
    pub features: Vec<SourceFeature>,
    pub skipped: usize,
}

/// The class of a landuse, natural or landcover area of `kind`, `other`
/// for a kind none of the classes names: the study's mapping
/// (`docs/drafts/map-renderer-roadmap-2026-10.md`, section 4), wood and
/// forest apart from the parks, meadows and fields with them.
fn cover(kind: &str, other: Class) -> Class {
    use Class::*;
    match kind {
        "wood" | "forest" | "scrub" => Forest,
        "garden" | "grass" | "park" | "playground" | "pitch" | "greenfield" | "meadow"
        | "cemetery" | "golf_course" | "allotments" | "farmland" | "grassland" => Park,
        "pedestrian" => Pedestrian,
        "sand" | "beach" | "barren" => Sand,
        "urban_area" => Urban,
        _ => other,
    }
}

/// The class of a feature of `layer` with `kind`, drawn as an area for a
/// polygon and as a line for a line string.
pub fn class_of(layer: &str, kind: &str, geometry: i32) -> Option<(Kind, Class)> {
    use Class::*;
    Some(match layer {
        "earth" => (Kind::Area, Earth),
        "landuse" => (Kind::Area, cover(kind, Urban)),
        "natural" | "landcover" => (Kind::Area, cover(kind, Park)),
        "water" if geometry == LINESTRING => (Kind::Line, Waterway),
        "water" => (Kind::Area, Water),
        "buildings" => (Kind::Area, Building),
        "roads" => (
            Kind::Line,
            match kind {
                "highway" => MajorRoad,
                "major_road" | "medium_road" => SecondaryRoad,
                "path" => Path,
                // 4.x carries the railways here, not in `transit`.
                "rail" => Rail,
                _ => MinorRoad,
            },
        ),
        "transit" => (Kind::Line, Rail),
        "physical_line" => (Kind::Line, Waterway),
        _ => return None,
    })
}

/// The flags of a line from its properties: [`BRIDGE`] and [`TUNNEL`] from
/// `is_bridge` and `is_tunnel` (schema 4.x) or from `pmap:level`, 1 and -1
/// (3.x).
fn line_flags(l: &Layer, f: &Feature) -> u8 {
    let mut flags = 0;
    for c in f.tags.chunks_exact(2) {
        let (Some(key), Some(v)) = (l.keys.get(c[0] as usize), l.values.get(c[1] as usize)) else {
            continue;
        };
        let yes = v.bool_value == Some(true);
        let level = v.sint_value.or(v.int_value).unwrap_or(0);
        flags |= match key.as_str() {
            "is_bridge" if yes => BRIDGE,
            "is_tunnel" if yes => TUNNEL,
            "pmap:level" if level > 0 => BRIDGE,
            "pmap:level" if level < 0 => TUNNEL,
            _ => 0,
        };
    }
    flags
}

/// Decode one uncompressed MVT tile.
pub fn decode(raw: &[u8]) -> Res<Decoded> {
    let tile = Tile::decode(raw)?;
    let mut out = Decoded {
        features: Vec::new(),
        skipped: 0,
    };
    for l in &tile.layers {
        if l.extent.unwrap_or(EXTENT) != EXTENT {
            return Err(format!("layer {} extent {:?}", l.name, l.extent).into());
        }
        let kidx = l
            .keys
            .iter()
            .position(|k| k == "pmap:kind")
            .or_else(|| l.keys.iter().position(|k| k == "kind"));
        for f in &l.features {
            let mut kind = "";
            if let Some(ki) = kidx {
                for c in f.tags.chunks(2) {
                    if c[0] as usize == ki {
                        kind = c
                            .get(1)
                            .and_then(|v| l.values.get(*v as usize))
                            .and_then(|v| v.string_value.as_deref())
                            .unwrap_or("");
                    }
                }
            }
            let geometry = f.r#type.unwrap_or(0);
            let Some((k, class)) = class_of(&l.name, kind, geometry) else {
                out.skipped += 1;
                continue;
            };
            let mut d = GeometryDecoder::new(&f.geometry);
            let parts: Vec<Vec<Vec<[i32; 2]>>> = match (geometry, k) {
                (POLYGON, Kind::Area) => d.decode_polygons()?,
                (LINESTRING, Kind::Line) => d
                    .decode_linestrings()?
                    .into_iter()
                    .map(|l| vec![l])
                    .collect(),
                _ => {
                    out.skipped += 1;
                    continue;
                }
            };
            let flags = match k {
                Kind::Line => line_flags(l, f),
                Kind::Area => 0,
            };
            out.features.push(SourceFeature {
                kind: k,
                class,
                flags,
                parts,
            });
        }
    }
    Ok(out)
}
