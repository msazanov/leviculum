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
    /// name is set before the marker at the picture's right edge.
    pub col: u16,
    pub row: u16,
    /// The marker, the name and the age, as drawn from `col` on.
    pub text: String,
    pub stale: bool,
    pub selected: bool,
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

/// The pins of `pins` that land in `view` drawn over `area`, as cells.
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
    let mut out: Vec<PinCell> = pins::place(view, &as_pins)
        .into_iter()
        .filter_map(|placed| {
            let pin = pins.get(placed.index)?;
            let selected = Some(pin.peer) == selected;
            let marker = match (ascii, selected) {
                (false, false) => MARKER,
                (false, true) => SELECTED,
                (true, false) => '*',
                (true, true) => '@',
            };
            let col = u16::try_from(placed.x / u32::from(setup.cell_px.0.max(1))).ok()?;
            let row = u16::try_from(placed.y / u32::from(setup.cell_px.1.max(1))).ok()?;
            let words = match &pin.age {
                Some(age) => format!("{} · {age}", pin.label),
                None => pin.label.clone(),
            };
            // The name right of the marker, or left of it where the right
            // has no room: the marker stays on the pin's cell either way.
            let len = words.chars().count() as u16 + 2;
            let (col, text) = match col + len <= area.width || col + 1 < len {
                true => (col, format!("{marker} {words}")),
                false => (col + 1 - len, format!("{words} {marker}")),
            };
            Some(PinCell {
                peer: pin.peer,
                col: area.x + col,
                row: area.y + row,
                text,
                stale: pin.stale,
                selected,
            })
        })
        .collect();
    out.sort_by_key(|p| p.selected);
    out
}

/// The pin drawn at a cell, the topmost where texts overlap.
pub fn pin_at(cells: &[PinCell], area: Rect, col: u16, row: u16) -> Option<[u8; 16]> {
    cells
        .iter()
        .rev()
        .find(|p| {
            let end = p
                .col
                .saturating_add(p.text.chars().count() as u16)
                .min(area.x + area.width);
            p.row == row && (p.col..end).contains(&col)
        })
        .map(|p| p.peer)
}
