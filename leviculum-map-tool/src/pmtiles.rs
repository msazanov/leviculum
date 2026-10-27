//! A PMTiles v3 archive read from a file: the header, the directories
//! (root and leaves) into one sorted tile list, and the tiles by `pread`.
//!
//! The format is the one `https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md`
//! describes: a 127-byte header, directories of varint columns (tile id
//! deltas, run lengths, lengths, offsets), leaf directories behind entries
//! of run length 0, tile ids along a Hilbert curve per zoom.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::Path;

use leviculum_map::region::TileId;

use crate::Res;

const HEADER_LEN: usize = 127;
/// `internal_compression` / `tile_compression` values.
const NONE: u8 = 1;
const GZIP: u8 = 2;
/// `tile_type` MVT.
const MVT: u8 = 1;
/// Leaf directories nest; the spec's writers use one level, this allows a few.
const MAX_DEPTH: u32 = 4;

/// The header fields the cut uses.
#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub root: (u64, u64),
    pub metadata: (u64, u64),
    pub leaves: u64,
    pub data: u64,
    pub internal_compression: u8,
    pub tile_compression: u8,
    pub min_zoom: u8,
    pub max_zoom: u8,
}

/// An opened archive.
pub struct Archive {
    file: File,
    pub header: Header,
    /// Every addressed tile, ascending, with its blob's absolute offset
    /// and length. A run is expanded into one entry per tile.
    tiles: Vec<(TileId, u64, u32)>,
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

/// Parse the 127-byte header.
pub fn parse_header(b: &[u8]) -> Res<Header> {
    if b.len() < HEADER_LEN || &b[..7] != b"PMTiles" {
        return Err("not a PMTiles archive".into());
    }
    if b[7] != 3 {
        return Err(format!("PMTiles version {}, this reads 3", b[7]).into());
    }
    if b[99] != MVT {
        return Err(format!("tile type {}, this reads MVT (1)", b[99]).into());
    }
    Ok(Header {
        root: (u64_at(b, 8), u64_at(b, 16)),
        metadata: (u64_at(b, 24), u64_at(b, 32)),
        leaves: u64_at(b, 40),
        data: u64_at(b, 56),
        internal_compression: b[97],
        tile_compression: b[98],
        min_zoom: b[100],
        max_zoom: b[101],
    })
}

/// Undo the archive's compression of a directory, the metadata or a tile.
pub fn decompress(bytes: &[u8], compression: u8) -> Res<Vec<u8>> {
    match compression {
        NONE => Ok(bytes.to_vec()),
        GZIP => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(bytes).read_to_end(&mut out)?;
            Ok(out)
        }
        c => Err(format!("compression {c} not supported").into()),
    }
}

fn varint(b: &[u8], at: &mut usize) -> Res<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*at).ok_or("directory ends inside a varint")?;
        *at += 1;
        v |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("varint longer than 64 bits".into())
}

/// One directory entry: tile id, offset, length, run length (0: a leaf).
pub type DirEntry = (u64, u64, u32, u32);

/// Decode a directory's varint columns.
pub fn parse_directory(b: &[u8]) -> Res<Vec<DirEntry>> {
    let mut at = 0;
    let n = usize::try_from(varint(b, &mut at)?)?;
    if n > b.len() {
        return Err("directory claims more entries than it has bytes".into());
    }
    let mut e: Vec<DirEntry> = vec![(0, 0, 0, 0); n];
    let mut id = 0u64;
    for x in e.iter_mut() {
        id = id
            .checked_add(varint(b, &mut at)?)
            .ok_or("tile id overflow")?;
        x.0 = id;
    }
    for x in e.iter_mut() {
        x.3 = u32::try_from(varint(b, &mut at)?)?;
    }
    for x in e.iter_mut() {
        x.2 = u32::try_from(varint(b, &mut at)?)?;
    }
    for i in 0..n {
        let v = varint(b, &mut at)?;
        e[i].1 = if v == 0 && i > 0 {
            e[i - 1].1 + u64::from(e[i - 1].2)
        } else {
            v.checked_sub(1).ok_or("offset 0 on the first entry")?
        };
    }
    Ok(e)
}

