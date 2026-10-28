//! The map's picture in the terminal (`docs/src/concepts/loquax-map.md`,
//! sections 6 and 7, slice (c)): the backend ladder, the region files the
//! reader placed by hand, and the cells the picture and the pins become.
//!
//! The model ([`loquax::ui::map`]) says which view to draw and where the
//! pins go; this module draws the view's picture under them and the
//! attribution line below it. The ladder, decided 2026-10-27: kitty or
//! sixel where the terminal answers for one, else half-blocks in colour,
//! else braille, which is also the backend under `--no-color` and behind
//! mosh (1088). The `--graphics` flag forces a rung, and `tui.log` names
//! the one taken and why (`GRAPHICS backend= reason=`), beside the `KEYS`
//! line.
//!
//! The region files, where they are read from, and the frame one view is
//! drawn into are the library's ([`loquax::picture`]), shared with the GUI.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use embedded_graphics::pixelcolor::Rgb565;
use leviculum_map::draw::Ink;
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::frame::{rgb888, Frame};
use leviculum_mapfill::style::BACKGROUND;
use leviculum_ui::color::Tier;
use leviculum_ui_ratatui::images::{
    self, Backend, Between, FontSize, Picker, Pictures, ProtocolType, SlicedProtocol,
};
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame as TermFrame;
use tokio::sync::Notify;

use loquax::picture::{Drawn, Maps, Painter};
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

/// The rung the ladder took, and the facts that kept it off the rungs
/// above.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    pub graphics: Graphics,
    /// `tui.log`'s `reason=`: `forced` for a `--graphics` rung; under
    /// `auto` `no-color`, `no-graphics` (the terminal answered for no
    /// kitty or sixel), `mosh`, `no-truecolor` (half-blocks in the 256
    /// colour palette) and `16-colors`. Empty on the top rung.
    pub reasons: Vec<&'static str>,
}

impl Chosen {
    /// The reasons as `tui.log` writes them, `best` where there is none.
    pub fn reason(&self) -> String {
        match self.reasons.is_empty() {
            true => "best".to_string(),
            false => self.reasons.join(","),
        }
    }
}

/// The ladder: a forced rung as it is; under `auto` kitty or sixel when
/// the terminal answered for one, half-blocks with 256 colours or more,
/// braille below that and without colour. A terminal that answered for
/// iTerm2's protocol climbs on to half-blocks: the decision names kitty and
/// sixel only.
///
/// Behind mosh it is braille (1088). mosh answers the protocol query
/// itself and drops sixel and kitty pictures, so no protocol is taken
/// there. Half-blocks are not either: mosh 1.4.0 was measured to pass
/// their colours and glyph unchanged, yet Lew's map from Termux over mosh
/// (2026-10-28) showed an empty box in every cell, so the reader there
/// gets the rung that carries no colour and only one glyph family.
/// `--graphics halfblocks` still forces them.
pub fn choose_graphics(
    flag: GraphicsFlag,
    detected: Option<ProtocolType>,
    tier: Tier,
    between: Between,
) -> Chosen {
    let forced = |graphics| Chosen {
        graphics,
        reasons: vec!["forced"],
    };
    match flag {
        GraphicsFlag::Kitty => return forced(Graphics::Kitty),
        GraphicsFlag::Sixel => return forced(Graphics::Sixel),
        GraphicsFlag::Halfblocks => return forced(Graphics::Halfblocks),
        GraphicsFlag::Braille => return forced(Graphics::Braille),
        GraphicsFlag::Auto => {}
    }
    let take = |graphics, reasons| Chosen { graphics, reasons };
    if tier == Tier::Mono {
        return take(Graphics::Braille, vec!["no-color"]);
    }
    let mut reasons = Vec::new();
    match (detected, between.mosh) {
        (Some(ProtocolType::Kitty), false) => return take(Graphics::Kitty, reasons),
        (Some(ProtocolType::Sixel), false) => return take(Graphics::Sixel, reasons),
        (Some(ProtocolType::Kitty | ProtocolType::Sixel), true) => {}
        _ => reasons.push("no-graphics"),
    }
    if between.mosh {
        reasons.push("mosh");
        return take(Graphics::Braille, reasons);
    }
    match tier {
        Tier::Truecolor => take(Graphics::Halfblocks, reasons),
        Tier::Ansi256 => {
            reasons.push("no-truecolor");
            take(Graphics::Halfblocks, reasons)
        }
        Tier::Ansi16 | Tier::Mono => {
            reasons.push("16-colors");
            take(Graphics::Braille, reasons)
        }
    }
}

/// A no-op the terminal carries out before a picture sent again only to
/// erase stale labels: save and restore the cursor, which makes the
/// picture's cell differ from the last frame's so ratatui sends it.
const NUDGE: &str = "\u{1b}7\u{1b}8";

/// One label drawn over the picture.
#[derive(Clone, Debug, PartialEq)]
struct Label {
    col: u16,
    row: u16,
    text: String,
    style: Style,
}

/// How long a picture behind the view is kept on screen while every
/// finished paint is already overtaken by a newer key (1075). A paint that
/// finishes for a view the reader has moved on from is dropped, the newest
/// view wins; but where keys come faster than paints, dropping every one
/// would freeze the map until the key is let go, so after this long the
/// overtaken paint is shown as the nearest there is.
const CATCH_UP_MS: u64 = 100;

/// An encoded picture.
enum Picture {
    /// Kitty, or a sixel through ratatui-image's quantiser: under tmux, or
    /// past 256 colours.
    Sliced(SlicedProtocol),
    /// A sixel of the frame's own colours ([`images::sixel_indexed`]).
    Whole(images::Protocol),
}

/// One view drawn: its pixels, what to credit, and under kitty or sixel
/// the encoded picture for `cells`.
struct Painted {
    view: Viewport,
    cells: (u16, u16),
    /// The maps directory's epoch it was painted at.
    epoch: u64,
    fb: Vec<u16>,
    attribution: String,
    picture: Option<Picture>,
}

/// One paint asked for: the view, the cells its picture covers, and the
/// maps directory's epoch, so a region that landed paints the same view
/// again (1089).
#[derive(Copy, Clone, Debug, PartialEq)]
struct Ask {
    view: Viewport,
    cells: (u16, u16),
    epoch: u64,
}

