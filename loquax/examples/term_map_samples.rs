//! Throwaway: the samples and the cost table of
//! `docs/drafts/terminal-map-rendering-2026-10.md` (1090). Not part of any
//! shipping graph: an example, built only with `--features picture`.
//!
//! ```text
//! cargo run --release -p loquax --features picture --example term_map_samples -- \
//!     /home/lew/scratch/1062 docs/drafts/terminal-map-samples
//! ```
//!
//! Every view is drawn once by the map core ([`Painter`]) into a source
//! frame of 4 x 8 pixels a cell, one zoom deeper than the view's name and at
//! twice the line width: the pixel density of today's braille at the named
//! zoom, at a finer grain. Each technique then encodes that same frame into
//! cells, so the samples differ by the encoding alone. The source frame is
//! drawn twice, in colour and as lines alone ([`Ink::Lines`]), because
//! braille and the box-drawing hybrid need to know which pixels are lines.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use embedded_graphics::pixelcolor::Rgb565;
use leviculum_map::draw::Ink;
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::pins::{labels, Mark, Metrics};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::frame::{rgb888, Frame};
use leviculum_mapfill::style::{area_color, line_style, Class, BACKGROUND};
use leviculum_ui::color::{rgb_to_ansi16, rgb_to_ansi256};
use loquax::picture::{load, Painter};

/// Source pixels across and down one cell.
const SX: usize = 4;
const SY: usize = 8;
const CELL_PX: usize = SX * SY;

type Rgb = [u8; 3];

/// One view drawn at the source density.
struct Source {
    cols: usize,
    rows: usize,
    rgb: Vec<Rgb>,
    line: Vec<bool>,
    /// The lines again at half the width: one braille dot.
    thin: Vec<bool>,
    attribution: String,
}

impl Source {
    fn px(&self, col: usize, row: usize, x: usize, y: usize) -> usize {
        (row * SY + y) * self.cols * SX + col * SX + x
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Cell {
    ch: char,
    fg: Option<Rgb>,
    bg: Option<Rgb>,
}

const BLANK: Cell = Cell {
    ch: ' ',
    fg: None,
    bg: None,
};

struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
}

#[derive(Clone, Copy, PartialEq)]
enum Technique {
    BrailleMono,
    BrailleColour,
    BrailleBg,
    BrailleThin,
    Half,
    Quadrant,
    Sextant,
    Octant,
    BlocksBest,
    AllBest,
    BoxHybrid,
    Mosaic,
}

const TECHNIQUES: [Technique; 12] = [
    Technique::BrailleMono,
    Technique::BrailleColour,
    Technique::BrailleBg,
    Technique::BrailleThin,
    Technique::Half,
    Technique::Quadrant,
    Technique::Sextant,
    Technique::Octant,
    Technique::BlocksBest,
    Technique::AllBest,
    Technique::BoxHybrid,
    Technique::Mosaic,
];

impl Technique {
    fn name(self) -> &'static str {
        match self {
            Self::BrailleMono => "braille-mono",
            Self::BrailleColour => "braille-colour",
            Self::BrailleBg => "braille-bg",
            Self::BrailleThin => "braille-thin",
            Self::Half => "half",
            Self::Quadrant => "quadrant",
            Self::Sextant => "sextant",
            Self::Octant => "octant",
            Self::BlocksBest => "blocks-best",
            Self::AllBest => "all-best",
            Self::BoxHybrid => "box-hybrid",
            Self::Mosaic => "mosaic",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tier {
    Truecolor,
    A256,
    A16,
}

impl Tier {
    fn suffix(self) -> &'static str {
        match self {
            Self::Truecolor => "",
            Self::A256 => "-256",
            Self::A16 => "-16",
        }
    }
}

/// A glyph and the source pixels it inks, bit `y * SX + x`.
#[derive(Clone, Copy)]
struct Glyph {
    ch: char,
    mask: u32,
}

fn rect(x0: usize, x1: usize, y0: usize, y1: usize) -> u32 {
    let mut m = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            m |= 1 << (y * SX + x);
        }
    }
    m
}

/// The quadrant characters by pattern, bit 1 top left, 2 top right, 4
/// bottom left, 8 bottom right.
const QUADRANTS: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

