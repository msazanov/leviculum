//! The map view as a model (`docs/src/concepts/loquax-map.md`, section 1,
//! "The map view", and slice (c)): which pins there are, in which order
//! `Tab` steps them, where the view is and where each pin lands in cells.
//!
//! The model knows no pixels of its own. The shell tells it how many image
//! pixels one cell carries for the backend it drew with, and which zooms
//! its installed regions reach ([`MapSetup`]); from those and the pane's
//! cells the model makes the [`Viewport`] the shell draws, and places the
//! pins in the cells the reader clicks. So a click and the frame cannot
//! disagree, and a test of the model needs no terminal.
//!
//! A pin is a peer with a known position, our own among them when one is
//! known, named by the display rule ([`crate::names::Names::label`]) with
//! the fix's age beside it, dimmed once it is stale
//! ([`crate::telemetry::STALE_AFTER_SECS`]).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_map::mercator::{project, LatLon, World};
use leviculum_map::pins::{self, Pin};
use leviculum_map::viewport::Viewport;
use leviculum_ui::Rect;

use crate::telemetry;

/// What the shell drew with: image pixels per cell, and the zooms the
/// installed regions and the outline reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapSetup {
    /// Pixels across and down one cell: (1, 2) for half-blocks, (2, 4) for
    /// braille, the font's cell for a graphics protocol.
    pub cell_px: (u16, u16),
    /// The shallowest and the deepest zoom the reader may go to.
    pub zooms: (u8, u8),
}

impl Default for MapSetup {
    /// Braille's cell over the outline's zooms: what a shell that said
    /// nothing gets.
    fn default() -> Self {
        Self {
            cell_px: (2, 4),
            zooms: (0, leviculum_map::outline::MAX_ZOOM),
        }
    }
}

/// The open map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapState {
    pub centre: World,
    pub zoom: u8,
    /// The selected pin's peer, which `Enter` opens.
    pub selected: Option<[u8; 16]>,
}

/// One pin, in `Tab` order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapPin {
    pub peer: [u8; 16],
    pub at: World,
    /// The display rule's name, `you` for our own position.
    pub label: String,
    /// How old the fix is, when the shell has said the time.
    pub age: Option<String>,
    pub stale: bool,
}

/// A pin that lands in the picture, in screen cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinCell {
    pub peer: [u8; 16],
    /// Where the text starts: the marker's cell, or left of it when the
    /// name is set before the marker.
    pub col: u16,
    pub row: u16,
    /// The marker, with the name and the age beside it when they are set
    /// on the marker's row, as drawn from `col` on.
    pub text: String,
    /// The name and the age on the row above or below the marker, where
    /// neither side of it had room.
    pub apart: Option<LabelCell>,
    pub stale: bool,
    pub selected: bool,
}

/// A pin's name set on a row of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelCell {
    pub col: u16,
    pub row: u16,
    pub text: String,
}

/// Where a pin's name goes, relative to its marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// `● name`, from the marker's cell on.
    Right,
    /// `name ●`, ending on the marker's cell.
    Left,
    /// The row above, from the marker's column, shifted left to fit.
    Above,
    /// The row below, likewise.
    Below,
}

/// One pin as [`labels`] sees it: its marker's cell in the picture and how
/// many cells its name takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LabelAsk {
    pub col: u16,
    pub row: u16,
    pub len: u16,
}

/// One row's run of cells, `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    row: u16,
    start: u16,
    end: u16,
}

impl Run {
    fn overlaps(&self, other: &Run) -> bool {
        self.row == other.row && self.start < other.end && other.start < self.end
    }
}

/// The cells `ask`'s name covers on `side` in a `width` x `height`
/// picture, without its marker, or `None` where it does not fit. Right and
/// left include the space between marker and name.
fn run(ask: &LabelAsk, side: Side, width: u16, height: u16) -> Option<Run> {
    let (c, r, len) = (ask.col, ask.row, ask.len);
    let apart = |row: u16| {
        (len <= width).then(|| {
            let start = c.min(width - len);
            Run {
                row,
                start,
                end: start + len,
            }
        })
    };
    match side {
        Side::Right => (u32::from(c) + 2 + u32::from(len) <= u32::from(width)).then(|| Run {
            row: r,
            start: c + 1,
            end: c + 2 + len,
        }),
        Side::Left => (c > len).then(|| Run {
            row: r,
            start: c - len - 1,
            end: c,
        }),
        Side::Above => r.checked_sub(1).and_then(apart),
        Side::Below => (r + 1 < height).then_some(r + 1).and_then(apart),
    }
}

