//! The map on a pixel screen frame by frame (1134, 1135, 1144): what the
//! phone's map screen and the window share between the view a frontend
//! keeps and the RGBA it shows.
//!
//! A frontend keeps an [`ExactView`], the zoom between the steps a pinch
//! or a wheel leaves, held to one world and the regions' zooms by
//! [`hold`], and asks [`Frames::frame`] for it with the pins and whether
//! the view is moving ([`Ask`]). The frame is drawn with margins past the
//! screen, so a pan reveals drawn map while the next frame is drawn. The
//! map comes from [`TileCache`]: the tiles of the step nearest the zoom,
//! scaled to it, and while the view moves the step the last frame was
//! composed from as long as the zoom is within a step of it, with at most
//! [`MOVING_TILES_PER_WORKER`] missing tiles drawn a worker, the rest
//! shown from cached steps and owed ([`Shot::missing`]: ask again).
//!
//! The names are placed at rest, over the frame after the pins, and kept
//! as sprites; a frame while the view moves draws them where the view
//! moved them ([`moved`]), so they never leave the picture. The pins are
//! placed in every frame, and drawn in unless the frontend sets them
//! itself ([`Ask::draw_pins`]).
//!
//! A frame at rest that owes no tile leaves the view to prefetch (1135):
//! [`Frames::prefetch`] then draws one batch of [`tiles::plan`]'s tiles a
//! call, one a worker, ordered by the pan's direction ([`Motion`], fed by
//! [`Frames::pan`] and [`Frames::zoom`]), up to the plan's budget. Any
//! frame drops the plan; the next frame at rest makes a new one.
//!
//! The cache drops its tiles when the maps directory's epoch or the style
//! changes (the theme, the screen's density), so a frontend needs no
//! invalidation of its own.

use leviculum_map::labels::SUBPIXEL;
use leviculum_map::viewport::{ExactView, MILLI};
use leviculum_mapfill::style::Style;

use crate::mapnames::{self, Sprite, Typesetter};
use crate::picture::{Maps, Painter};
use crate::screenmap::{self, Placed};
use crate::tiles::{self, Frame as TileFrame, Motion, Plan, Stats, TileCache, BUDGET_MB};
use crate::ui::map::MapPin;

/// Missing tiles a frame draws while the view moves, per worker thread:
/// the rest are shown from the step before and drawn in the frames that
/// follow.
pub const MOVING_TILES_PER_WORKER: usize = 2;

/// One frame asked of [`Frames::frame`].
#[derive(Clone, Copy)]
pub struct Ask<'a> {
    /// The screen's view, held ([`hold`]).
    pub view: ExactView,
    /// Pixels drawn past the screen on the left and right, and on the top
    /// and bottom.
    pub margin: (u32, u32),
    /// A finger or a pointer moves the view: the names are kept, the
    /// tiles drawn a few a frame.
    pub moving: bool,
    pub style: Style,
    /// Screen pixels a logical one.
    pub scale: f32,
    pub pins: &'a [MapPin],
    pub selected: Option<[u8; 16]>,
    /// The zooms the regions allow, the floor the screen's width sets
    /// first: how far a prefetch goes out and in.
    pub zooms: (u8, u8),
    /// Draw the pins into the picture; a frontend that sets them itself
    /// over it asks without.
    pub draw_pins: bool,
    /// The width of a pin's label as the frontend sets it, when it sets
    /// them itself; the map's own typesetter's otherwise.
    pub measure: Option<fn(&str, f32) -> u32>,
}

/// A frame drawn.
pub struct Shot {
    /// RGBA rows, `view`'s size.
    pub rgba: Vec<u8>,
    /// The frame drawn: the screen's view, the margins around it, the
    /// same centre, on a whole pixel at a whole step.
    pub view: ExactView,
    /// The zoom step its tiles are.
    pub tile_zoom: u8,
    /// The step nearest the view's zoom: the data's zoom.
    pub rest_zoom: u8,
    /// Tiles it needed and did not draw: ask again.
    pub missing: usize,
    /// The credit of the regions drawn, the outline's when none.
    pub attribution: String,
    /// The maps directory's epoch it was drawn at.
    pub epoch: u64,
}

/// One prefetch call's work.
#[derive(Clone, Copy, Debug, Default)]
pub struct Prefetched {
    /// Tiles drawn.
    pub drawn: usize,
    /// More is to draw.
    pub more: bool,
    /// The frame at rest the plan was made for, on the call that made it:
    /// what a frontend asks the autoload past.
    pub planned: Option<TileFrame>,
}

