//! The world outline: what a view shows where no region covers it
//! (`docs/src/concepts/loquax-map.md`, section 7, "No default region").
//!
//! Natural Earth's 1:50m land polygons, version 5.1.2, public domain,
//! baked by `scripts/bake-world-outline.py` into a table of 159,357 bytes
//! (1,422 rings, 59,239 points) that is part of the binary. The format is
//! the script's: `LWO2`, the bits a coordinate has, a ring count, then per
//! ring its point count with a hole flag, the length of its deltas, its
//! first point, its box as distances from that point, and the deltas to
//! each further point, packed at a width the ring chose with an escape for
//! a long step. Counts and distances are LEB128 varints. A point is the
//! world coordinate shifted right by `32 - bits`; at 18 bits a step is one
//! pixel at [`MAX_ZOOM`], so a coast there is never more than half a pixel
//! off.
//!
//! [`Ink::Colour`] fills the land in the earth colour over water;
//! [`Ink::Lines`] draws the coasts alone, one pixel wide. A ring is clipped
//! to a box a few pixels outside the view before it reaches the filler, so
//! every coordinate stays inside the filler's limit at any zoom, and the
//! edges the clip adds lie outside the view, where a stroked coast does not
//! show them.

use alloc::vec;
use alloc::vec::Vec;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use leviculum_mapfill::fill::{Edge, FillRule, FillStats, Filler, Scratch};
use leviculum_mapfill::stroke::stroke;
use leviculum_mapfill::style::{area_color, Class, BACKGROUND};
use leviculum_mapfill::SUBPIXEL;

use crate::draw::Ink;
use crate::viewport::Viewport;

/// The table, Natural Earth 1:50m land.
pub const WORLD_50M: &[u8] = include_bytes!("../data/ne_50m_land.lwo");

/// The attribution line a view of the outline carries.
pub const ATTRIBUTION: &str = "Natural Earth";

/// The deepest zoom the outline is worth drawing at without a region: at
/// z10 a step of the table is one pixel, and 1:50m data holds no detail
/// finer than that.
pub const MAX_ZOOM: u8 = 10;

/// How far outside the view, in sub-pixels, a ring is clipped: more than
/// the coast's width, so the clip's own edges never show.
const GUARD: i64 = 4 * SUBPIXEL as i64;

/// One ring of the table.
#[derive(Clone, Copy, Debug)]
pub struct Ring<'a> {
    /// A hole in the ring before it.
    pub hole: bool,
    /// West, north, east, south of its points.
    pub bbox: [u32; 4],
    len: usize,
    first: (u32, u32),
    /// Bits a coordinate has, and bits a packed delta has.
    bits: u32,
    width: u32,
    deltas: &'a [u8],
}

/// Reads bits least significant first; past the end it reads zeros.
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n {
            let byte = self.bytes.get(self.at / 8).copied().unwrap_or(0);
            v |= u32::from((byte >> (self.at % 8)) & 1) << i;
            self.at += 1;
        }
        v
    }

    /// One delta: `width` bits, or the escape and `bits + 1` more.
    fn delta(&mut self, width: u32, bits: u32) -> i64 {
        let escape = (1u32 << width) - 1;
        let z = match self.take(width) {
            v if v == escape => self.take(bits + 1),
            v => v,
        };
        i64::from(z >> 1) ^ -i64::from(z & 1)
    }
}

impl Ring<'_> {
    /// Its points, as table units.
    pub fn points(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let mut bits = Bits {
            bytes: self.deltas,
            at: 0,
        };
        let (mut x, mut y) = (i64::from(self.first.0), i64::from(self.first.1));
        let (width, coord) = (self.width, self.bits);
        (0..self.len).map(move |i| {
            if i > 0 {
                x += bits.delta(width, coord);
                y += bits.delta(width, coord);
            }
            (x.max(0) as u32, y.max(0) as u32)
        })
    }

    /// How many points it has.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// An unsigned LEB128 varint at `at`, which moves past it; `None` past the
