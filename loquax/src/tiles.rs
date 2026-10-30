//! The map's tile cache (1134, `docs/drafts/map-renderer-roadmap-2026-10.md`,
//! section 5): the map without its names and pins drawn in square tiles of
//! one zoom step's pixel grid, kept in memory by last use, and a screen
//! composed from them at any zoom between the steps.
//!
//! A tile is [`TILE`] pixels of zoom `z` on a side, tile `(x, y)` the
//! pixels `x * TILE ..` and `y * TILE ..` of the world at that zoom, drawn
//! by [`Painter::draw`] as a view of its own. A view of the world is the
//! same pixels however it is cut, so tiles side by side make the picture
//! one view makes, seam for seam. The names are not baked in: they are
//! placed across the whole view in world terms and drawn over the tiles
//! (`crate::mapnames::paint_placed`), as the pins are, so a pin that
//! moves costs no tile and a name never breaks at a seam.
//!
//! A frame takes the tiles its view needs from the cache and draws the
//! missing ones on worker threads, one [`Painter`] each, as many as the
//! host has cores up to [`WORKERS`]. A frame may be told to draw only a
//! few ([`Frame::render_limit`]): the ones it leaves are taken from
//! another zoom's tiles that are cached, scaled, so a pinch that crosses
//! a step keeps showing the map until the new step's tiles land.
//!
//! The cache is dropped when the maps directory changes (its epoch) and
//! when the style does (the light or dark table, the line widths of the
//! screen's density). [`BUDGET_MB`] bounds it; a prefetched tile no frame
//! used yet goes first, then the least recently used.
//!
//! A view at rest prefetches (1135): [`plan`] lists the ring of tiles
//! around the frame, then the next zoom in over its central part, the
//! zoom out after a pinch that went out, then a second ring, each ordered
//! by the last pan's direction ([`Motion`]), and
//! [`TileCache::prefetch`] draws them a batch a call, within
//! [`PREFETCH_BUDGET`] tiles a plan, never evicting a tile the last frame
//! showed nor one prefetched since.

use std::collections::HashMap;
use std::string::{String, ToString};
use std::sync::Arc;
use std::vec::Vec;

use leviculum_map::draw::Ink;
use leviculum_map::mercator::World;
use leviculum_map::outline;
use leviculum_map::viewport::{ExactView, Viewport, MAX_VIEW_ZOOM};
use leviculum_mapfill::frame::rgb888;
use leviculum_mapfill::style::Style;

use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::pixelcolor::Rgb565;

use crate::picture::{Maps, Painter};

/// A tile's side in pixels of its zoom step.
pub const TILE: u32 = 256;
/// The cache's bound: 384 tiles of 256 KiB, a 1080 x 2400 screen with a
/// quarter screen of margin three times over.
pub const BUDGET_MB: usize = 96;
/// The most worker threads a frame draws its missing tiles on.
pub const WORKERS: usize = 8;
/// A tile's bytes.
pub const TILE_BYTES: usize = (TILE * TILE * 4) as usize;
/// Tiles one view at rest prefetches at most (1135): 32 MB, about the
/// ring and the zoom in around a phone's frame with its margins.
pub const PREFETCH_BUDGET: usize = 128;
/// The pan velocity's moving average reaches a new speed within about
/// this many milliseconds of panning.
pub const MOTION_SMOOTH_MS: f64 = 250.0;
/// At rest the velocity decays by `e` every this many milliseconds:
/// from a fling of 5000 pixels a second below [`MOTION_FLOOR`] in 2.3 s.
pub const MOTION_REST_MS: f64 = 500.0;
/// A velocity below this many screen pixels a second has no direction.
pub const MOTION_FLOOR: f64 = 50.0;
/// The time between two pans the average counts at most: a pan after a
/// rest is one frame's worth of motion, not one spread over the rest.
const MOTION_GAP_MS: u64 = 250;

/// What tells one tile from another: where it is, and what it was drawn
/// in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub zoom: u8,
    pub x: u32,
    pub y: u32,
    pub style: Style,
    /// The line widths, per cent of the zoom's.
    pub percent: u32,
}

/// A tile as a frame holds it: its pixels and its credit.
type Held = (Arc<Vec<u8>>, Arc<str>);

/// Where a pixel samples along one axis: the tile and the offset in it of
/// the sample and of its next neighbour, and the neighbour's weight out
/// of 256.
type Step = ((usize, usize), (usize, usize), u32);

struct Entry {
    rgba: Arc<Vec<u8>>,
    attribution: Arc<str>,
    used: u64,
    /// Drawn ahead by [`TileCache::prefetch`] and not used by a frame
    /// since: evicted first.
    prefetched: bool,
}

/// What the cache did since it was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Tiles a frame found cached.
    pub hits: u64,
    /// Tiles a frame did not find.
    pub misses: u64,
    /// Tiles a frame drew.
    pub rendered: u64,
    /// Tiles drawn ahead by [`TileCache::prefetch`].
    pub prefetched: u64,
    /// Hits on a prefetched tile no frame had used before.
    pub prefetch_hits: u64,
    /// Tiles dropped for room.
    pub evicted: u64,
    /// Times the whole cache was dropped: a new epoch or style.
    pub cleared: u64,
}