/// The frames of one maps directory, kept from frame to frame: the tile
/// cache, the painter the names are looked up in, the typesetter, the
/// names the last frame at rest placed, the motion, the prefetch's plan.
pub struct Frames {
    painter: Painter,
    type_: Typesetter,
    /// The map without names and pins, in tiles (1134).
    tiles: TileCache,
    /// The zoom step the last frame was composed from.
    last_zoom: Option<u8>,
    /// The names the last frame at rest placed.
    names: Vec<Sprite>,
    /// The frame they were placed in.
    names_view: Option<ExactView>,
    /// The pan's velocity and the last zoom's way (1135).
    motion: Motion,
    /// The last frame, when it was at rest and owed no tile, with the
    /// zooms the regions allow.
    rest: Option<(TileFrame, (u8, u8))>,
    /// What the prefetch draws for that frame, made on its first call.
    plan: Option<Plan>,
    /// The pins the last frame placed, in its pixels.
    placed: Vec<Placed>,
}

impl Frames {
    /// The frames over `maps`, the cache within [`BUDGET_MB`].
    pub fn new(maps: Maps) -> Self {
        Self {
            painter: Painter::new(maps.clone()),
            type_: Typesetter::new(),
            tiles: TileCache::new(maps, BUDGET_MB << 20),
            last_zoom: None,
            names: Vec::new(),
            names_view: None,
            motion: Motion::default(),
            rest: None,
            plan: None,
            placed: Vec::new(),
        }
    }

    /// The painter the names are looked up in: the regions a fit counts.
    pub fn painter(&self) -> &Painter {
        &self.painter
    }

    /// Read the regions again when the directory changed; whether it did.
    pub fn refresh(&mut self) -> bool {
        self.painter.refresh()
    }

    /// The view was panned by `(dx, dy)` screen pixels at `now_ms`.
    pub fn pan(&mut self, dx: f64, dy: f64, now_ms: u64) {
        self.motion.pan(dx, dy, now_ms);
    }

    /// The view was zoomed by `by_milli` thousandths of a step.
    pub fn zoom(&mut self, by_milli: i64) {
        self.motion.zoom(by_milli);
    }

    /// One frame for `ask`.
    pub fn frame(&mut self, ask: &Ask<'_>) -> Shot {
        self.rest = None;
        self.plan = None;
        self.painter.refresh();
        let view = ask.view;
        let rest_zoom = view.tile_zoom();
        // At a whole step the frame's centre sits on a whole pixel of it:
        // the tiles are that step's pixels shown one to one, so the pins
        // and the names land on the map as a view drawn whole puts them.
        // Not held to one world itself, so a margin past the world's edge
        // stays empty rather than moving the view.
        let (sw, sh) = view.size();
        let (mx, my) = ask.margin;
        let mut drawn = view.on_whole_pixel();
        drawn.resize(
            sw.saturating_add(mx.saturating_mul(2)),
            sh.saturating_add(my.saturating_mul(2)),
        );
        // While the view moves the frame keeps the step it was composed
        // from as long as the zoom is within a step of it, the tiles
        // scaled, and draws a few missing tiles a frame.
        let kept = self
            .last_zoom
            .filter(|&z| ask.moving && (view.zoom() - f64::from(z)).abs() <= 1.0);
        let tile_zoom = kept.unwrap_or(rest_zoom);
        let asked = TileFrame {
            view: drawn,
            zoom: tile_zoom,
            style: ask.style,
            scale: ask.scale,
            render_limit: ask
                .moving
                .then(|| self.tiles.workers() * MOVING_TILES_PER_WORKER),
            fallback: self.last_zoom,
        };
        let composed = self.tiles.compose(&asked);
        self.last_zoom = Some(tile_zoom);
        if !ask.moving && composed.missing == 0 {
            self.rest = Some((asked, ask.zooms));
        }
        let mut rgba = composed.rgba;
        let (w, h) = drawn.size();
        let (scale, theme) = (ask.scale, ask.style.theme);
        let type_ = &mut self.type_;
        let placed = match ask.measure {
            Some(measure) => screenmap::place_exact(ask.pins, &drawn, ask.selected, scale, measure),
            None => screenmap::place_exact(ask.pins, &drawn, ask.selected, scale, |t, s| {
                type_.width(t, s)
            }),
        };
        if !ask.moving {
            // At rest the names are placed anew over the frame, at the
            // exact scale, after the pins, and kept in world terms.
            let base = drawn.at_zoom(rest_zoom);
            let labels = self.painter.labels(&base, true);
            let (bw, bh) = base.size();
            let s = drawn.scale_at(rest_zoom);
            let sub = SUBPIXEL as f64;
            let px = |v: (i64, i64)| {
                (
                    (v.0 as f64 / sub - f64::from(bw) / 2.0) * s + f64::from(w) / 2.0,
                    (v.1 as f64 / sub - f64::from(bh) / 2.0) * s + f64::from(h) / 2.0,
                )
            };
            let taken = screenmap::taken(&placed, scale);
            let names = mapnames::place(w, h, &labels, px, &taken, scale, &mut self.type_);
            self.names = names.iter().map(Sprite::of).collect();
            self.names_view = Some(drawn);
        }
        if let Some(rest) = self.names_view {
            for name in &self.names {
                let (x, y) = moved(&rest, &drawn, name.centre);
                let (dx, dy) = (x - name.centre.0, y - name.centre.1);
                name.draw(&mut rgba, w, h, theme, dx.round() as i64, dy.round() as i64);
            }
        }
        if ask.draw_pins {
            screenmap::draw_pins(&mut rgba, w, h, &placed, scale, theme, &mut self.type_);
        }
        self.placed = placed;
        Shot {
            rgba,
            view: drawn,
            tile_zoom,
            rest_zoom,
            missing: composed.missing,
            attribution: composed.attribution,
            epoch: self.painter.epoch(),
        }
    }