/// What draws a view: the regions, the backend, the scratch. It does not
/// know the terminal's frame, so it runs on the painter's own thread as
/// well as inline.
struct Canvas {
    painter: Painter,
    graphics: Graphics,
    pictures: Pictures,
}

impl Canvas {
    /// The ink of this backend: braille draws lines alone.
    fn ink(&self) -> Ink {
        match self.graphics {
            Graphics::Braille => Ink::Lines(Rgb565::new(0, 0, 0)),
            _ => Ink::Colour,
        }
    }

    /// Draw `ask`'s view, and encode it under kitty or sixel.
    fn paint(&mut self, ask: Ask) -> Painted {
        let view = &ask.view;
        let (w, h) = view.size();
        let Drawn {
            mut fb,
            attribution,
        } = self.painter.draw(view, self.ink(), 100);
        let picture = match self.graphics {
            Graphics::Kitty | Graphics::Sixel => self.encode(&mut fb, w, h, ask.cells),
            Graphics::Halfblocks | Graphics::Braille => None,
        };
        Painted {
            view: *view,
            cells: ask.cells,
            epoch: ask.epoch,
            fb,
            attribution,
            picture,
        }
    }

    /// The picture of `fb` for `cells`. A sixel of at most 256 colours,
    /// which the map's style always is, is written with those colours as
    /// its palette and no quantiser; anything else goes through
    /// ratatui-image.
    fn encode(&self, fb: &mut [u16], w: u32, h: u32, cells: (u16, u16)) -> Option<Picture> {
        if self.graphics == Graphics::Sixel {
            if let Some((palette, indices)) = indexed(fb) {
                if let Some(sixel) = images::sixel_indexed(&palette, &indices, w, h, cells) {
                    return Some(Picture::Whole(sixel));
                }
            }
        }
        let mut rgb = Vec::with_capacity(fb.len() * 3);
        if let Some(f) = Frame::new(fb, w, h) {
            f.rgb888_into(&mut rgb);
        }
        images::rgb_image(w, h, rgb)
            .and_then(|image| self.pictures.encode(image, cells).ok())
            .map(Picture::Sliced)
    }
}

/// `fb` as a palette of its colours in RGB888 and one index a pixel;
/// `None` past 256 colours.
fn indexed(fb: &[u16]) -> Option<(Vec<[u8; 3]>, Vec<u8>)> {
    const NONE: u16 = u16::MAX;
    let mut slot = vec![NONE; 1 << 16];
    let mut palette = Vec::new();
    let mut indices = Vec::with_capacity(fb.len());
    for &v in fb {
        let mut i = slot[usize::from(v)];
        if i == NONE {
            i = u16::try_from(palette.len()).ok().filter(|&i| i < 256)?;
            slot[usize::from(v)] = i;
            palette.push(rgb888(Rgb565::from(raw(v))));
        }
        indices.push(i as u8);
    }
    Some((palette, indices))
}

/// Between the painter and its thread: the newest paint asked for, which
/// replaces one not yet started, and the last one finished.
#[derive(Default)]
struct Slot {
    asked: Option<Ask>,
    done: Option<Painted>,
    quit: bool,
}

impl Slot {
    /// Ask for `ask`; a paint asked before and not yet started is never
    /// drawn.
    fn ask(&mut self, ask: Ask) {
        self.asked = Some(ask);
    }

    /// The paint to start next, the newest asked.
    fn start(&mut self) -> Option<Ask> {
        self.asked.take()
    }

    /// A paint finished; one finished before and not yet taken is
    /// overtaken by it.
    fn finish(&mut self, painted: Painted) {
        self.done = Some(painted);
    }
}

/// Whether a finished paint is shown: when it is the view wanted now, or
/// when the picture on screen has been there [`CATCH_UP_MS`] or longer
/// (or there is none) and the paint is at least nearer than nothing.
fn keep(done: &Ask, wanted: &Ask, shown_for: Option<Duration>) -> bool {
    done == wanted || shown_for.is_none_or(|d| d >= Duration::from_millis(CATCH_UP_MS))
}

/// The thread the painter draws on, so a key is read while the last one's
/// view is drawn and encoded.
struct Worker {
    shared: Arc<(Mutex<Slot>, Condvar)>,
    ready: Arc<Notify>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The last paint asked for, so a frame of the same view asks nothing.
    asked: Option<Ask>,
}

impl Worker {
    fn spawn(mut canvas: Canvas) -> Self {
        let shared = Arc::new((Mutex::new(Slot::default()), Condvar::new()));
        let ready = Arc::new(Notify::new());
        let (theirs, notify) = (Arc::clone(&shared), Arc::clone(&ready));
        let thread = std::thread::Builder::new()
            .name("map-painter".to_string())
            .spawn(move || {
                let (slot, wake) = &*theirs;
                loop {
                    let ask = {
                        let Ok(mut held) = slot.lock() else { return };
                        loop {
                            if held.quit {
                                return;
                            }
                            if let Some(ask) = held.start() {
                                break ask;
                            }
                            held = match wake.wait(held) {
                                Ok(held) => held,
                                Err(_) => return,
                            };
                        }
                    };
                    let painted = canvas.paint(ask);
                    let Ok(mut held) = slot.lock() else { return };
                    held.finish(painted);
                    drop(held);
                    notify.notify_one();
                }
            })
            .ok();
        Self {
            shared,
            ready,
            thread,
            asked: None,
        }
    }

    fn ask(&mut self, ask: Ask) {
        if self.asked == Some(ask) {
            return;
        }
        self.asked = Some(ask);
        let (slot, wake) = &*self.shared;
        if let Ok(mut held) = slot.lock() {
            held.ask(ask);
        }
        wake.notify_one();
    }

