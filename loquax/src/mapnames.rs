//! The map's names on a pixel screen (`docs/drafts/map-renderer-roadmap-2026-10.md`,
//! section 1, "Placement" and "Fonts"): set into the picture's raster once,
//! when the view is at rest, so they ride along with it and cost nothing a
//! frame. Placed by the map core's one rule ([`leviculum_map::labels::place`])
//! in screen pixels, after the pins and their names, which nothing covers.
//! The window (1100) and the Android app (1128) set them through this one
//! module.
//!
//! Places, waters and parks are set centred on their anchor; a street's
//! or a river's name on the middle of the longest run of its line (its
//! pieces joined by the map core) that turns less than [`STRAIGHT_DEG`] a
//! vertex, or where that is taken further along it or on the next run, at
//! the run's angle and turned to read left to right, with
//! squares along its baseline as its boxes (the study's first step;
//! bending text along curves is later). Rivers are blue and slanted, the
//! font's own glyphs sheared, since the font has no italic here. Every name
//! carries a halo of the map's ground colour, its glyphs' coverage widened
//! by [`HALO`] pixels and drawn first.
//!
//! The text is shaped by cosmic-text and rasterised by swash in Fira Sans,
//! the face iced embeds, carried here as `fonts/FiraSans-Regular.ttf` (the
//! same file, under the SIL Open Font License, `fonts/OFL.txt`): one font
//! on every host and on the phone, so a picture and its golden agree.

use std::string::{String, ToString};
use std::vec::Vec;

use cosmic_text::{self as ct, fontdb, Attrs, Buffer, Family, FontSystem, Shaping, SwashCache};
use leviculum_map::labels::{self, Candidate, LabelKind, Rect, Rule, ViewLabel, SUBPIXEL};
use leviculum_mapfill::style::MapTheme;

/// Fira Sans Regular 4.203, the file iced 0.14 embeds with its `fira-sans`
/// feature, byte for byte.
pub const FIRA_SANS_REGULAR: &[u8] = include_bytes!("../fonts/FiraSans-Regular.ttf");

/// The most a street's straight run may turn at a vertex, in degrees.
pub const STRAIGHT_DEG: f64 = 12.0;
/// The halo's width in logical pixels.
pub const HALO: f32 = 1.5;
/// The space around a name no other may enter, in logical pixels.
pub const MARGIN: f32 = 3.0;
/// How far apart two names of one street stand, in logical pixels.
pub const REPEAT: f32 = 300.0;

/// A name's text size in logical pixels, by what it names.
pub fn size_of(kind: LabelKind) -> f32 {
    match kind {
        LabelKind::City => 16.0,
        LabelKind::Town => 14.0,
        LabelKind::District | LabelKind::Village => 13.0,
        LabelKind::MajorRoad | LabelKind::SecondaryRoad | LabelKind::MinorRoad => 11.0,
        _ => 12.0,
    }
}

/// A name's text colour and its halo's, RGB, in `theme`: the ground's
/// colour as the halo, waters blue and parks green.
pub fn colours(kind: LabelKind, theme: MapTheme) -> ([u8; 3], [u8; 3]) {
    let dark = theme == MapTheme::Dark;
    let ink = match (kind, dark) {
        (LabelKind::Water | LabelKind::River, false) => [0x1f, 0x4e, 0x8c],
        (LabelKind::Water | LabelKind::River, true) => [0x8c, 0xb4, 0xe6],
        (LabelKind::Park, false) => [0x2e, 0x6b, 0x2e],
        (LabelKind::Park, true) => [0x94, 0xc8, 0x8c],
        (_, false) => [0x26, 0x26, 0x26],
        (_, true) => [0xe6, 0xe6, 0xe6],
    };
    let halo = match dark {
        false => [0xff, 0xff, 0xff],
        true => [0x18, 0x18, 0x18],
    };
    (ink, halo)
}

/// One name's glyph coverage, unturned: `w` by `h`, a byte a pixel, the
/// baseline `base` rows from the top, `pad` pixels of room on every side
/// for the halo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub pad: usize,
    pub alpha: Vec<u8>,
}