    /// Whether the last frame was at rest and owed nothing: a prefetch
    /// has something to draw around.
    pub fn at_rest(&self) -> bool {
        self.rest.is_some()
    }

    /// One batch of the prefetch around the last frame at rest (1135):
    /// on the first call the plan, ordered by the pan's direction at
    /// `now_ms`; then a tile a worker.
    pub fn prefetch(&mut self, now_ms: u64) -> Prefetched {
        let Some((frame, zooms)) = self.rest else {
            return Prefetched::default();
        };
        let mut planned = None;
        if self.plan.is_none() {
            let heading = self.motion.heading(now_ms);
            self.plan = Some(tiles::plan(&frame, heading, self.motion.zoomed_out, zooms));
            planned = Some(frame);
        }
        let workers = self.tiles.workers();
        let (drawn, more) = match self.plan.as_mut() {
            Some(plan) => (self.tiles.prefetch(plan, workers), !plan.done()),
            None => (0, false),
        };
        Prefetched {
            drawn,
            more,
            planned,
        }
    }

    /// The pins the last frame placed, in its pixels, margins included.
    pub fn placed(&self) -> &[Placed] {
        &self.placed
    }

    /// The names the last frame at rest placed, as text.
    pub fn names(&self) -> Vec<String> {
        self.names.iter().map(|n| n.text.clone()).collect()
    }

    /// What the tile cache did.
    pub fn tile_stats(&self) -> Stats {
        self.tiles.stats
    }

    /// The tile cache's tiles and bytes.
    pub fn tile_memory(&self) -> (usize, usize) {
        (self.tiles.len(), self.tiles.bytes())
    }

    /// The threads the cache draws on.
    pub fn workers(&self) -> usize {
        self.tiles.workers()
    }

    /// Draw the tiles on `n` threads at most: a measuring tool's one-core
    /// row.
    pub fn set_workers(&mut self, n: usize) {
        self.tiles.set_workers(n);
    }

    /// Drop every tile: a measuring tool's cold frame.
    pub fn clear_tiles(&mut self) {
        self.tiles.clear();
    }
}

/// `view` as every frontend shows it: one world at most, the zoom between
/// `floor` and `ceiling` (the regions' deepest, never above the floor's
/// clamp) about the centre.
pub fn hold(view: ExactView, floor: u8, ceiling: u8) -> ExactView {
    let (floor, ceiling) = (u32::from(floor) * MILLI, u32::from(ceiling) * MILLI);
    let mut v = view.one_world();
    let z = v.zoom_milli().clamp(floor, ceiling.max(floor));
    let (cx, cy) = (f64::from(v.size().0) / 2.0, f64::from(v.size().1) / 2.0);
    v.zoom_about(z, cx, cy);
    v.one_world()
}

/// Where the point `c` of the frame `rest` is in the frame `now`: the
/// pan and the zoom between them. A pan of whole pixels at the same zoom
/// moves it by whole pixels, so a name drawn again is the same pixels.
pub fn moved(rest: &ExactView, now: &ExactView, c: (f64, f64)) -> (f64, f64) {
    let k = now.scale_at(0) / rest.scale_at(0);
    let (rw, rh) = rest.size();
    let (nw, nh) = now.size();
    let (cx, cy) = now.to_screen(rest.centre);
    let (mut dx, mut dy) = (cx - f64::from(nw) / 2.0, cy - f64::from(nh) / 2.0);
    if rest.zoom_milli() == now.zoom_milli() {
        let whole = |v: f64| match (v - v.round()).abs() < 1e-3 {
            true => v.round(),
            false => v,
        };
        (dx, dy) = (whole(dx), whole(dy));
    }
    (
        (c.0 - f64::from(rw) / 2.0) * k + f64::from(nw) / 2.0 + dx,
        (c.1 - f64::from(rh) / 2.0) * k + f64::from(nh) / 2.0 + dy,
    )
}