    fn take(&self) -> Option<Painted> {
        let (slot, _) = &*self.shared;
        slot.lock().ok().and_then(|mut held| held.done.take())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let (slot, wake) = &*self.shared;
        if let Ok(mut held) = slot.lock() {
            held.quit = true;
        }
        wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where views are painted: in the frame that wants them, for a key script
/// and a test whose frames must be the same on every run, or on the
/// worker thread for the terminal.
enum Engine {
    Inline(Canvas),
    Worker(Worker),
}

/// The backend, the paints, and the last view drawn: a frame that shows
/// the same view again costs nothing, which matters for a sixel encoded
/// whole.
pub struct MapPainter {
    graphics: Graphics,
    maps: Maps,
    setup: MapSetup,
    engine: Engine,
    painted: Option<Painted>,
    /// When the picture on screen was put there, and how many were.
    shown_at: Option<Instant>,
    paints: u64,
    /// The labels of the last frame with the view they were placed for,
    /// and whether its picture carried [`NUDGE`]: flipped when the labels
    /// change, so the picture is sent again then and only then.
    labels: Vec<Label>,
    labels_view: Option<Viewport>,
    nudge: bool,
}

impl MapPainter {
    /// A painter over the regions of `maps`, drawing with `graphics` in the
    /// frame that asks. `picker` is the terminal's answer, needed for kitty
    /// and sixel; without one those draw for `ratatui-image`'s assumed 10 x
    /// 20 font.
    pub fn new(maps: Maps, graphics: Graphics, picker: Option<Picker>) -> Self {
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
        let painter = Painter::new(maps.clone());
        let setup = MapSetup {
            cell_px: graphics.cell_px(font),
            zooms: painter.zooms(),
            regions: painter.headers(),
        };
        Self {
            graphics,
            maps,
            setup,
            engine: Engine::Inline(Canvas {
                painter,
                graphics,
                pictures: Pictures::new(picker, backend),
            }),
            painted: None,
            shown_at: None,
            paints: 0,
            labels: Vec::new(),
            labels_view: None,
            nudge: false,
        }
    }

    /// The same painter drawing on its own thread: a frame shows the
    /// newest paint finished and asks for its own view, and
    /// [`MapPainter::ready`] wakes the loop when that is drawn.
    pub fn threaded(self) -> Self {
        let engine = match self.engine {
            Engine::Inline(canvas) => Engine::Worker(Worker::spawn(canvas)),
            worker => worker,
        };
        Self { engine, ..self }
    }

    pub fn graphics(&self) -> Graphics {
        self.graphics
    }

    /// What the model is told: the cell's pixels, the zooms the regions
    /// and the outline reach.
    pub fn setup(&self) -> MapSetup {
        self.setup.clone()
    }

    /// How many paints were put on screen.
    pub fn paints(&self) -> u64 {
        self.paints
    }

    /// The view of the picture on screen.
    #[cfg(test)]
    pub fn painted_view(&self) -> Option<Viewport> {
        self.painted.as_ref().map(|p| p.view)
    }

    /// Resolves when the thread finished a paint; never inline.
    pub async fn ready(&self) {
        match &self.engine {
            Engine::Worker(worker) => worker.ready.notified().await,
            Engine::Inline(_) => std::future::pending().await,
        }
    }

    /// Make `ask` the picture: drawn now inline; on the thread asked for,
    /// and the newest paint finished taken when [`keep`] says so.
    fn paint(&mut self, ask: Ask, now: Instant) {
        let current = |p: &Option<Painted>| {
            p.as_ref()
                .is_some_and(|p| p.view == ask.view && p.cells == ask.cells && p.epoch == ask.epoch)
        };
        match &mut self.engine {
            Engine::Inline(canvas) => {
                if !current(&self.painted) {
                    self.painted = Some(canvas.paint(ask));
                    self.shown_at = Some(now);
                    self.paints += 1;
                }
            }
            Engine::Worker(worker) => {
                if let Some(done) = worker.take() {
                    let shown_for = self.shown_at.map(|t| now.saturating_duration_since(t));
                    let of = Ask {
                        view: done.view,
                        cells: done.cells,
                        epoch: done.epoch,
                    };
                    if keep(&of, &ask, shown_for) {
                        self.painted = Some(done);
                        self.shown_at = Some(now);
                        self.paints += 1;
                    }
                }
                if !current(&self.painted) {
                    worker.ask(ask);
                }
            }
        }
    }

    /// Draw the open map: the picture over `shown.area`, the pins over it,
    /// the attribution on the row below. On the thread the picture may be
    /// a paint behind `shown`'s view for a frame; its labels are then the
    /// ones placed for it, so the pins never stand off the streets.
    pub fn render(&mut self, frame: &mut TermFrame, shown: &MapView, muted: Style) {
        let ask = Ask {
            view: shown.viewport,
            cells: (shown.area.width, shown.area.height),
            epoch: self.maps.epoch(),
        };
        self.paint(ask, Instant::now());
        let graphics = self.graphics;
        // The view is the model's, laid out for the size it last heard of;
        // the frame is the terminal's now. Between a shrink and its resize
        // event they differ, and nothing is written outside the frame
        // (1087).
        let bounds = frame.area();
        let area = Rect::new(
            shown.area.x,
            shown.area.y,
            shown.area.width,
            shown.area.height,
        )
        .intersection(bounds);
        let Some(painted) = self.painted.as_ref() else {
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
        // A picture painted for another pane size is not drawn over this
        // one: its cells would not cover the pane.
        let fits = painted.cells == ask.cells && (area.width, area.height) == ask.cells;
        match graphics {
            _ if !fits => {}
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
            Graphics::Kitty | Graphics::Sixel => match &painted.picture {
                Some(Picture::Sliced(picture)) => images::render(frame, area, picture, 0, 0),
                Some(Picture::Whole(picture)) => images::render_whole(frame, area, picture),
                None => {}
            },
        }
        // The labels placed for a paint of another pane size belong to
        // that size's layout, not to this one: a shrink would put them
        // below the frame (1087).
        let behind = fits && painted.view != ask.view && self.labels_view == Some(painted.view);
        let labels = match behind {
            true => self.labels.clone(),
            false => labels_of(shown, muted),
        };
        let buf = frame.buffer_mut();
        if matches!(graphics, Graphics::Kitty | Graphics::Sixel) {
            // A graphics protocol marks the cells under its picture as
            // skipped, so ratatui never rewrites the cells a label left:
            // the terminal keeps the old text there. When the labels moved,
            // the picture is sent again, its erase clearing them (1074).
            if labels != self.labels {
                self.nudge = !self.nudge;
            }
            if self.nudge {
                for row in area.y..area.y + area.height {
                    if let Some(cell) = buf.cell_mut((area.x, row)) {
                        if matches!(cell.diff_option, CellDiffOption::ForcedWidth(_)) {
                            let nudged = format!("{NUDGE}{}", cell.symbol());
                            cell.set_symbol(&nudged);
                        }
                    }
                }
            }
        }
        for label in &labels {
            let (col, row) = (label.col, label.row);
            if !area.contains(ratatui::layout::Position::new(col, row)) {
                continue;
            }
            let room = usize::from((area.x + area.width).saturating_sub(col));
            let (end, _) = buf.set_stringn(col, row, &label.text, room, label.style);
            // Drawn over the picture every frame: a picture sent again
            // erased the label even where its cells did not change, and
            // a blank in it is the same blank as last frame's.
            for x in col..end {
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_diff_option(CellDiffOption::AlwaysUpdate);
                }
            }
        }
        if fits && !behind {
            self.labels_view = Some(painted.view);
        }
        self.labels = labels;
        // The region's own line, "© OpenStreetMap contributors", or the
        // outline's; always on the view.
        let credit = format!("{} ", painted.attribution);
        let len = credit.chars().count() as u16;
        let x = (area.x + area.width).saturating_sub(len).max(area.x);
        if area.y + area.height >= bounds.bottom() {
            return;
        }
        buf.set_stringn(
            x,
            area.y + area.height,
            &credit,
            usize::from(area.width),
            muted,
        );
    }
}

/// The labels of `shown`'s pins, styled.
fn labels_of(shown: &MapView, muted: Style) -> Vec<Label> {
    let mut labels = Vec::new();
    for pin in &shown.pins {
        let mut style = Style::default();
        if pin.stale {
            style = style.patch(muted).add_modifier(Modifier::DIM);
        }
        if pin.selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let apart = pin.apart.iter().map(|a| (a.col, a.row, a.text.clone()));
        labels.extend(
            std::iter::once((pin.col, pin.row, pin.text.clone()))
                .chain(apart)
                .map(|(col, row, text)| Label {
                    col,
                    row,
                    text,
                    style,
                }),
        );
    }
    labels
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

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon, World};
    use loquax::ui::map::PinCell;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    /// Every combination of what the terminal answered, its colour tier
    /// and mosh, under `auto`, against the rung and the reasons.
    #[test]
    fn the_ladder_takes_kitty_or_sixel_then_halfblocks_then_braille() {
        use GraphicsFlag::Auto;
        use ProtocolType::{Halfblocks as None_, Iterm2, Kitty, Sixel};
        let direct = Between::default();
        let mosh = Between {
            mosh: true,
            ..direct
        };
        let tmux = Between {
            tmux: true,
            ..direct
        };
        let screen = Between {
            screen: true,
            ..direct
        };
        let detections = [None, Some(None_), Some(Iterm2), Some(Kitty), Some(Sixel)];
        let tiers = [Tier::Mono, Tier::Ansi16, Tier::Ansi256, Tier::Truecolor];
        for between in [direct, mosh, tmux, screen] {
            for detected in detections {
                for tier in tiers {
                    let chosen = choose_graphics(Auto, detected, tier, between);
                    let protocol = match detected {
                        Some(Kitty) => Some(Graphics::Kitty),
                        Some(Sixel) => Some(Graphics::Sixel),
                        _ => None,
                    };
                    let want: (Graphics, &[&str]) = match (tier, protocol, between.mosh) {
                        (Tier::Mono, _, _) => (Graphics::Braille, &["no-color"]),
                        (_, Some(g), false) => (g, &[]),
                        (_, Some(_), true) => (Graphics::Braille, &["mosh"]),
                        (_, None, true) => (Graphics::Braille, &["no-graphics", "mosh"]),
                        (Tier::Truecolor, None, false) => (Graphics::Halfblocks, &["no-graphics"]),
                        (Tier::Ansi256, None, false) => {
                            (Graphics::Halfblocks, &["no-graphics", "no-truecolor"])
                        }
                        (Tier::Ansi16, None, false) => {
                            (Graphics::Braille, &["no-graphics", "16-colors"])
                        }
                    };
                    assert_eq!(
                        (chosen.graphics, chosen.reasons.as_slice()),
                        want,
                        "{detected:?} {tier:?} {between:?}"
                    );
                }
            }
        }
        // Lew's line from Termux over mosh, 2026-10-28, and what it is now.
        let chosen = choose_graphics(Auto, Some(None_), Tier::Ansi256, mosh);
        assert_eq!(chosen.graphics, Graphics::Braille);
        assert_eq!(chosen.reason(), "no-graphics,mosh");
        // foot, on foot.
        let chosen = choose_graphics(Auto, Some(Sixel), Tier::Ansi256, direct);
        assert_eq!(
            (chosen.graphics, chosen.reason()),
            (Graphics::Sixel, "best".to_string())
        );
        // A forced rung is taken as it is, whatever was found.
        for (flag, graphics) in [
            (GraphicsFlag::Kitty, Graphics::Kitty),
            (GraphicsFlag::Sixel, Graphics::Sixel),
            (GraphicsFlag::Halfblocks, Graphics::Halfblocks),
            (GraphicsFlag::Braille, Graphics::Braille),
        ] {
            for between in [direct, mosh] {
                for tier in tiers {
                    let chosen = choose_graphics(flag, Some(Kitty), tier, between);
                    assert_eq!(
                        (chosen.graphics, chosen.reason()),
                        (graphics, "forced".into())
                    );
                }
            }
        }
    }

    /// Half-blocks at 256 colours reach the terminal as palette indexes
    /// only: the frame's downgrade, which the screen runs over every
    /// frame, turns the map's RGB into `38;5`/`48;5`, and no `38;2` or
    /// `48;2` is written that a 256 colour terminal would misread.
    #[test]
    fn halfblocks_at_256_colours_emit_only_palette_sgr() {
        use leviculum_ui::color::ColorDepth;
        use leviculum_ui_ratatui::style::downgrade_buffer;
        use ratatui::backend::CrosstermBackend;
        let mut painter = MapPainter::new(Maps::default(), Graphics::Halfblocks, None);
        let shown = shown(painter.setup().cell_px);
        // The backend's writer is private in ratatui; a shared buffer
        // keeps what it wrote.
        #[derive(Clone, Default)]
        struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
        impl std::io::Write for Shared {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let bytes = Shared::default();
        // A fixed viewport: the size is not asked of a terminal there is not.
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 32, 10)),
        };
        let mut terminal = Terminal::with_options(CrosstermBackend::new(bytes.clone()), options)
            .expect("terminal");
        terminal
            .draw(|f| {
                painter.render(f, &shown, Style::default());
                downgrade_buffer(f, ColorDepth::Ansi256, false);
            })
            .expect("draw");
        let out = String::from_utf8_lossy(&bytes.0.borrow()).into_owned();
        // The 30 x 8 picture, less the pin label over it.
        assert!(out.matches('▀').count() >= 30 * 8 - 20, "{out:?}");
        let sgr: Vec<&str> = out
            .split("\u{1b}[")
            .skip(1)
            .filter_map(|s| s.split_once('m').map(|(p, _)| p))
            .filter(|p| p.chars().all(|c| c.is_ascii_digit() || c == ';'))
            .collect();
        assert!(sgr.iter().any(|p| p.contains("38;5;")));
        assert!(sgr.iter().any(|p| p.contains("48;5;")));
        for p in &sgr {
            assert!(
                !p.contains("38;2") && !p.contains("48;2"),
                "24-bit SGR {p:?}"
            );
        }
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
        let mut painter = MapPainter::new(Maps::default(), graphics, picker);
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

