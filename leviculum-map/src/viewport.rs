//! The viewport: a centre, an integer zoom and a size in pixels, and what
//! follows from them: where a world point lands, which tiles a view needs
//! and where each one is placed.
//!
//! Screen positions are sub-pixels (1/16 pixel, `leviculum-mapfill`'s
//! unit) from the view's top-left corner. A world point is quantised to the
//! zoom's sub-pixel grid before it is placed, the same way for a pin and
//! for a tile corner, so a pin and the map under it cannot drift apart by
//! rounding. The view centre sits at sub-pixel `(width * 8, height * 8)`.
//!
//! x wraps: a point is placed at its copy nearest the centre, and the tiles
//! of a view that crosses the antimeridian are taken modulo the zoom's
//! width. y does not wrap; rows outside the projection are not selected.

use alloc::vec::Vec;
use core::ops::RangeInclusive;

use embedded_graphics::prelude::Point;

use crate::mercator::World;
use crate::region::TileId;

/// The deepest view zoom: a sub-pixel is one world unit there.
pub const MAX_VIEW_ZOOM: u8 = 20;
/// How many zooms past a region's deepest the view may go and still draw
/// it, each doubling the scale. Eight keeps every placed vertex inside the
/// filler's coordinate limit (`region::check_tile` bounds the vertices).
pub const MAX_OVERZOOM: u8 = 8;

const SUB: i64 = leviculum_mapfill::SUBPIXEL as i64;

/// A view of the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    pub centre: World,
    zoom: u8,
    width: u32,
    height: u32,
}

/// One tile a view needs, and where it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewTile {
    pub id: TileId,
    /// View zoom minus tile zoom: extra left shift on top of the tile's own.
    pub overzoom: u8,
    /// The tile's top-left corner in sub-pixels from the view's.
    pub origin: Point,
}

