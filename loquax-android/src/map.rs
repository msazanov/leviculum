//! The map screen's one call (1128): Java hosts a bitmap, Rust draws it.
//!
//! The frame is loquax's own map on a pixel screen: the regions of the
//! app's maps directory and the world outline under them
//! ([`loquax::picture`]), composed from loquax's tile cache
//! ([`loquax::tiles`]), the map's names set in Fira Sans
//! ([`loquax::mapnames`]), the pins placed by the core's rule
//! ([`leviculum_map::pins`]) and drawn in ([`loquax::screenmap`]), in the
//! map's dark table when the phone is dark, or in the table the map
//! screen's own switch chose (1133, the config's `map_theme`). The window
//! draws through the same functions, so the phone shows the map the
//! desktop shows.
//!
//! Java keeps the last frame's view and hands it back with what the finger
//! did since: a pan in pixels, a zoom in thousandths of a step about a
//! point, a tap. The view keeps the zoom a pinch leaves, between the steps
//! ([`ExactView`], 1134); fitting is the model's (`loquax::ui::map`:
//! [`fit`], [`fit_at`], one world at most), one pixel a cell as in the
//! window. The pins are the running session's, or the store's when none
//! runs ([`pins_in`]).
//!
//! A frame is drawn with margins past the screen (1131), so a finger that
//! pans reveals drawn map while the next frame is drawn; Java asks again
//! as the finger moves and shows each frame as it lands, moved by what the
//! finger did since it asked. The map comes from the tiles of the step
//! nearest the zoom, scaled to it; the names are placed at rest and kept,
//! and a frame while the finger moves draws them where the view moved
//! them, so they never leave the picture (1134). While a pinch stays
//! within a step of the tiles it started on, it keeps them, and a frame
//! draws at most [`MOVING_TILES_PER_WORKER`] missing tiles a worker,
//! showing the step before where the rest are owed.
//!
//! The autoload (1089) is the terminal's and the window's: the places of
//! the first pins when they first fit, a report's place when it opens the
//! map, the view on "load this area", one load at a time into the maps
//! directory under the config's `map_source` and `map_cache_mb`. A frame
//! says when a load runs, and Java asks again until it landed.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use leviculum_map::labels::SUBPIXEL;
use leviculum_map::mercator::{project, LatLon, World};
use leviculum_map::viewport::{ExactView, MILLI};
use leviculum_mapfill::style::{MapTheme, Style};
use leviculum_ui::theme::{Theme, ThemeFlag};
use loquax::mapload::{Loader, Source, Step, CACHE_DEFAULT_MB};
use loquax::mapnames::{self, Sprite, Typesetter};
use loquax::picture::{map_theme, Maps, Painter};
use loquax::screenmap::{self, Placed};
use loquax::tiles::{Frame as TileFrame, Stats, TileCache, BUDGET_MB};
use loquax::ui::map::{fit, fit_at, want_pins, want_place, want_view, zoom_floor, MapPin, MapWant};
use loquax::when::WallTime;

use crate::schema::{MapRequest, MapShown, VERSION};
use crate::session::pins_in;

/// Missing tiles a frame draws while the finger moves, per worker
/// thread: the rest are shown from the step before and drawn in the
/// frames that follow.
pub const MOVING_TILES_PER_WORKER: usize = 2;

/// What the map screen says on "load this area" over a view the regions
/// hold already: the terminal's words.
pub const LOADED_ALREADY: &str = "map: this area is loaded already";

/// The renderer of one app directory, kept from frame to frame: the
/// painter with its regions, the typesetter, the loader, and what the
/// last frame placed.
pub struct Renderer {
    dir: PathBuf,
    painter: Painter,
    type_: Typesetter,
    loader: Loader,
    autoload: bool,
    theme: Option<ThemeFlag>,
    origin: Instant,
    /// The view the last fit chose: a region that lands fits again while
    /// the view is still there.
    fitted_at: Option<(World, u32)>,
    /// Whether a fit with pins asked for their places already.
    pins_asked: bool,
    placed: Vec<Placed>,
    /// The map without names and pins, in tiles (1134).
    tiles: TileCache,
    /// The zoom step the last frame was composed from.
    last_zoom: Option<u8>,
    /// The names the last frame at rest placed: a frame while the finger
    /// moves draws these where the view moved them (1134).
    names: Vec<Sprite>,
    /// The frame they were placed in.
    names_view: Option<ExactView>,
}