impl Mask {
    fn at(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return 0;
        }
        self.alpha[y as usize * self.w + x as usize]
    }

    /// The coverage widened by `r` pixels: each pixel the strongest within
    /// a disc of radius `r`, the halo's shape.
    fn widened(&self, r: f32) -> Mask {
        let ri = r.ceil() as i64;
        let mut out = vec![0u8; self.alpha.len()];
        for y in 0..self.h as i64 {
            for x in 0..self.w as i64 {
                let mut best = 0u8;
                for dy in -ri..=ri {
                    for dx in -ri..=ri {
                        if ((dx * dx + dy * dy) as f32) <= r * r + 0.5 {
                            best = best.max(self.at(x + dx, y + dy));
                        }
                    }
                }
                out[y as usize * self.w + x as usize] = best;
            }
        }
        Mask {
            alpha: out,
            ..self.clone()
        }
    }

    /// Bilinear coverage at a fractional pixel.
    fn sample(&self, x: f64, y: f64) -> f64 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let a = |dx: i64, dy: i64| f64::from(self.at(x0 + dx, y0 + dy));
        let top = a(0, 0) * (1.0 - fx) + a(1, 0) * fx;
        let bottom = a(0, 1) * (1.0 - fx) + a(1, 1) * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

/// Shapes and rasterises names in the embedded font.
pub struct Typesetter {
    fonts: FontSystem,
    cache: SwashCache,
}

impl Default for Typesetter {
    fn default() -> Self {
        Self::new()
    }
}

impl Typesetter {
    /// A font system holding Fira Sans alone, so no host font changes a
    /// picture.
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        db.load_font_data(FIRA_SANS_REGULAR.to_vec());
        Self {
            fonts: FontSystem::new_with_locale_and_db("en-US".to_string(), db),
            cache: SwashCache::new(),
        }
    }

    /// `text`'s width at `size` pixels, shaped and not drawn: what a
    /// screen without iced measures a pin's label with.
    pub fn width(&mut self, text: &str, size: f32) -> u32 {
        let buffer = self.shaped(text, size, false);
        let width = buffer
            .layout_runs()
            .fold(0f32, |width, run| width.max(run.line_w));
        width.ceil() as u32
    }

    fn shaped(&mut self, text: &str, size: f32, italic: bool) -> Buffer {
        let metrics = ct::Metrics::new(size, (size * 1.25).ceil());
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_size(&mut self.fonts, None, None);
        let mut attrs = Attrs::new().family(Family::Name("Fira Sans"));
        if italic {
            attrs = attrs.cache_key_flags(ct::CacheKeyFlags::FAKE_ITALIC);
        }
        buffer.set_text(&mut self.fonts, text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer
    }

    /// `text`'s coverage at `size` pixels, with `pad` pixels around;
    /// slanted with `italic`.
    pub fn mask(&mut self, text: &str, size: f32, pad: usize, italic: bool) -> Mask {
        let buffer = self.shaped(text, size, italic);
        let (mut width, mut height) = (0f32, 0f32);
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
        }
        // The slant leans the glyphs' tops right by about a quarter of
        // the size: room for it past the advance.
        let lean = match italic {
            true => (size * 0.25).ceil() as usize,
            false => 0,
        };
        let (w, h) = (
            width.ceil() as usize + lean + 2 * pad,
            height.ceil() as usize + 2 * pad,
        );
        let mut alpha = vec![0u8; w * h];
        let white = ct::Color::rgb(0xff, 0xff, 0xff);
        buffer.draw(
            &mut self.fonts,
            &mut self.cache,
            white,
            |x, y, cw, ch, c| {
                for yy in y..y + ch as i32 {
                    for xx in x..x + cw as i32 {
                        let (px, py) = (xx + pad as i32, yy + pad as i32);
                        if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                            let i = py as usize * w + px as usize;
                            alpha[i] = alpha[i].max(c.a());
                        }
                    }
                }
            },
        );
        Mask { w, h, pad, alpha }
    }
}

