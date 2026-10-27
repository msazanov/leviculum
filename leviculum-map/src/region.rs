//! The region file: one tile pyramid of `leviculum-mapfill`'s flat
//! geometry, read in place (`docs/src/concepts/loquax-map.md`, section 4,
//! "Region file").
//!
//! All integers little-endian. Offsets are from the start of the file.
//!
//! ```text
//! header  64 bytes
//!   0  "LMR1"
//!   4  u16 version (1)          6  u16 header length (64)
//!   8  i32 west, south, east, north, degrees x 10^7 (west > east: the
//!      region crosses the antimeridian)
//!  24  u8 min zoom  25 u8 max zoom (<= 16)  26 u8 layer profile  27 u8 0
//!  28  u32 attribution offset   32 u32 attribution length
//!  36  u32 notice offset        40 u32 notice length
//!  44  u32 tile count           48 u64 index offset
//!  56  u64 file length
//! text    the attribution (one line, shown on the map) and the notice
//!         (the licence and the source), UTF-8, both required
//! tiles   one `LMF1` fixture per tile: view 256x256, one tile at offset
//!         (0, 0), its shift taking the tile's extent to 4096 sub-pixels
//! index   tile count x 20 bytes, ascending by (z, x, y), last in the file:
//!         u8 z  u8 0  u16 x  u16 y  u16 0  u64 offset  u32 length
//! ```
//!
//! A tile with nothing in it under the profile has no entry; a reader
//! draws the background there. Two entries may share one blob (the cut
//! stores identical small tiles once). The index sits last so the writer
//! can stream tiles and learn their count at the end, and it is searched
//! in place, 20 bytes per probe: a whole country's index is megabytes,
//! which the M9 does not load.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use leviculum_mapfill::fixture::{Fixture, FixtureError};

/// The file's first four bytes.
pub const MAGIC: [u8; 4] = *b"LMR1";
/// The format version this reader reads.
pub const VERSION: u16 = 1;
/// Header length in bytes.
pub const HEADER_LEN: usize = 64;
/// Index entry length in bytes.
pub const ENTRY_LEN: usize = 20;
/// The highest zoom a region may hold: x and y are `u16`.
pub const MAX_ZOOM: u8 = 16;
/// Longest attribution or notice a reader accepts.
pub const MAX_TEXT: u32 = 16 * 1024;
/// The view every tile blob is written for: one tile at 256 pixels.
pub const TILE_VIEW: u16 = 256;

/// Why bytes are not a region, or a tile in it is unusable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionError {
    /// No `LMR1` at the start.
    Magic,
    /// A version this reader does not know.
    Version(u16),
    /// The file is shorter than its header says.
    Truncated,
    /// A header field is out of range; names the field.
    Header(&'static str),
    /// Index entry `n` is out of range or out of order.
    Entry(u32),
    /// The attribution or notice is not UTF-8, or is empty.
    Text,
    /// The storage could not be read.
    Io,
    /// A tile blob is not a fixture, or not a one-tile one.
    Tile(TileId, FixtureError),
}

/// A tile address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    /// Whether x and y lie on zoom z's grid.
    pub fn is_valid(&self) -> bool {
        self.z <= MAX_ZOOM && self.x < (1 << self.z) && self.y < (1 << self.z)
    }
}

/// A bounding box in degrees x 10^7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub west: i32,
    pub south: i32,
    pub east: i32,
    pub north: i32,
}

/// The decoded header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub bounds: Bounds,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub profile: u8,
    /// Offset and length of the attribution.
    pub attribution: (u32, u32),
    /// Offset and length of the notice.
    pub notice: (u32, u32),
    pub tiles: u32,
    pub index: u64,
    pub len: u64,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

impl Header {
    /// Decode and check the fields that need nothing but the header. The
    /// checks against the file's length are [`Region::open`]'s.
    pub fn parse(b: &[u8; HEADER_LEN]) -> Result<Self, RegionError> {
        if b[..4] != MAGIC {
            return Err(RegionError::Magic);
        }
        let version = u16_at(b, 4);
        if version != VERSION {
            return Err(RegionError::Version(version));
        }
        if usize::from(u16_at(b, 6)) != HEADER_LEN {
            return Err(RegionError::Header("header length"));
        }
        let i = |at| u32_at(b, at) as i32;
        let bounds = Bounds {
            west: i(8),
            south: i(12),
            east: i(16),
            north: i(20),
        };
        let lon = -1_800_000_000..=1_800_000_000;
        let lat = -crate::mercator::MAX_LAT_E7..=crate::mercator::MAX_LAT_E7;
        if !lon.contains(&bounds.west)
            || !lon.contains(&bounds.east)
            || !lat.contains(&bounds.south)
            || !lat.contains(&bounds.north)
            || bounds.south >= bounds.north
            || bounds.west == bounds.east
        {
            return Err(RegionError::Header("bounds"));
        }
        let (min_zoom, max_zoom) = (b[24], b[25]);
        if min_zoom > max_zoom || max_zoom > MAX_ZOOM {
            return Err(RegionError::Header("zoom"));
        }
        if b[27] != 0 {
            return Err(RegionError::Header("reserved"));
        }
        Ok(Self {
            bounds,
            min_zoom,
            max_zoom,
            profile: b[26],
            attribution: (u32_at(b, 28), u32_at(b, 32)),
            notice: (u32_at(b, 36), u32_at(b, 40)),
            tiles: u32_at(b, 44),
            index: u64_at(b, 48),
            len: u64_at(b, 56),
        })
    }