/// One frame asked of the cache.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// The view, at its exact zoom.
    pub view: ExactView,
    /// The zoom step whose tiles it is composed from.
    pub zoom: u8,
    pub style: Style,
    /// Screen pixels a logical one: the line widths.
    pub scale: f32,
    /// Draw at most this many missing tiles; all with `None`.
    pub render_limit: Option<usize>,
    /// The zoom step whose cached tiles stand in for those not drawn.
    pub fallback: Option<u8>,
}

impl Frame {
    /// The line widths, per cent of the zoom's.
    fn percent(&self) -> u32 {
        (self.scale * 100.0).round().max(1.0) as u32
    }
}

/// A frame composed.
#[derive(Debug)]
pub struct Composed {
    /// RGBA rows, the view's size.
    pub rgba: Vec<u8>,
    /// The credit of the regions drawn, the outline's when none.
    pub attribution: String,
    /// Tiles the frame needed and did not draw: ask again.
    pub missing: usize,
}

/// The cache over one maps directory.
pub struct TileCache {
    maps: Maps,
    budget: usize,
    epoch: u64,
    style: Option<(Style, u32)>,
    entries: HashMap<TileKey, Entry>,
    bytes: usize,
    tick: u64,
    /// The tick the last frame started at: an entry used since is on
    /// screen, or prefetched since.
    shown_from: u64,
    painters: Vec<Painter>,
    workers: usize,
    pub stats: Stats,
}

/// The pixel colour off the projection, north and south of the world:
/// what a view drawn whole has there, nothing drawn.
const OFF: [u8; 4] = [0, 0, 0, 0xff];

impl TileCache {
    /// A cache over `maps` of at most `budget` bytes.
    pub fn new(maps: Maps, budget: usize) -> Self {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .clamp(1, WORKERS);
        Self {
            epoch: maps.epoch(),
            maps,
            budget,
            style: None,
            entries: HashMap::new(),
            bytes: 0,
            tick: 0,
            shown_from: 0,
            painters: Vec::new(),
            workers,
            stats: Stats::default(),
        }
    }

