//! The flat geometry fixture: decoded tile geometry the device reads from
//! flash without a protobuf decoder.
//!
//! Written once on the host by the harness (`measure/`, `extract`) from the
//! MVT tiles; read here with no allocation and no copying. All integers are
//! little-endian.
//!
//! ```text
//! header   "LMF1"  u16 view_w  u16 view_h  i8 shift  u8 tiles  u16 0
//! tiles    tiles x (i32 off_x, i32 off_y)
//! count    u32 features
//! feature  u8 kind (1 area, 2 line)  u8 class  u8 tile  u8 flags  u16 rings
//!          rings x (u16 n, n x (i16 x, i16 y))
//! ```
//!
//! The flags byte was a zero until 1099, and a reader of that age never
//! looks at it, so every bit is one such a reader may ignore: [`BRIDGE`]
//! and [`TUNNEL`] on a line (drawn as on the ground), and [`VARIANT`], the
//! class's newer half (forest on a park, a secondary road on a major road,
//! drawn as the park and the major road). Bits a reader does not know are
//! ignored as well, so a later one can be added the same way.
//!
//! A tile's offset is its origin minus the view's origin, in tile units.
//! A vertex `v` of a feature in tile `t` lands at sub-pixel
//! `(t.off + v) << shift` (or `>> -shift`), the whole of the projection
//! once the view's origin is known: for extent 4096 drawn at 256 pixels
//! the shift is 0, one tile unit is one sub-pixel.

use embedded_graphics::prelude::Point;

use crate::style::Class;

const MAGIC: &[u8; 4] = b"LMF1";
const HEADER: usize = 12;
const TILE: usize = 8;
const FEATURE: usize = 6;

/// Flag: the line runs on a bridge.
pub const BRIDGE: u8 = 1;
/// Flag: the line runs in a tunnel.
pub const TUNNEL: u8 = 2;
/// Flag: the class byte's newer class ([`Class::wire`]).
pub const VARIANT: u8 = 4;

/// Why bytes are not a fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureError {
    /// No `LMF1` at the start.
    Magic,
    /// The bytes end inside a record, at this offset.
    Truncated(usize),
    /// A feature names a tile the header does not hold, at this offset.
    Tile(usize),
    /// Unknown kind or class byte, or a class of the wrong kind, at this offset.
    Class(usize),
    /// Bytes left after the last feature.
    Trailing(usize),
}

/// Area or line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Area,
    Line,
}

/// A validated fixture.
#[derive(Clone, Copy, Debug)]
pub struct Fixture<'a> {
    width: u16,
    height: u16,
    shift: i8,
    tiles: &'a [u8],
    count: u32,
    body: &'a [u8],
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn i32_at(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

impl<'a> Fixture<'a> {
    /// Check `bytes` end to end, so that walking the result cannot fail.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FixtureError> {
        if bytes.get(..4) != Some(MAGIC.as_slice()) {
            return Err(FixtureError::Magic);
        }
        let t = FixtureError::Truncated;
        let width = u16_at(bytes, 4).ok_or(t(4))?;
        let height = u16_at(bytes, 6).ok_or(t(6))?;
        let shift = *bytes.get(8).ok_or(t(8))? as i8;
        let ntiles = *bytes.get(9).ok_or(t(9))? as usize;
        let tiles_end = HEADER + ntiles * TILE;
        let tiles = bytes.get(HEADER..tiles_end).ok_or(t(HEADER))?;
        let count = u32::from_le_bytes(
            bytes
                .get(tiles_end..tiles_end + 4)
                .ok_or(t(tiles_end))?
                .try_into()
                .map_err(|_| t(tiles_end))?,
        );
        let body = &bytes[tiles_end + 4..];
        let mut at = 0usize;
        for _ in 0..count {
            let base = tiles_end + 4 + at;
            let h = body.get(at..at + FEATURE).ok_or(t(base))?;
            let class = Class::from_wire(h[1], h[3]).ok_or(FixtureError::Class(base))?;
            let kind_ok = match h[0] {
                1 => class.is_area(),
                2 => !class.is_area(),
                _ => false,
            };
            if !kind_ok {
                return Err(FixtureError::Class(base));
            }
            if h[2] as usize >= ntiles {
                return Err(FixtureError::Tile(base));
            }
            let rings = u16::from_le_bytes([h[4], h[5]]);
            at += FEATURE;
            for _ in 0..rings {
                let n = u16_at(body, at).ok_or(t(tiles_end + 4 + at))? as usize;
                at += 2 + n * 4;
                if at > body.len() {
                    return Err(t(tiles_end + 4 + at));
                }
            }
        }
        if at != body.len() {
            return Err(FixtureError::Trailing(tiles_end + 4 + at));
        }
        Ok(Self {
            width,
            height,
            shift,
            tiles,
            count,
            body,
        })
    }

    /// The view the fixture was cut for, in pixels.
    pub fn view(&self) -> (u32, u32) {
        (u32::from(self.width), u32::from(self.height))
    }

    /// Tile units to sub-pixels: a left shift when positive.
    pub fn shift(&self) -> i8 {
        self.shift
    }

    /// Number of tiles the header holds.
    pub fn tile_count(&self) -> usize {
        self.tiles.len() / TILE
    }

    /// Number of features.
    pub fn len(&self) -> usize {
        self.count as usize
    }

    /// Whether the fixture holds no feature.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The features in draw order.
    pub fn features(&self) -> Features<'a> {
        Features {
            fx: *self,
            at: 0,
            left: self.count,
        }
    }

    fn tile_offset(&self, i: u8) -> Point {
        let at = usize::from(i) * TILE;
        Point::new(
            i32_at(self.tiles, at).unwrap_or(0),
            i32_at(self.tiles, at + 4).unwrap_or(0),
        )
    }
}

