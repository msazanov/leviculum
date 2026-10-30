//! Web Mercator between degrees and the world coordinate.
//!
//! The world coordinate is a pair of `u32`: the whole world is 2^32 units
//! wide and high, x growing east from the antimeridian, y growing south
//! from the top of the projection (85.0511 N). One unit is 40,075,017 m /
//! 2^32 = 9.3 mm at the equator. At zoom `z` a 256-pixel tile is
//! 2^(32 - z) units, a pixel 2^(24 - z) and a sub-pixel 2^(20 - z): at z15
//! a pixel is 512 units (4.78 m at the equator, times the cosine of the
//! latitude) and a sub-pixel 32 units (0.30 m). x wraps, so the antimeridian
//! is ordinary wrapping arithmetic.
//!
//! Longitude to x is integer arithmetic and exact. Latitude to y needs a
//! logarithm, done in `f64` through `libm`, which is software on the M9's
//! single-precision FPU: a few microseconds per call, made once per pin and
//! per viewport change. Map geometry never passes through here; a region
//! file holds it already projected, as tile units.

/// Degrees times 10^7, the PMTiles header's and the region file's unit.
/// One step is 1.1 cm of latitude.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LatLon {
    pub lat_e7: i32,
    pub lon_e7: i32,
}

/// A point on the projection, 2^32 units around the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct World {
    pub x: u32,
    pub y: u32,
}

/// The projection's northern and southern edge, 85.0511287798 degrees.
pub const MAX_LAT_E7: i32 = 850_511_288;

const E7: f64 = 1e7;
const LON_SPAN_E7: u64 = 3_600_000_000;
const HALF_LON_E7: i64 = 1_800_000_000;
const WORLD: f64 = 4_294_967_296.0;

impl LatLon {
    /// From degrees times 10^6, the telemetry codec's unit
    /// (`leviculum-lxmf`, `Location`).
    pub fn from_e6(lat_e6: i32, lon_e6: i32) -> Self {
        Self {
            lat_e7: lat_e6.saturating_mul(10),
            lon_e7: lon_e6.saturating_mul(10),
        }
    }

    /// From degrees.
    pub fn from_degrees(lat: f64, lon: f64) -> Self {
        Self {
            lat_e7: libm::round(lat * E7) as i32,
            lon_e7: libm::round(lon * E7) as i32,
        }
    }
}

/// The world x of a longitude; 180 E and 180 W are the same x, 0.
pub fn x_of_lon(lon_e7: i32) -> u32 {
    let from_west = (i64::from(lon_e7) + HALF_LON_E7).rem_euclid(LON_SPAN_E7 as i64) as u64;
    // Round to nearest: (v * 2^32 + span / 2) / span, all below 2^64.
    // 180 E is 0 by the `rem_euclid` above, and the last e7 step west of
    // it rounds to u32::MAX, so the mask never cuts; it keeps the cast
    // from depending on that arithmetic.
    ((((from_west << 32) + LON_SPAN_E7 / 2) / LON_SPAN_E7) & 0xFFFF_FFFF) as u32
}

/// The longitude of a world x, in [-180, 180).
pub fn lon_of_x(x: u32) -> i32 {
    let e7 = (u64::from(x) * LON_SPAN_E7 + (1 << 31)) >> 32;
    (e7 as i64 - HALF_LON_E7) as i32
}

/// The world y of a latitude, clamped to the projection's edges.
pub fn y_of_lat(lat_e7: i32) -> u32 {
    let lat = lat_e7.clamp(-MAX_LAT_E7, MAX_LAT_E7);
    let phi = f64::from(lat) / E7 * core::f64::consts::PI / 180.0;
    let t = libm::log(libm::tan(core::f64::consts::FRAC_PI_4 + phi / 2.0));
    let y = (1.0 - t / core::f64::consts::PI) / 2.0 * WORLD;
    libm::round(y).clamp(0.0, WORLD - 1.0) as u32
}

/// The latitude of a world y.
pub fn lat_of_y(y: u32) -> i32 {
    let n = core::f64::consts::PI * (1.0 - 2.0 * f64::from(y) / WORLD);
    let deg = libm::atan(libm::sinh(n)) * 180.0 / core::f64::consts::PI;
    libm::round(deg * E7) as i32
}

/// Degrees to the world coordinate.
pub fn project(p: LatLon) -> World {
    World {
        x: x_of_lon(p.lon_e7),
        y: y_of_lat(p.lat_e7),
    }
}

/// The world coordinate to degrees; longitude in [-180, 180).
pub fn unproject(w: World) -> LatLon {
    LatLon {
        lat_e7: lat_of_y(w.y),
        lon_e7: lon_of_x(w.x),
    }
}
