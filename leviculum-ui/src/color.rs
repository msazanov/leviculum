//! The colour tiers and the colours a token names.
//!
//! A program draws in one of four [`Tier`]s, resolved once at startup by the
//! ladder of `docs/src/concepts/loquax-keys-and-layout.md`, section 5,
//! "Colour", part 1: monochrome, the sixteen ANSI colours, the xterm-256
//! palette, or 24-bit true colour. [`resolve_tier`] is that ladder, pure over
//! a snapshot of the environment ([`TierEnv`]) so a test feeds it every case.
//!
//! A colour is an [`Ink`]: an RGB triple, which a page or a truecolor token
//! authors, or a palette index, which a 256 or 16 token names itself because
//! the nearest index does not keep contrast. [`rgb_to_ansi256`] and
//! [`rgb_to_ansi16`] downgrade an RGB triple where the tier has no true
//! colour: lnomad's micron pages carry absolute RGB and need it.

/// A colour as a style carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ink {
    /// 24-bit RGB, downgraded on a tier without true colour.
    Rgb(u8, u8, u8),
    /// A palette index the terminal resolves itself: 0 to 15 are the sixteen
    /// ANSI colours, 16 to 255 the xterm-256 cube and grey ramp. Drawn as
    /// named, whatever the tier.
    Indexed(u8),
}

impl From<(u8, u8, u8)> for Ink {
    fn from((r, g, b): (u8, u8, u8)) -> Self {
        Ink::Rgb(r, g, b)
    }
}

/// The terminal's colour depth, for drawing an RGB [`Ink`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorDepth {
    /// 24-bit true colour: RGB triples emit as `38;2;r;g;b` unchanged.
    #[default]
    Truecolor,
    /// The xterm-256 palette: RGB triples are downgraded to the nearest indexed
    /// colour (`38;5;idx`) via [`rgb_to_ansi256`].
    Ansi256,
    /// The sixteen ANSI colours: RGB triples are downgraded to the nearest of
    /// them via [`rgb_to_ansi16`].
    Ansi16,
}

/// A colour tier: what the screen is drawn in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tier {
    /// No colour at all: alignment, glyphs and reverse video. The look the
    /// snapshot frames pin, and the tier a fresh model starts in.
    #[default]
    Mono,
    /// The sixteen ANSI colours.
    Ansi16,
    /// The xterm-256 palette.
    Ansi256,
    /// 24-bit true colour.
    Truecolor,
}

impl Tier {
    /// The depth RGB colours are drawn at, or `None` in monochrome.
    pub fn depth(self) -> Option<ColorDepth> {
        match self {
            Tier::Mono => None,
            Tier::Ansi16 => Some(ColorDepth::Ansi16),
            Tier::Ansi256 => Some(ColorDepth::Ansi256),
            Tier::Truecolor => Some(ColorDepth::Truecolor),
        }
    }

    /// The tier of a program that keeps `no_color` and a depth apart.
    pub fn of(no_color: bool, depth: ColorDepth) -> Tier {
        match (no_color, depth) {
            (true, _) => Tier::Mono,
            (false, ColorDepth::Ansi16) => Tier::Ansi16,
            (false, ColorDepth::Ansi256) => Tier::Ansi256,
            (false, ColorDepth::Truecolor) => Tier::Truecolor,
        }
    }

    /// The name `--color` and a key script's `tier` step take.
    pub fn name(self) -> &'static str {
        match self {
            Tier::Mono => "mono",
            Tier::Ansi16 => "16",
            Tier::Ansi256 => "256",
            Tier::Truecolor => "truecolor",
        }
    }

    /// The tier a name stands for, the inverse of [`name`](Tier::name).
    pub fn from_name(name: &str) -> Option<Tier> {
        [Tier::Mono, Tier::Ansi16, Tier::Ansi256, Tier::Truecolor]
            .into_iter()
            .find(|tier| tier.name() == name)
    }
}