impl Renderer {
    /// A renderer over `dir`: its `maps/`, its config.
    pub fn new(dir: &Path) -> Self {
        let config = loquax::config::load(dir).unwrap_or_default();
        let maps = Maps::new(Some(dir.join("maps")));
        let source = Source::of(config.map_source.as_deref());
        let cache = config.map_cache_mb.unwrap_or(CACHE_DEFAULT_MB) << 20;
        Self {
            dir: dir.to_path_buf(),
            painter: Painter::new(maps.clone()),
            tiles: TileCache::new(maps.clone(), BUDGET_MB << 20),
            last_zoom: None,
            names: Vec::new(),
            names_view: None,
            type_: Typesetter::new(),
            loader: Loader::new(maps, source, cache),
            autoload: config.map_autoload.unwrap_or(true),
            theme: config.map_theme,
            origin: Instant::now(),
            fitted_at: None,
            pins_asked: false,
            placed: Vec::new(),
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Ask for `want` when the autoload is on; a line for the user when it
    /// failed at once.
    fn ask(&mut self, want: MapWant, notice: &mut Option<String>) {
        if !self.autoload {
            return;
        }
        let now = self.now_ms();
        let steps = self.loader.ask(want, now);
        self.steps(steps, notice);
    }

    /// The loader's steps: a landed region is read before the frame is
    /// drawn, a failure's line is the frame's notice. Whether a region
    /// landed.
    fn steps(&mut self, steps: Vec<Step>, notice: &mut Option<String>) -> bool {
        let mut landed = false;
        for step in steps {
            match step {
                Step::Done {
                    want,
                    landed: l,
                    elapsed_ms,
                } => {
                    tracing::info!(
                        "MAPLOAD done zooms={}-{} bytes={} file={} elapsed_ms={elapsed_ms} reason={}",
                        l.zooms.0,
                        l.zooms.1,
                        l.bytes,
                        l.name,
                        want.reason.word()
                    );
                    landed = true;
                }
                Step::Failed { detail, toast, .. } => {
                    tracing::info!("MAPLOAD failed detail={detail:?}");
                    if toast.is_some() {
                        *notice = toast;
                    }
                }
                Step::Started(want) => {
                    tracing::info!("MAPLOAD start reason={}", want.reason.word());
                }
                Step::Skipped { why, .. } => tracing::info!("MAPLOAD skipped why={why}"),
            }
        }
        if landed {
            self.painter.refresh();
        }
        landed
    }

    /// The map's own light or dark choice (1133): a toggle stores the
    /// table other than the one the map shows over `screen`, "follow the
    /// phone" stores `auto`; both in the config, so the choice outlives
    /// the app. A write that failed keeps the old choice and says so.
    fn choose_theme(&mut self, request: &MapRequest, screen: Theme, notice: &mut Option<String>) {
        let next = if request.follow_phone {
            ThemeFlag::Auto
        } else if request.toggle_theme {
            match map_theme(self.theme.or(Some(ThemeFlag::Auto)), Some(screen)) {
                MapTheme::Dark => ThemeFlag::Light,
                MapTheme::Light => ThemeFlag::Dark,
            }
        } else {
            return;
        };
        let word = match next {
            ThemeFlag::Auto => "auto",
            ThemeFlag::Light => "light",
            ThemeFlag::Dark => "dark",
        };
        match loquax::config::save(&self.dir, "map_theme", word) {
            Ok(()) => self.theme = Some(next),
            Err(error) => *notice = Some(format!("map: the choice was not stored: {error}")),
        }
    }

    /// One frame for `request` over `pins`.
    pub fn frame(&mut self, request: &MapRequest, pins: &[MapPin]) -> (MapShown, Vec<u8>) {
        let started = Instant::now();
        let mut notice = None;
        let now = self.now_ms();
        let polled = self.loader.poll(now);
        let landed = self.steps(polled, &mut notice);
        let setup = screenmap::setup(&self.painter);
        let area = screenmap::area((request.w.max(1), request.h.max(1)));
        let scale = (request.density_milli.max(100) as f32) / 1000.0;
        let mut selected = request
            .selected
            .as_deref()
            .and_then(|hex| loquax::address::parse(hex).ok());
        let floor = u32::from(zoom_floor(area, &setup)) * MILLI;
        let ceiling = u32::from(setup.zooms.1) * MILLI;
        let asked = request
            .zoom_milli
            .or(request.zoom.map(|z| u32::from(z) * MILLI));
        let mut view = ExactView::new(
            World {
                x: request.x.unwrap_or(0),
                y: request.y.unwrap_or(0),
            },
            asked.unwrap_or(u32::from(setup.zooms.0) * MILLI),
            u32::from(area.width),
            u32::from(area.height),
        );
        let at = match (request.at_lat_e6, request.at_lon_e6) {
            (Some(lat), Some(lon)) => Some(project(LatLon::from_e6(lat, lon))),
            _ => None,
        };
        let first = request.x.is_none() || request.y.is_none() || asked.is_none();
        let fitted = |view: &ExactView| (view.centre, view.zoom_milli());
        let refit = landed && self.fitted_at == Some(fitted(&view));
        let fit_to = |view: &mut ExactView, (centre, zoom): (World, u8)| {
            *view = ExactView::new(
                centre,
                u32::from(zoom) * MILLI,
                view.size().0,
                view.size().1,
            );
        };
        if let Some(at) = at {
            fit_to(&mut view, fit_at(at, area, &setup));
            self.fitted_at = None;
            if let Some(want) = want_place(at, &setup.regions) {
                self.ask(want, &mut notice);
            }
        } else if first || request.fit || refit {
            fit_to(&mut view, fit(pins, area, &setup));
            self.fitted_at = Some(fitted(&view));
            if selected.is_none() {
                selected = pins.first().map(|p| p.peer);
            }
            if !self.pins_asked && !pins.is_empty() {
                self.pins_asked = true;
                for want in want_pins(pins, &setup.regions) {
                    self.ask(want, &mut notice);
                }
            }
        }
        let held = |v: ExactView| {
            let mut v = v.one_world();
            let z = v.zoom_milli().clamp(floor, ceiling.max(floor));
            let (cx, cy) = (f64::from(v.size().0) / 2.0, f64::from(v.size().1) / 2.0);
            v.zoom_about(z, cx, cy);
            v.one_world()
        };
        view = held(view);
        if request.pan_x != 0 || request.pan_y != 0 {
            view.pan(request.pan_x, request.pan_y);
            view = held(view);
        }
        let by = i64::from(request.zoom_by) * i64::from(MILLI) + i64::from(request.zoom_by_milli);
        if by != 0 {
            let zoom = (i64::from(view.zoom_milli()) + by)
                .clamp(i64::from(floor), i64::from(ceiling.max(floor)));
            let about = |v: i32, of: u16| f64::from(v.clamp(0, i32::from(of)));
            view.zoom_about(
                u32::try_from(zoom).unwrap_or(floor),
                about(request.about_x, area.width),
                about(request.about_y, area.height),
            );
            view = held(view);
        }
        // At most a screen on each side: a frame of nine screens is as
        // far as any gesture between two frames reaches.
        let (mx, my) = (
            request.margin_x.min(area.width.into()),
            request.margin_y.min(area.height.into()),
        );
        let mut tapped = None;
        if let (Some(x), Some(y)) = (request.tap_x, request.tap_y) {
            // The pins were placed in the last frame, margins included.
            let (x, y) = (x as f32 + mx as f32, y as f32 + my as f32);
            tapped = screenmap::hit(&self.placed, x, y, scale);
            if tapped.is_some() {
                selected = tapped;
            }
        }
        let rest_zoom = view.tile_zoom();
        if request.load {
            match want_view(&view.at_zoom(rest_zoom), &setup.regions) {
                Some(want) => self.ask(want, &mut notice),
                None => notice = Some(LOADED_ALREADY.to_string()),
            }
        }
        let screen = match request.dark {
            true => Theme::Dark,
            false => Theme::Light,
        };
        self.choose_theme(request, screen, &mut notice);
        // Unset follows the phone; the config's `map_theme` still wins.
        let flag = self.theme.or(Some(ThemeFlag::Auto));
        let table = map_theme(flag, Some(screen));
        let style = Style::PC.in_theme(table);
        // The screen's view with the margins drawn around it, the same
        // centre: not held to one world itself, so a margin past the
        // world's edge stays empty rather than moving the view.
        // At a whole step the frame's centre sits on a whole pixel of it:
        // the tiles are that step's pixels shown one to one, so the pins
        // and the names land on the map as a view drawn whole puts them.
        // The view itself keeps its centre.
        let (sw, sh) = view.size();
        let mut drawn = view.on_whole_pixel();
        drawn.resize(
            sw.saturating_add(mx.saturating_mul(2)),
            sh.saturating_add(my.saturating_mul(2)),
        );
        // While a finger moves the frame keeps the step it was composed
        // from as long as the pinch is within a step of it, the tiles
        // scaled, and draws a few missing tiles a frame, the rest shown
        // from the step before until they land.
        let kept = self
            .last_zoom
            .filter(|&z| request.moving && (view.zoom() - f64::from(z)).abs() <= 1.0);
        let tile_zoom = kept.unwrap_or(rest_zoom);
        let composed = self.tiles.compose(&TileFrame {
            view: drawn,
            zoom: tile_zoom,
            style,
            scale,
            render_limit: request
                .moving
                .then(|| self.tiles.workers() * MOVING_TILES_PER_WORKER),
            fallback: self.last_zoom,
        });
        self.last_zoom = Some(tile_zoom);
        let mut rgba = composed.rgba;
        let (w, h) = drawn.size();
        let placed = screenmap::place_exact(pins, &drawn, selected, scale, |text, size| {
            self.type_.width(text, size)
        });
        if !request.moving {
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
                name.draw(
                    &mut rgba,
                    w,
                    h,
                    style.theme,
                    dx.round() as i64,
                    dy.round() as i64,
                );
            }
        }
        screenmap::draw_pins(
            &mut rgba,
            w,
            h,
            &placed,
            scale,
            style.theme,
            &mut self.type_,
        );
        self.placed = placed;
        let name = |peer: [u8; 16]| pins.iter().find(|p| p.peer == peer).map(|p| p.words());
        let shown = MapShown {
            v: VERSION,
            ok: true,
            error: None,
            w,
            h,
            margin_x: mx,
            margin_y: my,
            x: view.centre.x,
            y: view.centre.y,
            zoom: rest_zoom,
            zoom_milli: view.zoom_milli(),
            tile_zoom,
            pending: composed.missing > 0,
            min_zoom: zoom_floor(area, &setup),
            max_zoom: setup.zooms.1,
            selected: selected.map(|p| loquax::address::to_hex(&p)),
            attribution: composed.attribution,
            pins: u32::try_from(self.placed.len()).unwrap_or(u32::MAX),
            tapped: tapped.map(|p| loquax::address::to_hex(&p)),
            tapped_name: tapped.and_then(name),
            loading: self.loader.busy(),
            notice,
            ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            dark: table == MapTheme::Dark,
            theme_chosen: matches!(self.theme, Some(ThemeFlag::Light | ThemeFlag::Dark)),
        };
        (shown, rgba)
    }

    /// What the tile cache did, for a test and the measuring tool.
    pub fn tile_stats(&self) -> Stats {
        self.tiles.stats
    }

    /// The tile cache's tiles and bytes.
    pub fn tile_memory(&self) -> (usize, usize) {
        (self.tiles.len(), self.tiles.bytes())
    }

    /// The names the last frame at rest placed, as text.
    pub fn names(&self) -> Vec<String> {
        self.names.iter().map(|n| n.text.clone()).collect()
    }
}

/// Where the point `c` of the frame `rest` is in the frame `now`: the
/// pan and the zoom between them. A pan of whole pixels at the same zoom
/// moves it by whole pixels, so a name drawn again is the same pixels.
fn moved(rest: &ExactView, now: &ExactView, c: (f64, f64)) -> (f64, f64) {
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

/// The process's renderers, one a directory: the app has one; a test
/// process one a test.
static RENDERERS: Mutex<Vec<Renderer>> = Mutex::new(Vec::new());

/// One map frame for `request`, as [`crate::schema::frame`] lays it out.
/// The pins are `pins`' when given, the running session's, or the store's
/// in the request's directory.
pub fn render(request: &MapRequest, pins: Option<Vec<MapPin>>, now: WallTime) -> Vec<u8> {
    let dir = PathBuf::from(&request.dir);
    let pins = match pins {
        Some(pins) => Ok(pins),
        None => pins_in(&dir, now),
    };
    let pins = match pins {
        Ok(pins) => pins,
        Err(error) => {
            tracing::info!("map: no pins: {error}");
            Vec::new()
        }
    };
    let mut renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let at = match renderers.iter().position(|r| r.dir == dir) {
        Some(at) => at,
        None => {
            renderers.push(Renderer::new(&dir));
            renderers.len() - 1
        }
    };
    let renderer = &mut renderers[at];
    let (shown, rgba) = renderer.frame(request, &pins);
    crate::schema::frame(&shown, &rgba)
}

/// What the tile cache of the renderer over `dir` did, when one runs:
/// for a test that a pin costs no tile.
pub fn tile_stats(dir: &Path) -> Option<Stats> {
    let renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    renderers
        .iter()
        .find(|r| r.dir == dir)
        .map(Renderer::tile_stats)
}

/// The tiles and bytes the tile cache of the renderer over `dir` holds.
pub fn tile_memory(dir: &Path) -> Option<(usize, usize)> {
    let renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    renderers
        .iter()
        .find(|r| r.dir == dir)
        .map(Renderer::tile_memory)
}

/// Draw the tiles of the renderer over `dir` on `n` threads at most, and
/// with `clear` drop the tiles it holds: the measuring tool's one-core
/// row and its cold frames.
pub fn tune_tiles(dir: &Path, n: Option<usize>, clear: bool) {
    let mut renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(r) = renderers.iter_mut().find(|r| r.dir == dir) {
        if let Some(n) = n {
            r.tiles.set_workers(n);
        }
        if clear {
            r.tiles.clear();
        }
    }
}

/// A frame that says why there is no picture.
pub fn failed(error: impl Into<String>) -> MapShown {
    MapShown {
        v: VERSION,
        ok: false,
        error: Some(error.into()),
        ..MapShown::default()
    }
}
