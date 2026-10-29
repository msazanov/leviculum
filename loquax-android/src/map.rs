//! The map screen's one call (1128): Java hosts a bitmap, Rust draws it.
//!
//! The frame is loquax's own map on a pixel screen
//! ([`loquax::screenmap::render`]): the regions of the app's maps
//! directory and the world outline under them ([`loquax::picture`]), the
//! map's names set in Fira Sans ([`loquax::mapnames`]), the pins placed by
//! the core's rule ([`leviculum_map::pins`]) and drawn in, in the map's
//! dark table when the phone is dark, or in the table the map screen's
//! own switch chose (1133, the config's `map_theme`). The window draws through the same
//! functions, so the phone shows the map the desktop shows.
//!
//! Java keeps the last frame's view and hands it back with what the finger
//! did since: a pan in pixels, zoom steps about a point, a tap. The view's
//! rules are the model's (`loquax::ui::map`: [`fit`], [`fit_at`],
//! [`zoom_at`], one world at most), one pixel a cell as in the window. The
//! pins are the running session's, or the store's when none runs
//! ([`pins_in`]).
//!
//! A frame is drawn with margins past the screen (1131), so a finger that
//! pans reveals drawn map while the next frame is drawn, and without the
//! map's names while a finger is on it, so frames come as fast as the
//! renderer goes; Java asks again as the finger moves and shows each frame
//! as it lands, moved by what the finger did since it asked.
//!
//! The autoload (1089) is the terminal's and the window's: the places of
//! the first pins when they first fit, a report's place when it opens the
//! map, the view on "load this area", one load at a time into the maps
//! directory under the config's `map_source` and `map_cache_mb`. A frame
//! says when a load runs, and Java asks again until it landed.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use leviculum_map::mercator::{project, LatLon, World};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::style::{MapTheme, Style};
use leviculum_ui::theme::{Theme, ThemeFlag};
use loquax::mapload::{Loader, Source, Step, CACHE_DEFAULT_MB};
use loquax::mapnames::Typesetter;
use loquax::picture::{map_theme, Maps, Painter};
use loquax::screenmap::{self, Placed};
use loquax::ui::map::{
    fit, fit_at, settle, viewport, want_pins, want_place, want_view, zoom_at, zoom_floor, MapPin,
    MapState, MapWant,
};
use loquax::when::WallTime;

use crate::schema::{MapRequest, MapShown, VERSION};
use crate::session::pins_in;

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
    fitted_at: Option<(World, u8)>,
    /// Whether a fit with pins asked for their places already.
    pins_asked: bool,
    placed: Vec<Placed>,
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
        let selected = request
            .selected
            .as_deref()
            .and_then(|hex| loquax::address::parse(hex).ok());
        let mut state = MapState {
            centre: World {
                x: request.x.unwrap_or(0),
                y: request.y.unwrap_or(0),
            },
            zoom: request.zoom.unwrap_or(setup.zooms.0),
            selected,
        };
        let at = match (request.at_lat_e6, request.at_lon_e6) {
            (Some(lat), Some(lon)) => Some(project(LatLon::from_e6(lat, lon))),
            _ => None,
        };
        let first = request.x.is_none() || request.y.is_none() || request.zoom.is_none();
        let refit = landed && self.fitted_at == Some((state.centre, state.zoom));
        if let Some(at) = at {
            (state.centre, state.zoom) = fit_at(at, area, &setup);
            self.fitted_at = None;
            if let Some(want) = want_place(at, &setup.regions) {
                self.ask(want, &mut notice);
            }
        } else if first || request.fit || refit {
            (state.centre, state.zoom) = fit(pins, area, &setup);
            self.fitted_at = Some((state.centre, state.zoom));
            if state.selected.is_none() {
                state.selected = pins.first().map(|p| p.peer);
            }
            if !self.pins_asked && !pins.is_empty() {
                self.pins_asked = true;
                for want in want_pins(pins, &setup.regions) {
                    self.ask(want, &mut notice);
                }
            }
        }
        if request.pan_x != 0 || request.pan_y != 0 {
            let mut view = viewport(&state, area, &setup);
            view.pan(request.pan_x, request.pan_y);
            state.centre = view.centre;
            settle(&mut state, area, &setup);
        }
        if request.zoom_by != 0 {
            let zoom = (i32::from(state.zoom) + request.zoom_by)
                .clamp(i32::from(setup.zooms.0), i32::from(setup.zooms.1));
            let clamp = |v: i32| u16::try_from(v.max(0)).unwrap_or(u16::MAX);
            zoom_at(
                &mut state,
                area,
                &setup,
                clamp(request.about_x),
                clamp(request.about_y),
                u8::try_from(zoom).unwrap_or(setup.zooms.0),
            );
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
                state.selected = tapped;
            }
        }
        let view = viewport(&state, area, &setup);
        if request.load {
            match want_view(&view, &setup.regions) {
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
        let (sw, sh) = view.size();
        let drawn = Viewport::new(
            view.centre,
            view.zoom(),
            sw.saturating_add(mx.saturating_mul(2)),
            sh.saturating_add(my.saturating_mul(2)),
        );
        let rendered = screenmap::render(
            &mut self.painter,
            &mut self.type_,
            &drawn,
            style,
            scale,
            pins,
            state.selected,
            !request.moving,
        );
        self.placed = rendered.placed;
        let name = |peer: [u8; 16]| pins.iter().find(|p| p.peer == peer).map(|p| p.words());
        let (w, h) = drawn.size();
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
            zoom: view.zoom(),
            min_zoom: zoom_floor(area, &setup),
            max_zoom: setup.zooms.1,
            selected: state.selected.map(|p| loquax::address::to_hex(&p)),
            attribution: rendered.picture.attribution.clone(),
            pins: u32::try_from(self.placed.len()).unwrap_or(u32::MAX),
            tapped: tapped.map(|p| loquax::address::to_hex(&p)),
            tapped_name: tapped.and_then(name),
            loading: self.loader.busy(),
            notice,
            ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            dark: table == MapTheme::Dark,
            theme_chosen: matches!(self.theme, Some(ThemeFlag::Light | ThemeFlag::Dark)),
        };
        (shown, rendered.picture.rgba)
    }
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

/// A frame that says why there is no picture.
pub fn failed(error: impl Into<String>) -> MapShown {
    MapShown {
        v: VERSION,
        ok: false,
        error: Some(error.into()),
        ..MapShown::default()
    }
}