/// The `--color` command-line choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorFlag {
    /// Climb the ladder (the default).
    #[default]
    Auto,
    /// Force 24-bit true colour.
    Truecolor,
    /// Force the xterm-256 palette.
    Ansi256,
    /// Force the sixteen ANSI colours.
    Ansi16,
}

/// What the ladder reads: the two flags and the three environment variables,
/// as the process found them (`None` for an unset variable).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TierEnv<'a> {
    /// `--no-color`.
    pub no_color_flag: bool,
    /// `--color`.
    pub color: ColorFlag,
    /// `NO_COLOR`.
    pub no_color: Option<&'a str>,
    /// `COLORTERM`.
    pub colorterm: Option<&'a str>,
    /// `TERM`.
    pub term: Option<&'a str>,
}

/// The ladder, the first rung that holds:
///
/// 1. `--no-color`, or `NO_COLOR` set and not empty: monochrome.
/// 2. `--color truecolor|256|16`: that tier.
/// 3. `COLORTERM` is `truecolor` or `24bit`: truecolor.
/// 4. `TERM` contains `256color`: 256.
/// 5. `TERM` is `dumb` or unset: monochrome.
/// 6. Anything else: 16.
pub fn resolve_tier(env: &TierEnv) -> Tier {
    if env.no_color_flag || env.no_color.is_some_and(|v| !v.is_empty()) {
        return Tier::Mono;
    }
    match env.color {
        ColorFlag::Truecolor => return Tier::Truecolor,
        ColorFlag::Ansi256 => return Tier::Ansi256,
        ColorFlag::Ansi16 => return Tier::Ansi16,
        ColorFlag::Auto => {}
    }
    if matches!(env.colorterm, Some("truecolor") | Some("24bit")) {
        return Tier::Truecolor;
    }
    match env.term {
        Some(term) if term.contains("256color") => Tier::Ansi256,
        None | Some("dumb") | Some("") => Tier::Mono,
        Some(_) => Tier::Ansi16,
    }
}

/// [`resolve_tier`] over this process's `NO_COLOR`, `COLORTERM` and `TERM`.
#[cfg(feature = "std")]
pub fn resolve_tier_from_env(no_color_flag: bool, color: ColorFlag) -> Tier {
    let no_color = std::env::var("NO_COLOR").ok();
    let colorterm = std::env::var("COLORTERM").ok();
    let term = std::env::var("TERM").ok();
    resolve_tier(&TierEnv {
        no_color_flag,
        color,
        no_color: no_color.as_deref(),
        colorterm: colorterm.as_deref(),
        term: term.as_deref(),
    })
}

/// The xterm default values of the sixteen ANSI colours, what the
/// 16-colour downgrade measures distance against.
pub const XTERM_16: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0xcd, 0x00, 0x00),
    (0x00, 0xcd, 0x00),
    (0xcd, 0xcd, 0x00),
    (0x00, 0x00, 0xee),
    (0xcd, 0x00, 0xcd),
    (0x00, 0xcd, 0xcd),
    (0xe5, 0xe5, 0xe5),
    (0x7f, 0x7f, 0x7f),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
    (0xff, 0xff, 0x00),
    (0x5c, 0x5c, 0xff),
    (0xff, 0x00, 0xff),
    (0x00, 0xff, 0xff),
    (0xff, 0xff, 0xff),
];

/// The values the xterm-256 palette gives an index: the sixteen at their
/// xterm defaults, the 6x6x6 cube, the grey ramp.
pub fn ansi256_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => XTERM_16[usize::from(index)],
        16..=231 => {
            let i = usize::from(index - 16);
            (
                CUBE_LEVELS[i / 36],
                CUBE_LEVELS[(i / 6) % 6],
                CUBE_LEVELS[i % 6],
            )
        }
        _ => {
            let g = 8 + 10 * (index - 232);
            (g, g, g)
        }
    }
}