/// The sextant characters by pattern, bit `k - 1` for BLOCK SEXTANT-k,
/// generated from UnicodeData.txt 16.0; the four patterns encoded before
/// Unicode 13 are space, the full block and the two half blocks.
const SEXTANTS: [char; 64] = [
    '\u{20}',
    '\u{1FB00}',
    '\u{1FB01}',
    '\u{1FB02}',
    '\u{1FB03}',
    '\u{1FB04}',
    '\u{1FB05}',
    '\u{1FB06}',
    '\u{1FB07}',
    '\u{1FB08}',
    '\u{1FB09}',
    '\u{1FB0A}',
    '\u{1FB0B}',
    '\u{1FB0C}',
    '\u{1FB0D}',
    '\u{1FB0E}',
    '\u{1FB0F}',
    '\u{1FB10}',
    '\u{1FB11}',
    '\u{1FB12}',
    '\u{1FB13}',
    '\u{258C}',
    '\u{1FB14}',
    '\u{1FB15}',
    '\u{1FB16}',
    '\u{1FB17}',
    '\u{1FB18}',
    '\u{1FB19}',
    '\u{1FB1A}',
    '\u{1FB1B}',
    '\u{1FB1C}',
    '\u{1FB1D}',
    '\u{1FB1E}',
    '\u{1FB1F}',
    '\u{1FB20}',
    '\u{1FB21}',
    '\u{1FB22}',
    '\u{1FB23}',
    '\u{1FB24}',
    '\u{1FB25}',
    '\u{1FB26}',
    '\u{1FB27}',
    '\u{2590}',
    '\u{1FB28}',
    '\u{1FB29}',
    '\u{1FB2A}',
    '\u{1FB2B}',
    '\u{1FB2C}',
    '\u{1FB2D}',
    '\u{1FB2E}',
    '\u{1FB2F}',
    '\u{1FB30}',
    '\u{1FB31}',
    '\u{1FB32}',
    '\u{1FB33}',
    '\u{1FB34}',
    '\u{1FB35}',
    '\u{1FB36}',
    '\u{1FB37}',
    '\u{1FB38}',
    '\u{1FB39}',
    '\u{1FB3A}',
    '\u{1FB3B}',
    '\u{2588}',
];
/// The octant characters by pattern, bit `k - 1` for BLOCK OCTANT-k,
/// generated from UnicodeData.txt 16.0; the 26 patterns encoded elsewhere
/// (space, full, halves, quadrants, quarter blocks) take those characters.
const OCTANTS: [char; 256] = [
    '\u{20}',
    '\u{1CEA8}',
    '\u{1CEAB}',
    '\u{1FB82}',
    '\u{1CD00}',
    '\u{2598}',
    '\u{1CD01}',
    '\u{1CD02}',
    '\u{1CD03}',
    '\u{1CD04}',
    '\u{259D}',
    '\u{1CD05}',
    '\u{1CD06}',
    '\u{1CD07}',
    '\u{1CD08}',
    '\u{2580}',
    '\u{1CD09}',
    '\u{1CD0A}',
    '\u{1CD0B}',
    '\u{1CD0C}',
    '\u{1FBE6}',
    '\u{1CD0D}',
    '\u{1CD0E}',
    '\u{1CD0F}',
    '\u{1CD10}',
    '\u{1CD11}',
    '\u{1CD12}',
    '\u{1CD13}',
    '\u{1CD14}',
    '\u{1CD15}',
    '\u{1CD16}',
    '\u{1CD17}',
    '\u{1CD18}',
    '\u{1CD19}',
    '\u{1CD1A}',
    '\u{1CD1B}',
    '\u{1CD1C}',
    '\u{1CD1D}',
    '\u{1CD1E}',
    '\u{1CD1F}',
    '\u{1FBE7}',
    '\u{1CD20}',
    '\u{1CD21}',
    '\u{1CD22}',
    '\u{1CD23}',
    '\u{1CD24}',
    '\u{1CD25}',
    '\u{1CD26}',
    '\u{1CD27}',
    '\u{1CD28}',
    '\u{1CD29}',
    '\u{1CD2A}',
    '\u{1CD2B}',
    '\u{1CD2C}',
    '\u{1CD2D}',
    '\u{1CD2E}',
    '\u{1CD2F}',
    '\u{1CD30}',
    '\u{1CD31}',
    '\u{1CD32}',
    '\u{1CD33}',
    '\u{1CD34}',
    '\u{1CD35}',
    '\u{1FB85}',
    '\u{1CEA3}',
    '\u{1CD36}',
    '\u{1CD37}',
    '\u{1CD38}',
    '\u{1CD39}',
    '\u{1CD3A}',
    '\u{1CD3B}',
    '\u{1CD3C}',
    '\u{1CD3D}',
    '\u{1CD3E}',
    '\u{1CD3F}',
    '\u{1CD40}',
    '\u{1CD41}',
    '\u{1CD42}',
    '\u{1CD43}',
    '\u{1CD44}',
    '\u{2596}',
    '\u{1CD45}',
    '\u{1CD46}',
    '\u{1CD47}',
    '\u{1CD48}',
    '\u{258C}',
    '\u{1CD49}',
    '\u{1CD4A}',
    '\u{1CD4B}',
    '\u{1CD4C}',
    '\u{259E}',
    '\u{1CD4D}',
    '\u{1CD4E}',
    '\u{1CD4F}',
    '\u{1CD50}',
    '\u{259B}',
    '\u{1CD51}',
    '\u{1CD52}',
    '\u{1CD53}',
    '\u{1CD54}',
    '\u{1CD55}',
    '\u{1CD56}',
    '\u{1CD57}',
    '\u{1CD58}',
    '\u{1CD59}',
    '\u{1CD5A}',
    '\u{1CD5B}',
    '\u{1CD5C}',
    '\u{1CD5D}',
    '\u{1CD5E}',
    '\u{1CD5F}',
    '\u{1CD60}',
    '\u{1CD61}',
    '\u{1CD62}',
    '\u{1CD63}',
    '\u{1CD64}',
    '\u{1CD65}',
    '\u{1CD66}',
    '\u{1CD67}',
    '\u{1CD68}',
    '\u{1CD69}',
    '\u{1CD6A}',
    '\u{1CD6B}',
    '\u{1CD6C}',
    '\u{1CD6D}',
    '\u{1CD6E}',
    '\u{1CD6F}',
    '\u{1CD70}',
    '\u{1CEA0}',
    '\u{1CD71}',
    '\u{1CD72}',
    '\u{1CD73}',
    '\u{1CD74}',
    '\u{1CD75}',
    '\u{1CD76}',
    '\u{1CD77}',
    '\u{1CD78}',
    '\u{1CD79}',
    '\u{1CD7A}',
    '\u{1CD7B}',
    '\u{1CD7C}',
    '\u{1CD7D}',
    '\u{1CD7E}',
    '\u{1CD7F}',
    '\u{1CD80}',
    '\u{1CD81}',
    '\u{1CD82}',
    '\u{1CD83}',
    '\u{1CD84}',
    '\u{1CD85}',
    '\u{1CD86}',
    '\u{1CD87}',
    '\u{1CD88}',
    '\u{1CD89}',
    '\u{1CD8A}',
    '\u{1CD8B}',
    '\u{1CD8C}',
    '\u{1CD8D}',
    '\u{1CD8E}',
    '\u{1CD8F}',
    '\u{2597}',
    '\u{1CD90}',
    '\u{1CD91}',
    '\u{1CD92}',
    '\u{1CD93}',
    '\u{259A}',
    '\u{1CD94}',
    '\u{1CD95}',
    '\u{1CD96}',
    '\u{1CD97}',
    '\u{2590}',
    '\u{1CD98}',
    '\u{1CD99}',
    '\u{1CD9A}',
    '\u{1CD9B}',
    '\u{259C}',
    '\u{1CD9C}',
    '\u{1CD9D}',
    '\u{1CD9E}',
    '\u{1CD9F}',
    '\u{1CDA0}',
    '\u{1CDA1}',
    '\u{1CDA2}',
    '\u{1CDA3}',
    '\u{1CDA4}',
    '\u{1CDA5}',
    '\u{1CDA6}',
    '\u{1CDA7}',
    '\u{1CDA8}',
    '\u{1CDA9}',
    '\u{1CDAA}',
    '\u{1CDAB}',
    '\u{2582}',
    '\u{1CDAC}',
    '\u{1CDAD}',
    '\u{1CDAE}',
    '\u{1CDAF}',
    '\u{1CDB0}',
    '\u{1CDB1}',
    '\u{1CDB2}',
    '\u{1CDB3}',
    '\u{1CDB4}',
    '\u{1CDB5}',
    '\u{1CDB6}',
    '\u{1CDB7}',
    '\u{1CDB8}',
    '\u{1CDB9}',
    '\u{1CDBA}',
    '\u{1CDBB}',
    '\u{1CDBC}',
    '\u{1CDBD}',
    '\u{1CDBE}',
    '\u{1CDBF}',
    '\u{1CDC0}',
    '\u{1CDC1}',
    '\u{1CDC2}',
    '\u{1CDC3}',
    '\u{1CDC4}',
    '\u{1CDC5}',
    '\u{1CDC6}',
    '\u{1CDC7}',
    '\u{1CDC8}',
    '\u{1CDC9}',
    '\u{1CDCA}',
    '\u{1CDCB}',
    '\u{1CDCC}',
    '\u{1CDCD}',
    '\u{1CDCE}',
    '\u{1CDCF}',
    '\u{1CDD0}',
    '\u{1CDD1}',
    '\u{1CDD2}',
    '\u{1CDD3}',
    '\u{1CDD4}',
    '\u{1CDD5}',
    '\u{1CDD6}',
    '\u{1CDD7}',
    '\u{1CDD8}',
    '\u{1CDD9}',
    '\u{1CDDA}',
    '\u{2584}',
    '\u{1CDDB}',
    '\u{1CDDC}',
    '\u{1CDDD}',
    '\u{1CDDE}',
    '\u{2599}',
    '\u{1CDDF}',
    '\u{1CDE0}',
    '\u{1CDE1}',
    '\u{1CDE2}',
    '\u{259F}',
    '\u{1CDE3}',
    '\u{2586}',
    '\u{1CDE4}',
    '\u{1CDE5}',
    '\u{2588}',
];

