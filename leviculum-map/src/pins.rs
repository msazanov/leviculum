//! Pins and their labels as data. The core says where each visible pin
//! lands and where its label goes ([`labels`]); a frontend draws the marker
//! and sets the label in its own way, cells over the picture in the
//! terminal, text in the GUI. The label rule counts in whatever unit the
//! frontend hands it, cells or pixels ([`Metrics`]), and asks the frontend
//! how long each label is, because only the frontend knows its font.

use alloc::vec::Vec;
use core::cmp::Ordering;

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

/// The order `Tab` steps through `pins`, as indices: by label, ignoring
/// case, the way the mailbox's display rule names a peer; equal labels by
/// `tie` (the peer's hash), so the order never depends on arrival.
pub fn order<K: Ord>(pins: &[Pin<'_>], tie: impl Fn(usize) -> K) -> Vec<usize> {
    let mut out: Vec<usize> = (0..pins.len()).collect();
    out.sort_by(|&a, &b| {
        let folded = |i: usize| pins[i].label.chars().flat_map(char::to_lowercase);
        match folded(a).cmp(folded(b)) {
            Ordering::Equal => tie(a).cmp(&tie(b)),
            other => other,
        }
    });
    out
}

/// Where a pin's label goes, relative to its marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// `● name`, from the marker on.
    Right,
    /// `name ●`, ending at the marker.
    Left,
    /// The line above, from the marker's left edge, shifted left to fit.
    Above,
    /// The line below, likewise.
    Below,
}

/// The sizes [`labels`] counts with, in the frontend's unit: one each for
/// the terminal's cells ([`Metrics::CELLS`]), pixels for a GUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// How wide a marker is.
    pub marker: u32,
    /// The space between a marker and a label beside it.
    pub gap: u32,
    /// How tall a line of label is, and a marker.
    pub line: u32,
}

impl Metrics {
    /// The terminal: a marker is one cell, the gap one, a line one row.
    pub const CELLS: Self = Self {
        marker: 1,
        gap: 1,
        line: 1,
    };
}

/// A marker's top left corner in the picture, in the [`Metrics`]' unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub x: u32,
    pub y: u32,
}

/// Where a label is set: its side, and the top left corner of its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label {
    pub side: Side,
    pub x: u32,
    pub y: u32,
}

/// The box a label and its gap cover, `x0..x1` by `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Area {
    x0: u32,
    x1: u32,
    y0: u32,
    y1: u32,
}

impl Area {
    fn overlaps(&self, other: &Area) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }
}

/// What a label of `len` beside `mark` covers on `side` of a `size`
/// picture, its gap included and its marker not, with the text's corner;
/// `None` where it does not fit.
fn area(mark: Mark, len: u32, side: Side, size: (u32, u32), m: Metrics) -> Option<(Area, Mark)> {
    let (width, height) = size;
    let line = |y: u32| Area {
        x0: 0,
        x1: 0,
        y0: y,
        y1: y.saturating_add(m.line),
    };
    let apart = |y: u32| {
        (len <= width).then(|| {
            let x0 = mark.x.min(width - len);
            let a = Area {
                x0,
                x1: x0 + len,
                ..line(y)
            };
            (a, Mark { x: x0, y })
        })
    };
    match side {
        Side::Right => {
            let x0 = mark.x.saturating_add(m.marker);
            let x1 = x0.saturating_add(m.gap).saturating_add(len);
            (x1 <= width).then(|| {
                let text = Mark {
                    x: x0 + m.gap,
                    y: mark.y,
                };
                (
                    Area {
                        x0,
                        x1,
                        ..line(mark.y)
                    },
                    text,
                )
            })
        }
        Side::Left => {
            let x0 = mark.x.checked_sub(m.gap.saturating_add(len))?;
            Some((
                Area {
                    x0,
                    x1: mark.x,
                    ..line(mark.y)
                },
                Mark { x: x0, y: mark.y },
            ))
        }
        Side::Above => mark.y.checked_sub(m.line).and_then(apart),
        Side::Below => {
            let y = mark.y.saturating_add(m.line);
            (y.saturating_add(m.line) <= height)
                .then_some(y)
                .and_then(apart)
        }
    }
}

/// Where each pin's label goes so no label covers another's, or a marker:
/// right of the marker, then left, then the line above and below. A label
/// that fits nowhere is dropped (`None`) while its marker stays. The pin
/// `selected` (an index into `marks`) chooses first and always keeps its
/// label: where every side is taken it is set right, or left at the
/// picture's edge, over what lies there. The others choose in order.
///
/// `measure(i)` is how long pin `i`'s label is in the unit of `metrics`:
/// characters in the terminal, the font's advance in a GUI. A pure
/// function, so a frame and a test agree.
pub fn labels(
    marks: &[Mark],
    measure: impl Fn(usize) -> u32,
    selected: Option<usize>,
    size: (u32, u32),
    metrics: Metrics,
) -> Vec<Option<Label>> {
    let markers: Vec<Area> = marks
        .iter()
        .map(|k| Area {
            x0: k.x,
            x1: k.x.saturating_add(metrics.marker),
            y0: k.y,
            y1: k.y.saturating_add(metrics.line),
        })
        .collect();
    let mut taken: Vec<Area> = Vec::new();
    let mut out = alloc::vec![None; marks.len()];
    let order = selected
        .filter(|&i| i < marks.len())
        .into_iter()
        .chain((0..marks.len()).filter(|&i| Some(i) != selected));
    for i in order {
        let (mark, len) = (marks[i], measure(i));
        let free = [Side::Right, Side::Left, Side::Above, Side::Below]
            .into_iter()
            .find_map(|side| {
                let (covers, text) = area(mark, len, side, size, metrics)?;
                let clear = markers
                    .iter()
                    .chain(taken.iter())
                    .all(|m| !m.overlaps(&covers));
                clear.then_some((side, covers, text))
            });
        let chosen = match free {
            Some(found) => Some(found),
            None if Some(i) == selected => {
                let fits = |side| area(mark, len, side, size, metrics);
                let side = match fits(Side::Left) {
                    Some(_) if fits(Side::Right).is_none() => Side::Left,
                    _ => Side::Right,
                };
                let (covers, text) = fits(side).unwrap_or_else(|| {
                    let x0 = mark.x.saturating_add(metrics.marker);
                    let covers = Area {
                        x0,
                        x1: x0.saturating_add(metrics.gap).saturating_add(len),
                        y0: mark.y,
                        y1: mark.y.saturating_add(metrics.line),
                    };
                    let text = Mark {
                        x: x0.saturating_add(metrics.gap),
                        y: mark.y,
                    };
                    (covers, text)
                });
                Some((side, covers, text))
            }
            None => None,
        };
        if let Some((side, covers, text)) = chosen {
            taken.push(covers);
            out[i] = Some(Label {
                side,
                x: text.x,
                y: text.y,
            });
        }
    }
    out
}
