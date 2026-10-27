//! The map's picture in the terminal (`docs/src/concepts/loquax-map.md`,
//! sections 6 and 7, slice (c)): the backend ladder, the region files the
//! reader placed by hand, and the cells the picture and the pins become.
//!
//! The model ([`loquax::ui::map`]) says which view to draw and where the
//! pins go; this module draws the view's picture under them and the
//! attribution line below it. The ladder, decided 2026-10-27: kitty or
//! sixel where the terminal answers for one, else half-blocks in colour,
//! else braille, which is also the backend under `--no-color`. The
//! `--graphics` flag forces a rung, and `tui.log` names the one taken
//! (`GRAPHICS backend=`), beside the `KEYS` line.
//!
//! Regions are read from `$LNMSG_HOME/maps/` when `LNMSG_HOME` is set and
//! from `${XDG_DATA_HOME:-~/.local/share}/loquax/maps/` otherwise: every
//! `*.lmr` there, by file name. `LNMSG_HOME` replaces the whole default
//! base for the identity and the store ([`loquax::identity::home_dir`]),
//! so it replaces this one too; a test or a second profile then sees only
//! its own maps. Where no region covers the view the world outline is
//! drawn ([`leviculum_map::outline`]).

use std::fs::File;
use std::path::{Path, PathBuf};

use embedded_graphics::pixelcolor::Rgb565;
use leviculum_map::choice::{choose, view_zooms};
use leviculum_map::draw::{draw, draw_tiles, Ink};
use leviculum_map::outline;
use leviculum_map::region::Region;
use leviculum_map::viewport::{ViewTile, Viewport};
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::frame::{rgb888, Frame};
use leviculum_mapfill::render::{NoClock, Work};
use leviculum_mapfill::style::{Profile, BACKGROUND};
use leviculum_ui::color::Tier;
use leviculum_ui_ratatui::images::{
    self, Backend, FontSize, Picker, Pictures, ProtocolType, SlicedProtocol,
};
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame as TermFrame;

use loquax::identity::HOME_ENV;
use loquax::ui::map::{MapSetup, MapView};

/// `--graphics`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum GraphicsFlag {
    /// Ask the terminal and climb the ladder.
    Auto,
    /// The kitty graphics protocol.
    Kitty,
    /// Sixel.
    Sixel,
    /// Two pixels a cell in colour.
    Halfblocks,
    /// Eight dots a cell, lines only, one colour.
    Braille,
}

/// How the map is drawn.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Graphics {
    Kitty,
    Sixel,
    Halfblocks,
    Braille,
}

impl Graphics {
    /// The name `tui.log` and `--graphics` use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::Sixel => "sixel",
            Self::Halfblocks => "halfblocks",
            Self::Braille => "braille",
        }
    }

    /// Image pixels across and down one cell.
    pub fn cell_px(self, font: FontSize) -> (u16, u16) {
        match self {
            Self::Kitty | Self::Sixel => (font.width.max(1), font.height.max(1)),
            Self::Halfblocks => (1, 2),
            Self::Braille => (2, 4),
        }
    }

    fn protocol(self) -> Option<ProtocolType> {
        match self {
            Self::Kitty => Some(ProtocolType::Kitty),
            Self::Sixel => Some(ProtocolType::Sixel),
            _ => None,
        }
    }
}

/// The ladder: a forced rung as it is; under `auto` kitty or sixel when
/// the terminal answered for one, half-blocks with 256 colours or more,
/// braille below that and without colour. A terminal that answered for
/// iTerm2's protocol climbs on to half-blocks: the decision names kitty and
/// sixel only.
pub fn choose_graphics(flag: GraphicsFlag, detected: Option<ProtocolType>, tier: Tier) -> Graphics {
    match flag {
        GraphicsFlag::Kitty => Graphics::Kitty,
        GraphicsFlag::Sixel => Graphics::Sixel,
        GraphicsFlag::Halfblocks => Graphics::Halfblocks,
        GraphicsFlag::Braille => Graphics::Braille,
        GraphicsFlag::Auto => match (tier, detected) {
            (Tier::Mono, _) => Graphics::Braille,
            (_, Some(ProtocolType::Kitty)) => Graphics::Kitty,
            (_, Some(ProtocolType::Sixel)) => Graphics::Sixel,
            (Tier::Truecolor | Tier::Ansi256, _) => Graphics::Halfblocks,
            (Tier::Ansi16, _) => Graphics::Braille,
        },
    }
}

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