/// The nearest of the sixteen ANSI colours to an RGB triple, by squared
/// distance against their xterm values.
pub fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> u8 {
    let mut best = 0;
    let mut best_d = i32::MAX;
    for (i, &rgb) in XTERM_16.iter().enumerate() {
        let d = dist2((r, g, b), rgb);
        if d < best_d {
            best_d = d;
            best = i as u8;
        }
    }
    best
}

/// The six per-channel levels of the xterm 6x6x6 colour cube.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Map [`ColorDepth::Truecolor`] RGB to the nearest xterm-256 palette index.
///
/// Considers both the 6x6x6 colour cube (`16 + 36*r6 + 6*g6 + b6`, each channel
/// snapped to its nearest cube level) and the 24-step grayscale ramp
/// (`232..=255`), and returns whichever candidate is nearer to the input by
/// squared Euclidean distance. A near-grey input therefore lands on the ramp,
/// which is finer than the cube's grey diagonal, while a saturated colour lands
/// in the cube. Pure and total.
pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    let (cube_idx, cube_rgb) = nearest_cube(r, g, b);
    let (gray_idx, gray_rgb) = nearest_gray(r, g, b);
    if dist2((r, g, b), gray_rgb) < dist2((r, g, b), cube_rgb) {
        gray_idx
    } else {
        cube_idx
    }
}

/// The nearest colour-cube index for an RGB triple, with the cube colour it
/// resolves to (for the cube-vs-gray distance comparison).
fn nearest_cube(r: u8, g: u8, b: u8) -> (u8, (u8, u8, u8)) {
    let r6 = nearest_cube_level(r);
    let g6 = nearest_cube_level(g);
    let b6 = nearest_cube_level(b);
    let idx = 16 + 36 * r6 + 6 * g6 + b6;
    (
        idx as u8,
        (CUBE_LEVELS[r6], CUBE_LEVELS[g6], CUBE_LEVELS[b6]),
    )
}