/// Where each pin's name goes so no name covers another's, or a marker:
/// right of the marker, then left, then the row above and below. A name
/// that fits nowhere is dropped (`None`) while its marker stays. The pin
/// `selected` (an index into `asks`) chooses first and always keeps its
/// name: where every side is taken it is set right, or left at the
/// picture's edge, over what lies there. The others choose in order.
///
/// A pure function over cells, so a frame and a test agree and the rule
/// can move into `leviculum_map::pins` beside the placement it follows.
pub fn labels(
    asks: &[LabelAsk],
    selected: Option<usize>,
    width: u16,
    height: u16,
) -> Vec<Option<Side>> {
    let markers: Vec<Run> = asks
        .iter()
        .map(|a| Run {
            row: a.row,
            start: a.col,
            end: a.col + 1,
        })
        .collect();
    let mut taken: Vec<Run> = Vec::new();
    let mut out = alloc::vec![None; asks.len()];
    let order = selected
        .filter(|&i| i < asks.len())
        .into_iter()
        .chain((0..asks.len()).filter(|&i| Some(i) != selected));
    for i in order {
        let ask = &asks[i];
        let free = [Side::Right, Side::Left, Side::Above, Side::Below]
            .into_iter()
            .find_map(|side| {
                let cells = run(ask, side, width, height)?;
                let clear = markers
                    .iter()
                    .chain(taken.iter())
                    .all(|m| !m.overlaps(&cells));
                clear.then_some((side, cells))
            });
        let chosen = match free {
            Some(found) => Some(found),
            None if Some(i) == selected => {
                let side = match run(ask, Side::Left, width, height) {
                    Some(_) if run(ask, Side::Right, width, height).is_none() => Side::Left,
                    _ => Side::Right,
                };
                let cells = run(ask, side, width, height).unwrap_or(Run {
                    row: ask.row,
                    start: ask.col.saturating_add(1),
                    end: ask.col.saturating_add(2).saturating_add(ask.len),
                });
                Some((side, cells))
            }
            None => None,
        };
        if let Some((side, cells)) = chosen {
            taken.push(cells);
            out[i] = Some(side);
        }
    }
    out
}

/// The map as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapView {
    /// The cells the picture covers.
    pub area: Rect,
    /// The view the picture shows, `area` times the cell's pixels.
    pub viewport: Viewport,
    pub cell_px: (u16, u16),
    /// The pins in view, the selected one last so it is drawn on top.
    pub pins: Vec<PinCell>,
}

/// Pixels the fit leaves free at each side, so a pin is not on the edge.
const FIT_MARGIN_CELLS: u32 = 2;

/// The marker before a pin's name; `*` in ASCII.
pub const MARKER: char = '●';

/// The selected pin's marker, which `Enter` opens; `@` in ASCII. A marker
/// of its own and not only reverse video, so braille and a monochrome
/// screen show it, and a text frame does too.
pub const SELECTED: char = '◉';

/// The pins of `positions` (peer to fix) in `Tab` order, `own` named `you`.
pub fn pins_of(
    positions: &alloc::collections::BTreeMap<[u8; 16], telemetry::Fix>,
    label: impl Fn(&[u8; 16]) -> String,
    own: Option<[u8; 16]>,
    now: Option<i64>,
) -> Vec<MapPin> {
    let all: Vec<MapPin> = positions
        .iter()
        .map(|(peer, fix)| {
            let at = project(LatLon::from_e6(
                fix.location.latitude_e6,
                fix.location.longitude_e6,
            ));
            let age = now
                .filter(|_| !fix.ahead())
                .map(|now| telemetry::span(now - fix.fix_time()));
            MapPin {
                peer: *peer,
                at,
                label: match Some(*peer) == own {
                    true => "you".to_string(),
                    false => label(peer),
                },
                age,
                stale: now.is_some_and(|now| fix.stale(now)),
            }
        })
        .collect();
    let as_pins: Vec<Pin<'_>> = all
        .iter()
        .map(|p| Pin {
            at: p.at,
            label: &p.label,
            stale: p.stale,
        })
        .collect();
    let order = pins::order(&as_pins, |i| all[i].peer);
    order.into_iter().map(|i| all[i].clone()).collect()
}

