//! The map's picture, for every desktop frontend (`docs/src/concepts/
//! loquax-map.md`, sections 6 and 7): the region files the reader placed by
//! hand, and one view of them drawn into an Rgb565 frame with the world
//! outline under whatever no region covers. The terminal encodes the frame
//! as sixel, kitty, half-blocks or braille (`tui::map` in the binary); the
//! GUI shows it as an image (`gui::map`). Both draw through this module, so
//! the region a view chooses and the credit under it are one rule.
//!
//! Regions are read from `$LNMSG_HOME/maps/` when `LNMSG_HOME` is set and
//! from `${XDG_DATA_HOME:-~/.local/share}/loquax/maps/` otherwise: every
//! `*.lmr` there, by file name. `LNMSG_HOME` replaces the whole default base
//! for the identity and the store ([`crate::identity::home_dir`]), so it
//! replaces this one too; a test or a second profile then sees only its own
//! maps. Where no region covers the view the world outline is drawn
//! ([`leviculum_map::outline`]).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::string::{String, ToString};
use std::vec::Vec;

use embedded_graphics::prelude::{Dimensions, Point};
use embedded_graphics::primitives::Rectangle;
use leviculum_map::choice::{choose, view_zooms};
use leviculum_map::draw::{draw_in_scaled, draw_tiles, Ink};
use leviculum_map::outline;
use leviculum_map::region::{Header, Region};
use leviculum_map::viewport::{ViewTile, Viewport};
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{NoClock, Work};
use leviculum_mapfill::style::Profile;

use crate::identity::HOME_ENV;

/// Where the region files are read from.
pub fn maps_dir() -> Option<PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if let Some(home) = set(HOME_ENV) {
        return Some(PathBuf::from(home).join("maps"));
    }
    let data = set("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".local/share")))?;
    Some(data.join("loquax").join("maps"))
}

/// One region file, open.
pub struct Installed {
    pub name: String,
    pub region: Region<File>,
}

/// Every `*.lmr` in `dir`, by name, and a line for each that did not open.
/// A missing directory is no regions, not an error.
pub fn load(dir: &Path) -> (Vec<Installed>, Vec<String>) {
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "lmr"))
            .collect(),
        Err(_) => return (Vec::new(), Vec::new()),
    };
    paths.sort();
    let (mut regions, mut problems) = (Vec::new(), Vec::new());
    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match File::open(&path)
            .map_err(|e| e.to_string())
            .and_then(|f| Region::open(f).map_err(|e| format!("{e:?}")))
        {
            Ok(region) => regions.push(Installed { name, region }),
            Err(error) => problems.push(format!("map {}: {error}", path.display())),
        }
    }
    (regions, problems)
}

/// One view drawn: its pixels, Rgb565 in the frame's byte order, and the
/// credit the picture carries.
pub struct Drawn {
    pub fb: Vec<u16>,
    pub attribution: String,
}

/// The installed regions and the buffers a view is read into, kept from
/// one view to the next.
pub struct Painter {
    regions: Vec<Installed>,
    read: Vec<(ViewTile, Vec<u8>)>,
}

impl Painter {
    pub fn new(regions: Vec<Installed>) -> Self {
        Self {
            regions,
            read: Vec::new(),
        }
    }

    pub fn regions(&self) -> &[Installed] {
        &self.regions
    }

    /// The regions' headers, which say how deep a fit goes.
    pub fn headers(&self) -> Vec<Header> {
        self.regions.iter().map(|r| *r.region.header()).collect()
    }

    /// The zooms a frontend offers over these regions and the outline.
    pub fn zooms(&self) -> (u8, u8) {
        let zooms = view_zooms(self.regions.iter().map(|r| r.region.header()));
        (*zooms.start(), *zooms.end())
    }

    /// Draw `view` in `ink` with lines `percent` per cent as wide as the
    /// zoom's ([`leviculum_mapfill::render::Look::scaled`]): the region
    /// [`choose`] picks inside its box with the outline under the rest, or
    /// the outline alone where none is chosen or a tile does not read.
    pub fn draw(&mut self, view: &Viewport, ink: Ink, percent: u32) -> Drawn {
        let (w, h) = view.size();
        let mut fb = vec![0u16; (w as usize) * (h as usize)];
        let chosen = choose(
            self.regions
                .iter()
                .map(|r| (r.name.as_str(), r.region.header())),
            view.centre,
            view.zoom(),
        );
        let mut attribution = outline::ATTRIBUTION.to_string();
        let drawn = match chosen.and_then(|i| self.regions.get(i)) {
            Some(installed) => {
                let drawn = Frame::new(&mut fb, w, h).is_some_and(|mut frame| {
                    // The region ends at its box at every zoom; the
                    // outline goes under the rest of the view (1077).
                    let area = installed.region.area(view);
                    if area != frame.bounding_box() {
                        let Ok(_) = outline::draw(outline::WORLD_50M, view, ink, &mut frame);
                    }
                    region_into(
                        &installed.region,
                        view,
                        area,
                        ink,
                        percent,
                        &mut self.read,
                        &mut frame,
                    )
                });
                if drawn {
                    attribution = installed.region.attribution().to_string();
                }
                drawn
            }
            None => false,
        };
        if !drawn {
            if let Some(mut frame) = Frame::new(&mut fb, w, h) {
                let Ok(_) = outline::draw(outline::WORLD_50M, view, ink, &mut frame);
            }
        }
        Drawn { fb, attribution }
    }
}

/// Draw `region`'s tiles of `view` into `area` of `frame`; `false` when a
/// tile could not be read, and the outline is drawn instead.
fn region_into(
    region: &Region<File>,
    view: &Viewport,
    area: Rectangle,
    ink: Ink,
    percent: u32,
    read: &mut Vec<(ViewTile, Vec<u8>)>,
    frame: &mut Frame<'_>,
) -> bool {
    if region.read_view(view, read).is_err() {
        return false;
    }
    let Ok(tiles) = draw_tiles(read) else {
        return false;
    };
    let (_, h) = view.size();
    let mut edges = vec![Edge::default(); 8192];
    let mut active = vec![0u16; 8192];
    let mut heads = vec![0u16; h as usize];
    let mut points = vec![Point::zero(); 8192];
    let mut ring_ends = vec![0u16; 2048];
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut work = Work {
        points: &mut points,
        ring_ends: &mut ring_ends,
    };
    let Ok(_) = draw_in_scaled(
        &tiles,
        area,
        Profile::Full,
        ink,
        percent,
        frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_maps_directory_follows_the_home_rule() {
        // Read only, no environment written: the rule is checked on what
        // this process has.
        let dir = maps_dir();
        match std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            Some(home) => assert_eq!(dir, Some(PathBuf::from(home).join("maps"))),
            None => assert!(dir.is_none_or(|d| d.ends_with("loquax/maps"))),
        }
        let empty = tempfile::tempdir().expect("tmp");
        let (regions, problems) = load(empty.path());
        assert!(regions.is_empty() && problems.is_empty());
        std::fs::write(empty.path().join("broken.lmr"), b"not a region").expect("write");
        let (regions, problems) = load(empty.path());
        assert!(regions.is_empty());
        assert_eq!(problems.len(), 1, "{problems:?}");
    }
}
