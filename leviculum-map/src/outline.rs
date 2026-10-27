//! The world outline: what a view shows where no region covers it
//! (`docs/src/concepts/loquax-map.md`, section 7, "No default region").
//!
//! Natural Earth's 1:110m land polygons, version 5.1.2, public domain,
//! baked by `scripts/bake-world-outline.py` into a table of 21,298 bytes
//! (128 rings, 5,003 points) that is part of the binary. The format is the
//! script's: `LWO1`, a ring count, then per ring its point count (bit 15
//! set for a hole of the ring before), its box, and its points, all `u16`
//! little-endian, each point the world coordinate shifted right by 16.
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

/// The table, Natural Earth 1:110m land.
pub const WORLD_110M: &[u8] = include_bytes!("../data/ne_110m_land.lwo");

/// The attribution line a view of the outline carries.
pub const ATTRIBUTION: &str = "Natural Earth";

/// The deepest zoom the outline is worth drawing at without a region: at
/// z10 one of its 611 m steps is about four pixels, and 1:110m data is
/// coarser than that by far.
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
    pub bbox: [u16; 4],
    points: &'a [u8],
}

impl Ring<'_> {
    /// Its points, as table units.
    pub fn points(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.points.chunks_exact(4).map(|p| {
            (
                u16::from_le_bytes([p[0], p[1]]),
                u16::from_le_bytes([p[2], p[3]]),
            )
        })
    }

    /// How many points it has.
    pub fn len(&self) -> usize {
        self.points.len() / 4
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// The rings of a table; a table cut short ends early, it never panics.
pub fn rings(table: &[u8]) -> impl Iterator<Item = Ring<'_>> {
    let count = match table.get(..6) {
        Some(head) if head[..4] == *b"LWO1" => usize::from(u16::from_le_bytes([head[4], head[5]])),
        _ => 0,
    };
    let mut at = 6usize;
    (0..count).map_while(move |_| {
        let head = table.get(at..at + 10)?;
        let word = |i: usize| u16::from_le_bytes([head[i], head[i + 1]]);
        let n = usize::from(word(0) & 0x7FFF);
        let points = table.get(at + 10..at + 10 + 4 * n)?;
        at += 10 + 4 * n;
        Some(Ring {
            hole: word(0) & 0x8000 != 0,
            bbox: [word(2), word(4), word(6), word(8)],
            points,
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

/// Draw the outline of `table` (normally [`WORLD_110M`]) for `view` into
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
    // Table units to view sub-pixels: a unit is 2^16 world units, a
    // sub-pixel 2^(20 - zoom).
    let up = i32::from(view.zoom()) - 4;
    let scale = |v: u16| -> i64 {
        match up >= 0 {
            true => i64::from(v) << up,
            false => i64::from(v) >> -up,
        }
    };
    let world = 1i64 << (12 + u32::from(view.zoom()));
    let mut edges = vec![Edge::EMPTY; 8192];
    let mut active = vec![0u16; 8192];
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