/// Quadrants plus the eighth blocks the 4 x 8 grid can hold: the set
/// DejaVu Sans Mono, Hack, Noto Sans Mono and JetBrains Mono all carry.
fn blocks() -> Vec<Glyph> {
    let mut g: Vec<Glyph> = (0..16)
        .map(|p| {
            let mut mask = 0;
            for (bit, (x0, y0)) in [(0, 0), (2, 0), (0, 4), (2, 4)].into_iter().enumerate() {
                if p & (1 << bit) != 0 {
                    mask |= rect(x0, x0 + 2, y0, y0 + 4);
                }
            }
            Glyph {
                ch: QUADRANTS[p],
                mask,
            }
        })
        .collect();
    for (ch, k) in ['▁', '▂', '▃', '▅', '▆', '▇']
        .into_iter()
        .zip([1, 2, 3, 5, 6, 7])
    {
        g.push(Glyph {
            ch,
            mask: rect(0, SX, SY - k, SY),
        });
    }
    g.push(Glyph {
        ch: '▔',
        mask: rect(0, SX, 0, 1),
    });
    g.push(Glyph {
        ch: '▎',
        mask: rect(0, 1, 0, SY),
    });
    g.push(Glyph {
        ch: '▊',
        mask: rect(0, 3, 0, SY),
    });
    g
}

fn halves() -> Vec<Glyph> {
    vec![
        Glyph { ch: ' ', mask: 0 },
        Glyph {
            ch: '▀',
            mask: rect(0, SX, 0, SY / 2),
        },
    ]
}

fn quadrants() -> Vec<Glyph> {
    blocks().into_iter().take(16).collect()
}

/// Sextant rows on the 8-pixel cell: 3, 3 and 2 pixels.
const SEXTANT_ROWS: [(usize, usize); 3] = [(0, 3), (3, 6), (6, 8)];

fn sextants() -> Vec<Glyph> {
    (0..64)
        .map(|p| {
            let mut mask = 0;
            for k in 0..6 {
                if p & (1 << k) != 0 {
                    let (y0, y1) = SEXTANT_ROWS[k / 2];
                    let x0 = (k % 2) * 2;
                    mask |= rect(x0, x0 + 2, y0, y1);
                }
            }
            Glyph {
                ch: SEXTANTS[p],
                mask,
            }
        })
        .collect()
}

fn octants() -> Vec<Glyph> {
    (0..256)
        .map(|p| {
            let mut mask = 0;
            for k in 0..8 {
                if p & (1 << k) != 0 {
                    let (x0, y0) = ((k % 2) * 2, (k / 2) * 2);
                    mask |= rect(x0, x0 + 2, y0, y0 + 2);
                }
            }
            Glyph {
                ch: OCTANTS[p],
                mask,
            }
        })
        .collect()
}