/// The runs of `line` whose vertices turn less than [`STRAIGHT_DEG`], in
/// pixels, longest first (in line order where two are as long): each its
/// middle, its angle in radians turned to read left to right, and its
/// length. The places a name may go along its line, so a name whose best
/// run is taken tries the next; none for a line without a segment.
pub fn straight_runs(line: &[Vec<(f64, f64)>]) -> Vec<((f64, f64), f64, f64)> {
    let mut runs: Vec<((f64, f64), f64, f64)> = Vec::new();
    let limit = STRAIGHT_DEG.to_radians();
    for piece in line {
        let mut start = 0;
        while start + 1 < piece.len() {
            // Grow the run while each next segment turns little against the
            // run's first one.
            let dir = |i: usize| {
                let (a, b) = (piece[i], piece[i + 1]);
                (b.1 - a.1).atan2(b.0 - a.0)
            };
            let first = dir(start);
            let mut end = start + 1;
            while end + 1 < piece.len() {
                let turn = (dir(end) - first + std::f64::consts::PI)
                    .rem_euclid(2.0 * std::f64::consts::PI)
                    - std::f64::consts::PI;
                if turn.abs() >= limit {
                    break;
                }
                end += 1;
            }
            let (a, b) = (piece[start], piece[end]);
            let len = (b.0 - a.0).hypot(b.1 - a.1);
            if len > 0.0 {
                let mut angle = (b.1 - a.1).atan2(b.0 - a.0);
                if angle > std::f64::consts::FRAC_PI_2 {
                    angle -= std::f64::consts::PI;
                } else if angle <= -std::f64::consts::FRAC_PI_2 {
                    angle += std::f64::consts::PI;
                }
                runs.push((((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0), angle, len));
            }
            start = end;
        }
    }
    runs.sort_by(|a, b| b.2.total_cmp(&a.2));
    runs
}

/// One name to set: where, at which angle, its mask.
struct Setting {
    centre: (f64, f64),
    angle: f64,
    mask: Mask,
    kind: LabelKind,
}

/// Set the names of `names` that the rule places into `rgba`, `w` by `h`
/// pixels, at `scale` screen pixels a logical one, after `taken` (the pins
/// and their names, in screen pixels). Returns the names set, in the
/// order the rule placed them.
#[allow(clippy::too_many_arguments)]
pub fn set(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    names: &[ViewLabel],
    taken: &[Rect],
    scale: f32,
    theme: MapTheme,
    type_: &mut Typesetter,
) -> Vec<String> {
    let px = |v: (i64, i64)| (v.0 as f64 / SUBPIXEL as f64, v.1 as f64 / SUBPIXEL as f64);
    let pad = (HALO * scale).ceil() as usize + 1;
    let (wf, hf) = (f64::from(w), f64::from(h));
    let mut settings: Vec<Setting> = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for l in names {
        let river = l.kind == LabelKind::River;
        let mask = type_.mask(&l.text, size_of(l.kind) * scale, pad, river);
        let (tw, th) = ((mask.w - 2 * pad) as f64, (mask.h - 2 * pad) as f64);
        // Where the name may go, best first: a point's anchor, or each
        // straight run of a line long enough to hold it.
        let spots: Vec<((f64, f64), f64)> = match l.kind.is_line() {
            false => vec![(px(l.anchor), 0.0)],
            true => {
                let line: Vec<Vec<(f64, f64)>> = l
                    .line
                    .iter()
                    .map(|p| p.iter().map(|&v| px(v)).collect())
                    .collect();
                straight_runs(&line)
                    .into_iter()
                    .take_while(|&(_, _, len)| len >= tw + th)
                    .flat_map(|(mid, angle, len)| along_run(mid, angle, len, tw + th))
                    .collect()
            }
        };
        for (centre, angle) in spots {
            let boxes = boxes_of(centre, angle, tw, th);
            let whole = boxes
                .iter()
                .all(|b| b.x0 >= 0 && b.y0 >= 0 && f64::from(b.x1) <= wf && f64::from(b.y1) <= hf);
            if !whole {
                continue;
            }
            candidates.push(Candidate {
                kind: l.kind,
                rank: l.rank,
                text: l.text.clone(),
                at: l.at,
                boxes,
            });
            settings.push(Setting {
                centre,
                angle,
                mask: mask.clone(),
                kind: l.kind,
            });
        }
    }
    let rule = Rule {
        margin: (MARGIN * scale).round() as i32,
        block: (0, 0),
        repeat: (REPEAT * scale).round() as i32,
    };
    let placed = labels::place(&candidates, taken, rule);
    for &i in &placed {
        let s = &settings[i];
        let (ink, halo) = colours(s.kind, theme);
        let wide = s.mask.widened(HALO * scale);
        paint(rgba, w, h, &wide, s.centre, s.angle, halo);
        paint(rgba, w, h, &s.mask, s.centre, s.angle, ink);
    }
    placed.iter().map(|&i| candidates[i].text.clone()).collect()
}

/// The middles a name `need` pixels long may take on a straight run of
/// `len` pixels about `mid` at `angle`: the run's middle first, then half
/// a name further each way in turn while the name stays on the run.
fn along_run(mid: (f64, f64), angle: f64, len: f64, need: f64) -> Vec<((f64, f64), f64)> {
    let step = need / 2.0;
    let room = (len - need) / 2.0;
    let mut out = vec![(mid, angle)];
    let mut d = step;
    while d <= room {
        for along in [d, -d] {
            out.push((
                (mid.0 + along * angle.cos(), mid.1 + along * angle.sin()),
                angle,
            ));
        }
        d += step;
    }
    out
}

/// The boxes a name of `tw` by `th` pixels covers, its middle at `centre`,
/// turned by `angle`: one for a level name, squares of its height along
/// the baseline for a turned one.
fn boxes_of(centre: (f64, f64), angle: f64, tw: f64, th: f64) -> Vec<Rect> {
    match angle == 0.0 {
        true => vec![Rect {
            x0: (centre.0 - tw / 2.0).floor() as i32,
            y0: (centre.1 - th / 2.0).floor() as i32,
            x1: (centre.0 + tw / 2.0).ceil() as i32,
            y1: (centre.1 + th / 2.0).ceil() as i32,
        }],
        // Squares of the text's height along the baseline.
        false => {
            let n = (tw / th).ceil().max(1.0) as i32;
            (0..n)
                .map(|i| {
                    let along = -tw / 2.0 + th * (f64::from(i) + 0.5);
                    let (cx, cy) = (
                        centre.0 + along * angle.cos(),
                        centre.1 + along * angle.sin(),
                    );
                    Rect {
                        x0: (cx - th / 2.0).floor() as i32,
                        y0: (cy - th / 2.0).floor() as i32,
                        x1: (cx + th / 2.0).ceil() as i32,
                        y1: (cy + th / 2.0).ceil() as i32,
                    }
                })
                .collect()
        }
    }
}

/// Blend `mask` in `colour` into `rgba`, its middle at `centre`, turned by
/// `angle`: each pixel of the turned mask's box samples the mask through
/// the inverse turn.
fn paint(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    mask: &Mask,
    centre: (f64, f64),
    angle: f64,
    colour: [u8; 3],
) {
    paint_faded(rgba, w, h, mask, centre, angle, colour, 1.0);
}

/// [`paint`] at `opacity`: a pin's dimmed label.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_faded(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    mask: &Mask,
    centre: (f64, f64),
    angle: f64,
    colour: [u8; 3],
    opacity: f64,
) {
    let (mw, mh) = (mask.w as f64, mask.h as f64);
    let (c, s) = (angle.cos(), angle.sin());
    let reach = (mw.hypot(mh) / 2.0).ceil();
    let (x0, x1) = (
        (centre.0 - reach).floor().max(0.0),
        (centre.0 + reach).ceil().min(f64::from(w)),
    );
    let (y0, y1) = (
        (centre.1 - reach).floor().max(0.0),
        (centre.1 + reach).ceil().min(f64::from(h)),
    );
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let (dx, dy) = (x + 0.5 - centre.0, y + 0.5 - centre.1);
            // Into the mask's frame: turn back by the angle.
            let (mx, my) = (
                dx * c + dy * s + mw / 2.0 - 0.5,
                -dx * s + dy * c + mh / 2.0 - 0.5,
            );
            let a = match angle == 0.0 {
                true => f64::from(mask.at(mx.round() as i64, my.round() as i64)),
                false => mask.sample(mx, my),
            } / 255.0
                * opacity;
            if a > 0.0 {
                let i = ((y as usize) * (w as usize) + x as usize) * 4;
                if let Some(px) = rgba.get_mut(i..i + 3) {
                    for (p, &k) in px.iter_mut().zip(colour.iter()) {
                        *p = (f64::from(*p) * (1.0 - a) + f64::from(k) * a).round() as u8;
                    }
                }
            }
            x += 1.0;
        }
        y += 1.0;
    }
}