    /// The 64 header bytes; the writer's half, kept beside the reader.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[..4].copy_from_slice(&MAGIC);
        b[4..6].copy_from_slice(&VERSION.to_le_bytes());
        b[6..8].copy_from_slice(&(HEADER_LEN as u16).to_le_bytes());
        let bd = self.bounds;
        for (at, v) in [(8, bd.west), (12, bd.south), (16, bd.east), (20, bd.north)] {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[24] = self.min_zoom;
        b[25] = self.max_zoom;
        b[26] = self.profile;
        for (at, v) in [
            (28, self.attribution.0),
            (32, self.attribution.1),
            (36, self.notice.0),
            (40, self.notice.1),
            (44, self.tiles),
        ] {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[48..56].copy_from_slice(&self.index.to_le_bytes());
        b[56..64].copy_from_slice(&self.len.to_le_bytes());
        b
    }
}

/// One index entry: a tile and where its blob is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: TileId,
    pub offset: u64,
    pub len: u32,
}

impl Entry {
    /// Decode an entry; `None` when a reserved byte is set.
    pub fn parse(b: &[u8; ENTRY_LEN]) -> Option<Self> {
        if b[1] != 0 || b[6] != 0 || b[7] != 0 {
            return None;
        }
        Some(Self {
            id: TileId {
                z: b[0],
                x: u32::from(u16_at(b, 2)),
                y: u32::from(u16_at(b, 4)),
            },
            offset: u64_at(b, 8),
            len: u32_at(b, 16),
        })
    }

    /// The 20 entry bytes. `id` must be valid ([`TileId::is_valid`]).
    pub fn encode(&self) -> [u8; ENTRY_LEN] {
        let mut b = [0u8; ENTRY_LEN];
        b[0] = self.id.z;
        b[2..4].copy_from_slice(&(self.id.x as u16).to_le_bytes());
        b[4..6].copy_from_slice(&(self.id.y as u16).to_le_bytes());
        b[8..16].copy_from_slice(&self.offset.to_le_bytes());
        b[16..20].copy_from_slice(&self.len.to_le_bytes());
        b
    }
}

/// Random-access bytes: a slice in flash or memory, a file, a card.
pub trait Storage {
    /// Length in bytes.
    fn size(&self) -> u64;
    /// Fill `buf` from `offset`; an error when the range is not there.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), RegionError>;
}

impl Storage for &[u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), RegionError> {
        let start = usize::try_from(offset).map_err(|_| RegionError::Truncated)?;
        let src = start
            .checked_add(buf.len())
            .and_then(|end| self.get(start..end))
            .ok_or(RegionError::Truncated)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

#[cfg(all(feature = "std", unix))]
impl Storage for std::fs::File {
    fn size(&self) -> u64 {
        self.metadata().map(|m| m.len()).unwrap_or(0)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), RegionError> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset).map_err(|_| RegionError::Io)
    }
}

/// An opened region: the header checked against the storage, the two
/// texts read, the index left where it is.
pub struct Region<S> {
    storage: S,
    header: Header,
    attribution: String,
    notice: String,
    /// Where the blobs may lie: after the header, before the index.
    data_end: u64,
}