/// Every set together, a mask kept once, the older character first.
fn all_glyphs() -> Vec<Glyph> {
    let mut out: Vec<Glyph> = Vec::new();
    for g in blocks().into_iter().chain(sextants()).chain(octants()) {
        if !out.iter().any(|o| o.mask == g.mask) {
            out.push(g);
        }
    }
    out
}

/// The glyph of `set` and the two colours that fit one cell's pixels with
/// the least squared error, the way chafa chooses its symbols.
fn fit(px: &[Rgb; CELL_PX], set: &[Glyph]) -> Cell {
    let mut total = [0i64; 3];
    for p in px {
        for c in 0..3 {
            total[c] += i64::from(p[c]);
        }
    }
    let n = CELL_PX as i64;
    let mut best = (i64::MIN, set[0], total, 0i64);
    for g in set {
        let mut sf = [0i64; 3];
        let mut nf = 0i64;
        let mut m = g.mask;
        while m != 0 {
            let i = m.trailing_zeros() as usize;
            m &= m - 1;
            nf += 1;
            for c in 0..3 {
                sf[c] += i64::from(px[i][c]);
            }
        }
        let nb = n - nf;
        // Maximising sum |S|^2 / n over both parts minimises the error;
        // scaled by n so the comparison stays integral.
        let part = |s: [i64; 3], k: i64| match k {
            0 => 0,
            k => (s[0] * s[0] + s[1] * s[1] + s[2] * s[2]) * n / k,
        };
        let sb = [total[0] - sf[0], total[1] - sf[1], total[2] - sf[2]];
        let score = part(sf, nf) + part(sb, nb);
        if score > best.0 {
            best = (score, *g, sf, nf);
        }
    }
    let (_, g, sf, nf) = best;
    let mean = |s: [i64; 3], k: i64| -> Rgb {
        let k = k.max(1);
        [(s[0] / k) as u8, (s[1] / k) as u8, (s[2] / k) as u8]
    };
    let sb = [total[0] - sf[0], total[1] - sf[1], total[2] - sf[2]];
    let bg = mean(sb, n - nf);
    let fg = match nf {
        0 => bg,
        _ => mean(sf, nf),
    };
    let bg = match nf {
        k if k == n => fg,
        _ => bg,
    };
    Cell {
        ch: g.ch,
        fg: Some(fg),
        bg: Some(bg),
    }
}

/// The most frequent colour of `it`.
fn mode(it: impl Iterator<Item = Rgb>) -> Option<Rgb> {
    let mut seen: Vec<(Rgb, u32)> = Vec::new();
    for c in it {
        match seen.iter_mut().find(|(s, _)| *s == c) {
            Some((_, k)) => *k += 1,
            None => seen.push((c, 1)),
        }
    }
    seen.into_iter().max_by_key(|&(_, k)| k).map(|(c, _)| c)
}

/// The braille character for dots `dots[dx][dy]`.
fn braille(dot: impl Fn(usize, usize) -> bool) -> char {
    const BITS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
    let mut v = 0;
    for (dx, col) in BITS.iter().enumerate() {
        for (dy, bit) in col.iter().enumerate() {
            if dot(dx, dy) {
                v |= bit;
            }
        }
    }
    char::from_u32(0x2800 + v).unwrap_or(' ')
}

/// The box-drawing character joining the edges `t r b l` (bits 1 2 4 8).
fn box_char(edges: usize, heavy: bool) -> char {
    const LIGHT: [char; 16] = [
        ' ', '╵', '╶', '└', '╷', '│', '┌', '├', '╴', '┘', '─', '┴', '┐', '┤', '┬', '┼',
    ];
    const HEAVY: [char; 16] = [
        ' ', '╹', '╺', '┗', '╻', '┃', '┏', '┣', '╸', '┛', '━', '┻', '┓', '┫', '┳', '╋',
    ];
    match heavy {
        true => HEAVY[edges & 15],
        false => LIGHT[edges & 15],
    }
}

fn rgb_of(c: Rgb565) -> Rgb {
    rgb888(c)
}

/// The line colours of the style, which no area colour shares.
fn line_colours() -> Vec<Rgb> {
    [
        Class::Waterway,
        Class::Path,
        Class::MinorRoad,
        Class::MajorRoad,
        Class::Rail,
    ]
    .into_iter()
    .map(|c| rgb_of(line_style(c).0))
    .collect()
}

/// What a line pixel that is no line class (a coast, an outline) is inked.
const COAST: Rgb = [70, 110, 160];

/// A line's colour as braille dots: a dot is a few pixels of a glyph, so
/// the white of the minor roads vanishes on the light ground; grey keeps it.
fn braille_ink(c: Rgb) -> Rgb {
    match c == rgb_of(line_style(Class::MinorRoad).0) {
        true => [128, 128, 132],
        false => c,
    }
}