/// end or past 32 bits.
fn varint(table: &[u8], at: &mut usize) -> Option<u32> {
    let mut v = 0u32;
    for shift in (0..35).step_by(7) {
        let byte = *table.get(*at)?;
        *at += 1;
        v |= u32::from(byte & 0x7F).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// The bits a coordinate of `table` has, `None` for no table.
pub fn bits(table: &[u8]) -> Option<u32> {
    match table.get(..5) {
        Some(head) if head[..4] == *b"LWO2" && (8..=24).contains(&head[4]) => {
            Some(u32::from(head[4]))
        }
        _ => None,
    }
}

/// The rings of a table; a table cut short ends early, it never panics.
pub fn rings(table: &[u8]) -> impl Iterator<Item = Ring<'_>> {
    let bits = bits(table).unwrap_or(16);
    let mut at = 5usize;
    let count = match self::bits(table) {
        Some(_) => varint(table, &mut at).unwrap_or(0),
        None => 0,
    };
    (0..count).map_while(move |_| {
        let mut v = || varint(table, &mut at);
        let (head, size, x, y) = (v()?, v()?, v()?, v()?);
        let (west, north, east, south) = (v()?, v()?, v()?, v()?);
        let width = u32::from(*table.get(at)?);
        if !(1..=bits + 1).contains(&width) {
            return None;
        }
        let deltas = table.get(at + 1..at + 1 + size as usize)?;
        at += 1 + size as usize;
        Some(Ring {
            hole: head & 1 != 0,
            bbox: [
                x.checked_sub(west)?,
                y.checked_sub(north)?,
                x.checked_add(east)?,
                y.checked_add(south)?,
            ],
            len: (head >> 1) as usize,
            first: (x, y),
            bits,
            width,
            deltas,
        })
    })
}

/// The outline's water colour, the region's water.
pub fn water() -> Rgb565 {
    area_color(Class::Water)
}

/// What one outline view drew.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutlineStats {
    /// Ring copies at least partly in view.
    pub rings: u32,
    /// Ring copies wholly outside it.
    pub culled: u32,
    /// Points handed to the filler after the clip.
    pub vertices: u32,
    /// Land filled ([`Ink::Colour`]).
    pub fill: FillStats,
    /// Coasts stroked ([`Ink::Lines`]).
    pub stroke: FillStats,
    /// Rings dropped because the edge storage was too small.
    pub overflows: u32,
}

/// Clip `ring` to `[x0, x1] x [y0, y1]`, Sutherland-Hodgman, edge by edge.
fn clip(ring: &[(i64, i64)], x0: i64, y0: i64, x1: i64, y1: i64, out: &mut Vec<(i64, i64)>) {
    let mut cur: Vec<(i64, i64)> = ring.to_vec();
    // Each side: inside test, and the crossing of a-b with the side.
    type Side = (fn(i64, i64, i64) -> bool, bool, i64);
    let sides: [Side; 4] = [
        (|v, _, lim| v >= lim, true, x0),
        (|v, _, lim| v <= lim, true, x1),
        (|_, v, lim| v >= lim, false, y0),
        (|_, v, lim| v <= lim, false, y1),
    ];
    for (inside, vertical, lim) in sides {
        let mut next = Vec::with_capacity(cur.len() + 4);
        let Some(&last) = cur.last() else {
            break;
        };
        let mut prev = last;
        for &p in &cur {
            let (pin, qin) = (inside(p.0, p.1, lim), inside(prev.0, prev.1, lim));
            if pin != qin {
                // The crossing, rounded toward the inside does not matter:
                // the clip box is outside the view.
                let cross = match vertical {
                    true => {
                        let t_num = lim - prev.0;
                        let den = p.0 - prev.0;
                        (lim, prev.1 + (p.1 - prev.1) * t_num / den)
                    }
                    false => {
                        let t_num = lim - prev.1;
                        let den = p.1 - prev.1;
                        (prev.0 + (p.0 - prev.0) * t_num / den, lim)
                    }
                };
                next.push(cross);
            }
            if pin {
                next.push(p);
            }
            prev = p;
        }
        cur = next;
    }
    out.clear();
    out.extend(cur);
}

