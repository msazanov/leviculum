//! Which region a view draws when several are installed, and how far a
//! frontend lets the reader zoom (leviculum 1062's report, items 4 and 7).
//!
//! A view draws one region or the world outline, never a patchwork: the
//! region of the highest max zoom whose box holds the view's centre and
//! whose zooms reach the view's ([`useful_zooms`]), ties by file name; the
//! outline where none does.

use core::ops::RangeInclusive;

use crate::mercator::{unproject, LatLon, World};
use crate::outline;
use crate::region::{Bounds, Header};
use crate::viewport::{MAX_OVERZOOM, MAX_VIEW_ZOOM};

impl Bounds {
    /// Whether `p` lies in the box, edges included; a box whose west is
    /// east of its east crosses the antimeridian.
    pub fn contains(&self, p: LatLon) -> bool {
        let lat = (self.south..=self.north).contains(&p.lat_e7);
        let lon = match self.west <= self.east {
            true => (self.west..=self.east).contains(&p.lon_e7),
            false => p.lon_e7 >= self.west || p.lon_e7 <= self.east,
        };
        lat && lon
    }
}

/// The view zooms a region draws at: its shallowest, down to
/// [`MAX_OVERZOOM`] past its deepest.
pub fn useful_zooms(h: &Header) -> RangeInclusive<u8> {
    h.min_zoom..=h.max_zoom.saturating_add(MAX_OVERZOOM).min(MAX_VIEW_ZOOM)
}

/// The zooms a frontend offers with these regions installed: from the
/// whole world down to the deepest any region draws at, and at least to
/// the outline's [`outline::MAX_ZOOM`].
pub fn view_zooms<'a>(headers: impl IntoIterator<Item = &'a Header>) -> RangeInclusive<u8> {
    let deepest = headers
        .into_iter()
        .map(|h| *useful_zooms(h).end())
        .fold(outline::MAX_ZOOM, u8::max);
    0..=deepest
}

/// The deepest zoom a fit centred on `centre` goes to: the deepest max
/// zoom of the regions whose box holds it, their own data and not the
/// overzoom past it, which the reader reaches by zooming in; the outline's
/// [`outline::MAX_ZOOM`] where none does. A fit to one point would
/// otherwise land at the deepest view zoom, a few metres of plain colour
/// (1074).
pub fn fit_ceiling<'a>(headers: impl IntoIterator<Item = &'a Header>, centre: World) -> u8 {
    let at = unproject(centre);
    headers
        .into_iter()
        .filter(|h| h.bounds.contains(at))
        .map(|h| h.max_zoom)
        .max()
        .unwrap_or(outline::MAX_ZOOM)
}

/// The region a view at `centre` and `zoom` draws, as an index into
/// `regions` (file name, header); `None` for the outline.
pub fn choose<'a>(
    regions: impl IntoIterator<Item = (&'a str, &'a Header)>,
    centre: World,
    zoom: u8,
) -> Option<usize> {
    let at = unproject(centre);
    let mut best: Option<(usize, u8, &str)> = None;
    for (i, (name, h)) in regions.into_iter().enumerate() {
        if !h.bounds.contains(at) || !useful_zooms(h).contains(&zoom) {
            continue;
        }
        let better = match best {
            None => true,
            Some((_, max, held)) => h.max_zoom > max || (h.max_zoom == max && name < held),
        };
        if better {
            best = Some((i, h.max_zoom, name));
        }
    }
    best.map(|(i, _, _)| i)
}
