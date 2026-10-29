//! A view's picture as terminal cells: the text techniques of
//! `docs/drafts/terminal-map-rendering-2026-10.md` (1090), built here so the
//! terminal frontend and the draft's sample generator encode through one
//! code path.
//!
//! A view is drawn once into a [`Source`] of [`SX`] x [`SY`] pixels a cell,
//! in colour and as lines alone, and every technique encodes that same
//! source into one [`Cell`] a cell: a glyph and its two colours.
//!
//! - The two-colour techniques ([`Encoding::Octant`],
//!   [`Encoding::BlocksBest`], the half, quadrant and sextant grids) choose
//!   for each cell the glyph of their set and the two colours that fit the
//!   cell's 32 pixels with the least squared error, the way chafa chooses
//!   its symbols ([`fit`]). Octants have their own fit on the eight 2 x 2
//!   means ([`fit_octant`]), which chooses exactly what [`fit`] chooses over
//!   the octant set at a quarter of the work and without a division.
//! - The braille techniques set a dot where any of its 2 x 2 source pixels
//!   is a line, colour the dots with the commonest line colour of the cell
//!   and the cell's background with its commonest area colour
//!   ([`Encoding::BrailleThin`], the lines one dot wide).
//! - [`Encoding::Mosaic`] needs no glyph: one colour a cell as background.
//!
//! The colours stay RGB here; which of the sixteen a 16-colour terminal
//! draws a class in is [`ansi16`], the frontend's to apply.

use alloc::vec::Vec;

use embedded_graphics::pixelcolor::Rgb565;

use crate::style::{line_style, Class};

/// Source pixels across one cell.
pub const SX: usize = 4;
/// Source pixels down one cell.
pub const SY: usize = 8;
/// Source pixels in one cell.
pub const CELL_PX: usize = SX * SY;

/// A colour as the terminal is sent it.
pub type Rgb = [u8; 3];

/// One cell: its glyph and colours, `None` for the terminal's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
}

/// A cell with nothing in it.
pub const BLANK: Cell = Cell {
    ch: ' ',
    fg: None,
    bg: None,
};

/// One view drawn at the source density: `cols` x `rows` cells of
/// [`SX`] x [`SY`] pixels, row by row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Source {
    pub cols: usize,
    pub rows: usize,
    /// The view in colour.
    pub rgb: Vec<Rgb>,
    /// Which pixels the lines cover, at the colour drawing's width.
    pub line: Vec<bool>,
    /// Which pixels the lines cover at half that width: one braille dot.
    pub thin: Vec<bool>,
}

impl Source {
    /// The index of pixel `(x, y)` of cell `(col, row)`.
    pub fn px(&self, col: usize, row: usize, x: usize, y: usize) -> usize {
        (row * SY + y) * self.cols * SX + col * SX + x
    }

    /// Cell `(col, row)`'s pixels, bit `y * SX + x` of a [`Glyph`] mask.
    pub fn block(&self, col: usize, row: usize) -> [Rgb; CELL_PX] {
        let mut block = [[0u8; 3]; CELL_PX];
        for y in 0..SY {
            for x in 0..SX {
                block[y * SX + x] = self
                    .rgb
                    .get(self.px(col, row, x, y))
                    .copied()
                    .unwrap_or_default();
            }
        }
        block
    }

    fn at(plane: &[bool], i: usize) -> bool {
        plane.get(i).copied().unwrap_or(false)
    }
}

/// How a source becomes cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Braille dots in the terminal's colour, lines only.
    BrailleMono,
    /// Braille dots in their line's colour.
    BrailleColour,
    /// Braille dots in colour over the cell's area colour.
    BrailleBg,
    /// As [`Encoding::BrailleBg`], the lines one dot wide.
    BrailleThin,
    /// `▀` a cell: 1 x 2.
    Half,
    /// Quadrants: 2 x 2.
    Quadrant,
    /// Sextants: 2 x 3, rows of 3, 3 and 2 source pixels.
    Sextant,
    /// Octants: 2 x 4, the finest grid of two colours a cell.
    Octant,
    /// Quadrants and eighth blocks, chosen per cell: the set nearly every
    /// mono font carries.
    BlocksBest,
    /// Blocks, sextants and octants, chosen per cell.
    AllBest,
    /// One colour a cell, no glyph.
    Mosaic,
}