    /// Draw on `n` threads at most (at least one).
    pub fn set_workers(&mut self, n: usize) {
        self.workers = n.max(1);
    }

    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Tiles held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Drop every tile.
    pub fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.stats.cleared += 1;
        }
        self.entries.clear();
        self.bytes = 0;
    }

    /// Whether the tile of `key` is held.
    pub fn holds(&self, key: &TileKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Drop the cache when the directory or the style changed since the
    /// last frame.
    fn check(&mut self, style: Style, percent: u32) {
        let epoch = self.maps.epoch();
        if epoch != self.epoch {
            self.epoch = epoch;
            self.clear();
        }
        if self.style.is_some_and(|s| s != (style, percent)) {
            self.clear();
        }
        self.style = Some((style, percent));
    }

    /// `frame` composed from the tiles of its zoom step.
    pub fn compose(&mut self, frame: &Frame) -> Composed {
        let percent = frame.percent();
        self.check(frame.style, percent);
        self.shown_from = self.tick + 1;
        let (w, h) = frame.view.size();
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        for px in rgba.chunks_exact_mut(4) {
            px.copy_from_slice(&OFF);
        }
        let grid = Grid::of(&frame.view, frame.zoom);
        let keys = grid.keys(frame.style, percent);
        let mut got: Vec<Option<Held>> = Vec::with_capacity(keys.len());
        let mut missing: Vec<usize> = Vec::new();
        for (i, key) in keys.iter().enumerate() {
            let found = key.and_then(|k| self.get(&k));
            match (key, &found) {
                (Some(_), Some(_)) => self.stats.hits += 1,
                (Some(_), None) => {
                    self.stats.misses += 1;
                    missing.push(i);
                }
                (None, _) => {}
            }
            got.push(found);
        }
        let limit = frame.render_limit.unwrap_or(usize::MAX).min(missing.len());
        let (draw, left) = missing.split_at(limit);
        let wanted: Vec<TileKey> = draw.iter().filter_map(|&i| keys[i]).collect();
        let drawn = self.render(&wanted);
        self.stats.rendered += drawn.len() as u64;
        for (&i, (key, rgba, attribution)) in draw.iter().zip(drawn) {
            let tile = (Arc::new(rgba), Arc::<str>::from(attribution));
            self.put(key, tile.0.clone(), tile.1.clone());
            got[i] = Some(tile);
        }
        if !left.is_empty() {
            if let Some(z) = frame.fallback.filter(|&z| z != frame.zoom) {
                let under = Grid::of(&frame.view, z);
                let tiles: Vec<Option<Held>> = under
                    .keys(frame.style, percent)
                    .iter()
                    .map(|k| k.and_then(|k| self.get(&k)))
                    .collect();
                under.sample(&frame.view, &tiles, &mut rgba, self.workers);
            }
        }
        grid.sample(&frame.view, &got, &mut rgba, self.workers);
        let attribution = got
            .iter()
            .flatten()
            .map(|(_, a)| a)
            .find(|a| a.as_ref() != outline::ATTRIBUTION)
            .map_or_else(|| outline::ATTRIBUTION.to_string(), |a| a.to_string());
        Composed {
            rgba,
            attribution,
            missing: left.len(),
        }
    }

    fn get(&mut self, key: &TileKey) -> Option<Held> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.get_mut(key)?;
        entry.used = tick;
        if entry.prefetched {
            entry.prefetched = false;
            self.stats.prefetch_hits += 1;
        }
        Some((entry.rgba.clone(), entry.attribution.clone()))
    }

    fn put(&mut self, key: TileKey, rgba: Arc<Vec<u8>>, attribution: Arc<str>) {
        self.tick += 1;
        self.bytes += rgba.len();
        let old = self.entries.insert(
            key,
            Entry {
                rgba,
                attribution,
                used: self.tick,
                prefetched: false,
            },
        );
        if let Some(old) = old {
            self.bytes -= old.rgba.len();
        }
        while self.bytes > self.budget && self.entries.len() > 1 {
            if !self.evict(|_| true) {
                break;
            }
        }
    }

    /// Drop the entry `may` allows that goes first: a prefetched one no
    /// frame used, then the least recently used. Whether one went.
    fn evict(&mut self, may: impl Fn(&Entry) -> bool) -> bool {
        let Some(first) = self
            .entries
            .iter()
            .filter(|(_, e)| may(e))
            .min_by_key(|(_, e)| (!e.prefetched, e.used))
            .map(|(k, _)| *k)
        else {
            return false;
        };
        if let Some(e) = self.entries.remove(&first) {
            self.bytes -= e.rgba.len();
            self.stats.evicted += 1;
        }
        true
    }

    /// Hold a prefetched tile, making room only from tiles neither the
    /// last frame showed nor a prefetch since drew. Whether it is held.
    fn put_prefetched(&mut self, key: TileKey, rgba: Vec<u8>, attribution: String) -> bool {
        let shown_from = self.shown_from;
        while self.bytes + rgba.len() > self.budget {
            if !self.evict(|e| e.used < shown_from) {
                return false;
            }
        }
        self.tick += 1;
        self.bytes += rgba.len();
        let old = self.entries.insert(
            key,
            Entry {
                rgba: Arc::new(rgba),
                attribution: Arc::from(attribution),
                used: self.tick,
                prefetched: true,
            },
        );
        if let Some(old) = old {
            self.bytes -= old.rgba.len();
        }
        true
    }

    /// Draw the next tiles of `plan` not held, at most `n` (a batch, one
    /// a worker so a gesture waits for one tile's time at most), as
    /// prefetched tiles. A tile that wants spare room stops the plan when
    /// the cache has none; a tile that finds no room it may take stops it
    /// too. The tiles drawn.
    pub fn prefetch(&mut self, plan: &mut Plan, n: usize) -> usize {
        if let Some(first) = plan.wants.first() {
            self.check(first.key.style, first.key.percent);
        }
        let mut batch: Vec<TileKey> = Vec::new();
        while batch.len() < n && plan.drawn + batch.len() < plan.budget {
            let Some(want) = plan.wants.get(plan.next).copied() else {
                break;
            };
            if self.holds(&want.key) || batch.contains(&want.key) {
                plan.next += 1;
                continue;
            }
            let after = self.bytes + (batch.len() + 1) * TILE_BYTES;
            if want.spare_only && after > self.budget {
                plan.next = plan.wants.len();
                break;
            }
            plan.next += 1;
            batch.push(want.key);
        }
        let mut stored = 0;
        for (key, rgba, attribution) in self.render(&batch) {
            if !self.put_prefetched(key, rgba, attribution) {
                plan.next = plan.wants.len();
                break;
            }
            stored += 1;
        }
        self.stats.prefetched += stored as u64;
        plan.drawn += stored;
        if plan.drawn >= plan.budget {
            plan.next = plan.wants.len();
        }
        stored
    }

    /// Draw `keys`, split over the workers, each with its own painter.
    fn render(&mut self, keys: &[TileKey]) -> Vec<(TileKey, Vec<u8>, String)> {
        if keys.is_empty() {
            return Vec::new();
        }
        let n = self.workers.min(keys.len());
        while self.painters.len() < n {
            self.painters.push(Painter::new(self.maps.clone()));
        }
        if n == 1 {
            let painter = &mut self.painters[0];
            return keys.iter().map(|k| draw_tile(painter, k)).collect();
        }
        let per = keys.len().div_ceil(n);
        std::thread::scope(|s| {
            let jobs: Vec<_> = self
                .painters
                .iter_mut()
                .zip(keys.chunks(per))
                .map(|(painter, chunk)| {
                    s.spawn(move || {
                        chunk
                            .iter()
                            .map(|k| draw_tile(painter, k))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            jobs.into_iter()
                .flat_map(|j| j.join().unwrap_or_default())
                .collect()
        })
    }
}

/// The tile of `key` drawn: RGBA and its credit.
fn draw_tile(painter: &mut Painter, key: &TileKey) -> (TileKey, Vec<u8>, String) {
    let shift = 24 - u32::from(key.zoom);
    let mid = |t: u32| ((u64::from(t) * u64::from(TILE) + u64::from(TILE / 2)) << shift) as u32;
    let view = Viewport::new(
        World {
            x: mid(key.x),
            y: mid(key.y),
        },
        key.zoom,
        TILE,
        TILE,
    );
    let drawn = painter.draw(&view, Ink::Colour, key.style, key.percent);
    let mut rgba = Vec::with_capacity(drawn.fb.len() * 4);
    for &px in &drawn.fb {
        let [r, g, b] = rgb888(Rgb565::from(RawU16::new(u16::from_be(px))));
        rgba.extend_from_slice(&[r, g, b, 0xff]);
    }
    (*key, rgba, drawn.attribution)
}

/// The pan's velocity, a moving average in screen pixels a second that
/// decays at rest (1135): what [`plan`] orders the tiles by, those ahead
/// of the last movement first. And whether the last pinch went out.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    velocity: (f64, f64),
    at_ms: Option<u64>,
    /// The last zoom the fingers made went out.
    pub zoomed_out: bool,
}

impl Motion {
    /// The view moved by `(dx, dy)` screen pixels at `now_ms`.
    pub fn pan(&mut self, dx: f64, dy: f64, now_ms: u64) {
        let (vx, vy) = self.velocity(now_ms);
        let dt = self
            .at_ms
            .map_or(MOTION_GAP_MS, |at| now_ms.saturating_sub(at))
            .clamp(1, MOTION_GAP_MS) as f64;
        let a = 1.0 - (-dt / MOTION_SMOOTH_MS).exp();
        let (ix, iy) = (dx * 1000.0 / dt, dy * 1000.0 / dt);
        self.velocity = (vx + a * (ix - vx), vy + a * (iy - vy));
        self.at_ms = Some(now_ms);
    }

    /// The fingers zoomed by `by_milli` thousandths of a step.
    pub fn zoom(&mut self, by_milli: i64) {
        if by_milli != 0 {
            self.zoomed_out = by_milli < 0;
        }
    }

    /// The velocity at `now_ms`, decayed since the last pan.
    pub fn velocity(&self, now_ms: u64) -> (f64, f64) {
        let Some(at) = self.at_ms else {
            return (0.0, 0.0);
        };
        let k = (-(now_ms.saturating_sub(at) as f64) / MOTION_REST_MS).exp();
        (self.velocity.0 * k, self.velocity.1 * k)
    }

    /// The direction the view last moved in, when it still moves faster
    /// than [`MOTION_FLOOR`].
    pub fn heading(&self, now_ms: u64) -> Option<(f64, f64)> {
        let (vx, vy) = self.velocity(now_ms);
        (vx.hypot(vy) >= MOTION_FLOOR).then_some((vx, vy))
    }
}

/// One tile a [`Plan`] draws ahead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Want {
    pub key: TileKey,
    /// Drawn only into room the cache has spare: the second ring.
    pub spare_only: bool,
}

/// The tiles a view at rest draws ahead, in order, and how far
/// [`TileCache::prefetch`] got.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub wants: Vec<Want>,
    /// The next want to look at.
    pub next: usize,
    /// Tiles drawn so far.
    pub drawn: usize,
    /// Tiles it draws at most.
    pub budget: usize,
}