impl<S: Storage> Region<S> {
    /// Check the header against the storage and read the two texts.
    pub fn open(storage: S) -> Result<Self, RegionError> {
        let mut hb = [0u8; HEADER_LEN];
        if storage.size() < HEADER_LEN as u64 {
            // Too short for a header: say Magic when even that is wrong.
            let n = storage.size() as usize;
            let mut head = [0u8; 4];
            if n >= 4 {
                storage.read_at(0, &mut head)?;
                if head != MAGIC {
                    return Err(RegionError::Magic);
                }
            }
            return Err(RegionError::Truncated);
        }
        storage.read_at(0, &mut hb)?;
        let header = Header::parse(&hb)?;
        if header.len != storage.size() {
            return Err(RegionError::Truncated);
        }
        let index_len = u64::from(header.tiles) * ENTRY_LEN as u64;
        if header.index < HEADER_LEN as u64
            || header.index.checked_add(index_len) != Some(header.len)
        {
            return Err(RegionError::Header("index"));
        }
        let text = |(off, len): (u32, u32)| -> Result<String, RegionError> {
            if len == 0 || len > MAX_TEXT {
                return Err(RegionError::Text);
            }
            let end = u64::from(off) + u64::from(len);
            if u64::from(off) < HEADER_LEN as u64 || end > header.index {
                return Err(RegionError::Header("text"));
            }
            let mut b = vec![0u8; len as usize];
            storage.read_at(u64::from(off), &mut b)?;
            String::from_utf8(b).map_err(|_| RegionError::Text)
        };
        let attribution = text(header.attribution)?;
        let notice = text(header.notice)?;
        Ok(Self {
            data_end: header.index,
            storage,
            header,
            attribution,
            notice,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The line every map view shows, "© OpenStreetMap contributors".
    pub fn attribution(&self) -> &str {
        &self.attribution
    }

    /// The licence and the source the region was cut from.
    pub fn notice(&self) -> &str {
        &self.notice
    }

    /// The zoom range the region holds.
    pub fn zooms(&self) -> core::ops::RangeInclusive<u8> {
        self.header.min_zoom..=self.header.max_zoom
    }

    /// Index entry `n`, checked: a valid tile in the region's zooms and a
    /// blob inside the data area.
    pub fn entry(&self, n: u32) -> Result<Entry, RegionError> {
        if n >= self.header.tiles {
            return Err(RegionError::Entry(n));
        }
        let mut b = [0u8; ENTRY_LEN];
        self.storage
            .read_at(self.header.index + u64::from(n) * ENTRY_LEN as u64, &mut b)?;
        let e = Entry::parse(&b).ok_or(RegionError::Entry(n))?;
        let in_data = e.offset >= HEADER_LEN as u64
            && e.len > 0
            && e.offset
                .checked_add(u64::from(e.len))
                .is_some_and(|end| end <= self.data_end);
        if !e.id.is_valid() || !self.zooms().contains(&e.id.z) || !in_data {
            return Err(RegionError::Entry(n));
        }
        Ok(e)
    }

    /// The entry of `id`, by binary search over the index in place;
    /// `None` when the region has nothing there.
    pub fn find(&self, id: TileId) -> Result<Option<Entry>, RegionError> {
        let (mut lo, mut hi) = (0u32, self.header.tiles);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let e = self.entry(mid)?;
            match e.id.cmp(&id) {
                core::cmp::Ordering::Equal => return Ok(Some(e)),
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(None)
    }

    /// Read `e`'s blob into `buf`, replacing its contents.
    pub fn read_tile(&self, e: &Entry, buf: &mut Vec<u8>) -> Result<(), RegionError> {
        buf.clear();
        buf.resize(e.len as usize, 0);
        self.storage.read_at(e.offset, buf)
    }

    /// Walk the whole index: every entry in range, strictly ascending, and
    /// every blob a one-tile fixture. What the cut runs on its own output
    /// and a frontend may run once on a file it did not make.
    pub fn verify(&self) -> Result<(), RegionError> {
        let mut prev: Option<TileId> = None;
        let mut buf = Vec::new();
        for n in 0..self.header.tiles {
            let e = self.entry(n)?;
            if prev.is_some_and(|p| p >= e.id) {
                return Err(RegionError::Entry(n));
            }
            prev = Some(e.id);
            self.read_tile(&e, &mut buf)?;
            check_tile(e.id, &buf)?;
        }
        Ok(())
    }
}

/// Tile shifts a region blob may carry: extents 4096 (0) down to 256 (4).
pub const TILE_SHIFTS: core::ops::RangeInclusive<i8> = 0..=4;

/// Parse a tile blob and hold it to what the region format promises: one
/// tile at offset (0, 0), the tile view, a shift in [`TILE_SHIFTS`], and
/// every vertex within one extent of the tile. The last bound is what keeps
/// a placed vertex inside the filler's coordinate limit at any overzoom the
/// viewport selects ([`crate::viewport::MAX_OVERZOOM`]).
pub fn check_tile(id: TileId, blob: &[u8]) -> Result<Fixture<'_>, RegionError> {
    let fx = Fixture::parse(blob).map_err(|e| RegionError::Tile(id, e))?;
    let bad = RegionError::Tile(id, FixtureError::Tile(0));
    if fx.tile_count() != 1
        || fx.view() != (u32::from(TILE_VIEW), u32::from(TILE_VIEW))
        || !TILE_SHIFTS.contains(&fx.shift())
    {
        return Err(bad);
    }
    let extent = 4096i32 >> fx.shift();
    let range = -extent..2 * extent;
    for f in fx.features() {
        if f.offset.x != 0 || f.offset.y != 0 {
            return Err(bad);
        }
        for ring in f.rings() {
            for (x, y) in ring.points() {
                if !range.contains(&i32::from(x)) || !range.contains(&i32::from(y)) {
                    return Err(bad);
                }
            }
        }
    }
    Ok(fx)
}

impl<'a> Region<&'a [u8]> {
    /// A region in memory or flash, its tiles borrowed in place.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, RegionError> {
        Self::open(bytes)
    }

    /// The fixture of `id`, borrowed from the region; `None` when the
    /// region has nothing there.
    pub fn fixture(&self, id: TileId) -> Result<Option<Fixture<'a>>, RegionError> {
        let Some(e) = self.find(id)? else {
            return Ok(None);
        };
        // `entry` checked the range against the data area.
        let start = e.offset as usize;
        let blob = self
            .storage
            .get(start..start + e.len as usize)
            .ok_or(RegionError::Truncated)?;
        check_tile(id, blob).map(Some)
    }
}