/// The Hilbert-curve tile id of `t`.
pub fn tile_id(t: TileId) -> u64 {
    let mut acc = 0u64;
    for z in 0..t.z {
        acc += 1u64 << (2 * u32::from(z));
    }
    let n = 1u64 << t.z;
    let (mut x, mut y) = (u64::from(t.x), u64::from(t.y));
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from(x & s > 0);
        let ry = u64::from(y & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        rotate(n, &mut x, &mut y, rx, ry);
        s /= 2;
    }
    acc + d
}

fn rotate(n: u64, x: &mut u64, y: &mut u64, rx: u64, ry: u64) {
    if ry == 0 {
        if rx == 1 {
            *x = n - 1 - *x;
            *y = n - 1 - *y;
        }
        core::mem::swap(x, y);
    }
}

/// The tile of a Hilbert-curve tile id; `None` past zoom 16.
pub fn tile_of(id: u64) -> Option<TileId> {
    let mut acc = 0u64;
    for z in 0..=leviculum_map::region::MAX_ZOOM {
        let count = 1u64 << (2 * u32::from(z));
        if id < acc + count {
            let n = 1u64 << z;
            let mut t = id - acc;
            let (mut x, mut y) = (0u64, 0u64);
            let mut s = 1u64;
            while s < n {
                let rx = 1 & (t / 2);
                let ry = 1 & (t ^ rx);
                rotate(s, &mut x, &mut y, rx, ry);
                x += s * rx;
                y += s * ry;
                t /= 4;
                s *= 2;
            }
            return Some(TileId {
                z,
                x: x as u32,
                y: y as u32,
            });
        }
        acc += count;
    }
    None
}

impl Archive {
    pub fn open(path: &Path) -> Res<Self> {
        let file = File::open(path)?;
        let mut hb = [0u8; HEADER_LEN];
        file.read_exact_at(&mut hb, 0)?;
        let header = parse_header(&hb)?;
        let mut a = Self {
            file,
            header,
            tiles: Vec::new(),
        };
        let mut tiles = Vec::new();
        a.walk(header.root.0, header.root.1, 0, &mut tiles)?;
        tiles.sort_unstable_by_key(|t| t.0);
        a.tiles = tiles;
        Ok(a)
    }

    fn read(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
        let mut b = vec![0u8; usize::try_from(len)?];
        self.file.read_exact_at(&mut b, offset)?;
        Ok(b)
    }

    fn walk(
        &self,
        offset: u64,
        len: u64,
        depth: u32,
        out: &mut Vec<(TileId, u64, u32)>,
    ) -> Res<()> {
        if depth > MAX_DEPTH {
            return Err("leaf directories nested too deep".into());
        }
        let raw = self.read(offset, len)?;
        let dir = parse_directory(&decompress(&raw, self.header.internal_compression)?)?;
        for (id, off, length, run) in dir {
            if run == 0 {
                self.walk(self.header.leaves + off, u64::from(length), depth + 1, out)?;
                continue;
            }
            for k in 0..u64::from(run) {
                let t = tile_of(id + k).ok_or("tile id beyond zoom 16")?;
                out.push((t, self.header.data + off, length));
            }
        }
        Ok(())
    }

    /// Every tile the archive addresses, ascending.
    pub fn tiles(&self) -> impl Iterator<Item = TileId> + '_ {
        self.tiles.iter().map(|t| t.0)
    }

    /// The decompressed MVT bytes of `t`; `None` when the archive does not
    /// hold it.
    pub fn mvt(&self, t: TileId) -> Res<Option<Vec<u8>>> {
        let Ok(i) = self.tiles.binary_search_by_key(&t, |e| e.0) else {
            return Ok(None);
        };
        let (_, off, len) = self.tiles[i];
        let raw = self.read(off, u64::from(len))?;
        Ok(Some(decompress(&raw, self.header.tile_compression)?))
    }

    /// The metadata JSON.
    pub fn metadata(&self) -> Res<serde_json::Value> {
        let (off, len) = self.header.metadata;
        if len == 0 {
            return Ok(serde_json::Value::Null);
        }
        let raw = self.read(off, len)?;
        Ok(serde_json::from_slice(&decompress(
            &raw,
            self.header.internal_compression,
        )?)?)
    }
}