impl Plan {
    /// Nothing left to draw.
    pub fn done(&self) -> bool {
        self.next >= self.wants.len()
    }
}

/// What `frame`, a view at rest, draws ahead (1135): the ring of tiles
/// around its tiles at its step, the step in (no deeper than `zooms.1`)
/// over the frame's central half, the step out (no shallower than
/// `zooms.0`) when the last pinch went out, and a second ring into spare
/// room only. Each group ordered by its tiles' offset from the view's
/// centre along `heading`, ahead first, nearest first between equals and
/// without a heading.
pub fn plan(frame: &Frame, heading: Option<(f64, f64)>, zoomed_out: bool, zooms: (u8, u8)) -> Plan {
    let percent = frame.percent();
    let mut wants: Vec<Want> = Vec::new();
    let mut add = |spots: Vec<Spot>, spare_only: bool| {
        for spot in ordered(spots, heading) {
            let key = TileKey {
                zoom: spot.zoom,
                x: spot.x,
                y: spot.y,
                style: frame.style,
                percent,
            };
            if !wants.iter().any(|w| w.key == key) {
                wants.push(Want { key, spare_only });
            }
        }
    };
    let z = frame.zoom;
    add(Grid::of(&frame.view, z).ring(1), false);
    if z < zooms.1.min(MAX_VIEW_ZOOM) {
        let mut inner = frame.view;
        let (w, h) = inner.size();
        inner.resize(w / 2, h / 2);
        add(Grid::of(&inner, z + 1).ring(0), false);
    }
    if zoomed_out && z > zooms.0 {
        add(Grid::of(&frame.view, z - 1).ring(0), false);
    }
    add(Grid::of(&frame.view, z).ring(2), true);
    Plan {
        wants,
        next: 0,
        drawn: 0,
        budget: PREFETCH_BUDGET,
    }
}

/// A tile of a ring and its centre's offset from the view's centre in
/// screen pixels.
#[derive(Clone, Copy, Debug)]
struct Spot {
    zoom: u8,
    x: u32,
    y: u32,
    offset: (f64, f64),
}

/// `spots` with those furthest along `heading` first; nearest first
/// between equals, and without one.
fn ordered(mut spots: Vec<Spot>, heading: Option<(f64, f64)>) -> Vec<Spot> {
    let unit = heading.map(|(x, y)| {
        let n = x.hypot(y).max(f64::MIN_POSITIVE);
        (x / n, y / n)
    });
    // Ahead by whole tiles of screen, so a tile a little to the side of
    // the heading is no later than one straight ahead of it.
    let ahead = |s: &Spot| {
        unit.map_or(0, |(ux, uy)| {
            -((s.offset.0 * ux + s.offset.1 * uy) / f64::from(TILE)).round() as i64
        })
    };
    let near = |s: &Spot| s.offset.0.hypot(s.offset.1);
    spots.sort_by(|a, b| ahead(a).cmp(&ahead(b)).then(near(a).total_cmp(&near(b))));
    spots
}