fn encode(src: &Source, t: Technique, sets: &Sets) -> Grid {
    let lines = line_colours();
    let major = rgb_of(line_style(Class::MajorRoad).0);
    let mut cells = Vec::with_capacity(src.cols * src.rows);
    let mut block = [[0u8; 3]; CELL_PX];
    for row in 0..src.rows {
        for col in 0..src.cols {
            for y in 0..SY {
                for x in 0..SX {
                    block[y * SX + x] = src.rgb[src.px(col, row, x, y)];
                }
            }
            let line = |x: usize, y: usize| src.line[src.px(col, row, x, y)];
            let thin = |x: usize, y: usize| src.thin[src.px(col, row, x, y)];
            let line_rgb = || {
                (0..CELL_PX).filter(|&i| line(i % SX, i / SX)).map(|i| {
                    match lines.contains(&block[i]) {
                        true => braille_ink(block[i]),
                        false => COAST,
                    }
                })
            };
            let area_rgb = || {
                (0..CELL_PX)
                    .filter(|&i| !line(i % SX, i / SX))
                    .map(|i| block[i])
            };
            let dots_of = |on: &dyn Fn(usize, usize) -> bool| {
                braille(|dx, dy| (0..2).any(|y| (0..2).any(|x| on(dx * 2 + x, dy * 2 + y))))
            };
            let dots = || dots_of(&line);
            let cell = match t {
                Technique::BrailleMono => Cell {
                    ch: dots(),
                    ..BLANK
                },
                Technique::BrailleColour => Cell {
                    ch: dots(),
                    fg: mode(line_rgb()),
                    bg: None,
                },
                Technique::BrailleBg => Cell {
                    ch: dots(),
                    fg: mode(line_rgb()),
                    bg: mode(area_rgb()).or(mode(block.iter().copied())),
                },
                Technique::BrailleThin => {
                    let ink = (0..CELL_PX).filter(|&i| thin(i % SX, i / SX)).map(|i| {
                        match lines.contains(&block[i]) {
                            true => braille_ink(block[i]),
                            false => COAST,
                        }
                    });
                    Cell {
                        ch: dots_of(&thin),
                        fg: mode(ink),
                        bg: mode(area_rgb()).or(mode(block.iter().copied())),
                    }
                }
                Technique::Half => fit(&block, &sets.half),
                Technique::Quadrant => fit(&block, &sets.quadrant),
                Technique::Sextant => fit(&block, &sets.sextant),
                Technique::Octant => fit(&block, &sets.octant),
                Technique::BlocksBest => fit(&block, &sets.blocks),
                Technique::AllBest => fit(&block, &sets.all),
                // No glyph at all: one pixel a cell, the cell's own colour.
                Technique::Mosaic => Cell {
                    ch: ' ',
                    fg: None,
                    bg: mode(block.iter().copied()),
                },
                Technique::BoxHybrid => {
                    let any = |pts: &[(usize, usize)]| pts.iter().any(|&(x, y)| line(x, y));
                    let edges = usize::from(any(&[(1, 0), (2, 0)]))
                        | usize::from(any(&[(3, 3), (3, 4)])) << 1
                        | usize::from(any(&[(1, 7), (2, 7)])) << 2
                        | usize::from(any(&[(0, 3), (0, 4)])) << 3;
                    let fg = mode(line_rgb());
                    let bg = mode(area_rgb()).or(mode(block.iter().copied()));
                    let ch = match (edges, fg) {
                        (0, Some(_)) => dots(),
                        (0, None) => ' ',
                        (e, _) => box_char(e, fg == Some(major)),
                    };
                    Cell { ch, fg, bg }
                }
            };
            cells.push(cell);
        }
    }
    Grid {
        cols: src.cols,
        rows: src.rows,
        cells,
    }
}

struct Sets {
    half: Vec<Glyph>,
    quadrant: Vec<Glyph>,
    sextant: Vec<Glyph>,
    octant: Vec<Glyph>,
    blocks: Vec<Glyph>,
    all: Vec<Glyph>,
}

/// One place to mark.
struct Place {
    name: &'static str,
    lat: f64,
    lon: f64,
}

/// Pins set over the picture with the core's label placement, a marker in
/// red and the name dark on white, the way loquax sets a pin's label.
fn pin(grid: &mut Grid, view: &Viewport, places: &[Place], dark: bool) {
    let mut marks = Vec::new();
    let mut names = Vec::new();
    for p in places {
        let (x, y) = view.to_screen(project(LatLon::from_degrees(p.lat, p.lon)));
        let (col, row) = (x / SX as i64, y / SY as i64);
        if (0..grid.cols as i64).contains(&col) && (0..grid.rows as i64).contains(&row) {
            marks.push(Mark {
                x: col as u32,
                y: row as u32,
            });
            names.push(p.name);
        }
    }
    let placed = labels(
        &marks,
        |i| names[i].chars().count() as u32,
        None,
        (grid.cols as u32, grid.rows as u32),
        Metrics::CELLS,
    );
    let (ink, paper) = match dark {
        true => ([235, 235, 235], [20, 22, 26]),
        false => ([20, 20, 20], [255, 255, 255]),
    };
    for (i, m) in marks.iter().enumerate() {
        let at = m.y as usize * grid.cols + m.x as usize;
        let under = grid.cells[at].bg;
        grid.cells[at] = Cell {
            ch: '●',
            fg: Some([210, 40, 40]),
            bg: under,
        };
        if let Some(Some(l)) = placed.get(i) {
            for (k, ch) in names[i].chars().enumerate() {
                let x = l.x as usize + k;
                if x < grid.cols && (l.y as usize) < grid.rows {
                    grid.cells[l.y as usize * grid.cols + x] = Cell {
                        ch,
                        fg: Some(ink),
                        bg: Some(paper),
                    };
                }
            }
        }
    }
}

fn sgr(out: &mut String, c: Option<Rgb>, fg: bool, tier: Tier) {
    match (c, tier) {
        (None, _) => out.push_str(if fg { "\x1b[39m" } else { "\x1b[49m" }),
        (Some([r, g, b]), Tier::Truecolor) => {
            let _ = write!(out, "\x1b[{};2;{r};{g};{b}m", if fg { 38 } else { 48 });
        }
        (Some([r, g, b]), Tier::A256) => {
            let _ = write!(
                out,
                "\x1b[{};5;{}m",
                if fg { 38 } else { 48 },
                rgb_to_ansi256(r, g, b)
            );
        }
        (Some([r, g, b]), Tier::A16) => {
            let i = rgb_to_ansi16(r, g, b);
            let base = match (fg, i < 8) {
                (true, true) => 30,
                (true, false) => 90 - 8,
                (false, true) => 40,
                (false, false) => 100 - 8,
            };
            let _ = write!(out, "\x1b[{}m", base + i);
        }
    }
}

