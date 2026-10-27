//! `leviculum-map-tool`: makes region files for `leviculum-map` (Codeberg
//! #474, `docs/src/concepts/loquax-map.md`, section 4).
//!
//! - [`pmtiles`]: a PMTiles v3 archive read from a file.
//! - [`mvt`]: vector tiles to classed geometry, the tree's one MVT decoder.
//! - [`simplify`]: Douglas-Peucker, quantisation, areas, all integer.
//! - [`cut`]: a bounding box and zoom range of a source under a layer
//!   profile, written as a region file.
//!
//! `std`, and in no shipping graph: prost and the MVT decoder stay here.

pub mod cut;
pub mod mvt;
pub mod pmtiles;
pub mod simplify;

use leviculum_map::region::TileId;

/// The error type of the tool: anything, sendable across the cut's threads.
pub type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Where the cut reads tiles from: a PMTiles archive, or a test's tiles.
pub trait TileSource: Sync {
    /// Every tile the source holds, in any order.
    fn tiles(&self) -> Vec<TileId>;
    /// The uncompressed MVT bytes of `t`; `None` when the source has none.
    fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>>;
}

impl TileSource for pmtiles::Archive {
    fn tiles(&self) -> Vec<TileId> {
        pmtiles::Archive::tiles(self).collect()
    }

    fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        pmtiles::Archive::mvt(self, t)
    }
}

/// Several archives read as one, for a region downloaded in parts: a tile
/// is read from the first archive that holds it. The parts of one build
/// hold the same bytes for a tile on their shared edge, so which one is
/// read does not change the cut.
impl TileSource for [pmtiles::Archive] {
    fn tiles(&self) -> Vec<TileId> {
        self.iter().flat_map(pmtiles::Archive::tiles).collect()
    }

    fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        for a in self {
            if let Some(b) = a.mvt(t)? {
                return Ok(Some(b));
            }
        }
        Ok(None)
    }
}

/// The attribution every region carries and every map view shows.
pub const ATTRIBUTION: &str = "© OpenStreetMap contributors";