/// The tiles of one zoom step a view covers, and how its pixels sample
/// them.
struct Grid {
    zoom: u8,
    /// The view's centre in pixels of the step's world.
    cx: f64,
    cy: f64,
    /// Screen pixels a pixel of the step.
    scale: f64,
    /// The first tile column and row, and how many.
    tx0: i64,
    ty0: i64,
    cols: usize,
    rows: usize,
}

impl Grid {
    fn of(view: &ExactView, zoom: u8) -> Self {
        let unit = f64::from(1u32 << (24 - u32::from(zoom)));
        let scale = view.scale_at(zoom);
        let (w, h) = view.size();
        let (cx, cy) = (
            f64::from(view.centre.x) / unit,
            f64::from(view.centre.y) / unit,
        );
        let t = f64::from(TILE);
        // A pixel's sample point and its right or lower neighbour.
        let span = |c: f64, n: u32| {
            let first = c + (0.5 - f64::from(n) / 2.0) / scale - 0.5;
            let last = c + (f64::from(n) - 0.5 - f64::from(n) / 2.0) / scale + 0.5;
            ((first / t).floor() as i64, (last / t).floor() as i64)
        };
        let (tx0, tx1) = span(cx, w);
        let (ty0, ty1) = span(cy, h);
        Self {
            zoom,
            cx,
            cy,
            scale,
            tx0,
            ty0,
            cols: (tx1 - tx0 + 1) as usize,
            rows: (ty1 - ty0 + 1) as usize,
        }
    }

    /// The tiles' keys, row by row; `None` off the projection.
    fn keys(&self, style: Style, percent: u32) -> Vec<Option<TileKey>> {
        let n = 1i64 << self.zoom;
        let mut out = Vec::with_capacity(self.cols * self.rows);
        for r in 0..self.rows as i64 {
            for c in 0..self.cols as i64 {
                let (tx, ty) = (self.tx0 + c, self.ty0 + r);
                out.push((0..n).contains(&ty).then(|| TileKey {
                    zoom: self.zoom,
                    x: tx.rem_euclid(n) as u32,
                    y: ty as u32,
                    style,
                    percent,
                }));
            }
        }
        out
    }

    /// The tiles `k` tiles outside the grid's on every side, all of them
    /// with `k` zero, and their centres' offsets from the view's centre
    /// in its pixels; none off the projection, each once.
    fn ring(&self, k: i64) -> Vec<Spot> {
        let n = 1i64 << self.zoom;
        let t = f64::from(TILE);
        let (x0, y0) = (self.tx0 - k, self.ty0 - k);
        let (x1, y1) = (
            self.tx0 + self.cols as i64 - 1 + k,
            self.ty0 + self.rows as i64 - 1 + k,
        );
        let mut out: Vec<Spot> = Vec::new();
        for ty in y0..=y1 {
            for tx in x0..=x1 {
                let edge = tx == x0 || tx == x1 || ty == y0 || ty == y1;
                if (k > 0 && !edge) || !(0..n).contains(&ty) {
                    continue;
                }
                let x = tx.rem_euclid(n) as u32;
                if out.iter().any(|s| s.x == x && s.y == ty as u32) {
                    continue;
                }
                let offset = (
                    ((tx as f64 + 0.5) * t - self.cx) * self.scale,
                    ((ty as f64 + 0.5) * t - self.cy) * self.scale,
                );
                out.push(Spot {
                    zoom: self.zoom,
                    x,
                    y: ty as u32,
                    offset,
                });
            }
        }
        out
    }