/// One view drawn: its pixels, what to credit, and the encoded picture of
/// a graphics protocol once it was asked for.
struct Painted {
    view: Viewport,
    fb: Vec<u16>,
    attribution: String,
    picture: Option<SlicedProtocol>,
}

/// The installed regions, the backend, and the last view drawn: a frame
/// that shows the same view again costs nothing, which matters for a sixel
/// encoded whole.
pub struct MapPainter {
    regions: Vec<Installed>,
    graphics: Graphics,
    pictures: Pictures,
    font: FontSize,
    painted: Option<Painted>,
    read: Vec<(ViewTile, Vec<u8>)>,
}

impl MapPainter {
    /// A painter over `regions`, drawing with `graphics`. `picker` is the
    /// terminal's answer, needed for kitty and sixel; without one those
    /// draw for `ratatui-image`'s assumed 10 x 20 font.
    pub fn new(regions: Vec<Installed>, graphics: Graphics, picker: Option<Picker>) -> Self {
        let mut picker = match (graphics.protocol(), picker) {
            (Some(_), Some(picker)) => Some(picker),
            (Some(_), None) => Some(Picker::halfblocks()),
            (None, _) => None,
        };
        let backend = match (graphics.protocol(), picker.as_mut()) {
            (Some(protocol), Some(picker)) => {
                picker.set_protocol_type(protocol);
                Backend::Graphics(protocol)
            }
            _ => Backend::Text,
        };
        let font = picker.as_ref().map_or(
            FontSize {
                width: 8,
                height: 16,
            },
            Picker::font_size,
        );
        Self {
            regions,
            graphics,
            pictures: Pictures::new(picker, backend),
            font,
            painted: None,
            read: Vec::new(),
        }
    }

    pub fn graphics(&self) -> Graphics {
        self.graphics
    }

    /// What the model is told: the cell's pixels, the zooms the regions
    /// and the outline reach.
    pub fn setup(&self) -> MapSetup {
        let zooms = view_zooms(self.regions.iter().map(|r| r.region.header()));
        MapSetup {
            cell_px: self.graphics.cell_px(self.font),
            zooms: (*zooms.start(), *zooms.end()),
        }
    }

    /// The ink of this backend: braille draws lines alone.
    fn ink(&self) -> Ink {
        match self.graphics {
            Graphics::Braille => Ink::Lines(Rgb565::new(0, 0, 0)),
            _ => Ink::Colour,
        }
    }

    /// Draw `view` unless it is the one drawn last.
    fn paint(&mut self, view: &Viewport) {
        if self.painted.as_ref().is_some_and(|p| p.view == *view) {
            return;
        }
        let (w, h) = view.size();
        let mut fb = vec![0u16; (w as usize) * (h as usize)];
        let ink = self.ink();
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
                    region_into(&installed.region, view, ink, &mut self.read, &mut frame)
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
        self.painted = Some(Painted {
            view: *view,
            fb,
            attribution,
            picture: None,
        });
    }