/// The picture's cells in the conversation pane `pane` (its outer
/// rectangle): inside the border, less the bottom row, which carries the
/// attribution.
pub fn picture_area(pane: Rect) -> Rect {
    Rect::new(
        pane.x.saturating_add(1),
        pane.y.saturating_add(1),
        pane.width.saturating_sub(2),
        pane.height.saturating_sub(2),
    )
}

/// The view of `state` over `area` at `setup`'s cell size.
pub fn viewport(state: &MapState, area: Rect, setup: &MapSetup) -> Viewport {
    let (w, h) = pixels(area, setup);
    Viewport::new(state.centre, state.zoom, w, h)
}

fn pixels(area: Rect, setup: &MapSetup) -> (u32, u32) {
    (
        u32::from(area.width) * u32::from(setup.cell_px.0.max(1)),
        u32::from(area.height) * u32::from(setup.cell_px.1.max(1)),
    )
}

/// The view that shows every pin, at the deepest zoom of `setup` that
/// does; the whole world at the shallowest when there is none.
pub fn fit(pins: &[MapPin], area: Rect, setup: &MapSetup) -> (World, u8) {
    let (w, h) = pixels(area, setup);
    let points: Vec<World> = pins.iter().map(|p| p.at).collect();
    let margin = FIT_MARGIN_CELLS * u32::from(setup.cell_px.0.max(1));
    match Viewport::fit(&points, w, h, margin, setup.zooms.0..=setup.zooms.1) {
        Some(view) => (view.centre, view.zoom()),
        None => (
            World {
                x: 1 << 31,
                y: 1 << 31,
            },
            setup.zooms.0,
        ),
    }
}

/// The pins of `pins` that land in `view` drawn over `area`, as cells,
/// their names set by [`labels`].
pub fn place(
    pins: &[MapPin],
    view: &Viewport,
    area: Rect,
    setup: &MapSetup,
    selected: Option<[u8; 16]>,
    ascii: bool,
) -> Vec<PinCell> {
    let as_pins: Vec<Pin<'_>> = pins
        .iter()
        .map(|p| Pin {
            at: p.at,
            label: &p.label,
            stale: p.stale,
        })
        .collect();
    let landed: Vec<(&MapPin, LabelAsk, String)> = pins::place(view, &as_pins)
        .into_iter()
        .filter_map(|placed| {
            let pin = pins.get(placed.index)?;
            let col = u16::try_from(placed.x / u32::from(setup.cell_px.0.max(1))).ok()?;
            let row = u16::try_from(placed.y / u32::from(setup.cell_px.1.max(1))).ok()?;
            let words = match &pin.age {
                Some(age) => format!("{} · {age}", pin.label),
                None => pin.label.clone(),
            };
            let len = u16::try_from(words.chars().count()).unwrap_or(u16::MAX);
            Some((pin, LabelAsk { col, row, len }, words))
        })
        .collect();
    let asks: Vec<LabelAsk> = landed.iter().map(|(_, ask, _)| *ask).collect();
    let chosen = selected.and_then(|peer| landed.iter().position(|(p, _, _)| p.peer == peer));
    let sides = labels(&asks, chosen, area.width, area.height);
    let mut out: Vec<PinCell> = landed
        .into_iter()
        .zip(sides)
        .map(|((pin, ask, words), side)| {
            let selected = Some(pin.peer) == selected;
            let marker = match (ascii, selected) {
                (false, false) => MARKER,
                (false, true) => SELECTED,
                (true, false) => '*',
                (true, true) => '@',
            };
            let (col, row) = (area.x + ask.col, area.y + ask.row);
            let apart = |row: u16| {
                Some(LabelCell {
                    col: area.x + ask.col.min(area.width.saturating_sub(ask.len)),
                    row,
                    text: words.clone(),
                })
            };
            let (col, text, apart) = match side {
                Some(Side::Right) => (col, format!("{marker} {words}"), None),
                Some(Side::Left) => (
                    col.saturating_sub(ask.len + 1),
                    format!("{words} {marker}"),
                    None,
                ),
                Some(Side::Above) => (col, marker.to_string(), apart(row - 1)),
                Some(Side::Below) => (col, marker.to_string(), apart(row + 1)),
                None => (col, marker.to_string(), None),
            };
            PinCell {
                peer: pin.peer,
                col,
                row,
                text,
                apart,
                stale: pin.stale,
                selected,
            }
        })
        .collect();
    out.sort_by_key(|p| p.selected);
    out
}