    /// What a terminal shows after the frames ratatui sent it: each
    /// frame's diff against the one before applied in order, as
    /// `Terminal::flush` sends it. A sixel's leading erase is carried out,
    /// `ECH` of its width on each of its rows, and the picture's cells are
    /// `▒`; a kitty row writes its placeholders across the row, also `▒`.
    /// A `TestBackend` alone keeps text an erase would remove.
    struct Screen {
        shown: ratatui::buffer::Buffer,
        last: ratatui::buffer::Buffer,
    }

    impl Screen {
        fn new(area: Rect) -> Self {
            Self {
                shown: ratatui::buffer::Buffer::empty(area),
                last: ratatui::buffer::Buffer::empty(area),
            }
        }

        fn frame(
            &mut self,
            terminal: &mut Terminal<TestBackend>,
            painter: &mut MapPainter,
            shown: &MapView,
        ) {
            let next = terminal
                .draw(|f| painter.render(f, shown, Style::default()))
                .expect("draw")
                .buffer
                .clone();
            let updates: Vec<(u16, u16, ratatui::buffer::Cell)> = self
                .last
                .diff(&next)
                .into_iter()
                .map(|(x, y, cell)| (x, y, cell.clone()))
                .collect();
            for (x, y, cell) in updates {
                self.write(x, y, &cell);
            }
            self.last = next;
        }