    /// Draw the open map: the picture over `shown.area`, the pins over it,
    /// the attribution on the row below.
    pub fn render(&mut self, frame: &mut TermFrame, shown: &MapView, muted: Style) {
        self.paint(&shown.viewport);
        let graphics = self.graphics;
        let area = Rect::new(
            shown.area.x,
            shown.area.y,
            shown.area.width,
            shown.area.height,
        );
        let Some(painted) = self.painted.as_mut() else {
            return;
        };
        let (w, _) = painted.view.size();
        let px = |x: u32, y: u32| -> Color {
            let i = (y as usize) * (w as usize) + x as usize;
            let raw = painted.fb.get(i).copied().unwrap_or(0);
            let c = embedded_graphics::pixelcolor::raw::RawU16::new(u16::from_be(raw));
            let [r, g, b] = rgb888(Rgb565::from(c));
            Color::Rgb(r, g, b)
        };
        match graphics {
            Graphics::Halfblocks => {
                let buf = frame.buffer_mut();
                for row in 0..area.height {
                    for col in 0..area.width {
                        let (x, y) = (u32::from(col), u32::from(row) * 2);
                        if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                            cell.set_symbol("▀").set_fg(px(x, y)).set_bg(px(x, y + 1));
                        }
                    }
                }
            }
            Graphics::Braille => {
                let ink = |x: u32, y: u32| {
                    let i = (y as usize) * (w as usize) + x as usize;
                    painted
                        .fb
                        .get(i)
                        .is_some_and(|&v| Rgb565::from(raw(v)) != BACKGROUND)
                };
                let buf = frame.buffer_mut();
                for row in 0..area.height {
                    for col in 0..area.width {
                        let (x, y) = (u32::from(col) * 2, u32::from(row) * 4);
                        let dots = braille(|dx, dy| ink(x + dx, y + dy));
                        if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                            cell.set_char(dots);
                        }
                    }
                }
            }
            Graphics::Kitty | Graphics::Sixel => {
                if painted.picture.is_none() {
                    let (pw, ph) = painted.view.size();
                    let mut rgb = Vec::with_capacity(painted.fb.len() * 3);
                    if let Some(f) = Frame::new(&mut painted.fb, pw, ph) {
                        f.rgb888_into(&mut rgb);
                    }
                    painted.picture = images::rgb_image(pw, ph, rgb).and_then(|image| {
                        self.pictures.encode(image, (area.width, area.height)).ok()
                    });
                }
                if let Some(picture) = &painted.picture {
                    images::render(frame, area, picture, 0, 0);
                }
            }
        }
        let buf = frame.buffer_mut();
        for pin in &shown.pins {
            let mut style = Style::default();
            if pin.stale {
                style = style.patch(muted).add_modifier(Modifier::DIM);
            }
            if pin.selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let apart = pin.apart.iter().map(|a| (a.col, a.row, &a.text));
            for (col, row, text) in std::iter::once((pin.col, pin.row, &pin.text)).chain(apart) {
                let room = usize::from((area.x + area.width).saturating_sub(col));
                let (end, _) = buf.set_stringn(col, row, text, room, style);
                // A graphics protocol marks the cells under its picture as
                // skipped; the pin's own cells are drawn over it.
                for x in col..end {
                    if let Some(cell) = buf.cell_mut((x, row)) {
                        cell.set_diff_option(CellDiffOption::None);
                    }
                }
            }
        }
        // The region's own line, "© OpenStreetMap contributors", or the
        // outline's; always on the view.
        let credit = format!("{} ", painted.attribution);
        let len = credit.chars().count() as u16;
        let x = (area.x + area.width).saturating_sub(len).max(area.x);
        buf.set_stringn(
            x,
            area.y + area.height,
            &credit,
            usize::from(area.width),
            muted,
        );
    }
}

fn raw(v: u16) -> embedded_graphics::pixelcolor::raw::RawU16 {
    embedded_graphics::pixelcolor::raw::RawU16::new(u16::from_be(v))
}

/// The braille cell whose dots `ink(dx, dy)` sets, dx in 0..2, dy in 0..4;
/// a blank where none is.
fn braille(ink: impl Fn(u32, u32) -> bool) -> char {
    // The Unicode braille bit of each dot, column by column.
    const BITS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
    let mut bits = 0u32;
    for (dx, column) in BITS.iter().enumerate() {
        for (dy, bit) in column.iter().enumerate() {
            if ink(dx as u32, dy as u32) {
                bits |= bit;
            }
        }
    }
    match bits {
        0 => ' ',
        bits => char::from_u32(0x2800 + bits).unwrap_or(' '),
    }
}