/// Iterator over a fixture's features.
#[derive(Clone, Debug)]
pub struct Features<'a> {
    fx: Fixture<'a>,
    at: usize,
    left: u32,
}

/// One feature: its kind, class, flags, tile offset and rings.
#[derive(Clone, Copy, Debug)]
pub struct Feature<'a> {
    pub kind: Kind,
    pub class: Class,
    /// [`BRIDGE`], [`TUNNEL`] and [`VARIANT`] as written.
    pub flags: u8,
    /// The feature's tile origin minus the view origin, in tile units.
    pub offset: Point,
    rings: u16,
    body: &'a [u8],
}

impl<'a> Iterator for Features<'a> {
    type Item = Feature<'a>;

    fn next(&mut self) -> Option<Feature<'a>> {
        if self.left == 0 {
            return None;
        }
        let b = self.fx.body;
        let h = b.get(self.at..self.at + FEATURE)?;
        let rings = u16::from_le_bytes([h[4], h[5]]);
        let start = self.at + FEATURE;
        let mut end = start;
        for _ in 0..rings {
            end += 2 + usize::from(u16_at(b, end)?) * 4;
        }
        self.at = end;
        self.left -= 1;
        Some(Feature {
            kind: if h[0] == 1 { Kind::Area } else { Kind::Line },
            class: Class::from_wire(h[1], h[3])?,
            flags: h[3],
            offset: self.fx.tile_offset(h[2]),
            rings,
            body: b.get(start..end)?,
        })
    }
}

impl<'a> Feature<'a> {
    /// Whether the line runs on a bridge.
    pub fn bridge(&self) -> bool {
        self.flags & BRIDGE != 0
    }

    /// Whether the line runs in a tunnel.
    pub fn tunnel(&self) -> bool {
        self.flags & TUNNEL != 0
    }

    /// Number of rings (areas) or line strings (lines).
    pub fn ring_count(&self) -> usize {
        usize::from(self.rings)
    }

    /// The rings, each as its raw vertices in tile units.
    pub fn rings(&self) -> Rings<'a> {
        Rings {
            body: self.body,
            at: 0,
        }
    }
}

/// Iterator over a feature's rings.
pub struct Rings<'a> {
    body: &'a [u8],
    at: usize,
}

/// One ring's vertices.
#[derive(Clone, Copy, Debug)]
pub struct Ring<'a> {
    bytes: &'a [u8],
}

impl<'a> Iterator for Rings<'a> {
    type Item = Ring<'a>;

    fn next(&mut self) -> Option<Ring<'a>> {
        let n = usize::from(u16_at(self.body, self.at)?);
        let start = self.at + 2;
        self.at = start + n * 4;
        Some(Ring {
            bytes: self.body.get(start..self.at)?,
        })
    }
}

impl<'a> Ring<'a> {
    /// Number of vertices.
    pub fn len(&self) -> usize {
        self.bytes.len() / 4
    }

    /// Whether the ring has no vertex.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The vertices in tile units.
    pub fn points(&self) -> impl Iterator<Item = (i16, i16)> + 'a {
        self.bytes.chunks_exact(4).map(|c| {
            (
                i16::from_le_bytes([c[0], c[1]]),
                i16::from_le_bytes([c[2], c[3]]),
            )
        })
    }
}

/// Builds a fixture; the host harness's writer, kept beside the reader so
/// the two cannot drift.
#[cfg(feature = "std")]
pub mod write {
    extern crate std;
    use std::vec::Vec;

    use super::{Kind, FEATURE, MAGIC};
    use crate::style::Class;

    /// One feature to write: kind, class, [`BRIDGE`](super::BRIDGE) and
    /// [`TUNNEL`](super::TUNNEL), tile index and rings. The class's
    /// [`VARIANT`](super::VARIANT) bit is the writer's to add.
    pub struct Feature {
        pub kind: Kind,
        pub class: Class,
        pub flags: u8,
        pub tile: u8,
        pub rings: Vec<Vec<(i16, i16)>>,
    }

    /// The fixture bytes for a view of `w` x `h` pixels, `shift`, the tile
    /// offsets and the features in draw order.
    pub fn fixture(
        w: u16,
        h: u16,
        shift: i8,
        tiles: &[(i32, i32)],
        features: &[Feature],
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.push(shift as u8);
        b.push(tiles.len() as u8);
        b.extend_from_slice(&[0, 0]);
        for &(x, y) in tiles {
            b.extend_from_slice(&x.to_le_bytes());
            b.extend_from_slice(&y.to_le_bytes());
        }
        b.extend_from_slice(&(features.len() as u32).to_le_bytes());
        for f in features {
            let start = b.len();
            b.push(match f.kind {
                Kind::Area => 1,
                Kind::Line => 2,
            });
            let (class, variant) = f.class.wire();
            b.push(class);
            b.push(f.tile);
            b.push(f.flags | variant);
            b.extend_from_slice(&(f.rings.len() as u16).to_le_bytes());
            debug_assert_eq!(b.len() - start, FEATURE);
            for r in &f.rings {
                b.extend_from_slice(&(r.len() as u16).to_le_bytes());
                for &(x, y) in r {
                    b.extend_from_slice(&x.to_le_bytes());
                    b.extend_from_slice(&y.to_le_bytes());
                }
            }
        }
        b
    }
}