/// The grid as ANSI: a colour sent only where it changes, each row ended
/// with a reset so the terminal's own colours fill the rest of the line.
fn ansi(grid: &Grid, tier: Tier, out: &mut String) {
    for row in 0..grid.rows {
        let (mut fg, mut bg): (Option<Option<Rgb>>, Option<Option<Rgb>>) = (None, None);
        for col in 0..grid.cols {
            let c = grid.cells[row * grid.cols + col];
            // A blank shows no foreground; keep the last one.
            if c.ch != ' ' && fg != Some(c.fg) {
                sgr(out, c.fg, true, tier);
                fg = Some(c.fg);
            }
            if bg != Some(c.bg) {
                sgr(out, c.bg, false, tier);
                bg = Some(c.bg);
            }
            out.push(c.ch);
        }
        out.push_str("\x1b[0m\n");
    }
}

/// The style's colours on a dark terminal: the light table remapped, one
/// colour for one colour, so a cell keeps its class.
fn darken(c: Rgb) -> Rgb {
    let table: [(Rgb565, Rgb); 13] = [
        (BACKGROUND, [28, 30, 34]),
        (area_color(Class::Earth), [28, 30, 34]),
        (area_color(Class::Urban), [44, 46, 52]),
        (area_color(Class::Park), [36, 66, 42]),
        (area_color(Class::Pedestrian), [52, 52, 62]),
        (area_color(Class::Sand), [78, 72, 48]),
        (area_color(Class::Water), [26, 52, 92]),
        (area_color(Class::Building), [66, 62, 60]),
        (line_style(Class::Waterway).0, [70, 120, 190]),
        (line_style(Class::Path).0, [110, 104, 98]),
        (line_style(Class::MinorRoad).0, [150, 150, 158]),
        (line_style(Class::MajorRoad).0, [222, 164, 64]),
        (line_style(Class::Rail).0, [170, 170, 170]),
    ];
    table
        .iter()
        .find(|(light, _)| rgb_of(*light) == c)
        .map_or(c, |&(_, d)| d)
}

/// The style's colours as the sixteen ANSI colours chosen per class, the
/// way a 16-colour token names an index itself rather than taking the
/// nearest one, which turns parks and the river into grey.
fn tokens16(c: Rgb) -> Rgb {
    let x = |i: usize| {
        let (r, g, b) = leviculum_ui::color::XTERM_16[i];
        [r, g, b]
    };
    let table: [(Rgb565, usize); 13] = [
        (BACKGROUND, 7),
        (area_color(Class::Earth), 7),
        (area_color(Class::Urban), 7),
        (area_color(Class::Park), 2),
        (area_color(Class::Pedestrian), 7),
        (area_color(Class::Sand), 11),
        (area_color(Class::Water), 12),
        (area_color(Class::Building), 8),
        (line_style(Class::Waterway).0, 4),
        (line_style(Class::Path).0, 8),
        (line_style(Class::MinorRoad).0, 15),
        (line_style(Class::MajorRoad).0, 3),
        (line_style(Class::Rail).0, 0),
    ];
    table
        .iter()
        .find(|(light, _)| rgb_of(*light) == c)
        .map_or(c, |&(_, i)| x(i))
}

fn draw(painter: &mut Painter, view: &Viewport, ink: Ink, percent: u32) -> (Vec<Rgb>, String) {
    let (w, h) = view.size();
    let mut drawn = painter.draw(view, ink, percent);
    let mut flat = Vec::with_capacity(drawn.fb.len() * 3);
    if let Some(f) = Frame::new(&mut drawn.fb, w, h) {
        f.rgb888_into(&mut flat);
    }
    let rgb = flat.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    (rgb, drawn.attribution)
}

/// The source frame of the view named `zoom`, `cols` x `rows` cells.
fn source(painter: &mut Painter, centre: LatLon, zoom: u8, cols: usize, rows: usize) -> Source {
    let view = source_view(centre, zoom, cols, rows);
    let (rgb, attribution) = draw(painter, &view, Ink::Colour, 200);
    let (lines, _) = draw(painter, &view, Ink::Lines(Rgb565::new(0, 0, 0)), 200);
    let (thin, _) = draw(painter, &view, Ink::Lines(Rgb565::new(0, 0, 0)), 100);
    let bg = rgb_of(BACKGROUND);
    Source {
        cols,
        rows,
        rgb,
        line: lines.iter().map(|&c| c != bg).collect(),
        thin: thin.iter().map(|&c| c != bg).collect(),
        attribution,
    }
}

fn source_view(centre: LatLon, zoom: u8, cols: usize, rows: usize) -> Viewport {
    Viewport::new(
        project(centre),
        zoom + 1,
        (cols * SX) as u32,
        (rows * SY) as u32,
    )
}

struct ViewSpec {
    name: &'static str,
    lat: f64,
    lon: f64,
    zoom: u8,
    places: &'static [Place],
}

const FRANKFURT: &[Place] = &[
    Place {
        name: "Hauptbahnhof",
        lat: 50.1071,
        lon: 8.6638,
    },
    Place {
        name: "Römer",
        lat: 50.1106,
        lon: 8.6821,
    },
    Place {
        name: "Messe",
        lat: 50.1117,
        lon: 8.6466,
    },
    Place {
        name: "Paulskirche",
        lat: 50.1115,
        lon: 8.6809,
    },
    Place {
        name: "Sachsenhausen",
        lat: 50.1010,
        lon: 8.6880,
    },
];

const BERLIN: &[Place] = &[
    Place {
        name: "Brandenburger Tor",
        lat: 52.5163,
        lon: 13.3777,
    },
    Place {
        name: "Alexanderplatz",
        lat: 52.5219,
        lon: 13.4132,
    },
    Place {
        name: "Hauptbahnhof",
        lat: 52.5251,
        lon: 13.3694,
    },
    Place {
        name: "Fernsehturm",
        lat: 52.5208,
        lon: 13.4094,
    },
    Place {
        name: "Tempelhof",
        lat: 52.4730,
        lon: 13.4039,
    },
];

