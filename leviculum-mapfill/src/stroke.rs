//! Wide lines as quads through the filler.
//!
//! Each segment of a polyline becomes a rectangle `width` across, extended
//! by half the width past both ends (a square cap), which also closes the
//! wedge at a joint up to a right angle. All quads are built with the same
//! orientation and filled together under [`FillRule::NonZero`], so where
//! they overlap the windings add and the union is filled once.

use embedded_graphics::prelude::*;

use crate::fill::{FillRule, FillStats, Filler};

/// Integer square root, `floor(sqrt(v))`.
pub fn isqrt(v: u64) -> u64 {
    if v < 2 {
        return v;
    }
    // Newton from a power of two at or above the root, below 2^33, so
    // `x + v / x` cannot overflow.
    let mut x = 1u64 << ((64 - v.leading_zeros()) / 2 + 1);
    let mut y = (x + v / x) / 2;
    while y < x {
        x = y;
        y = (x + v / x) / 2;
    }
    x
}

/// Stroke the polyline `pts` (sub-pixels) `width` sub-pixels wide.
///
/// Segments whose quad lies wholly outside the filler's clip are skipped.
/// When the scratch runs out of edge slots the quads so far are filled and
/// the walk goes on: an opaque colour painted twice is the same picture.
pub fn stroke<D: DrawTarget>(
    f: &mut Filler<'_, '_>,
    pts: &[Point],
    width: i32,
    target: &mut D,
    color: D::Color,
) -> Result<FillStats, D::Error> {
    let mut st = FillStats::default();
    let half = i64::from(width.max(1)) / 2;
    let (l, t, r, b) = f.clip_sub();
    for w in pts.windows(2) {
        let (a, z) = (w[0], w[1]);
        let pad = half as i32 + 1;
        if a.x.max(z.x) + pad < l
            || a.x.min(z.x) - pad > r
            || a.y.max(z.y) + pad < t
            || a.y.min(z.y) - pad > b
        {
            continue;
        }
        let dx = i64::from(z.x) - i64::from(a.x);
        let dy = i64::from(z.y) - i64::from(a.y);
        let len = isqrt((dx * dx + dy * dy) as u64) as i64;
        if len == 0 {
            continue;
        }
        // Half-width along the segment (e) and across it (n).
        let ex = (dx * half / len) as i32;
        let ey = (dy * half / len) as i32;
        let (nx, ny) = (-ey, ex);
        let q = [
            Point::new(a.x - ex + nx, a.y - ey + ny),
            Point::new(z.x + ex + nx, z.y + ey + ny),
            Point::new(z.x + ex - nx, z.y + ey - ny),
            Point::new(a.x - ex - nx, a.y - ey - ny),
        ];
        if f.room() < q.len() {
            st += f.fill(target, color, FillRule::NonZero)?;
        }
        // Overflows only on a scratch smaller than one quad, which cannot
        // stroke at all: drop the line rather than loop.
        if f.ring(&q).is_err() {
            f.clear();
            break;
        }
    }
    st += f.fill(target, color, FillRule::NonZero)?;
    Ok(st)
}
