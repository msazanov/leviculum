//! Layer profiles: per zoom, which classes a region keeps and how its
//! geometry was simplified. The cut applies a profile; the region header
//! names the one it applied.
//!
//! Profile 1 is the cut of the map measurement's "Simplifications the
//! numbers ask for" (`docs/drafts/map-measurement-2026-10.md`, items 1 and
//! 2): no buildings at any zoom (points, `pois` among them, are not in the
//! flat format at all), and at the region zooms, 12 and shallower, an
//! extent of 512 instead of 4096, Douglas-Peucker at one pixel and the
//! landuse, natural and landcover areas under 16 square pixels dropped.
//! From zoom 13 the extent stays 4096 and the tolerance is half a pixel.
//!
//! The line widths are not the cut's but the draw's, so they go by the view
//! zoom, not the tile's ([`line_widths`]): the fixtures' widths are right at
//! z15 and wrong at a region zoom, where a 4-pixel major road covers most
//! of a 240-pixel view (leviculum 1062, the Frankfurt z10 and Germany z7
//! renders).

use crate::style::{Class, LineWidths, LINE_WIDTHS};

/// The profile of the first cut.
pub const CUT_1: u8 = 1;

/// The deepest zoom of the region zooms, the ones thinned and coarsened.
pub const REGION_ZOOM_MAX: u8 = 12;

/// What one zoom of a region holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoomLayers {
    /// Bit `c` set: class byte `c` is kept.
    pub classes: u32,
    /// log2 of the tile extent the geometry is quantised to.
    pub extent_log2: u8,
    /// Douglas-Peucker tolerance in sub-pixels (1/16 pixel) at this zoom.
    pub tolerance: u16,
    /// Areas of a thinned class smaller than this many square pixels are
    /// dropped; 0 keeps them all.
    pub thin_px2: u16,
}

const fn bit(c: Class) -> u32 {
    1 << (c as u8)
}

const ALL_BUT_BUILDINGS: u32 = bit(Class::Earth)
    | bit(Class::Urban)
    | bit(Class::Park)
    | bit(Class::Pedestrian)
    | bit(Class::Sand)
    | bit(Class::Water)
    | bit(Class::Waterway)
    | bit(Class::Path)
    | bit(Class::MinorRoad)
    | bit(Class::MajorRoad)
    | bit(Class::Rail);

impl ZoomLayers {
    /// Whether this zoom keeps class `c`.
    pub fn keeps(&self, c: Class) -> bool {
        self.classes & bit(c) != 0
    }

    /// Whether small areas of class `c` are dropped at this zoom: the
    /// classes the landuse, natural and landcover layers map to.
    pub fn thins(&self, c: Class) -> bool {
        self.thin_px2 > 0
            && matches!(
                c,
                Class::Urban | Class::Park | Class::Pedestrian | Class::Sand
            )
    }

    /// Tile units to sub-pixels at 256 pixels per tile: the shift a tile
    /// blob of this extent carries.
    pub fn shift(&self) -> i8 {
        12 - self.extent_log2 as i8
    }
}

/// Profile `id` at zoom `z`; `None` for a profile this build does not know.
pub fn layers(id: u8, z: u8) -> Option<ZoomLayers> {
    match id {
        CUT_1 if z <= REGION_ZOOM_MAX => Some(ZoomLayers {
            classes: ALL_BUT_BUILDINGS,
            extent_log2: 9,
            tolerance: 16,
            thin_px2: 16,
        }),
        CUT_1 => Some(ZoomLayers {
            classes: ALL_BUT_BUILDINGS,
            extent_log2: 12,
            tolerance: 8,
            thin_px2: 0,
        }),
        _ => None,
    }
}

const PX: i32 = leviculum_mapfill::SUBPIXEL;

/// The line widths a view at zoom `z` draws with, waterway to rail
/// (`leviculum_mapfill::style::LineWidths`); 0 leaves the class out.
///
/// | view zoom | waterway | path | minor road | major road | rail |
/// |---|---|---|---|---|---|
/// | up to 8 | 1 | - | - | 1 | - |
/// | 9, 10 | 1 | - | - | 1.5 | 1 |
/// | 11, 12 | 1 | - | 1 | 2 | 1 |
/// | 13, 14 | 1.5 | 1 | 2 | 3 | 1 |
/// | 15 and deeper | 1.5 | 1 | 2.5 | 4 | 1 |
///
/// In pixels. Paths and minor roads go at the region zooms, where they
/// were a white mesh over the whole Florence view at z11 and z12; nothing
/// is thinner than one pixel, which the stroker would draw broken.
pub fn line_widths(z: u8) -> LineWidths {
    match z {
        0..=8 => [PX, 0, 0, PX, 0],
        9..=10 => [PX, 0, 0, 3 * PX / 2, PX],
        11..=12 => [PX, 0, PX, 2 * PX, PX],
        13..=14 => [3 * PX / 2, PX, 2 * PX, 3 * PX, PX],
        _ => LINE_WIDTHS,
    }
}