        fn write(&mut self, x: u16, y: u16, cell: &ratatui::buffer::Cell) {
            let symbol = cell.symbol();
            let mut picture = |x: u16, y: u16| {
                if let Some(c) = self.shown.cell_mut((x, y)) {
                    c.reset();
                    c.set_symbol("▒");
                }
            };
            if symbol.contains("\u{1b}P") {
                let width: u16 = symbol
                    .split_once("\u{1b}[")
                    .and_then(|(_, rest)| rest.split_once('X'))
                    .and_then(|(n, _)| n.parse().ok())
                    .expect("a sixel opens with an erase");
                let rows = symbol.matches("X\u{1b}[1B").count().max(1) as u16;
                for dy in 0..rows {
                    for dx in 0..width {
                        picture(x + dx, y + dy);
                    }
                }
            } else if symbol.contains('\u{10EEEE}') {
                let width = symbol.chars().filter(|&c| c == '\u{10EEEE}').count() as u16;
                for dx in 0..width {
                    picture(x + dx, y);
                }
            } else if let Some(c) = self.shown.cell_mut((x, y)) {
                *c = cell.clone();
            }
        }

        fn row(&self, y: u16) -> String {
            (0..self.shown.area.width)
                .filter_map(|x| self.shown.cell((x, y)).map(|c| c.symbol().to_string()))
                .collect()
        }
    }

    /// A 30 x 8 picture at (1, 1) of `centre` at `zoom`, with one pin at
    /// `(col, 5)`.
    fn one_pin(painter: &MapPainter, centre: World, zoom: u8, col: u16, text: &str) -> MapView {
        let cell_px = painter.setup().cell_px;
        let mut view = shown(cell_px);
        view.viewport = Viewport::new(
            centre,
            zoom,
            30 * u32::from(cell_px.0),
            8 * u32::from(cell_px.1),
        );
        view.pins[0].col = col;
        view.pins[0].text = text.to_string();
        view.pins[0].selected = false;
        view
    }

    fn graphics_painters() -> [MapPainter; 2] {
        [
            MapPainter::new(Maps::default(), Graphics::Sixel, Some(Picker::halfblocks())),
            MapPainter::new(Maps::default(), Graphics::Kitty, None),
        ]
    }

    /// A drag over open sea: the picture is the same plain colour before
    /// and after, so it is not sent again, and the label moved three cells
    /// left. Its old cells must not keep the old text (1074, Lew's foot:
    /// two labels of one peer).
    #[test]
    fn a_label_dragged_over_an_unchanged_picture_leaves_no_old_cells() {
        let sea = project(LatLon::from_degrees(40.0, -40.0));
        for mut painter in graphics_painters() {
            let name = painter.graphics().name();
            let (cw, _) = painter.setup().cell_px;
            let mut terminal = Terminal::new(TestBackend::new(32, 10)).expect("terminal");
            let mut screen = Screen::new(Rect::new(0, 0, 32, 10));
            let before = one_pin(&painter, sea, 8, 16, "● bob · 4 min");
            screen.frame(&mut terminal, &mut painter, &before);
            assert_eq!(screen.row(5).matches("bob").count(), 1, "{name}");
            let mut dragged = before.viewport;
            dragged.pan(3 * i32::from(cw), 0);
            let after = one_pin(&painter, dragged.centre, 8, 13, "● bob · 4 min");
            screen.frame(&mut terminal, &mut painter, &after);
            let row = screen.row(5);
            assert_eq!(row.matches("bob").count(), 1, "{name}: {row}");
            assert_eq!(row.matches("min").count(), 1, "{name}: {row}");
            assert!(row.contains("▒● bob · 4 min▒"), "{name}: {row}");
        }
    }