/// The pin drawn at a cell, its marker or its name, the topmost where
/// texts overlap.
pub fn pin_at(cells: &[PinCell], area: Rect, col: u16, row: u16) -> Option<[u8; 16]> {
    let hit = |at: u16, on: u16, text: &str| {
        let end = at
            .saturating_add(text.chars().count() as u16)
            .min(area.x + area.width);
        on == row && (at..end).contains(&col)
    };
    cells
        .iter()
        .rev()
        .find(|p| {
            hit(p.col, p.row, &p.text)
                || p.apart.as_ref().is_some_and(|a| hit(a.col, a.row, &a.text))
        })
        .map(|p| p.peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const AT: LabelAsk = LabelAsk {
        col: 20,
        row: 5,
        len: 8,
    };

    /// Two pins on one cell: the selected keeps the right, whichever comes
    /// first; the other goes left.
    #[test]
    fn two_pins_at_one_spot_share_the_sides_the_selected_right() {
        let asks = [AT, AT];
        assert_eq!(
            labels(&asks, Some(0), 60, 12),
            vec![Some(Side::Right), Some(Side::Left)]
        );
        assert_eq!(
            labels(&asks, Some(1), 60, 12),
            vec![Some(Side::Left), Some(Side::Right)]
        );
        assert_eq!(
            labels(&asks, None, 60, 12),
            vec![Some(Side::Right), Some(Side::Left)]
        );
    }

    /// Three on one cell: right, left, then the row above; four take the
    /// row below; a fifth is dropped while the selected never is.
    #[test]
    fn three_pins_at_one_spot_go_above_and_a_name_without_room_is_dropped() {
        assert_eq!(
            labels(&[AT; 3], Some(2), 60, 12),
            vec![Some(Side::Left), Some(Side::Above), Some(Side::Right)]
        );
        assert_eq!(
            labels(&[AT; 5], Some(4), 60, 12),
            vec![
                Some(Side::Left),
                Some(Side::Above),
                Some(Side::Below),
                None,
                Some(Side::Right)
            ]
        );
    }

    /// A neighbour's marker is not covered: the name beside it moves to
    /// the other side.
    #[test]
    fn a_name_does_not_cover_a_neighbours_marker() {
        let next = LabelAsk { col: 24, ..AT };
        assert_eq!(
            labels(&[AT, next], Some(1), 60, 12),
            vec![Some(Side::Left), Some(Side::Right)]
        );
    }

    /// Hemmed in on every side, the selected still keeps its name: right,
    /// or left at the picture's right edge.
    #[test]
    fn the_selected_keeps_its_name_where_no_side_is_free() {
        let corner = LabelAsk {
            col: 55,
            row: 0,
            len: 8,
        };
        let below = LabelAsk { row: 1, ..corner };
        let left = LabelAsk { col: 50, ..corner };
        let sides = labels(&[below, left, corner], Some(2), 60, 2);
        assert_eq!(sides[2], Some(Side::Left));
    }

    /// The cells `place` draws for two pins on one spot: neither name
    /// covers the other, and the selected one is drawn last.
    #[test]
    fn place_sets_two_names_on_one_spot_apart() {
        let at = leviculum_map::mercator::project(LatLon::from_degrees(43.775, 11.261));
        let pin = |peer: u8, label: &str| MapPin {
            peer: [peer; 16],
            at,
            label: label.to_string(),
            age: None,
            stale: false,
        };
        let pins = [pin(1, "you"), pin(2, "them")];
        let area = Rect::new(1, 1, 40, 10);
        let setup = MapSetup::default();
        let view = viewport(
            &MapState {
                centre: at,
                zoom: 4,
                selected: None,
            },
            area,
            &setup,
        );
        let cells = place(&pins, &view, area, &setup, Some([2; 16]), false);
        let texts: Vec<&str> = cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["you ●", "◉ them"]);
        assert_eq!(cells[0].col + 4, cells[1].col, "one marker cell");
        let (marker, row) = (cells[1].col, cells[1].row);
        assert_eq!(pin_at(&cells, area, marker, row), Some([2; 16]), "on top");
        assert_eq!(pin_at(&cells, area, marker - 4, row), Some([1; 16]));
    }
}
