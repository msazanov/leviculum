//! One view drawn from its tiles through `leviculum-mapfill`.
//!
//! A tile's features are written in class order, but a view of several
//! tiles cannot be drawn tile after tile: geometry reaches past its tile's
//! edge (the source tiles carry a buffer), and the second tile's areas
//! would paint over the first tile's roads there. So the draw goes class by
//! class across all the tiles of the view, the order a single fixture of
//! the whole view had in the measurement. That needs every tile of the view
//! at hand at once, a few tens of kilobytes at z15 under profile 1.
//!
//! The line widths follow the view zoom ([`crate::profile::line_widths`]),
//! and [`Ink::Lines`] draws the lines alone in one colour, for a terminal
//! that has one colour a cell and no fill (braille).

use alloc::vec::Vec;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use leviculum_mapfill::fill::Scratch;
use leviculum_mapfill::fixture::Fixture;
use leviculum_mapfill::render::{Clock, Look, Pass, Placement, RenderStats, Work};
use leviculum_mapfill::style::{Class, Profile};

use crate::region::{check_tile, Region, RegionError, Storage};
use crate::viewport::{ViewTile, Viewport};

/// A tile of the view with its geometry.
#[derive(Clone, Copy, Debug)]
pub struct DrawTile<'a> {
    pub fixture: Fixture<'a>,
    pub tile: ViewTile,
}

/// How a view is inked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    /// Areas filled, lines in the colour table.
    Colour,
    /// Lines only, all in this colour, on the background.
    Lines(Rgb565),
}

impl Ink {
    /// The mapfill look of this ink for a view at zoom `z`.
    pub fn look(self, z: u8) -> Look {
        Look {
            widths: crate::profile::line_widths(z),
            mono: match self {
                Self::Colour => None,
                Self::Lines(c) => Some(c),
            },
        }
    }
}

/// Draw `tiles` under `profile` into `target`, which is cleared first, in
/// `ink` at the widths of the view's zoom. `scratch` and `work` as
/// `leviculum_mapfill::render::render` takes them.
pub fn draw<D, C>(
    tiles: &[DrawTile<'_>],
    profile: Profile,
    ink: Ink,
    target: &mut D,
    scratch: &mut Scratch<'_>,
    work: &mut Work<'_>,
    clock: &mut C,
) -> Result<RenderStats, D::Error>
where
    D: DrawTarget<Color = Rgb565>,
    C: Clock,
{
    // Every tile of one view is at the view's zoom less its overzoom.
    let zoom = tiles
        .first()
        .map_or(0, |t| t.tile.id.z.saturating_add(t.tile.overzoom));
    let mut pass = Pass::begin(target, scratch, clock)?.with_look(ink.look(zoom));
    let mut cursors: Vec<_> = tiles
        .iter()
        .map(|t| {
            let at = Placement {
                shift: t.fixture.shift() + t.tile.overzoom as i8,
                origin: t.tile.origin,
            };
            (at, t.fixture.features().peekable())
        })
        .collect();
    for class in Class::ALL {
        for (at, features) in &mut cursors {
            // `<=`: a blob out of class order still has every feature drawn.
            while let Some(f) = features.next_if(|f| f.class <= class) {
                pass.feature(&f, *at, profile, target, work, clock)?;
            }
        }
    }
    Ok(pass.stats)
}

impl<'a> Region<&'a [u8]> {
    /// The tiles `view` needs that this region holds, borrowed in place.
    pub fn view(&self, view: &Viewport) -> Result<Vec<DrawTile<'a>>, RegionError> {
        let mut out = Vec::new();
        for tile in view.tiles(self.zooms()) {
            if let Some(fixture) = self.fixture(tile.id)? {
                out.push(DrawTile { fixture, tile });
            }
        }
        Ok(out)
    }
}

impl<S: Storage> Region<S> {
    /// Read the tiles `view` needs that this region holds into `out`, one
    /// buffer each, for storage that is not addressable memory: a file, a
    /// card. `out` is cleared first; its buffers are reused across calls.
    /// [`draw_tiles`] then borrows them for [`draw`].
    pub fn read_view(
        &self,
        view: &Viewport,
        out: &mut Vec<(ViewTile, Vec<u8>)>,
    ) -> Result<(), RegionError> {
        let mut spare: Vec<Vec<u8>> = out.drain(..).map(|(_, b)| b).collect();
        for tile in view.tiles(self.zooms()) {
            if let Some(e) = self.find(tile.id)? {
                let mut buf = spare.pop().unwrap_or_default();
                self.read_tile(&e, &mut buf)?;
                out.push((tile, buf));
            }
        }
        Ok(())
    }
}

/// The tiles [`Region::read_view`] read, checked and ready to draw.
pub fn draw_tiles(read: &[(ViewTile, Vec<u8>)]) -> Result<Vec<DrawTile<'_>>, RegionError> {
    read.iter()
        .map(|(tile, buf)| {
            Ok(DrawTile {
                fixture: check_tile(tile.id, buf)?,
                tile: *tile,
            })
        })
        .collect()
}