    /// The age shortens over a view that did not move: the picture is the
    /// one drawn last, and the cell the longer text took must not keep its
    /// last letter.
    #[test]
    fn a_label_that_shortens_over_the_same_view_leaves_no_old_cells() {
        let at = project(LatLon::from_degrees(48.0, 10.0));
        for mut painter in graphics_painters() {
            let name = painter.graphics().name();
            let mut terminal = Terminal::new(TestBackend::new(32, 10)).expect("terminal");
            let mut screen = Screen::new(Rect::new(0, 0, 32, 10));
            let long = one_pin(&painter, at, 1, 16, "● bob · 14 min");
            screen.frame(&mut terminal, &mut painter, &long);
            let short = one_pin(&painter, at, 1, 16, "● bob · 4 min");
            screen.frame(&mut terminal, &mut painter, &short);
            let row = screen.row(5);
            assert!(row.contains("▒● bob · 4 min▒"), "{name}: {row}");
        }
    }

    /// A zoom about the pin: the picture changes and is sent again, which
    /// erases what was under it, and the label stays on the same cells. It
    /// must be drawn again over the new picture.
    #[test]
    fn a_label_on_the_same_cells_over_a_new_picture_is_drawn_again() {
        let at = project(LatLon::from_degrees(48.0, 10.0));
        for mut painter in graphics_painters() {
            let name = painter.graphics().name();
            let mut terminal = Terminal::new(TestBackend::new(32, 10)).expect("terminal");
            let mut screen = Screen::new(Rect::new(0, 0, 32, 10));
            let out = one_pin(&painter, at, 1, 16, "● bob · 4 min");
            screen.frame(&mut terminal, &mut painter, &out);
            let zoomed = one_pin(&painter, at, 2, 16, "● bob · 4 min");
            screen.frame(&mut terminal, &mut painter, &zoomed);
            let row = screen.row(5);
            assert!(row.contains("▒● bob · 4 min▒"), "{name}: {row}");
        }
    }

