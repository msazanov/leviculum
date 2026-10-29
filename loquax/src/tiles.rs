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
//! screen's density). [`BUDGET_MB`] bounds it; the least recently used
//! tile goes first.

use std::collections::HashMap;
use std::string::{String, ToString};
use std::sync::Arc;
use std::vec::Vec;

use leviculum_map::draw::Ink;
use leviculum_map::mercator::World;
use leviculum_map::outline;
use leviculum_map::viewport::{ExactView, Viewport};
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
}

/// What the cache did since it was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Tiles a frame found cached.
    pub hits: u64,
    /// Tiles a frame did not find.
    pub misses: u64,
    /// Tiles drawn.
    pub rendered: u64,
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
        let percent = (frame.scale * 100.0).round().max(1.0) as u32;
        self.check(frame.style, percent);
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
            },
        );
        if let Some(old) = old {
            self.bytes -= old.rgba.len();
        }
        while self.bytes > self.budget && self.entries.len() > 1 {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(e) = self.entries.remove(&oldest) {
                self.bytes -= e.rgba.len();
                self.stats.evicted += 1;
            }
        }
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
        self.stats.rendered += keys.len() as u64;
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
}