impl Viewport {
    /// A view of `width` x `height` pixels (each at least 1) at `zoom`
    /// (clamped to [`MAX_VIEW_ZOOM`]).
    pub fn new(centre: World, zoom: u8, width: u32, height: u32) -> Self {
        Self {
            centre,
            zoom: zoom.min(MAX_VIEW_ZOOM),
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub fn zoom(&self) -> u8 {
        self.zoom
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// A different zoom about the same centre.
    pub fn set_zoom(&mut self, zoom: u8) {
        self.zoom = zoom.min(MAX_VIEW_ZOOM);
    }

    /// A different size about the same centre, for a resized pane.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
    }

    /// World units per sub-pixel, as a shift.
    fn sub_shift(&self) -> u32 {
        u32::from(MAX_VIEW_ZOOM - self.zoom)
    }

    /// The world's width in sub-pixels at this zoom.
    fn world_sub(&self) -> i64 {
        1i64 << (12 + u32::from(self.zoom))
    }

    /// The centre on this zoom's sub-pixel grid.
    fn centre_sub(&self) -> (i64, i64) {
        let s = self.sub_shift();
        (i64::from(self.centre.x >> s), i64::from(self.centre.y >> s))
    }

    /// The view's top-left corner in sub-pixels of the world at this zoom,
    /// x not wrapped: it may be negative or past the world's width, which
    /// is `1 << (12 + zoom)`. A drawer that places whole copies of the
    /// world (the outline) starts here.
    pub fn top_left_sub(&self) -> (i64, i64) {
        let (cx, cy) = self.centre_sub();
        (
            cx - i64::from(self.width) * SUB / 2,
            cy - i64::from(self.height) * SUB / 2,
        )
    }

    /// Where `w` lands, in sub-pixels from the view's top-left corner; x
    /// is the copy of `w` nearest the centre.
    pub fn to_screen_sub(&self, w: World) -> (i64, i64) {
        let s = self.sub_shift();
        let (cx, cy) = self.centre_sub();
        let world = self.world_sub();
        let dx = (i64::from(w.x >> s) - cx + world / 2).rem_euclid(world) - world / 2;
        let dy = i64::from(w.y >> s) - cy;
        (
            i64::from(self.width) * SUB / 2 + dx,
            i64::from(self.height) * SUB / 2 + dy,
        )
    }

    /// The pixel `w` lands in, which may be outside the view.
    pub fn to_screen(&self, w: World) -> (i64, i64) {
        let (x, y) = self.to_screen_sub(w);
        (x.div_euclid(SUB), y.div_euclid(SUB))
    }

    /// Whether `w` lands on a pixel of the view: left and top edges in,
    /// right and bottom edges out.
    pub fn contains(&self, w: World) -> bool {
        let (x, y) = self.to_screen(w);
        (0..i64::from(self.width)).contains(&x) && (0..i64::from(self.height)).contains(&y)
    }

    /// The world point at the top-left corner of pixel `(x, y)` of the
    /// view, for a click.
    pub fn to_world(&self, x: i32, y: i32) -> World {
        let s = self.sub_shift();
        let dx = (i64::from(x) * SUB - i64::from(self.width) * SUB / 2) << s;
        let dy = (i64::from(y) * SUB - i64::from(self.height) * SUB / 2) << s;
        World {
            x: (i64::from(self.centre.x) + dx).rem_euclid(1 << 32) as u32,
            y: (i64::from(self.centre.y) + dy).clamp(0, i64::from(u32::MAX)) as u32,
        }
    }

    /// A different zoom about pixel `(x, y)` of the view: the world point
    /// at that pixel's top-left corner stays there, as a wheel under the
    /// pointer expects. y stops at the edges as [`pan`](Self::pan) does.
    pub fn zoom_about(&mut self, zoom: u8, x: i32, y: i32) {
        let held = self.to_world(x, y);
        self.set_zoom(zoom);
        let s = self.sub_shift();
        let dx = (i64::from(x) * SUB - i64::from(self.width) * SUB / 2) << s;
        let dy = (i64::from(y) * SUB - i64::from(self.height) * SUB / 2) << s;
        self.centre = World {
            x: (i64::from(held.x) - dx).rem_euclid(1 << 32) as u32,
            y: (i64::from(held.y) - dy).clamp(0, i64::from(u32::MAX)) as u32,
        };
    }

    /// Move the centre by whole pixels; x wraps, y stops at the edges.
    pub fn pan(&mut self, dx: i32, dy: i32) {
        let px = 24 - u32::from(self.zoom);
        let (dx, dy) = (i64::from(dx) << px, i64::from(dy) << px);
        self.centre = World {
            x: (i64::from(self.centre.x) + dx).rem_euclid(1 << 32) as u32,
            y: (i64::from(self.centre.y) + dy).clamp(0, i64::from(u32::MAX)) as u32,
        };
    }

    /// The view that shows every point in `points` with `margin` pixels to
    /// spare on each side, at the deepest zoom in `zooms` that does; the
    /// shallowest when none does. `None` for no points.
    ///
    /// The box around the points is the narrowest one in x, which for
    /// points on both sides of the antimeridian is the one across it.
    pub fn fit(
        points: &[World],
        width: u32,
        height: u32,
        margin: u32,
        zooms: RangeInclusive<u8>,
    ) -> Option<Self> {
        let first = points.first()?;
        let mut xs: Vec<u32> = points.iter().map(|p| p.x).collect();
        xs.sort_unstable();
        // The widest gap between neighbours, the wrap-around one included,
        // is where the box does not go.
        let n = xs.len();
        let (mut gap, mut after) = (0u64, 0usize);
        for i in 0..n {
            let next = xs[(i + 1) % n];
            let g = if i + 1 == n {
                (1u64 << 32) - u64::from(xs[i]) + u64::from(next)
            } else {
                u64::from(next) - u64::from(xs[i])
            };
            if g > gap {
                gap = g;
                after = (i + 1) % n;
            }
        }
        let span_x = (1u64 << 32) - gap;
        let west = xs[after];
        let (mut north, mut south) = (first.y, first.y);
        for p in points {
            north = north.min(p.y);
            south = south.max(p.y);
        }
        let span_y = u64::from(south - north);
        let centre = World {
            x: west.wrapping_add((span_x / 2) as u32),
            y: north + (span_y / 2) as u32,
        };
        let room_w = u64::from(width.saturating_sub(2 * margin).max(1));
        let room_h = u64::from(height.saturating_sub(2 * margin).max(1));
        let (lo, hi) = (*zooms.start(), (*zooms.end()).min(MAX_VIEW_ZOOM));
        let mut zoom = lo;
        for z in (lo..=hi).rev() {
            // Pixels the box spans, plus one for the pixel the far edge
            // falls in, must fit in the room.
            let px = 24 - u32::from(z);
            if (span_x >> px) + 2 <= room_w && (span_y >> px) + 2 <= room_h {
                zoom = z;
                break;
            }
        }
        Some(Self::new(centre, zoom, width, height))
    }

    /// The tiles of a region holding `zooms` that this view needs, each
    /// placed. The tile zoom is the view's, clamped to the region's
    /// deepest; a view shallower than the region's shallowest, or more
    /// than [`MAX_OVERZOOM`] past its deepest, needs none. Rows outside
    /// the projection are skipped; columns wrap.
    pub fn tiles(&self, zooms: RangeInclusive<u8>) -> Vec<ViewTile> {
        let mut out = Vec::new();
        let (lo, hi) = (*zooms.start(), *zooms.end());
        if self.zoom < lo || lo > hi {
            return out;
        }
        let tz = self.zoom.min(hi);
        let overzoom = self.zoom - tz;
        if overzoom > MAX_OVERZOOM {
            return out;
        }
        let size = 1i64 << (12 + u32::from(overzoom));
        let (cx, cy) = self.centre_sub();
        let left = cx - i64::from(self.width) * SUB / 2;
        let top = cy - i64::from(self.height) * SUB / 2;
        let right = left + i64::from(self.width) * SUB;
        let bottom = top + i64::from(self.height) * SUB;
        let grid = 1i64 << tz;
        let (tx0, tx1) = (left.div_euclid(size), (right - 1).div_euclid(size));
        let ty0 = top.div_euclid(size).max(0);
        let ty1 = (bottom - 1).div_euclid(size).min(grid - 1);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                out.push(ViewTile {
                    id: TileId {
                        z: tz,
                        x: tx.rem_euclid(grid) as u32,
                        y: ty as u32,
                    },
                    overzoom,
                    origin: Point::new((tx * size - left) as i32, (ty * size - top) as i32),
                });
            }
        }
        out
    }
}