/// A glyph and the source pixels it inks, bit `y * SX + x`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub ch: char,
    pub mask: u32,
}

/// The mask of the pixels `x0..x1` across and `y0..y1` down.
pub fn rect(x0: usize, x1: usize, y0: usize, y1: usize) -> u32 {
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
pub const QUADRANTS: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

/// The sextant characters by pattern, bit `k - 1` for BLOCK SEXTANT-k,
/// generated from UnicodeData.txt 16.0; the four patterns encoded before
/// Unicode 13 are space, the full block and the two half blocks.
pub const SEXTANTS: [char; 64] = [
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
pub const OCTANTS: [char; 256] = [
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
pub fn blocks() -> Vec<Glyph> {
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

/// The upper half block and the blank.
pub fn halves() -> Vec<Glyph> {
    alloc::vec![
        Glyph { ch: ' ', mask: 0 },
        Glyph {
            ch: '▀',
            mask: rect(0, SX, 0, SY / 2),
        },
    ]
}

/// The sixteen quadrant patterns.
pub fn quadrants() -> Vec<Glyph> {
    blocks().into_iter().take(16).collect()
}

/// Sextant rows on the 8-pixel cell: 3, 3 and 2 pixels.
const SEXTANT_ROWS: [(usize, usize); 3] = [(0, 3), (3, 6), (6, 8)];

/// The sixty-four sextant patterns.
pub fn sextants() -> Vec<Glyph> {
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

/// The octant of bit `k`: its 2 x 2 source pixels.
fn octant_cell(k: usize) -> (usize, usize) {
    ((k % 2) * 2, (k / 2) * 2)
}

/// The 256 octant patterns, pattern `p` at index `p`.
pub fn octants() -> Vec<Glyph> {
    (0..256)
        .map(|p| {
            let mut mask = 0;
            for k in 0..8 {
                if p & (1 << k) != 0 {
                    let (x0, y0) = octant_cell(k);
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
pub fn all_glyphs() -> Vec<Glyph> {
    let mut out: Vec<Glyph> = Vec::new();
    for g in blocks().into_iter().chain(sextants()).chain(octants()) {
        if !out.iter().any(|o| o.mask == g.mask) {
            out.push(g);
        }
    }
    out
}

/// The glyph sets a [`Source`] is encoded with, built once.
#[derive(Clone, Debug)]
pub struct Sets {
    pub half: Vec<Glyph>,
    pub quadrant: Vec<Glyph>,
    pub sextant: Vec<Glyph>,
    pub blocks: Vec<Glyph>,
    pub all: Vec<Glyph>,
}

impl Default for Sets {
    fn default() -> Self {
        Self {
            half: halves(),
            quadrant: quadrants(),
            sextant: sextants(),
            blocks: blocks(),
            all: all_glyphs(),
        }
    }
}

/// How well a split of a cell's pixels into two parts with sums `sf` and
/// `sb` over `nf` and `nb` pixels fits: `|sf|^2 / nf + |sb|^2 / nb`, the
/// larger the smaller the squared error, kept as a fraction so that two
/// splits compare exactly and without a division. An empty part adds
/// nothing.
#[derive(Clone, Copy, Debug)]
struct Score {
    num: i64,
    den: i64,
}

impl Score {
    fn of(sf: [i64; 3], nf: i64, sb: [i64; 3], nb: i64) -> Self {
        let sq = |s: [i64; 3]| s[0] * s[0] + s[1] * s[1] + s[2] * s[2];
        match (nf, nb) {
            (0, _) => Self {
                num: sq(sb),
                den: nb.max(1),
            },
            (_, 0) => Self {
                num: sq(sf),
                den: nf,
            },
            _ => Self {
                num: sq(sf) * nb + sq(sb) * nf,
                den: nf * nb,
            },
        }
    }

    /// Whether this split fits better than `other`. The largest numerator
    /// is under 3 * 8160^2 * 64, the denominators under 257, so the cross
    /// products stay far inside an i64.
    fn beats(self, other: Self) -> bool {
        self.num * other.den > other.num * self.den
    }
}

/// The cell of a glyph whose inked pixels sum to `sf` over `nf`, of a cell
/// whose pixels sum to `total`: each colour the mean of its part, both the
/// same where one part is empty.
fn cell_of(ch: char, total: [i64; 3], sf: [i64; 3], nf: i64) -> Cell {
    let n = CELL_PX as i64;
    let mean = |s: [i64; 3], k: i64| -> Rgb {
        let k = k.max(1);
        let c = |v: i64| u8::try_from(v / k).unwrap_or(u8::MAX);
        [c(s[0]), c(s[1]), c(s[2])]
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
        ch,
        fg: Some(fg),
        bg: Some(bg),
    }
}

fn sum(px: &[Rgb; CELL_PX]) -> [i64; 3] {
    let mut total = [0i64; 3];
    for p in px {
        for (t, &v) in total.iter_mut().zip(p) {
            *t += i64::from(v);
        }
    }
    total
}

/// The glyph of `set` and the two colours that fit one cell's pixels with
/// the least squared error, the first of `set` among equals. An empty set
/// is a blank.
pub fn fit(px: &[Rgb; CELL_PX], set: &[Glyph]) -> Cell {
    let Some(&first) = set.first() else {
        return BLANK;
    };
    let total = sum(px);
    let n = CELL_PX as i64;
    let mut best: Option<(Score, Glyph, [i64; 3], i64)> = None;
    for g in set {
        let mut sf = [0i64; 3];
        let mut nf = 0i64;
        let mut m = g.mask;
        while m != 0 {
            let i = m.trailing_zeros() as usize;
            m &= m - 1;
            nf += 1;
            for (s, &v) in sf.iter_mut().zip(&px[i]) {
                *s += i64::from(v);
            }
        }
        let sb = [total[0] - sf[0], total[1] - sf[1], total[2] - sf[2]];
        let score = Score::of(sf, nf, sb, n - nf);
        if best.is_none_or(|(b, ..)| score.beats(b)) {
            best = Some((score, *g, sf, nf));
        }
    }
    let (_, g, sf, nf) = best.unwrap_or((Score { num: 0, den: 1 }, first, [0; 3], 0));
    cell_of(g.ch, total, sf, nf)
}

/// [`fit`] over the octant set, on the eight 2 x 2 sums instead of the 32
/// pixels: every octant pattern inks whole 2 x 2 blocks, so a pattern's
/// sums are the sums of its blocks and the same pattern wins. Only the
/// patterns without the last octant are tried, in Gray code order, one
/// block added or taken away a step: a pattern and its complement fit
/// equally, and the one without the last octant is the lower pattern,
/// which [`fit`] takes among equals. The draft measured the 32-pixel brute
/// force at 34 ms for 155 x 65 cells.
pub fn fit_octant(px: &[Rgb; CELL_PX]) -> Cell {
    let mut blocks = [[0i64; 3]; 8];
    for (k, b) in blocks.iter_mut().enumerate() {
        let (x0, y0) = octant_cell(k);
        for y in y0..y0 + 2 {
            for x in x0..x0 + 2 {
                for (s, &v) in b.iter_mut().zip(&px[y * SX + x]) {
                    *s += i64::from(v);
                }
            }
        }
    }
    let total = sum(px);
    let n = CELL_PX as i64;
    let mut sf = [0i64; 3];
    let mut on = 0i64;
    let score = |sf: [i64; 3], on: i64| {
        let sb = [total[0] - sf[0], total[1] - sf[1], total[2] - sf[2]];
        Score::of(sf, on * 4, sb, n - on * 4)
    };
    let mut best = (score(sf, 0), 0usize, sf, 0i64);
    for step in 1..128usize {
        // The bit that flips from Gray code step - 1 to step.
        let k = step.trailing_zeros() as usize;
        let pattern = step ^ (step >> 1);
        let sign = match pattern & (1 << k) {
            0 => -1,
            _ => 1,
        };
        for (s, b) in sf.iter_mut().zip(blocks[k]) {
            *s += sign * b;
        }
        on += sign;
        let s = score(sf, on);
        let better = s.beats(best.0);
        let equal = !better && !best.0.beats(s);
        if better || (equal && pattern < best.1) {
            best = (s, pattern, sf, on);
        }
    }
    let (_, pattern, sf, on) = best;
    cell_of(OCTANTS[pattern], total, sf, on * 4)
}

/// The most frequent colour of `it`, the first seen among equals.
pub fn mode(it: impl Iterator<Item = Rgb>) -> Option<Rgb> {
    let mut seen: Vec<(Rgb, u32)> = Vec::new();
    for c in it {
        match seen.iter_mut().find(|(s, _)| *s == c) {
            Some((_, k)) => *k += 1,
            None => seen.push((c, 1)),
        }
    }
    let mut best: Option<(Rgb, u32)> = None;
    for (c, k) in seen {
        if best.is_none_or(|(_, b)| k > b) {
            best = Some((c, k));
        }
    }
    best.map(|(c, _)| c)
}

/// The braille character whose dots `dot(dx, dy)` sets, dx in 0..2, dy in
/// 0..4; the blank braille pattern U+2800 where none is.
pub fn braille(dot: impl Fn(usize, usize) -> bool) -> char {
    // The Unicode braille bit of each dot, column by column.
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

/// A colour of the tables as the terminal is sent it.
pub fn rgb_of(c: Rgb565) -> Rgb {
    leviculum_mapfill::frame::rgb888(c)
}

/// The line colours of the style, which no area colour shares.
pub fn line_colours() -> [Rgb; 5] {
    [
        Class::Waterway,
        Class::Path,
        Class::MinorRoad,
        Class::MajorRoad,
        Class::Rail,
    ]
    .map(|c| rgb_of(line_style(c).0))
}

/// What a line pixel that is no line class (a coast, an outline) is inked.
pub const COAST: Rgb = [70, 110, 160];

/// The minor roads' colour as braille dots.
pub const MINOR_DOT: Rgb = [128, 128, 132];

/// A line's colour as braille dots: a dot is a few pixels of a glyph, so
/// the white of the minor roads vanishes on the light ground; grey keeps it.
pub fn braille_ink(c: Rgb) -> Rgb {
    match c == rgb_of(line_style(Class::MinorRoad).0) {
        true => MINOR_DOT,
        false => c,
    }
}

/// The index among the sixteen ANSI colours `c` is drawn in at 16
/// colours, chosen per class ([`crate::style::ansi16`]); the dot colours of
/// the braille techniques included. `None` for a colour that is no class's,
/// a mean of a two-colour fit, which takes the nearest.
pub fn ansi16(c: Rgb) -> Option<u8> {
    match c {
        COAST => Some(4),
        MINOR_DOT => Some(8),
        _ => {
            // Back to the table's Rgb565, kept only where it comes back
            // exactly: the expansion is one to one.
            let back = Rgb565::new(c[0] >> 3, c[1] >> 2, c[2] >> 3);
            crate::style::ansi16(back).filter(|_| rgb_of(back) == c)
        }
    }
}

/// Cell `(col, row)` of `src` in `encoding`.
pub fn encode_cell(src: &Source, col: usize, row: usize, encoding: Encoding, sets: &Sets) -> Cell {
    let block = src.block(col, row);
    let lines = line_colours();
    let line = |x: usize, y: usize| Source::at(&src.line, src.px(col, row, x, y));
    let thin = |x: usize, y: usize| Source::at(&src.thin, src.px(col, row, x, y));
    let ink_of = |on: &dyn Fn(usize, usize) -> bool| {
        mode((0..CELL_PX).filter(|&i| on(i % SX, i / SX)).map(
            |i| match lines.contains(&block[i]) {
                true => braille_ink(block[i]),
                false => COAST,
            },
        ))
    };
    let area = || {
        mode(
            (0..CELL_PX)
                .filter(|&i| !line(i % SX, i / SX))
                .map(|i| block[i]),
        )
        .or(mode(block.iter().copied()))
    };
    let dots_of = |on: &dyn Fn(usize, usize) -> bool| {
        braille(|dx, dy| (0..2).any(|y| (0..2).any(|x| on(dx * 2 + x, dy * 2 + y))))
    };
    match encoding {
        Encoding::BrailleMono => Cell {
            ch: dots_of(&line),
            ..BLANK
        },
        Encoding::BrailleColour => Cell {
            ch: dots_of(&line),
            fg: ink_of(&line),
            bg: None,
        },
        Encoding::BrailleBg => Cell {
            ch: dots_of(&line),
            fg: ink_of(&line),
            bg: area(),
        },
        Encoding::BrailleThin => Cell {
            ch: dots_of(&thin),
            fg: ink_of(&thin),
            bg: area(),
        },
        Encoding::Half => fit(&block, &sets.half),
        Encoding::Quadrant => fit(&block, &sets.quadrant),
        Encoding::Sextant => fit(&block, &sets.sextant),
        Encoding::Octant => fit_octant(&block),
        Encoding::BlocksBest => fit(&block, &sets.blocks),
        Encoding::AllBest => fit(&block, &sets.all),
        Encoding::Mosaic => Cell {
            ch: ' ',
            fg: None,
            bg: mode(block.iter().copied()),
        },
    }
}

/// Every cell of `src` in `encoding`, row by row.
pub fn encode(src: &Source, encoding: Encoding, sets: &Sets) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(src.cols * src.rows);
    for row in 0..src.rows {
        for col in 0..src.cols {
            cells.push(encode_cell(src, col, row, encoding, sets));
        }
    }
    cells
}

/// One line per glyph family, each as it should look, and the
/// `--graphics` values that need it: the reader sees at a glance which
/// ones the font draws (the draft's `glyph-test.ans`, one key in the map).
pub fn glyph_test() -> [(&'static str, Vec<char>, &'static str); 7] {
    [
        (
            "half blocks",
            "▀▄█▌▐ ▀▄▀▄▀▄".chars().collect(),
            "halfblocks",
        ),
        ("quadrants", QUADRANTS.to_vec(), "blocks"),
        ("eighths", "▁▂▃▄▅▆▇█ ▔▎▊".chars().collect(), "blocks"),
        (
            "braille",
            (0x2801..0x280a)
                .chain([0x28FF, 0x2847, 0x28B8])
                .filter_map(char::from_u32)
                .collect(),
            "braille-thin, braille",
        ),
        ("box drawing", "─│┌┐└┘├┤┬┴┼ ━┃┏┓┗┛╋".chars().collect(), ""),
        ("sextants", SEXTANTS[1..20].to_vec(), ""),
        ("octants", OCTANTS[4..24].to_vec(), "octant"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{area_color, BACKGROUND};

    /// A small deterministic generator, so the cells are the same on every
    /// run.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u8 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 56) as u8
        }
    }

    /// Cells as a map makes them: two or three of the style's colours in
    /// edges and blobs, and some of pure noise.
    fn cells() -> Vec<[Rgb; CELL_PX]> {
        let palette = [
            rgb_of(BACKGROUND),
            rgb_of(area_color(Class::Park)),
            rgb_of(area_color(Class::Water)),
            rgb_of(line_style(Class::MajorRoad).0),
            rgb_of(line_style(Class::MinorRoad).0),
        ];
        let mut lcg = Lcg(7);
        let mut out = Vec::new();
        for _ in 0..3000 {
            let kind = lcg.next() % 3;
            let mut block = [[0u8; 3]; CELL_PX];
            let (a, b) = (
                palette[usize::from(lcg.next()) % 5],
                palette[usize::from(lcg.next()) % 5],
            );
            let cut = usize::from(lcg.next()) % (CELL_PX + 1);
            for (i, p) in block.iter_mut().enumerate() {
                *p = match kind {
                    0 => [lcg.next(), lcg.next(), lcg.next()],
                    1 => match i < cut {
                        true => a,
                        false => b,
                    },
                    _ => match lcg.next() % 4 {
                        0 => a,
                        _ => b,
                    },
                };
            }
            out.push(block);
        }
        // Every pixel the same, and an exact tie between a pattern and its
        // complement.
        out.push([[9, 9, 9]; CELL_PX]);
        let mut tie = [[0u8; 3]; CELL_PX];
        for (i, p) in tie.iter_mut().enumerate() {
            *p = match (i % SX) < 2 {
                true => [200, 0, 0],
                false => [0, 0, 200],
            };
        }
        out.push(tie);
        out
    }

    /// The octant fit on eight sums chooses what the brute force over the
    /// 32 pixels and the 256 patterns chooses, glyph and colours, ties
    /// included.
    #[test]
    fn the_octant_fit_chooses_what_the_brute_force_chooses() {
        let set = octants();
        for (i, block) in cells().iter().enumerate() {
            assert_eq!(fit_octant(block), fit(block, &set), "cell {i}: {block:?}");
        }
    }

    /// A cell half one colour, half another, left and right: the left half
    /// block in its two colours, by either fit.
    #[test]
    fn a_split_cell_is_its_half_block_in_its_two_colours() {
        let mut block = [[0u8; 3]; CELL_PX];
        for (i, p) in block.iter_mut().enumerate() {
            *p = match (i % SX) < 2 {
                true => [10, 20, 30],
                false => [200, 210, 220],
            };
        }
        let left = Cell {
            ch: '▌',
            fg: Some([10, 20, 30]),
            bg: Some([200, 210, 220]),
        };
        assert_eq!(fit_octant(&block), left);
        assert_eq!(fit(&block, &blocks()), left);
        assert_eq!(fit(&block, &[]), BLANK);
    }

    /// Each technique keeps to its glyphs and its colours: octants to the
    /// octant table, blocks to quadrants and eighths, braille to U+2800,
    /// mosaic to the blank with a background.
    #[test]
    fn each_encoding_keeps_to_its_glyph_set() {
        let mut src = Source {
            cols: 6,
            rows: 5,
            ..Source::default()
        };
        let mut lcg = Lcg(3);
        let n = src.cols * src.rows * CELL_PX;
        let roads = [
            rgb_of(line_style(Class::MinorRoad).0),
            rgb_of(line_style(Class::MajorRoad).0),
        ];
        for _ in 0..n {
            let line = lcg.next().is_multiple_of(5);
            src.rgb.push(match line {
                true => roads[usize::from(lcg.next()) % 2],
                false => rgb_of(area_color(Class::Park)),
            });
            src.line.push(line);
            src.thin.push(line && lcg.next().is_multiple_of(2));
        }
        let sets = Sets::default();
        let block_chars: Vec<char> = blocks().iter().map(|g| g.ch).collect();
        for encoding in [
            Encoding::Octant,
            Encoding::BlocksBest,
            Encoding::BrailleThin,
            Encoding::Mosaic,
            Encoding::Half,
        ] {
            let cells = encode(&src, encoding, &sets);
            assert_eq!(cells.len(), 30);
            for c in &cells {
                let ok = match encoding {
                    Encoding::Octant => OCTANTS.contains(&c.ch),
                    Encoding::BlocksBest => block_chars.contains(&c.ch),
                    Encoding::BrailleThin => ('\u{2800}'..='\u{28FF}').contains(&c.ch),
                    Encoding::Mosaic => c.ch == ' ' && c.fg.is_none(),
                    _ => c.ch == ' ' || c.ch == '▀',
                };
                assert!(ok, "{encoding:?}: {c:?}");
                assert!(c.bg.is_some(), "{encoding:?}: a background");
            }
            if encoding == Encoding::BrailleThin {
                // The dots in a road's colour, the minor roads grey; the
                // background the park.
                for c in cells.iter().filter(|c| c.ch != '\u{2800}') {
                    assert!(c.fg == Some(MINOR_DOT) || c.fg == Some(roads[1]), "{c:?}");
                    assert_eq!(c.bg, Some(rgb_of(area_color(Class::Park))));
                }
            }
        }
    }

    #[test]
    fn braille_sets_the_dot_of_each_inked_pixel() {
        assert_eq!(braille(|_, _| false), '\u{2800}');
        assert_eq!(braille(|_, _| true), '⣿');
        assert_eq!(braille(|dx, dy| dx == 0 && dy == 0), '⠁');
        assert_eq!(braille(|dx, dy| dx == 1 && dy == 3), '⢀');
    }

    /// At 16 colours a class keeps its own colour: park green, water bright
    /// blue, the minor roads' dots grey; a mean of two classes takes the
    /// nearest instead.
    #[test]
    fn each_class_has_its_own_of_the_sixteen() {
        assert_eq!(ansi16(rgb_of(area_color(Class::Park))), Some(2));
        assert_eq!(ansi16(rgb_of(area_color(Class::Water))), Some(12));
        assert_eq!(ansi16(rgb_of(line_style(Class::MajorRoad).0)), Some(3));
        assert_eq!(ansi16(MINOR_DOT), Some(8));
        assert_eq!(ansi16(COAST), Some(4));
        assert_eq!(ansi16([123, 45, 67]), None);
        // Next to a table colour, not on it.
        let [r, g, b] = rgb_of(area_color(Class::Park));
        assert_eq!(ansi16([r, g, b ^ 1]), None);
    }

    #[test]
    fn the_glyph_test_names_a_rung_for_each_set_a_rung_needs() {
        let rows = glyph_test();
        assert_eq!(rows.len(), 7);
        for name in ["halfblocks", "blocks", "braille-thin", "octant"] {
            assert!(rows.iter().any(|(_, _, r)| r.contains(name)), "{name}");
        }
        assert!(rows.iter().all(|(_, glyphs, _)| glyphs.len() >= 10));
    }
}