/// Draw `region`'s tiles of `view` into `frame`; `false` when a tile could
/// not be read, and the outline is drawn instead.
fn region_into(
    region: &Region<File>,
    view: &Viewport,
    ink: Ink,
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
    let mut points = vec![embedded_graphics::prelude::Point::zero(); 8192];
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
    let Ok(_) = draw(
        &tiles,
        Profile::Full,
        ink,
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
    use leviculum_map::mercator::{project, LatLon};
    use loquax::ui::map::PinCell;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn the_ladder_takes_kitty_or_sixel_then_halfblocks_then_braille() {
        use GraphicsFlag::Auto;
        let kitty = Some(ProtocolType::Kitty);
        let sixel = Some(ProtocolType::Sixel);
        let none = Some(ProtocolType::Halfblocks);
        assert_eq!(
            choose_graphics(Auto, kitty, Tier::Truecolor),
            Graphics::Kitty
        );
        // foot: sixel, no kitty graphics.
        assert_eq!(
            choose_graphics(Auto, sixel, Tier::Truecolor),
            Graphics::Sixel
        );
        assert_eq!(choose_graphics(Auto, sixel, Tier::Ansi16), Graphics::Sixel);
        assert_eq!(
            choose_graphics(Auto, Some(ProtocolType::Iterm2), Tier::Truecolor),
            Graphics::Halfblocks
        );
        assert_eq!(
            choose_graphics(Auto, none, Tier::Ansi256),
            Graphics::Halfblocks
        );
        assert_eq!(
            choose_graphics(Auto, None, Tier::Truecolor),
            Graphics::Halfblocks
        );
        assert_eq!(choose_graphics(Auto, none, Tier::Ansi16), Graphics::Braille);
        // No colour: braille, whatever the terminal speaks.
        assert_eq!(choose_graphics(Auto, kitty, Tier::Mono), Graphics::Braille);
        // A forced rung is taken as it is.
        assert_eq!(
            choose_graphics(GraphicsFlag::Sixel, None, Tier::Mono),
            Graphics::Sixel
        );
        assert_eq!(
            choose_graphics(GraphicsFlag::Braille, kitty, Tier::Truecolor),
            Graphics::Braille
        );
    }

    #[test]
    fn braille_sets_the_dot_of_each_inked_pixel() {
        assert_eq!(braille(|_, _| false), ' ');
        assert_eq!(braille(|_, _| true), '⣿');
        assert_eq!(braille(|dx, dy| dx == 0 && dy == 0), '⠁');
        assert_eq!(braille(|dx, dy| dx == 1 && dy == 3), '⢀');
    }

    /// The outline over Europe at z1, coasts in view, in a 30 x 8 picture,
    /// with one pin.
    fn shown(cell_px: (u16, u16)) -> MapView {
        let area = leviculum_ui::Rect::new(1, 1, 30, 8);
        let at = project(LatLon::from_degrees(48.0, 10.0));
        MapView {
            area,
            viewport: Viewport::new(at, 1, 30 * u32::from(cell_px.0), 8 * u32::from(cell_px.1)),
            cell_px,
            pins: vec![PinCell {
                peer: [1; 16],
                col: 16,
                row: 5,
                text: "● bob · 4 min".to_string(),
                apart: None,
                stale: false,
                selected: true,
            }],
        }
    }

    fn drawn(graphics: Graphics, picker: Option<Picker>) -> (TestBackend, String) {
        let mut painter = MapPainter::new(Vec::new(), graphics, picker);
        let shown = shown(painter.setup().cell_px);
        let mut terminal = Terminal::new(TestBackend::new(32, 10)).expect("terminal");
        let Ok(_) = terminal.draw(|f| painter.render(f, &shown, Style::default()));
        let backend = terminal.backend().clone();
        let mut all = String::new();
        for y in 0..10 {
            for x in 0..32 {
                if let Some(cell) = backend.buffer().cell((x, y)) {
                    all.push_str(cell.symbol());
                }
            }
            all.push('\n');
        }
        (backend, all)
    }

    /// Kitty and sixel are not cells a snapshot can read; what is checked
    /// is that each produces its protocol's bytes, from a picker that asked
    /// no terminal.
    #[test]
    fn kitty_and_sixel_produce_their_protocol_bytes() {
        let (_, sixel) = drawn(Graphics::Sixel, Some(Picker::halfblocks()));
        assert!(sixel.contains("\u{1b}P"), "a sixel DCS");
        assert!(sixel.contains("● bob"), "the pin over the picture");
        assert!(sixel.contains("Natural Earth"));
        let (_, kitty) = drawn(Graphics::Kitty, None);
        assert!(kitty.contains("\u{1b}_G"), "a kitty APC");
        assert!(kitty.contains("● bob"));
    }

    /// Half-blocks paint every picture cell with the upper half block and
    /// two colours; braille draws the coasts as dots; both carry the pin
    /// and the attribution.
    #[test]
    fn halfblocks_and_braille_are_plain_cells() {
        let (backend, text) = drawn(Graphics::Halfblocks, None);
        let cell = backend.buffer().cell((2, 2)).expect("cell");
        assert_eq!(cell.symbol(), "▀");
        assert!(matches!(cell.fg, Color::Rgb(..)) && matches!(cell.bg, Color::Rgb(..)));
        assert!(text.contains("● bob · 4 min"));
        assert!(text.contains("Natural Earth"));
        let (_, text) = drawn(Graphics::Braille, None);
        assert!(text.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)));
        assert!(text.contains("● bob · 4 min"));
    }

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