/// The index (`0..=5`) of the cube level nearest to a channel value.
fn nearest_cube_level(c: u8) -> usize {
    let mut best = 0;
    let mut best_d = i32::MAX;
    for (i, &level) in CUBE_LEVELS.iter().enumerate() {
        let d = (c as i32 - level as i32).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

/// The nearest grayscale-ramp index (`232..=255`) for an RGB triple, using its
/// mean as the grey level, with the grey colour it resolves to. The ramp holds
/// 24 steps at `8 + 10*i`.
fn nearest_gray(r: u8, g: u8, b: u8) -> (u8, (u8, u8, u8)) {
    let mean = (r as i32 + g as i32 + b as i32) / 3;
    let step = (((mean - 8).max(0) + 5) / 10).min(23);
    let value = (8 + 10 * step) as u8;
    (232 + step as u8, (value, value, value))
}

/// Squared Euclidean distance between two RGB triples.
fn dist2(a: (u8, u8, u8), b: (u8, u8, u8)) -> i32 {
    let dr = a.0 as i32 - b.0 as i32;
    let dg = a.1 as i32 - b.1 as i32;
    let db = a.2 as i32 - b.2 as i32;
    dr * dr + dg * dg + db * db
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_colours_map_to_the_cube() {
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
        assert_eq!(rgb_to_ansi256(0, 255, 0), 46);
        assert_eq!(rgb_to_ansi256(0, 0, 255), 21);
    }

    #[test]
    fn black_and_white_map_to_the_cube_corners() {
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16);
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231);
    }

    #[test]
    fn mid_grey_maps_into_the_grayscale_ramp() {
        let idx = rgb_to_ansi256(128, 128, 128);
        assert!(
            (232..=255).contains(&idx),
            "mid-grey should land on the 232..=255 ramp, got {idx}"
        );
    }

    fn env<'a>(term: Option<&'a str>, colorterm: Option<&'a str>) -> TierEnv<'a> {
        TierEnv {
            term,
            colorterm,
            ..TierEnv::default()
        }
    }

    #[test]
    fn the_ladder_reads_term_and_colorterm() {
        let cases: [(Option<&str>, Option<&str>, Tier); 12] = [
            (Some("xterm-256color"), Some("truecolor"), Tier::Truecolor),
            (Some("xterm"), Some("24bit"), Tier::Truecolor),
            (None, Some("truecolor"), Tier::Truecolor),
            (Some("xterm-256color"), None, Tier::Ansi256),
            (Some("tmux-256color"), None, Tier::Ansi256),
            (Some("screen-256color"), Some("yes"), Tier::Ansi256),
            (Some("dumb"), None, Tier::Mono),
            (None, None, Tier::Mono),
            (Some("linux"), None, Tier::Ansi16),
            (Some("xterm"), None, Tier::Ansi16),
            (Some("screen"), None, Tier::Ansi16),
            (Some("vt220"), None, Tier::Ansi16),
        ];
        for (term, colorterm, tier) in cases {
            assert_eq!(
                resolve_tier(&env(term, colorterm)),
                tier,
                "TERM={term:?} COLORTERM={colorterm:?}"
            );
        }
    }

    #[test]
    fn no_color_set_and_not_empty_is_monochrome_and_empty_is_not() {
        let mut e = env(Some("xterm-256color"), Some("truecolor"));
        e.no_color = Some("1");
        assert_eq!(resolve_tier(&e), Tier::Mono);
        e.no_color = Some("");
        assert_eq!(resolve_tier(&e), Tier::Truecolor);
        e.no_color = None;
        e.no_color_flag = true;
        assert_eq!(resolve_tier(&e), Tier::Mono);
    }

    #[test]
    fn no_color_wins_over_the_color_flag_and_the_flag_over_the_environment() {
        let mut e = env(Some("dumb"), None);
        for (flag, tier) in [
            (ColorFlag::Truecolor, Tier::Truecolor),
            (ColorFlag::Ansi256, Tier::Ansi256),
            (ColorFlag::Ansi16, Tier::Ansi16),
        ] {
            e.color = flag;
            assert_eq!(resolve_tier(&e), tier);
        }
        e.no_color = Some("1");
        assert_eq!(resolve_tier(&e), Tier::Mono);
        let mut e = env(Some("xterm"), Some("truecolor"));
        e.color = ColorFlag::Ansi16;
        assert_eq!(resolve_tier(&e), Tier::Ansi16);
    }

    #[test]
    fn a_tier_names_itself_and_its_depth() {
        for tier in [Tier::Mono, Tier::Ansi16, Tier::Ansi256, Tier::Truecolor] {
            assert_eq!(Tier::from_name(tier.name()), Some(tier));
            match tier.depth() {
                Some(depth) => assert_eq!(Tier::of(false, depth), tier),
                None => assert_eq!(Tier::of(true, ColorDepth::Truecolor), tier),
            }
        }
        assert_eq!(Tier::from_name("8"), None);
    }

    #[test]
    fn the_256_values_are_the_xterm_ones() {
        assert_eq!(ansi256_rgb(4), (0, 0, 0xee));
        assert_eq!(ansi256_rgb(24), (0, 95, 135));
        assert_eq!(ansi256_rgb(231), (255, 255, 255));
        assert_eq!(ansi256_rgb(232), (8, 8, 8));
        assert_eq!(ansi256_rgb(255), (238, 238, 238));
        for i in 16..=255u8 {
            let (r, g, b) = ansi256_rgb(i);
            assert_eq!(rgb_to_ansi256(r, g, b), i, "index {i}");
        }
    }

    #[test]
    fn the_16_downgrade_takes_the_nearest_xterm_colour() {
        assert_eq!(rgb_to_ansi16(0, 0, 0), 0);
        assert_eq!(rgb_to_ansi16(255, 255, 255), 15);
        assert_eq!(rgb_to_ansi16(250, 10, 10), 9);
        assert_eq!(rgb_to_ansi16(38, 40, 49), 0);
        assert_eq!(rgb_to_ansi16(225, 228, 235), 7);
    }
}
