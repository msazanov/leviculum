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

use leviculum_map::choice::fit_ceiling;
use leviculum_map::mercator::{project, unproject, LatLon, World};
use leviculum_map::pins::{self, Mark, Metrics, Pin, Side};
use leviculum_map::region::{Bounds, Header};
use leviculum_map::viewport::Viewport;
use leviculum_ui::Rect;

use crate::telemetry;

/// What the shell drew with: image pixels per cell, and the zooms the
/// installed regions and the outline reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapSetup {
    /// Pixels across and down one cell: (1, 2) for half-blocks, (2, 4) for
    /// braille, the font's cell for a graphics protocol.
    pub cell_px: (u16, u16),
    /// The shallowest and the deepest zoom the reader may go to.
    pub zooms: (u8, u8),
    /// The installed regions' headers, which say how deep a fit goes
    /// ([`fit`]).
    pub regions: Vec<Header>,
    /// The ways this shell can draw the map, in the order a key cycles
    /// through them (1093); none where there is no choice.
    pub drawings: Vec<Drawing>,
    /// Which of [`MapSetup::drawings`] draws the map now.
    pub drawing: usize,
}

/// One way the shell can draw the map, as a key and the palette offer it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drawing {
    /// Its name, as `--graphics` and the map's title spell it.
    pub name: String,
    /// Whether the shell expects it to work here. One it does not is still
    /// offered, marked: the reader's eyes beat the detection.
    pub expected: bool,
}

impl Default for MapSetup {
    /// Braille's cell over the outline's zooms: what a shell that said
    /// nothing gets.
    fn default() -> Self {
        Self {
            cell_px: (2, 4),
            zooms: (0, leviculum_map::outline::MAX_ZOOM),
            regions: Vec::new(),
            drawings: Vec::new(),
            drawing: 0,
        }
    }
}

/// Why a region is loaded (1089): the reader went to a place, or asked for
/// the visible area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadReason {
    Place,
    View,
}

impl LoadReason {
    /// `tui.log`'s `reason=`.
    pub fn word(self) -> &'static str {
        match self {
            Self::Place => "place",
            Self::View => "view",
        }
    }
}

/// A region the model asks the shell to load: a box, the zooms, and how
/// far the deepest zoom may be lowered to stay under the shell's size cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapWant {
    pub bounds: Bounds,
    /// The shallowest and the deepest zoom asked for.
    pub zooms: (u8, u8),
    /// The shallowest the deepest zoom may be lowered to.
    pub floor: u8,
    pub reason: LoadReason,
    /// The place, or the view's centre: a place's load fits the map there
    /// again when it lands and the map has not moved.
    pub centre: World,
}

/// A place's region reaches this far to each side: a box of 12 by 12 km,
/// a town with its surroundings. Scaled from Frankfurt's 576 km² box of
/// 9.9 MB (loquax-map.md, section 4), a city's is 2 to 3 MB.
pub const PLACE_HALF_M: u32 = 6_000;
/// A place counts as loaded when a region holds this much around it at
/// the deepest zoom: no load for a fix a street away from the last.
pub const PLACE_MARGIN_M: u32 = 3_000;
/// A place's zooms. Below z10 its box is a few pixels, and the world
/// outline draws there anyway.
pub const PLACE_ZOOMS: (u8, u8) = (10, 15);
/// The deepest zoom a place's load lowers to under the size cap.
pub const PLACE_FLOOR: u8 = 13;
/// The deepest zoom any load asks for: the Protomaps builds end there.
pub const DEEPEST_LOAD: u8 = 15;
/// A load of the visible area starts this many zooms above the view's,
/// so zooming out a little still finds the region.
pub const VIEW_BELOW: u8 = 3;
/// The most places one opening of the map asks for, its first pins in
/// `Tab` order: a list of peers across a continent is not a download of it.
pub const MAX_PIN_LOADS: usize = 4;

/// Whether a region of `regions` holds `b` at `zooms`.
pub fn covered(regions: &[Header], b: &Bounds, zooms: (u8, u8)) -> bool {
    regions
        .iter()
        .any(|h| h.bounds.holds(b) && h.min_zoom <= zooms.0 && h.max_zoom >= zooms.1)
}

/// The region to load around `at`, unless `regions` hold it already
/// ([`PLACE_MARGIN_M`] around it at the deepest zoom).
pub fn want_place(at: World, regions: &[Header]) -> Option<MapWant> {
    let place = unproject(at);
    let inner = Bounds::around(place, PLACE_MARGIN_M);
    if covered(regions, &inner, (PLACE_ZOOMS.1, PLACE_ZOOMS.1)) {
        return None;
    }
    Some(MapWant {
        bounds: Bounds::around(place, PLACE_HALF_M),
        zooms: PLACE_ZOOMS,
        floor: PLACE_FLOOR,
        reason: LoadReason::Place,
        centre: at,
    })
}