/// Draw the outline of `table` (normally [`WORLD_50M`]) for `view` into
/// `target`, which is cleared first: to water under [`Ink::Colour`], to the
/// background under [`Ink::Lines`].
pub fn draw<D>(
    table: &[u8],
    view: &Viewport,
    ink: Ink,
    target: &mut D,
) -> Result<OutlineStats, D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let mut st = OutlineStats::default();
    target.clear(match ink {
        Ink::Colour => water(),
        Ink::Lines(_) => BACKGROUND,
    })?;
    let (w, h) = view.size();
    let (left, top) = view.top_left_sub();
    let sub = i64::from(SUBPIXEL);
    let (vw, vh) = (i64::from(w) * sub, i64::from(h) * sub);
    // Table units to view sub-pixels: a unit is 2^(32 - bits) world
    // units, a sub-pixel 2^(20 - zoom).
    let Some(bits) = bits(table) else {
        return Ok(st);
    };
    let up = i32::from(view.zoom()) + 12 - bits as i32;
    let scale = |v: u32| -> i64 {
        match up >= 0 {
            true => i64::from(v) << up,
            false => i64::from(v) >> -up,
        }
    };
    let world = 1i64 << (12 + u32::from(view.zoom()));
    // Room for the largest polygon, its holes in, twice over for the
    // corners the clip may add.
    let mut largest = 0usize;
    let mut polygon = 0usize;
    for r in rings(table) {
        polygon = match r.hole {
            true => polygon + r.len(),
            false => r.len(),
        };
        largest = largest.max(polygon);
    }
    let room = (2 * largest + 16).min(usize::from(u16::MAX));
    let mut edges = vec![Edge::EMPTY; room];
    let mut active = vec![0u16; room];
    let mut heads = vec![0u16; h as usize];
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut filler = Filler::new(&mut scratch, target.bounding_box());
    let mut ring = Vec::new();
    let mut clipped = Vec::new();
    let mut points: Vec<Point> = Vec::new();
    for k in left.div_euclid(world)..=(left + vw - 1).div_euclid(world) {
        let dx = k * world - left;
        for r in rings(table) {
            // A new polygon: fill the one before, whose holes are in.
            if !r.hole && ink == Ink::Colour && !filler.is_empty() {
                st.fill += filler.fill(target, BACKGROUND, FillRule::EvenOdd)?;
            }
            let [x0, y0, x1, y1] = r.bbox;
            if scale(x1) + dx < -GUARD
                || scale(x0) + dx > vw + GUARD
                || scale(y1) - top < -GUARD
                || scale(y0) - top > vh + GUARD
            {
                st.culled += 1;
                continue;
            }
            st.rings += 1;
            ring.clear();
            ring.extend(r.points().map(|(x, y)| (scale(x) + dx, scale(y) - top)));
            clip(&ring, -GUARD, -GUARD, vw + GUARD, vh + GUARD, &mut clipped);
            if clipped.len() < 3 {
                continue;
            }
            st.vertices += clipped.len() as u32;
            points.clear();
            points.extend(clipped.iter().map(|&(x, y)| Point::new(x as i32, y as i32)));
            match ink {
                Ink::Colour => {
                    if filler.ring(&points).is_err() {
                        filler.clear();
                        st.overflows += 1;
                    }
                }
                Ink::Lines(colour) => {
                    points.push(points[0]);
                    st.stroke += stroke(&mut filler, &points, SUBPIXEL, target, colour)?;
                }
            }
        }
        if ink == Ink::Colour && !filler.is_empty() {
            st.fill += filler.fill(target, BACKGROUND, FillRule::EvenOdd)?;
        }
    }
    Ok(st)
}