    /// The cost of one keyboard step, phase by phase, the way ratatui-image
    /// encoded it before 1075 and the way the painter does now.
    ///
    /// In the suite: the Florence fixture on a small pane, checking that a
    /// map frame keeps to the few colours the painter's own sixel needs,
    /// and that its sixel is no larger than ratatui-image's. A style that
    /// grew past 256 colours (shading, smoothing) would fall back to the
    /// quantiser silently, and the frame back to five times its cost.
    ///
    /// With `LOQUAX_FRAME_REGION` set to a region file: Lew's pane (foot,
    /// 9 x 19 cells, 155 x 65 of them) over Frankfurt at z11, z14 and z17,
    /// and a held key at 25 and 50 repeats a second against both ways.
    /// A measurement, printed: `LOQUAX_FRAME_REGION=frankfurt.lmr cargo
    /// test --release -p loquax --features tui --bins frame_cost --
    /// --nocapture`.
    #[test]
    fn frame_cost() {
        let measure = std::env::var_os("LOQUAX_FRAME_REGION");
        let path = measure.clone().map_or_else(
            || {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../leviculum-map/fixtures/firenze.lmr")
            },
            PathBuf::from,
        );
        // The region alone in a maps directory of its own, linked.
        let dir = tempfile::tempdir().expect("maps dir");
        let linked = std::fs::canonicalize(&path).expect("region");
        std::os::unix::fs::symlink(&linked, dir.path().join("region.lmr")).expect("link");
        let open = || Maps::new(Some(dir.path().to_path_buf()));
        let ((cols, rows), at, zooms): ((u16, u16), _, &[u8]) = match measure {
            Some(_) => (
                (155, 65),
                project(LatLon::from_degrees(50.1106, 8.6821)),
                &[11, 14, 17],
            ),
            None => (
                (40, 12),
                project(LatLon::from_degrees(43.7731, 11.2560)),
                &[13],
            ),
        };
        let font = FontSize {
            width: 9,
            height: 19,
        };
        let (w, h) = (u32::from(cols) * 9, u32::from(rows) * 19);
        let picker = || {
            // Deprecated for a program, which asks the terminal; the
            // measurement needs foot's 9 x 19 without one.
            #[allow(deprecated)]
            let mut picker = Picker::from_fontsize(font);
            picker.set_protocol_type(ProtocolType::Sixel);
            picker
        };
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let bytes_of = |render: &dyn Fn(&mut TermFrame)| -> usize {
            let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("terminal");
            let Ok(_) = terminal.draw(render);
            let buffer = terminal.backend().buffer();
            buffer.content().iter().map(|c| c.symbol().len()).sum()
        };
        let mut canvas = Canvas {
            painter: Painter::new(open()),
            graphics: Graphics::Sixel,
            pictures: Pictures::new(Some(picker()), Backend::Graphics(ProtocolType::Sixel)),
        };
        let mut read = Vec::new();
        let area = Rect::new(0, 0, cols, rows);
        let n = if measure_on() { 5 } else { 1 };
        for &zoom in zooms {
            let mut sums = [0f64; 6];
            let (mut before, mut after) = (0, 0);
            for i in 0..n {
                // A step east each round, so no round redraws the last.
                let mut view = Viewport::new(at, zoom, w, h);
                view.pan(i * (w as i32 / 8), 0);
                let mut fb = vec![0u16; (w * h) as usize];
                let t0 = Instant::now();
                canvas.painter.regions()[0]
                    .region
                    .read_view(&view, &mut read)
                    .expect("read");
                let t1 = Instant::now();
                let ask = Ask {
                    view,
                    cells: (cols, rows),
                    epoch: 0,
                };
                // Painted whole for the render's share: the picture is
                // encoded again below, both ways.
                let graphics = canvas.graphics;
                canvas.graphics = Graphics::Halfblocks;
                let painted = canvas.paint(ask);
                canvas.graphics = graphics;
                fb.copy_from_slice(&painted.fb);
                let t2 = Instant::now();
                let mut rgb = Vec::with_capacity(fb.len() * 3);
                if let Some(f) = Frame::new(&mut fb, w, h) {
                    f.rgb888_into(&mut rgb);
                }
                let t3 = Instant::now();
                let image = images::rgb_image(w, h, rgb).expect("image");
                let old = canvas.pictures.encode(image, (cols, rows)).expect("encode");
                let t4 = Instant::now();
                let (palette, indices) = indexed(&fb).expect("at most 256 colours");
                let t5 = Instant::now();
                let new =
                    images::sixel_indexed(&palette, &indices, w, h, (cols, rows)).expect("sixel");
                let t6 = Instant::now();
                before = bytes_of(&|f| images::render(f, area, &old, 0, 0));
                after = bytes_of(&|f| images::render_whole(f, area, &new));
                // The render reads the view again, warm: its read is taken
                // off it.
                let read = ms(t1 - t0);
                let phases = [
                    read,
                    ms(t2 - t1) - read,
                    ms(t3 - t2),
                    ms(t4 - t3),
                    ms(t5 - t4),
                    ms(t6 - t5),
                ];
                for (s, d) in sums.iter_mut().zip(phases) {
                    *s += d;
                }
            }
            assert!(after <= before, "z{zoom}: {after} bytes against {before}");
            let [read, render, rgb, quantised, index, indexed] = sums.map(|s| s / f64::from(n));
            println!(
                "FRAME zoom={zoom} px={w}x{h} read_ms={read:.1} render_ms={render:.1} \
                 before: rgb_ms={rgb:.1} encode_ms={quantised:.1} total_ms={:.1} bytes={before} \
                 after: index_ms={index:.1} encode_ms={indexed:.1} total_ms={:.1} bytes={after}",
                read + render + rgb + quantised,
                read + render + index + indexed,
            );
        }
        if !measure_on() {
            return;
        }
        // A held arrow for two seconds at z14, at foot's default repeat of
        // 25 a second and at 50. Before: every key a frame in turn on the
        // input path, encoded by ratatui-image. After: the painter on its
        // thread, each frame asking for the newest view.
        let step = |i: u32| {
            let mut view = Viewport::new(at, 14, w, h);
            view.pan(i as i32 * (w as i32 / 8), 0);
            Ask {
                view,
                cells: (cols, rows),
                epoch: 0,
            }
        };
        for per_s in [25u32, 50] {
            let repeat = Duration::from_millis(u64::from(1000 / per_s));
            let keys = 2 * per_s;
            let start = Instant::now();
            let mut done = start;
            for i in 0..keys {
                if let Some(wait) = (start + repeat * i).checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                let graphics = canvas.graphics;
                canvas.graphics = Graphics::Halfblocks;
                let mut painted = canvas.paint(step(i));
                canvas.graphics = graphics;
                let mut rgb = Vec::with_capacity(painted.fb.len() * 3);
                if let Some(f) = Frame::new(&mut painted.fb, w, h) {
                    f.rgb888_into(&mut rgb);
                }
                let image = images::rgb_image(w, h, rgb).expect("image");
                let _ = canvas.pictures.encode(image, (cols, rows)).expect("encode");
                done = Instant::now();
            }
            let last = start + repeat * (keys - 1);
            println!(
                "HELD before per_s={per_s} keys={keys} frames={keys} frames_per_s={:.1} \
                 lag_after_release_ms={:.0}",
                f64::from(keys) / (done - start).as_secs_f64(),
                ms(done - last)
            );
            let mut threaded = MapPainter::new(open(), Graphics::Sixel, Some(picker())).threaded();
            let start = Instant::now();
            let mut frames = 0u32;
            let mut shown = None;
            let mut frame = |painter: &mut MapPainter, ask: Ask| {
                painter.paint(ask, Instant::now());
                let now = painter.painted.as_ref().map(|p| p.view);
                if now != shown {
                    shown = now;
                    frames += 1;
                }
            };
            for i in 0..keys {
                // The loop draws when the painter wakes it: a frame a
                // millisecond stands for that, at the view of the last key.
                while Instant::now() < start + repeat * i {
                    frame(&mut threaded, step(i.saturating_sub(1)));
                    std::thread::sleep(Duration::from_millis(1));
                }
                frame(&mut threaded, step(i));
            }
            let last = start + repeat * (keys - 1);
            let wanted = step(keys - 1);
            while threaded.painted.as_ref().map(|p| p.view) != Some(wanted.view) {
                std::thread::sleep(Duration::from_millis(1));
                frame(&mut threaded, wanted);
            }
            let caught = Instant::now();
            println!(
                "HELD after per_s={per_s} keys={keys} frames={frames} frames_per_s={:.1} \
                 lag_after_release_ms={:.0}",
                f64::from(frames) / (caught - start).as_secs_f64(),
                ms(caught - last)
            );
        }
    }

    /// The frame's colours become the sixel's palette in the order they
    /// first appear; past 256 there is no palette, and ratatui-image's
    /// quantiser draws the frame instead.
    #[test]
    fn a_frame_is_indexed_by_its_colours_up_to_256() {
        let (palette, indices) = indexed(&[7, 9, 7, 0]).expect("three colours");
        assert_eq!(palette.len(), 3);
        assert_eq!(indices, [0, 1, 0, 2]);
        let full: Vec<u16> = (0..256).collect();
        assert_eq!(indexed(&full).map(|(p, _)| p.len()), Some(256));
        let over: Vec<u16> = (0..257).collect();
        assert!(indexed(&over).is_none());
    }

    /// Whether `frame_cost` measures Lew's pane rather than checks the
    /// fixture.
    fn measure_on() -> bool {
        std::env::var_os("LOQUAX_FRAME_REGION").is_some()
    }

    /// A view of the outline over Europe, 30 x 8 cells of braille, `i`
    /// eighths east.
    fn ask_east(i: i32) -> Ask {
        let mut view = Viewport::new(project(LatLon::from_degrees(48.0, 10.0)), 4, 60, 32);
        view.pan(i * 60 / 8, 0);
        Ask {
            view,
            cells: (30, 8),
            epoch: 0,
        }
    }

    /// The paint of `ask` as the thread would finish it.
    fn painted(ask: Ask) -> Painted {
        Painted {
            view: ask.view,
            cells: ask.cells,
            epoch: ask.epoch,
            fb: Vec::new(),
            attribution: String::new(),
            picture: None,
        }
    }