/// The places to load for `pins`, at most [`MAX_PIN_LOADS`], none held by
/// `regions` or by a place asked for before it.
pub fn want_pins(pins: &[MapPin], regions: &[Header]) -> Vec<MapWant> {
    let mut wants: Vec<MapWant> = Vec::new();
    for pin in pins {
        if wants.len() >= MAX_PIN_LOADS {
            break;
        }
        let inner = Bounds::around(unproject(pin.at), PLACE_MARGIN_M);
        if wants.iter().any(|w| w.bounds.holds(&inner)) {
            continue;
        }
        wants.extend(want_place(pin.at, regions));
    }
    wants
}

/// The region to load for `view`: its box, from [`VIEW_BELOW`] zooms above
/// its zoom to [`DEEPEST_LOAD`], the deepest lowered no further than the
/// view's own. `None` when `regions` hold the box at the view's zoom.
pub fn want_view(view: &Viewport, regions: &[Header]) -> Option<MapWant> {
    let zoom = view.zoom().min(DEEPEST_LOAD);
    let bounds = view.bounds();
    if covered(regions, &bounds, (zoom, zoom)) {
        return None;
    }
    Some(MapWant {
        bounds,
        zooms: (zoom.saturating_sub(VIEW_BELOW), DEEPEST_LOAD),
        floor: zoom,
        reason: LoadReason::View,
        centre: view.centre,
    })
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

impl MapPin {
    /// What is set beside the marker: the name, and the fix's age when
    /// the shell has said the time.
    pub fn words(&self) -> String {
        match &self.age {
            Some(age) => format!("{} · {age}", self.label),
            None => self.label.clone(),
        }
    }
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
/// does, but no deeper than the region under its centre has data for
/// ([`fit_ceiling`]): one pin fits at that region's max zoom, or at the
/// outline's, and `+` goes on into the overzoom. The whole world at the
/// shallowest when there is no pin.
pub fn fit(pins: &[MapPin], area: Rect, setup: &MapSetup) -> (World, u8) {
    let points: Vec<World> = pins.iter().map(|p| p.at).collect();
    fit_points(&points, area, setup)
}

/// The view centred on one point, at the zoom [`fit`] gives a single pin
/// there: the deepest the region under it has data for, or the outline's
/// without one. Where a report's fix opens the map (1076).
pub fn fit_at(at: World, area: Rect, setup: &MapSetup) -> (World, u8) {
    fit_points(&[at], area, setup)
}

fn fit_points(points: &[World], area: Rect, setup: &MapSetup) -> (World, u8) {
    let (w, h) = pixels(area, setup);
    let margin = FIT_MARGIN_CELLS * u32::from(setup.cell_px.0.max(1));
    match Viewport::fit(points, w, h, margin, setup.zooms.0..=setup.zooms.1) {
        Some(view) => {
            let ceiling = fit_ceiling(&setup.regions, view.centre).max(setup.zooms.0);
            (view.centre, view.zoom().min(ceiling))
        }
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
/// their names set by [`pins::labels`] in cells.
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
    let landed: Vec<(&MapPin, (u16, u16), u16, String)> = pins::place(view, &as_pins)
        .into_iter()
        .filter_map(|placed| {
            let pin = pins.get(placed.index)?;
            let col = u16::try_from(placed.x / u32::from(setup.cell_px.0.max(1))).ok()?;
            let row = u16::try_from(placed.y / u32::from(setup.cell_px.1.max(1))).ok()?;
            let words = pin.words();
            let len = u16::try_from(words.chars().count()).unwrap_or(u16::MAX);
            Some((pin, (col, row), len, words))
        })
        .collect();
    let marks: Vec<Mark> = landed
        .iter()
        .map(|(_, (col, row), _, _)| Mark {
            x: u32::from(*col),
            y: u32::from(*row),
        })
        .collect();
    let chosen = selected.and_then(|peer| landed.iter().position(|(p, ..)| p.peer == peer));
    let sides = pins::labels(
        &marks,
        |i| landed.get(i).map_or(0, |(_, _, len, _)| u32::from(*len)),
        chosen,
        (u32::from(area.width), u32::from(area.height)),
        Metrics::CELLS,
    );
    let mut out: Vec<PinCell> = landed
        .into_iter()
        .zip(sides)
        .map(|((pin, (ask_col, ask_row), len, words), label)| {
            let side = label.map(|l| l.side);
            let selected = Some(pin.peer) == selected;
            let marker = match (ascii, selected) {
                (false, false) => MARKER,
                (false, true) => SELECTED,
                (true, false) => '*',
                (true, true) => '@',
            };
            let (col, row) = (area.x + ask_col, area.y + ask_row);
            let apart = |row: u16| {
                Some(LabelCell {
                    col: area.x + ask_col.min(area.width.saturating_sub(len)),
                    row,
                    text: words.clone(),
                })
            };
            let (col, text, apart) = match side {
                Some(Side::Right) => (col, format!("{marker} {words}"), None),
                Some(Side::Left) => (
                    col.saturating_sub(len + 1),
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

/// How far, in cells either way, the pointer goes from where the left
/// button went down on the map before the press is a drag and no longer a
/// click. One: the terminal reports the pointer by the cell, so a report
/// of the next cell is a hand that moved, and a hand that held still sends
/// none.
pub const DRAG_CELLS: u16 = 1;

/// The left button held down on the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapPress {
    /// The cell it went down on.
    pub from: (u16, u16),
    /// The view's centre then.
    pub centre: World,
    /// The pin it went down on, which a release without a drag opens.
    pub pin: Option<[u8; 16]>,
    /// The pointer went [`DRAG_CELLS`] or more from `from`: the release
    /// opens nothing, and it stays a drag when the pointer comes back.
    pub dragging: bool,
}

impl MapPress {
    pub fn new(from: (u16, u16), centre: World, pin: Option<[u8; 16]>) -> Self {
        Self {
            from,
            centre,
            pin,
            dragging: false,
        }
    }

    /// The pointer is at `(col, row)`: a drag from there on once it is far
    /// enough from where the button went down.
    pub fn reach(&mut self, col: u16, row: u16) {
        let far = |a: u16, b: u16| a.abs_diff(b) >= DRAG_CELLS;
        self.dragging |= far(col, self.from.0) || far(row, self.from.1);
    }

    /// The centre with the pointer at `(col, row)`: the view at the press
    /// moved the other way by the pointer's distance in the backend's
    /// pixels, so the point under the pointer when the button went down is
    /// under it again. The press's own centre while it is not a drag.
    pub fn centre_at(&self, col: u16, row: u16, state: &MapState, setup: &MapSetup) -> World {
        if !self.dragging {
            return self.centre;
        }
        let cells = |to: u16, from: u16| i32::from(to) - i32::from(from);
        let mut view = Viewport::new(self.centre, state.zoom, 1, 1);
        view.pan(
            -cells(col, self.from.0) * i32::from(setup.cell_px.0.max(1)),
            -cells(row, self.from.1) * i32::from(setup.cell_px.1.max(1)),
        );
        view.centre
    }
}

/// `state` at `zoom`, about the cell `(col, row)` when it is on the
/// picture `area`: the point at the centre of that cell stays under it,
/// as the wheel under the pointer expects. Off the picture, about the
/// centre.
pub fn zoom_at(state: &mut MapState, area: Rect, setup: &MapSetup, col: u16, row: u16, zoom: u8) {
    let on = (area.x..area.x + area.width).contains(&col)
        && (area.y..area.y + area.height).contains(&row);
    if !on {
        state.zoom = zoom;
        return;
    }
    let (cw, ch) = (
        i32::from(setup.cell_px.0.max(1)),
        i32::from(setup.cell_px.1.max(1)),
    );
    let x = i32::from(col - area.x) * cw + cw / 2;
    let y = i32::from(row - area.y) * ch + ch / 2;
    let mut view = viewport(state, area, setup);
    view.zoom_about(zoom, x, y);
    state.centre = view.centre;
    state.zoom = view.zoom();
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

    /// `0` with one pin fits at the deepest zoom the region under it has
    /// data for, the Florence fixture's z15, not at the overzoom the
    /// setup offers past it (1074, Lew: one pin in Frankfurt fitted at z20,
    /// plain land); `+` still goes on. Without a region under the pin, at
    /// the outline's z10.
    #[test]
    fn one_pin_fits_at_the_regions_max_zoom_not_past_it() {
        use leviculum_map::choice::view_zooms;
        use leviculum_map::region::Region;
        const FLORENCE: &[u8] = include_bytes!("../../../leviculum-map/fixtures/firenze.lmr");
        let header = *Region::from_bytes(FLORENCE).expect("open").header();
        let zooms = view_zooms([&header]);
        let setup = MapSetup {
            cell_px: (9, 19),
            zooms: (*zooms.start(), *zooms.end()),
            regions: vec![header],
            ..MapSetup::default()
        };
        assert!(setup.zooms.1 > header.max_zoom, "overzoom is offered");
        let area = Rect::new(1, 1, 60, 20);
        let pin = |lat: f64, lon: f64| MapPin {
            peer: [1; 16],
            at: project(LatLon::from_degrees(lat, lon)),
            label: "bob".to_string(),
            age: None,
            stale: false,
        };
        let (_, zoom) = fit(&[pin(43.7731, 11.2560)], area, &setup);
        assert_eq!(zoom, header.max_zoom);
        // Frankfurt, outside the fixture: the outline's deepest.
        let (_, zoom) = fit(&[pin(50.1106, 8.6821)], area, &setup);
        assert_eq!(zoom, leviculum_map::outline::MAX_ZOOM);
        let none = MapSetup {
            regions: Vec::new(),
            ..setup.clone()
        };
        let (_, zoom) = fit(&[pin(43.7731, 11.2560)], area, &none);
        assert_eq!(zoom, leviculum_map::outline::MAX_ZOOM);
    }
}