    /// Sample `tiles` (in [`keys`](Self::keys)' order) into `rgba`, the
    /// view's pixels: nearest at a scale of one, so a frame at a whole
    /// step is the tiles' pixels, bilinear between steps. A pixel whose
    /// tile is absent is left as it is; a neighbour whose tile is absent
    /// gives way to the nearest sample. Rows split over `workers`.
    fn sample(&self, view: &ExactView, tiles: &[Option<Held>], rgba: &mut [u8], workers: usize) {
        let (w, h) = view.size();
        let t = i64::from(TILE);
        let nearest = self.scale == 1.0;
        // Per column: the tile column and offset of the sample and of its
        // right neighbour, and the neighbour's weight out of 256.
        let col = |x: u32| {
            let u = self.cx + (f64::from(x) + 0.5 - f64::from(w) / 2.0) / self.scale - 0.5;
            let (p, f) = match nearest {
                true => ((u + 0.5).floor() as i64, 0),
                false => (u.floor() as i64, ((u - u.floor()) * 256.0) as u32),
            };
            let at = |p: i64| {
                let c = (p.div_euclid(t) - self.tx0) as usize;
                (c.min(self.cols - 1), p.rem_euclid(t) as usize)
            };
            (at(p), at(p + 1), f)
        };
        let cols: Vec<Step> = (0..w).map(col).collect();
        let row = |y: u32| {
            let v = self.cy + (f64::from(y) + 0.5 - f64::from(h) / 2.0) / self.scale - 0.5;
            let (p, f) = match nearest {
                true => ((v + 0.5).floor() as i64, 0),
                false => (v.floor() as i64, ((v - v.floor()) * 256.0) as u32),
            };
            let at = |p: i64| {
                let r = (p.div_euclid(t) - self.ty0) as usize;
                (r.min(self.rows - 1), p.rem_euclid(t) as usize)
            };
            (at(p), at(p + 1), f)
        };
        let stride = w as usize * 4;
        let one_row = |y: u32, out: &mut [u8]| {
            let ((r0, o0), (r1, o1), fy) = row(y);
            let tile = |r: usize, c: usize| tiles.get(r * self.cols + c).and_then(|t| t.as_ref());
            let px = |r: usize, oy: usize, c: usize, ox: usize| {
                tile(r, c).map(|(rgba, _)| {
                    let i = (oy * TILE as usize + ox) * 4;
                    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
                })
            };
            for (x, &((c0, x0), (c1, x1), fx)) in cols.iter().enumerate() {
                let Some(a) = px(r0, o0, c0, x0) else {
                    continue;
                };
                let i = x * 4;
                if nearest {
                    out[i..i + 4].copy_from_slice(&a);
                    continue;
                }
                let b = px(r0, o0, c1, x1).unwrap_or(a);
                let c = px(r1, o1, c0, x0).unwrap_or(a);
                let d = px(r1, o1, c1, x1).unwrap_or(c);
                for k in 0..4 {
                    let top = u32::from(a[k]) * (256 - fx) + u32::from(b[k]) * fx;
                    let bottom = u32::from(c[k]) * (256 - fx) + u32::from(d[k]) * fx;
                    out[i + k] = ((top * (256 - fy) + bottom * fy + (1 << 15)) >> 16) as u8;
                }
            }
        };
        let bands = workers.clamp(1, h as usize);
        let per = (h as usize).div_ceil(bands);
        if bands == 1 {
            for (y, out) in rgba.chunks_exact_mut(stride).enumerate() {
                one_row(y as u32, out);
            }
            return;
        }
        std::thread::scope(|s| {
            for (b, band) in rgba.chunks_mut(per * stride).enumerate() {
                let one_row = &one_row;
                s.spawn(move || {
                    for (i, out) in band.chunks_exact_mut(stride).enumerate() {
                        one_row((b * per + i) as u32, out);
                    }
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};
    use leviculum_map::viewport::MILLI;

    const FLORENCE: &[u8] = include_bytes!("../../leviculum-map/fixtures/firenze.lmr");

    fn maps() -> (tempfile::TempDir, Maps) {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("firenze.lmr"), FLORENCE).expect("write");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        (dir, maps)
    }

    fn duomo() -> World {
        project(LatLon::from_degrees(43.7731, 11.2560))
    }

    fn frame(view: ExactView, zoom: u8) -> Frame {
        Frame {
            view,
            zoom,
            style: Style::PC,
            scale: 1.0,
            render_limit: None,
            fallback: None,
        }
    }

    /// A centre on a whole pixel of zoom `z`, so a view drawn whole and
    /// the tiles sample the same pixels.
    fn on_pixel(w: World, z: u8) -> World {
        let shift = 24 - u32::from(z);
        World {
            x: (w.x >> shift) << shift,
            y: (w.y >> shift) << shift,
        }
    }

    /// At a whole step the composed frame is the view drawn whole, pixel
    /// for pixel but where a line's antialiased end meets a seam.
    #[test]
    fn a_composed_frame_is_the_view_drawn_whole() {
        let (_dir, maps) = maps();
        let z = 15;
        let centre = on_pixel(duomo(), z);
        let (w, h) = (600, 500);
        let mut cache = TileCache::new(maps.clone(), BUDGET_MB << 20);
        let view = ExactView::new(centre, u32::from(z) * MILLI, w, h);
        let composed = cache.compose(&frame(view, z));
        assert_eq!(composed.missing, 0);
        let mut painter = Painter::new(maps);
        let whole = painter.draw(&Viewport::new(centre, z, w, h), Ink::Colour, Style::PC, 100);
        let mut differ = 0usize;
        for (i, &px) in whole.fb.iter().enumerate() {
            let [r, g, b] = rgb888(Rgb565::from(RawU16::new(u16::from_be(px))));
            let got = &composed.rgba[i * 4..i * 4 + 3];
            let d = got
                .iter()
                .zip([r, g, b])
                .map(|(&a, b)| a.abs_diff(b))
                .max()
                .unwrap_or(0);
            if d > 8 {
                differ += 1;
            }
        }
        let share = differ as f64 / whole.fb.len() as f64;
        assert!(
            share < 0.002,
            "{differ} pixels of {} differ",
            whole.fb.len()
        );
        assert_eq!(composed.attribution, whole.attribution);
    }

    #[test]
    fn a_second_frame_hits_and_a_pan_draws_only_what_it_uncovers() {
        let (_dir, maps) = maps();
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        let mut view = ExactView::new(duomo(), 15 * MILLI, 512, 512);
        cache.compose(&frame(view, 15));
        let first = cache.stats;
        assert!(first.misses > 0 && first.hits == 0);
        assert_eq!(first.rendered, first.misses);
        cache.compose(&frame(view, 15));
        assert_eq!(cache.stats.rendered, first.rendered, "nothing drawn again");
        assert_eq!(cache.stats.hits, first.misses);
        view.pan(300, 0);
        cache.compose(&frame(view, 15));
        let drawn = cache.stats.rendered - first.rendered;
        assert!(
            (1..=3).contains(&drawn),
            "a pan of 300 pixels draws one or two new columns of tiles: {drawn}"
        );
    }

    #[test]
    fn the_cache_stays_in_its_budget() {
        let (_dir, maps) = maps();
        let tile = (TILE * TILE * 4) as usize;
        let mut cache = TileCache::new(maps, 6 * tile);
        let mut view = ExactView::new(duomo(), 15 * MILLI, 512, 512);
        for _ in 0..6 {
            cache.compose(&frame(view, 15));
            view.pan(512, 0);
            assert!(cache.bytes() <= 6 * tile, "{} bytes", cache.bytes());
            assert!(cache.len() <= 6);
        }
        assert!(cache.stats.evicted > 0);
    }

    #[test]
    fn a_new_epoch_or_theme_drops_the_tiles() {
        let (dir, maps) = maps();
        let mut cache = TileCache::new(maps.clone(), BUDGET_MB << 20);
        let view = ExactView::new(duomo(), 15 * MILLI, 300, 300);
        cache.compose(&frame(view, 15));
        let held = cache.len();
        assert!(held > 0);
        let dark = Frame {
            style: Style::PC.in_theme(leviculum_mapfill::style::MapTheme::Dark),
            ..frame(view, 15)
        };
        cache.compose(&dark);
        assert_eq!(cache.stats.cleared, 1);
        assert_eq!(cache.len(), held, "only the dark tiles are held");
        std::fs::write(dir.path().join("again.lmr"), FLORENCE).expect("write");
        maps.changed();
        cache.compose(&dark);
        assert_eq!(cache.stats.cleared, 2);
        assert_eq!(cache.stats.rendered, 3 * held as u64);
    }

    /// A frame told to draw nothing shows another step's cached tiles,
    /// scaled, and says how many it still owes.
    #[test]
    fn a_frame_not_drawn_yet_shows_the_step_before() {
        let (_dir, maps) = maps();
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        let at14 = ExactView::new(duomo(), 14_600, 400, 400);
        cache.compose(&frame(at14, 14));
        let blank = {
            let none = Frame {
                render_limit: Some(0),
                ..frame(at14, 15)
            };
            cache.compose(&none)
        };
        assert!(blank.missing > 0);
        assert!(
            blank.rgba.chunks_exact(4).all(|p| p == OFF),
            "nothing to show"
        );
        let stand_in = Frame {
            render_limit: Some(0),
            fallback: Some(14),
            ..frame(at14, 15)
        };
        let shown = cache.compose(&stand_in);
        assert_eq!(shown.missing, blank.missing);
        let drawn = shown.rgba.chunks_exact(4).filter(|p| *p != OFF).count();
        assert!(drawn > 400 * 400 * 9 / 10, "the z14 tiles fill it: {drawn}");
    }

    /// The world pixel of `key`'s centre at its zoom, against the view's
    /// centre there, in screen pixels.
    fn offset(view: &ExactView, key: &TileKey) -> (f64, f64) {
        let unit = f64::from(1u32 << (24 - u32::from(key.zoom)));
        let s = view.scale_at(key.zoom);
        let c =
            |t: u32, at: u32| ((f64::from(t) + 0.5) * f64::from(TILE) - f64::from(at) / unit) * s;
        (c(key.x, view.centre.x), c(key.y, view.centre.y))
    }

    fn ring_one(plan: &Plan) -> Vec<TileKey> {
        let z = plan.wants[0].key.zoom;
        plan.wants
            .iter()
            .take_while(|w| w.key.zoom == z && !w.spare_only)
            .map(|w| w.key)
            .collect()
    }

    /// A pan north puts the tiles north of the view first, east the east
    /// ones; no heading puts the nearest first.
    #[test]
    fn the_tiles_ahead_of_the_last_pan_come_first() {
        let view = ExactView::new(duomo(), 15 * MILLI, 600, 900);
        let f = frame(view, 15);
        let mut north = Motion::default();
        for i in 0..10 {
            north.pan(0.0, -40.0, 1000 + i * 16);
        }
        let heading = north.heading(1200).expect("a heading");
        assert!(heading.1 < -500.0 && heading.0 == 0.0, "{heading:?}");
        let ring = ring_one(&plan(&f, Some(heading), false, (0, 20)));
        let top = ring.iter().map(|k| k.y).min().expect("a ring");
        let row = ring.iter().filter(|k| k.y == top).count();
        assert!(row >= 4);
        assert!(
            ring[..row].iter().all(|k| k.y == top),
            "the northern row first: {:?}",
            &ring[..row]
        );
        assert!(ring[ring.len() - row..].iter().all(|k| k.y > top + 2));

        let mut east = Motion::default();
        for i in 0..10 {
            east.pan(40.0, 0.0, 1000 + i * 16);
        }
        let ring = ring_one(&plan(&f, east.heading(1200), false, (0, 20)));
        let right = ring.iter().map(|k| k.x).max().expect("a ring");
        assert!(ring[0].x == right && ring[1].x == right, "{:?}", &ring[..3]);

        let ring = ring_one(&plan(&f, None, false, (0, 20)));
        let near: Vec<f64> = ring
            .iter()
            .map(|k| {
                let (x, y) = offset(&view, k);
                x.hypot(y)
            })
            .collect();
        assert!(
            near.windows(2).all(|w| w[0] <= w[1] + 1e-6),
            "nearest first: {near:?}"
        );
    }

    /// The plan's groups: the ring at the view's step, the step in over
    /// the centre, the step out only after a pinch out, the second ring
    /// last and into spare room only.
    #[test]
    fn a_plan_is_the_ring_the_step_in_and_a_second_ring() {
        let view = ExactView::new(duomo(), 15 * MILLI, 600, 900);
        let f = frame(view, 15);
        let zooms = |p: &Plan| {
            let mut z: Vec<(u8, bool)> =
                p.wants.iter().map(|w| (w.key.zoom, w.spare_only)).collect();
            z.dedup();
            z
        };
        let p = plan(&f, None, false, (0, 20));
        assert_eq!(zooms(&p), [(15, false), (16, false), (15, true)]);
        let p = plan(&f, None, true, (0, 20));
        assert_eq!(
            zooms(&p),
            [(15, false), (16, false), (14, false), (15, true)]
        );
        let p = plan(&f, None, true, (15, 15));
        assert_eq!(zooms(&p), [(15, false), (15, true)], "the regions' zooms");
        let grid = Grid::of(&view, 15);
        let held: Vec<TileKey> = grid.keys(Style::PC, 100).into_iter().flatten().collect();
        assert!(
            p.wants.iter().all(|w| !held.contains(&w.key)),
            "none on screen"
        );
    }

    /// The velocity decays at rest: no direction a few seconds after the
    /// finger stopped.
    #[test]
    fn a_heading_fades_at_rest() {
        let mut m = Motion::default();
        assert_eq!(m.heading(0), None);
        for i in 0..20 {
            m.pan(0.0, -80.0, i * 16);
        }
        assert!(m.heading(320).is_some());
        assert!(m.heading(320 + 1000).is_some());
        assert_eq!(m.heading(320 + 3000), None, "{:?}", m.velocity(3320));
        m.zoom(-300);
        assert!(m.zoomed_out);
        m.zoom(0);
        assert!(m.zoomed_out);
        m.zoom(200);
        assert!(!m.zoomed_out);
    }

    #[test]
    fn a_prefetch_stays_in_its_budget_a_batch_at_a_time() {
        let (_dir, maps) = maps();
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        let view = ExactView::new(duomo(), 15 * MILLI, 512, 512);
        let f = frame(view, 15);
        cache.compose(&f);
        let mut p = plan(&f, None, false, (0, 20));
        p.budget = 5;
        assert_eq!(cache.prefetch(&mut p, 2), 2);
        assert_eq!(cache.prefetch(&mut p, 2), 2);
        assert_eq!(cache.prefetch(&mut p, 2), 1);
        assert!(p.done());
        assert_eq!(cache.prefetch(&mut p, 2), 0);
        assert_eq!(cache.stats.prefetched, 5);
        assert_eq!(cache.stats.rendered, cache.stats.misses, "a frame's own");
        assert!(p.wants[..5].iter().all(|w| cache.holds(&w.key)));
    }

    /// A prefetch never evicts a tile the frame shows, nor one it drew
    /// itself; a frame evicts the prefetched tiles no frame used first,
    /// and a prefetched tile a frame uses counts as a hit.
    #[test]
    fn prefetched_tiles_go_first_and_never_push_out_the_view() {
        let (_dir, maps) = maps();
        let view = ExactView::new(duomo(), 15 * MILLI, 512, 512);
        let f = frame(view, 15);
        let shown: Vec<TileKey> = Grid::of(&view, 15)
            .keys(Style::PC, 100)
            .into_iter()
            .flatten()
            .collect();
        let mut cache = TileCache::new(maps, (shown.len() + 3) * TILE_BYTES);
        cache.compose(&f);
        let mut p = plan(&f, None, false, (0, 20));
        while !p.done() {
            cache.prefetch(&mut p, 4);
        }
        assert_eq!(cache.stats.prefetched, 3, "only the room left");
        assert_eq!(cache.stats.evicted, 0);
        assert!(shown.iter().all(|k| cache.holds(k)));

        // A pan of one tile: the frame draws a column, evicting the
        // prefetched tiles it does not use before any it showed.
        let mut moved = view;
        moved.pan(TILE as i32, 0);
        cache.compose(&frame(moved, 15));
        let ahead: Vec<TileKey> = p.wants[..3].iter().map(|w| w.key).collect();
        let now: Vec<TileKey> = Grid::of(&moved, 15)
            .keys(Style::PC, 100)
            .into_iter()
            .flatten()
            .collect();
        for k in &ahead {
            assert!(now.contains(k) || !cache.holds(k), "{k:?} unused but held");
        }
        let used = ahead.iter().filter(|k| now.contains(k)).count() as u64;
        assert_eq!(cache.stats.prefetch_hits, used);
        assert!(used < 3, "an unused prefetched tile to evict");
        assert!(now.iter().all(|k| cache.holds(k)), "the frame's tiles held");
    }

    /// The second ring draws into spare room only: in a full cache it
    /// stops the plan rather than evict.
    #[test]
    fn the_second_ring_takes_spare_room_only() {
        let (_dir, maps) = maps();
        let view = ExactView::new(duomo(), 15 * MILLI, 300, 300);
        let f = frame(view, 15);
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        cache.compose(&f);
        let mut p = plan(&f, None, false, (15, 15));
        let first = p.wants.iter().filter(|w| !w.spare_only).count();
        let room = cache.len() + first + 2;
        cache.budget = room * TILE_BYTES;
        p.budget = usize::MAX;
        while !p.done() {
            cache.prefetch(&mut p, 3);
        }
        assert_eq!(cache.stats.prefetched as usize, first + 2);
        assert_eq!(cache.stats.evicted, 0);
        assert!(cache.bytes() <= room * TILE_BYTES);
    }
}
