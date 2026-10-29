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
//! The encoders are the map core's ([`leviculum_map::cells`], 1093), which
//! the terminal map draws through; only the box-drawing hybrid, which is no
//! rung of the map, is this example's own.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use embedded_graphics::pixelcolor::Rgb565;
use leviculum_map::draw::Ink;
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::pins::{labels, Mark, Metrics};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::style::{area_color, line_style, Class, BACKGROUND};
use leviculum_ui::color::{rgb_to_ansi16, rgb_to_ansi256};
use loquax::picture::{Maps, Painter};

use leviculum_map::cells::{
    self, ansi16, braille, braille_ink, encode_cell, line_colours, mode, Cell, Encoding, Rgb, Sets,
    COAST, SX, SY,
};

/// One view drawn at the source density, and its credit.
struct Source {
    cells: cells::Source,
    attribution: String,
}

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

    /// The map core's encoding; the box-drawing hybrid is this example's
    /// own, not a rung of the terminal map.
    fn encoding(self) -> Option<Encoding> {
        Some(match self {
            Self::BrailleMono => Encoding::BrailleMono,
            Self::BrailleColour => Encoding::BrailleColour,
            Self::BrailleBg => Encoding::BrailleBg,
            Self::BrailleThin => Encoding::BrailleThin,
            Self::Half => Encoding::Half,
            Self::Quadrant => Encoding::Quadrant,
            Self::Sextant => Encoding::Sextant,
            Self::Octant => Encoding::Octant,
            Self::BlocksBest => Encoding::BlocksBest,
            Self::AllBest => Encoding::AllBest,
            Self::Mosaic => Encoding::Mosaic,
            Self::BoxHybrid => return None,
        })
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

/// The box-drawing hybrid's cell: roads as box drawing joined at the
/// cell's edges, braille where a line does not reach an edge, areas as the
/// background.
fn box_hybrid(src: &cells::Source, col: usize, row: usize) -> Cell {
    let lines = line_colours();
    let major = cells::rgb_of(line_style(Class::MajorRoad).0);
    let block = src.block(col, row);
    let line = |x: usize, y: usize| src.line[src.px(col, row, x, y)];
    let any = |pts: &[(usize, usize)]| pts.iter().any(|&(x, y)| line(x, y));
    let edges = usize::from(any(&[(1, 0), (2, 0)]))
        | usize::from(any(&[(3, 3), (3, 4)])) << 1
        | usize::from(any(&[(1, 7), (2, 7)])) << 2
        | usize::from(any(&[(0, 3), (0, 4)])) << 3;
    let fg =
        mode((0..SX * SY).filter(|&i| line(i % SX, i / SX)).map(
            |i| match lines.contains(&block[i]) {
                true => braille_ink(block[i]),
                false => COAST,
            },
        ));
    let bg = mode(
        (0..SX * SY)
            .filter(|&i| !line(i % SX, i / SX))
            .map(|i| block[i]),
    )
    .or(mode(block.iter().copied()));
    let dots = || braille(|dx, dy| (0..2).any(|y| (0..2).any(|x| line(dx * 2 + x, dy * 2 + y))));
    let ch = match (edges, fg) {
        (0, Some(_)) => dots(),
        (0, None) => ' ',
        (e, _) => box_char(e, fg == Some(major)),
    };
    Cell { ch, fg, bg }
}

fn encode(src: &Source, t: Technique, sets: &Sets) -> Grid {
    let src = &src.cells;
    let mut cells = Vec::with_capacity(src.cols * src.rows);
    for row in 0..src.rows {
        for col in 0..src.cols {
            cells.push(match t.encoding() {
                Some(encoding) => encode_cell(src, col, row, encoding, sets),
                None => box_hybrid(src, col, row),
            });
        }
    }
    Grid {
        cols: src.cols,
        rows: src.rows,
        cells,
    }
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
        .find(|(light, _)| cells::rgb_of(*light) == c)
        .map_or(c, |&(_, d)| d)
}

/// The style's colours as the sixteen ANSI colours chosen per class
/// ([`cells::ansi16`]), the way a 16-colour token names an index itself
/// rather than taking the nearest one, which turns parks and the river into
/// grey.
fn tokens16(c: Rgb) -> Rgb {
    ansi16(c).map_or(c, |i| {
        let (r, g, b) = leviculum_ui::color::XTERM_16[usize::from(i)];
        [r, g, b]
    })
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
    let bg = cells::rgb_of(BACKGROUND);
    Source {
        cells: cells::Source {
            cols,
            rows,
            rgb,
            line: lines.iter().map(|&c| c != bg).collect(),
            thin: thin.iter().map(|&c| c != bg).collect(),
        },
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
    let src = Source {
        cells: cells::Source {
            rgb: src
                .cells
                .rgb
                .iter()
                .map(|&c| match palette {
                    Palette::Dark => darken(c),
                    Palette::Tokens16 => tokens16(c),
                    Palette::Light => c,
                })
                .collect(),
            ..src.cells.clone()
        },
        attribution: src.attribution.clone(),
    };
    let mut grid = encode(&src, t, sets);
    let view = source_view(
        LatLon::from_degrees(spec.lat, spec.lon),
        spec.zoom,
        src.cells.cols,
        src.cells.rows,
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
/// at a glance which ones the font draws ([`cells::glyph_test`]).
fn glyph_test() -> String {
    let mut s = String::from(
        "Which glyph sets does this terminal draw? A box, a question mark or a gap means no.\n",
    );
    for (name, glyphs, _) in cells::glyph_test() {
        let glyphs: String = glyphs.into_iter().collect();
        let _ = writeln!(s, "  {name:<12}  {glyphs}");
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
    let maps = Maps::new(Some(PathBuf::from(maps)));
    for p in &maps.load().1 {
        eprintln!("{p}");
    }
    let mut painter = Painter::new(maps);
    let sets = Sets::default();
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