const GERMANY: &[Place] = &[
    Place {
        name: "Frankfurt",
        lat: 50.1109,
        lon: 8.6821,
    },
    Place {
        name: "Berlin",
        lat: 52.5200,
        lon: 13.4050,
    },
    Place {
        name: "München",
        lat: 48.1351,
        lon: 11.5820,
    },
    Place {
        name: "Hamburg",
        lat: 53.5511,
        lon: 9.9937,
    },
];

const VIEWS: [ViewSpec; 8] = [
    ViewSpec {
        name: "frankfurt-z11",
        lat: 50.1109,
        lon: 8.6821,
        zoom: 11,
        places: FRANKFURT,
    },
    ViewSpec {
        name: "frankfurt-z13",
        lat: 50.1090,
        lon: 8.6721,
        zoom: 13,
        places: FRANKFURT,
    },
    ViewSpec {
        name: "frankfurt-z15",
        lat: 50.1104,
        lon: 8.6815,
        zoom: 15,
        places: FRANKFURT,
    },
    ViewSpec {
        name: "berlin-z11",
        lat: 52.5100,
        lon: 13.4000,
        zoom: 11,
        places: BERLIN,
    },
    ViewSpec {
        name: "berlin-z13",
        lat: 52.5190,
        lon: 13.4000,
        zoom: 13,
        places: BERLIN,
    },
    ViewSpec {
        name: "berlin-z15",
        lat: 52.5200,
        lon: 13.4080,
        zoom: 15,
        places: BERLIN,
    },
    ViewSpec {
        name: "germany-z5",
        lat: 51.2,
        lon: 10.4,
        zoom: 5,
        places: GERMANY,
    },
    ViewSpec {
        name: "europe-z3",
        lat: 51.0,
        lon: 10.0,
        zoom: 3,
        places: GERMANY,
    },
];

/// Picture rows of the two sizes; one more row holds the caption.
const SIZES: [(usize, usize); 2] = [(80, 23), (155, 64)];

fn caption(grid: &Grid, text: &str, out: &mut String) {
    let mut line: String = text.chars().take(grid.cols).collect();
    while line.chars().count() < grid.cols {
        line.push(' ');
    }
    let _ = writeln!(out, "\x1b[0;7m{line}\x1b[0m");
}

