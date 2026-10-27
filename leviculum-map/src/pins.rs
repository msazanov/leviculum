//! Pins and their labels as data. The core says where each visible pin
//! lands; a frontend draws the marker and sets the label in its own way,
//! cells over the picture in the terminal, text in the GUI.

use alloc::vec::Vec;

use crate::mercator::World;
use crate::viewport::Viewport;

/// One thing to mark on the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pin<'a> {
    pub at: World,
    /// The name the frontend sets beside the marker.
    pub label: &'a str,
    /// Drawn dimmed: a fix older than the stale threshold.
    pub stale: bool,
}

/// A pin that lands in the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// Its index in the slice handed to [`place`].
    pub index: usize,
    /// The pixel it lands in.
    pub x: u32,
    pub y: u32,
}

/// The pins of `pins` that land in `view`, in the order given.
pub fn place(view: &Viewport, pins: &[Pin<'_>]) -> Vec<Placed> {
    let (w, h) = view.size();
    pins.iter()
        .enumerate()
        .filter_map(|(index, p)| {
            let (x, y) = view.to_screen(p.at);
            let x = u32::try_from(x).ok().filter(|&x| x < w)?;
            let y = u32::try_from(y).ok().filter(|&y| y < h)?;
            Some(Placed { index, x, y })
        })
        .collect()
}

/// The placed pin nearest pixel `(x, y)` within `radius` pixels
/// (Chebyshev), for a click; the first one on a tie.
pub fn hit(placed: &[Placed], x: u32, y: u32, radius: u32) -> Option<usize> {
    placed
        .iter()
        .map(|p| (p.x.abs_diff(x).max(p.y.abs_diff(y)), p.index))
        .filter(|&(d, _)| d <= radius)
        .min_by_key(|&(d, _)| d)
        .map(|(_, i)| i)
}