    /// The newest view wins (1075): views asked while a paint runs replace
    /// each other, so only the last is painted next; a paint that finishes
    /// for a view the reader has moved on from is dropped, unless the
    /// picture on screen has stood for [`CATCH_UP_MS`], and then it is the
    /// nearest there is.
    #[test]
    fn a_paint_overtaken_by_a_newer_view_is_dropped() {
        let (a, b, c) = (ask_east(1), ask_east(2), ask_east(3));
        let mut slot = Slot::default();
        slot.ask(a);
        assert_eq!(slot.start(), Some(a), "the thread starts A");
        slot.ask(b);
        slot.ask(c);
        slot.finish(painted(a));
        // A finished for a view behind C, with a picture shown just now.
        let fresh = Some(Duration::from_millis(10));
        let done = slot.done.take().expect("A is done");
        let of = Ask {
            view: done.view,
            cells: done.cells,
            epoch: done.epoch,
        };
        assert!(!keep(&of, &c, fresh), "A is dropped");
        assert_eq!(slot.start(), Some(c), "B is never painted");
        assert_eq!(slot.start(), None);
        slot.finish(painted(c));
        let done = slot.done.take().expect("C is done");
        let of = Ask {
            view: done.view,
            cells: done.cells,
            epoch: done.epoch,
        };
        assert!(keep(&of, &c, fresh), "C is the view wanted");
        // Keys faster than paints: after CATCH_UP_MS the paint behind is
        // shown, and so is any first picture.
        let stood = Some(Duration::from_millis(CATCH_UP_MS));
        assert!(keep(&ask_east(1), &c, stood));
        assert!(keep(&ask_east(1), &c, None));
        // Same view, other cells: a resize is another paint.
        let mut wider = c;
        wider.cells.0 += 1;
        assert!(!keep(&c, &wider, fresh));
    }

    /// The map laid out for a `width` x `height` screen, as the model does:
    /// the picture inside the pane's border, one pin "Lew Mobile" at
    /// `(39, row)`.
    fn laid_out(cell_px: (u16, u16), width: u16, height: u16, row: u16) -> MapView {
        let area = leviculum_ui::Rect::new(1, 1, width - 2, height - 3);
        let at = project(LatLon::from_degrees(48.0, 10.0));
        MapView {
            area,
            viewport: Viewport::new(
                at,
                10,
                u32::from(area.width) * u32::from(cell_px.0),
                u32::from(area.height) * u32::from(cell_px.1),
            ),
            cell_px,
            pins: vec![PinCell {
                peer: [1; 16],
                col: 39,
                row,
                text: "● Lew Mobile · 1 min".to_string(),
                apart: None,
                stale: false,
                selected: false,
            }],
        }
    }

    fn every_backend() -> [MapPainter; 4] {
        [
            MapPainter::new(Maps::default(), Graphics::Halfblocks, None),
            MapPainter::new(Maps::default(), Graphics::Braille, None),
            MapPainter::new(Maps::default(), Graphics::Sixel, Some(Picker::halfblocks())),
            MapPainter::new(Maps::default(), Graphics::Kitty, None),
        ]
    }

    /// The terminal shrank (a phone's keyboard came up) and the frame is
    /// drawn before the model heard of it: the view is laid out for 54 x 60,
    /// the frame is 54 x 30. Nothing is written outside it (1087: "index
    /// outside of buffer ... is (39, 49)").
    #[test]
    fn a_view_laid_out_for_a_taller_screen_draws_in_a_shorter_frame() {
        for mut painter in every_backend() {
            let graphics = painter.graphics();
            let tall = laid_out(painter.setup().cell_px, 54, 60, 49);
            let mut terminal = Terminal::new(TestBackend::new(54, 60)).expect("terminal");
            terminal
                .draw(|f| painter.render(f, &tall, Style::default()))
                .expect("tall");
            terminal.backend_mut().resize(54, 30);
            terminal
                .draw(|f| painter.render(f, &tall, Style::default()))
                .expect("short");
            let area = terminal.backend().buffer().area;
            assert_eq!((area.width, area.height), (54, 30), "{graphics:?}");
        }
    }

    /// The same shrink, heard by the model, on the painter's thread: the
    /// first frame at 54 x 30 still shows the 54 x 60 paint, and with it
    /// the labels placed for that paint, one of them on row 49.
    #[test]
    fn a_paint_for_the_old_size_does_not_place_its_labels_in_the_new_one() {
        for painter in every_backend() {
            let mut painter = painter.threaded();
            let graphics = painter.graphics();
            let cell_px = painter.setup().cell_px;
            let tall = laid_out(cell_px, 54, 60, 49);
            let mut terminal = Terminal::new(TestBackend::new(54, 60)).expect("terminal");
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                terminal
                    .draw(|f| painter.render(f, &tall, Style::default()))
                    .expect("tall");
                if painter.painted_view() == Some(tall.viewport) || Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(painter.painted_view(), Some(tall.viewport), "{graphics:?}");
            terminal.backend_mut().resize(54, 30);
            let short = laid_out(cell_px, 54, 30, 20);
            terminal
                .draw(|f| painter.render(f, &short, Style::default()))
                .expect("short");
            assert_eq!(
                painter.painted_view(),
                Some(tall.viewport),
                "{graphics:?}: the old paint still up"
            );
        }
    }

    /// The thread end to end: a frame asks, the next frame after the paint
    /// finished shows it, and a frame of the same view asks nothing more.
    #[test]
    fn the_threaded_painter_shows_the_view_asked_for() {
        let mut painter = MapPainter::new(Maps::default(), Graphics::Braille, None).threaded();
        let (a, b) = (ask_east(0), ask_east(5));
        let now = Instant::now();
        painter.paint(a, now);
        let deadline = Instant::now() + Duration::from_secs(10);
        while painter.painted_view() != Some(a.view) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
            painter.paint(a, Instant::now());
        }
        assert_eq!(painter.painted_view(), Some(a.view));
        assert_eq!(painter.paints(), 1);
        // B asked a moment after A went up: B, when it comes, is the view
        // wanted and is shown however soon.
        painter.paint(b, Instant::now());
        while painter.painted_view() != Some(b.view) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
            painter.paint(b, Instant::now());
        }
        assert_eq!(painter.painted_view(), Some(b.view));
        assert_eq!(painter.paints(), 2);
    }
}