fn render_file(
    src: &Source,
    spec: &ViewSpec,
    t: Technique,
    tier: Tier,
    palette: Palette,
    sets: &Sets,
) -> String {
    let dark = palette == Palette::Dark;
    let src = match palette {
        Palette::Dark | Palette::Tokens16 => Source {
            rgb: src
                .rgb
                .iter()
                .map(|&c| match palette {
                    Palette::Dark => darken(c),
                    _ => tokens16(c),
                })
                .collect(),
            line: src.line.clone(),
            thin: src.thin.clone(),
            attribution: src.attribution.clone(),
            ..*src
        },
        Palette::Light => Source {
            rgb: src.rgb.clone(),
            line: src.line.clone(),
            thin: src.thin.clone(),
            attribution: src.attribution.clone(),
            ..*src
        },
    };
    let mut grid = encode(&src, t, sets);
    let view = source_view(
        LatLon::from_degrees(spec.lat, spec.lon),
        spec.zoom,
        src.cols,
        src.rows,
    );
    pin(&mut grid, &view, spec.places, dark);
    let mut out = String::new();
    ansi(&grid, tier, &mut out);
    let tier_name = match tier {
        Tier::Truecolor => "truecolor",
        Tier::A256 => "256",
        Tier::A16 => "16",
    };
    caption(
        &grid,
        &format!(
            " {} {} {} {}{} {}",
            t.name(),
            spec.name,
            tier_name,
            match palette {
                Palette::Light => "",
                Palette::Dark => "dark ",
                Palette::Tokens16 => "class-tokens ",
            },
            format_args!("{}x{}", grid.cols, grid.rows + 1),
            src.attribution
        ),
        &mut out,
    );
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Palette {
    Light,
    Dark,
    Tokens16,
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// A line per set, each glyph family as it should look, so a reader sees
/// at a glance which ones the font draws.
fn glyph_test() -> String {
    let rows: [(&str, String); 7] = [
        ("half blocks  ", "▀▄█▌▐ ▀▄▀▄▀▄".to_string()),
        ("quadrants    ", QUADRANTS.iter().collect()),
        ("eighths      ", "▁▂▃▄▅▆▇█ ▔▎▊".to_string()),
        (
            "braille      ",
            (0x2800..0x2810)
                .chain([0x28FF, 0x2847, 0x28B8])
                .filter_map(char::from_u32)
                .collect(),
        ),
        ("box drawing  ", "─│┌┐└┘├┤┬┴┼ ━┃┏┓┗┛╋".to_string()),
        ("sextants U13 ", SEXTANTS[1..20].iter().collect()),
        ("octants U16  ", OCTANTS[4..24].iter().collect()),
    ];
    let mut s = String::from(
        "Which glyph sets does this terminal draw? A box, a question mark or a gap means no.\n",
    );
    for (name, glyphs) in rows {
        let _ = writeln!(s, "  {name} {glyphs}");
    }
    s
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(maps), Some(out)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: term_map_samples <maps dir> <out dir> [--large-to <dir>]");
        std::process::exit(2);
    };
    let large_dir = args
        .iter()
        .position(|a| a == "--large-to")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let out = Path::new(out);
    let samples = out.join("samples");
    let large = large_dir.clone().unwrap_or_else(|| samples.clone());
    for d in [&samples, &large] {
        if let Err(e) = std::fs::create_dir_all(d) {
            eprintln!("{}: {e}", d.display());
            std::process::exit(1);
        }
    }
    let (regions, problems) = load(Path::new(maps));
    for p in &problems {
        eprintln!("{p}");
    }
    let mut painter = Painter::new(regions);
    let sets = Sets {
        half: halves(),
        quadrant: quadrants(),
        sextant: sextants(),
        octant: octants(),
        blocks: blocks(),
        all: all_glyphs(),
    };
    let write = |path: PathBuf, s: &str| {
        if let Err(e) = std::fs::write(&path, s) {
            eprintln!("{}: {e}", path.display());
        }
    };
    let mut sheet = String::new();
    let mut costs = String::from(
        "size technique render_ms(source 4x8, colour+lines) encode_ms bytes_truecolor bytes_256 \
         bytes_16 cells_changed_on_one_cell_pan\n",
    );
    let mut native = String::from("size view render_ms(1x2 at z) render_ms(2x4 at z, lines)\n");
    for spec in &VIEWS {
        let centre = LatLon::from_degrees(spec.lat, spec.lon);
        for &(cols, rows) in &SIZES {
            let src = source(&mut painter, centre, spec.zoom, cols, rows);
            let big = cols != SIZES[0].0;
            let dir = if big { &large } else { &samples };
            let size = if big {
                format!("-{}x{}", cols, rows + 1)
            } else {
                String::new()
            };
            for &t in &TECHNIQUES {
                let s = render_file(&src, spec, t, Tier::Truecolor, Palette::Light, &sets);
                write(
                    dir.join(format!("{}-{}{size}.ans", t.name(), spec.name)),
                    &s,
                );
                if !big {
                    let _ = writeln!(sheet, "\x1b[0m== {} / {} ==", t.name(), spec.name);
                    sheet.push_str(&s);
                }
                let colour_tiers = !matches!(t, Technique::BrailleMono);
                let featured = matches!(
                    t,
                    Technique::BrailleBg
                        | Technique::BrailleThin
                        | Technique::Half
                        | Technique::Octant
                        | Technique::BlocksBest
                        | Technique::AllBest
                );
                if !big && featured && colour_tiers {
                    for tier in [Tier::A256, Tier::A16] {
                        let s = render_file(&src, spec, t, tier, Palette::Light, &sets);
                        write(
                            dir.join(format!("{}-{}{}.ans", t.name(), spec.name, tier.suffix())),
                            &s,
                        );
                    }
                    let s = render_file(&src, spec, t, Tier::Truecolor, Palette::Dark, &sets);
                    write(dir.join(format!("{}-{}-dark.ans", t.name(), spec.name)), &s);
                    let s = render_file(&src, spec, t, Tier::A16, Palette::Tokens16, &sets);
                    write(
                        dir.join(format!("{}-{}-16tok.ans", t.name(), spec.name)),
                        &s,
                    );
                }
            }
            // Cost: only the z13 and outline views, five rounds each.
            if spec.zoom != 13 && spec.zoom != 3 {
                continue;
            }
            let mut render_ms = Vec::new();
            for _ in 0..5 {
                let t0 = Instant::now();
                let _ = source(&mut painter, centre, spec.zoom, cols, rows);
                render_ms.push(t0.elapsed().as_secs_f64() * 1e3);
            }
            let render = median(render_ms);
            let mut nat_half = Vec::new();
            let mut nat_braille = Vec::new();
            for _ in 0..5 {
                let v = Viewport::new(project(centre), spec.zoom, cols as u32, (rows * 2) as u32);
                let t0 = Instant::now();
                let _ = draw(&mut painter, &v, Ink::Colour, 100);
                nat_half.push(t0.elapsed().as_secs_f64() * 1e3);
                let v = Viewport::new(
                    project(centre),
                    spec.zoom,
                    (cols * 2) as u32,
                    (rows * 4) as u32,
                );
                let t0 = Instant::now();
                let _ = draw(&mut painter, &v, Ink::Lines(Rgb565::new(0, 0, 0)), 100);
                nat_braille.push(t0.elapsed().as_secs_f64() * 1e3);
            }
            let _ = writeln!(
                native,
                "{}x{} {} {:.1} {:.1}",
                cols,
                rows + 1,
                spec.name,
                median(nat_half),
                median(nat_braille)
            );
            // One cell to the right: the source view panned by one cell.
            let mut panned_view = source_view(centre, spec.zoom, cols, rows);
            panned_view.pan(SX as i32, 0);
            let panned = source(
                &mut painter,
                leviculum_map::mercator::unproject(panned_view.centre),
                spec.zoom,
                cols,
                rows,
            );
            for &t in &TECHNIQUES {
                let mut enc_ms = Vec::new();
                let mut grid = encode(&src, t, &sets);
                for _ in 0..5 {
                    let t0 = Instant::now();
                    grid = encode(&src, t, &sets);
                    let mut s = String::new();
                    ansi(&grid, Tier::Truecolor, &mut s);
                    enc_ms.push(t0.elapsed().as_secs_f64() * 1e3);
                }
                let bytes = |tier| {
                    let mut s = String::new();
                    ansi(&grid, tier, &mut s);
                    s.len()
                };
                let other = encode(&panned, t, &sets);
                let changed = grid
                    .cells
                    .iter()
                    .zip(&other.cells)
                    .filter(|(a, b)| a != b)
                    .count();
                let _ = writeln!(
                    costs,
                    "{}x{} {} {} {:.1} {:.2} {} {} {} {}/{}",
                    cols,
                    rows + 1,
                    spec.name,
                    t.name(),
                    render,
                    median(enc_ms),
                    bytes(Tier::Truecolor),
                    bytes(Tier::A256),
                    bytes(Tier::A16),
                    changed,
                    grid.cells.len()
                );
            }
        }
    }
    write(out.join("all.ans"), &sheet);
    write(out.join("glyph-test.ans"), &glyph_test());
    write(out.join("costs.txt"), &costs);
    write(out.join("render-native.txt"), &native);
}
