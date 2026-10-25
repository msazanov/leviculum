//! The two flat colour tables: one for areas, one for lines, indexed by a
//! feature's [`Class`]. Draw order is the class order: the fixture writer
//! sorts features by class, so earth lies under parks, parks under
//! buildings, and roads over all areas.

use embedded_graphics::pixelcolor::Rgb565;

use crate::fill::SUBPIXEL;

/// What a feature is, as far as drawing it goes. The numbers are the
/// fixture's class byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Class {
    Earth = 0,
    Urban = 1,
    Park = 2,
    Pedestrian = 3,
    Sand = 4,
    Water = 5,
    Building = 6,
    Waterway = 16,
    Path = 17,
    MinorRoad = 18,
    MajorRoad = 19,
    Rail = 20,
}

impl Class {
    /// The class for a fixture byte, `None` for an unknown one.
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Earth,
            1 => Self::Urban,
            2 => Self::Park,
            3 => Self::Pedestrian,
            4 => Self::Sand,
            5 => Self::Water,
            6 => Self::Building,
            16 => Self::Waterway,
            17 => Self::Path,
            18 => Self::MinorRoad,
            19 => Self::MajorRoad,
            20 => Self::Rail,
            _ => return None,
        })
    }

    /// Whether features of this class are areas (filled) rather than lines.
    pub fn is_area(self) -> bool {
        (self as u8) < 16
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb565 {
    Rgb565::new(r >> 3, g >> 2, b >> 3)
}

/// The background a frame is cleared to before drawing.
pub const BACKGROUND: Rgb565 = rgb(242, 239, 233);

/// Area colours, the first table.
pub fn area_color(c: Class) -> Rgb565 {
    match c {
        Class::Earth => rgb(242, 239, 233),
        Class::Urban => rgb(224, 220, 214),
        Class::Park => rgb(196, 226, 170),
        Class::Pedestrian => rgb(232, 230, 240),
        Class::Sand => rgb(240, 228, 186),
        Class::Water => rgb(150, 194, 238),
        Class::Building => rgb(206, 194, 182),
        _ => rgb(255, 0, 255),
    }
}

/// Line colours and widths in sub-pixels, the second table.
pub fn line_style(c: Class) -> (Rgb565, i32) {
    match c {
        Class::Waterway => (rgb(120, 170, 230), 3 * SUBPIXEL / 2),
        Class::Path => (rgb(170, 160, 150), SUBPIXEL),
        Class::MinorRoad => (rgb(255, 255, 255), 5 * SUBPIXEL / 2),
        Class::MajorRoad => (rgb(250, 200, 90), 4 * SUBPIXEL),
        Class::Rail => (rgb(110, 110, 110), SUBPIXEL),
        _ => (rgb(255, 0, 255), SUBPIXEL),
    }
}

/// Which features a view draws: the draft's two layer profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Every class in the fixture.
    Full,
    /// Without buildings, the profile the first measurement found fast
    /// enough (`docs/drafts/map-measurement-2026-10.md`, "Simplifications").
    /// The draft also strips `pois` and `physical_point`; the fixture holds
    /// no point features, so buildings are what is left to drop.
    Stripped,
}

impl Profile {
    /// Whether this profile draws `c`.
    pub fn includes(self, c: Class) -> bool {
        match self {
            Self::Full => true,
            Self::Stripped => c != Class::Building,
        }
    }

    /// The name the measurement lines print.
    pub fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Stripped => "stripped",
        }
    }
}
