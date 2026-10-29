//! The cut: tiles of a source in a bounding box and a zoom range, under a
//! layer profile, into a region file.
//!
//! Deterministic: the tiles are visited in index order whatever the source
//! lists, each tile's blob depends on that tile's bytes alone, the worker
//! threads only compute blobs that are then written in order, and nothing
//! of the time or the machine goes into the file. Same input, same bytes.

use std::collections::{BTreeMap, HashMap};
use std::io::{Seek, SeekFrom, Write};
use std::ops::RangeInclusive;

use leviculum_map::mercator::{x_of_lon, y_of_lat};
use leviculum_map::profile::{self, ZoomLayers};
use leviculum_map::region::{Bounds, Entry, Header, TileId, ENTRY_LEN, HEADER_LEN, TILE_VIEW};
use leviculum_mapfill::fixture::{write, Kind};

use crate::mvt;
use crate::simplify::{area2, douglas_peucker, quantise_path, simplify_ring, Pt};
use crate::{Res, TileSource};

/// Blobs up to this size are stored once and shared by every tile that
/// has the same bytes: the uniform tiles of sea, forest and farmland.
const SHARE_MAX: usize = 4096;
/// Tiles handed to the workers per round.
const ROUND: usize = 1024;
/// The device's work capacity per feature (`leviculum-mapfill/m9`): a
/// feature with more vertices is skipped there as an overflow.
pub const DEVICE_POINTS: usize = 4096;
/// The device's rings per feature (`leviculum-mapfill/m9`).
pub const DEVICE_RINGS: usize = 1024;
/// The coarsest tolerance a feature over [`DEVICE_POINTS`] is simplified
/// at, in sub-pixels: four pixels.
pub const MAX_TOLERANCE: i64 = 64;

/// What to cut.
pub struct CutOptions {
    pub bounds: Bounds,
    pub zooms: RangeInclusive<u8>,
    pub profile: u8,
    /// One line, shown on every map view.
    pub attribution: String,
    /// The licence and the source.
    pub notice: String,
    pub threads: usize,
}

/// Counts for one zoom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ZoomStats {
    /// Source tiles in the box.
    pub tiles: u64,
    /// Index entries written.
    pub entries: u64,
    /// Tiles with nothing left under the profile, not written.
    pub empty: u64,
    /// Entries that reuse an earlier blob.
    pub shared: u64,
    /// Blob bytes written for this zoom.
    pub bytes: u64,
    pub features_in: u64,
    pub features_out: u64,
    pub vertices_in: u64,
    pub vertices_out: u64,
    /// Parts over [`DEVICE_POINTS`] vertices or [`DEVICE_RINGS`] rings even
    /// at [`MAX_TOLERANCE`].
    pub over_device: u64,
    /// Vertices clamped to within one extent of their tile.
    pub clamped: u64,
    /// Doublings of the tolerance for parts the device could not hold.
    pub resimplified: u64,
}

impl core::ops::AddAssign for ZoomStats {
    fn add_assign(&mut self, o: Self) {
        self.tiles += o.tiles;
        self.entries += o.entries;
        self.empty += o.empty;
        self.shared += o.shared;
        self.bytes += o.bytes;
        self.features_in += o.features_in;
        self.features_out += o.features_out;
        self.vertices_in += o.vertices_in;
        self.vertices_out += o.vertices_out;
        self.over_device += o.over_device;
        self.clamped += o.clamped;
        self.resimplified += o.resimplified;
    }
}

/// What the cut wrote.
#[derive(Debug, Default)]
pub struct CutStats {
    pub zooms: BTreeMap<u8, ZoomStats>,
    /// The whole file.
    pub bytes: u64,
}

/// Columns (one range, or two across the antimeridian) and rows of the
/// tiles at zoom `z` that touch `b`.
pub fn tile_range(b: &Bounds, z: u8) -> (Vec<RangeInclusive<u32>>, RangeInclusive<u32>) {
    let s = 32 - u32::from(z);
    let col = |lon| (u64::from(x_of_lon(lon)) >> s) as u32;
    let row = |lat| (u64::from(y_of_lat(lat)) >> s) as u32;
    let (x0, x1) = (col(b.west), col(b.east));
    let last = ((1u64 << z) - 1) as u32;
    let xs = if b.west < b.east {
        vec![x0..=x1]
    } else {
        vec![x0..=last, 0..=x1]
    };
    (xs, row(b.north)..=row(b.south))
}

/// Whether tile `t` touches `b`.
pub fn in_bounds(t: TileId, b: &Bounds) -> bool {
    let (xs, ys) = tile_range(b, t.z);
    ys.contains(&t.y) && xs.iter().any(|r| r.contains(&t.x))
}

/// One tile's blob under `layers`, `None` when nothing is left.
pub fn cut_tile(raw: &[u8], layers: ZoomLayers) -> Res<(Option<Vec<u8>>, ZoomStats)> {
    let mut st = ZoomStats {
        tiles: 1,
        ..ZoomStats::default()
    };
    let decoded = mvt::decode(raw)?;
    let k = 12 - u32::from(layers.extent_log2);
    let extent = 1i32 << layers.extent_log2;
    let tol = i64::from(layers.tolerance);
    // Square tile units per square pixel at this extent, times two for the
    // doubled shoelace area.
    let units_per_px = i64::from(extent / i32::from(TILE_VIEW)).max(1);
    let min_area2 = 2 * i64::from(layers.thin_px2) * units_per_px * units_per_px;
    let mut feats: Vec<write::Feature> = Vec::new();
    for f in decoded.features {
        st.features_in += 1;
        st.vertices_in += f
            .parts
            .iter()
            .flatten()
            .map(|r| r.len() as u64)
            .sum::<u64>();
        if !layers.keeps(f.class) {
            continue;
        }
        let thin = layers.thins(f.class);
        // One part simplified at `tol`: an area's outer ring and the holes
        // that survive, or the pieces of one line.
        let part_at = |part: &[Vec<Pt>], tol: i64| -> Vec<Vec<Pt>> {
            let mut rings: Vec<Vec<Pt>> = Vec::new();
            match f.kind {
                Kind::Area => {
                    for (i, ring) in part.iter().enumerate() {
                        let r = quantise_path(&simplify_ring(ring, tol), k, true);
                        let a = area2(&r).abs();
                        let keep = r.len() >= 3 && a > 0 && !(thin && a < min_area2);
                        if i == 0 && !keep {
                            // Holes without their outer ring would fill.
                            break;
                        }
                        if keep {
                            rings.push(r);
                        }
                    }
                }
                Kind::Line => {
                    for line in part {
                        let l = quantise_path(&douglas_peucker(line, tol), k, false);
                        // A line longer than the device holds is cut into
                        // pieces sharing their end points.
                        let mut at = 0;
                        while l.len() >= 2 && at + 1 < l.len() {
                            let end = (at + DEVICE_POINTS).min(l.len());
                            rings.push(l[at..end].to_vec());
                            at = end - 1;
                        }
                    }
                }
            }
            rings
        };
        let fits = |rings: &[Vec<Pt>]| {
            rings.iter().map(Vec::len).sum::<usize>() <= DEVICE_POINTS
                && rings.len() <= DEVICE_RINGS
        };
        let mut parts: Vec<Vec<Vec<Pt>>> = Vec::new();
        for part in &f.parts {
            // A polygon the device cannot hold whole is not drawn there at
            // all, so it is simplified harder, doubling the tolerance up to
            // [`MAX_TOLERANCE`], until it fits.
            let mut t = tol;
            let mut rings = part_at(part, t);
            while !fits(&rings) && f.kind == Kind::Area && t < MAX_TOLERANCE {
                t = (t * 2).clamp(1, MAX_TOLERANCE);
                rings = part_at(part, t);
                st.resimplified += 1;
            }
            match f.kind {
                // Each piece of a line is a part of its own, so the packing
                // below can put the pieces of a long line in separate
                // features.
                Kind::Line => parts.extend(rings.into_iter().map(|r| vec![r])),
                Kind::Area => {
                    if !fits(&rings) {
                        st.over_device += 1;
                    }
                    if !rings.is_empty() {
                        parts.push(rings);
                    }
                }
            }
        }
        // The parts packed into as few features as the device's capacity
        // allows; whole parts only, so no outer ring loses its holes.
        let mut chunks: Vec<Vec<Vec<Pt>>> = Vec::new();
        for rings in parts {
            let (n, r) = (rings.iter().map(Vec::len).sum::<usize>(), rings.len());
            let room = chunks.last().is_some_and(|c| {
                c.iter().map(Vec::len).sum::<usize>() + n <= DEVICE_POINTS
                    && c.len() + r <= DEVICE_RINGS
            });
            match chunks.last_mut() {
                Some(c) if room => c.extend(rings),
                _ => chunks.push(rings),
            }
        }
        let range = -extent..=2 * extent - 1;
        for rings in chunks {
            if rings.len() > usize::from(u16::MAX)
                || rings.iter().any(|r| r.len() > usize::from(u16::MAX))
            {
                continue;
            }
            let mut out = Vec::with_capacity(rings.len());
            for r in rings {
                st.vertices_out += r.len() as u64;
                out.push(
                    r.into_iter()
                        .map(|[x, y]| {
                            let c = [
                                x.clamp(*range.start(), *range.end()),
                                y.clamp(*range.start(), *range.end()),
                            ];
                            if c != [x, y] {
                                st.clamped += 1;
                            }
                            // In range of i16: the extent is at most 4096.
                            (c[0] as i16, c[1] as i16)
                        })
                        .collect(),
                );
            }
            st.features_out += 1;
            feats.push(write::Feature {
                kind: f.kind,
                class: f.class,
                flags: f.flags,
                tile: 0,
                rings: out,
            });
        }
    }
    if feats.is_empty() {
        st.empty = 1;
        return Ok((None, st));
    }
    // Draw order: by class, stable, so source order holds within a class.
    feats.sort_by_key(|f| f.class);
    let blob = write::fixture(TILE_VIEW, TILE_VIEW, layers.shift(), &[(0, 0)], &feats);
    Ok((Some(blob), st))
}

/// Cut `src` into `out` under `opts`.
pub fn cut<S: TileSource + ?Sized, W: Write + Seek>(
    src: &S,
    opts: &CutOptions,
    mut out: W,
) -> Res<CutStats> {
    let (lo, hi) = (*opts.zooms.start(), *opts.zooms.end());
    if lo > hi || hi > leviculum_map::region::MAX_ZOOM {
        return Err(format!("zoom range {lo}..={hi}").into());
    }
    let mut layers = BTreeMap::new();
    for z in lo..=hi {
        let l =
            profile::layers(opts.profile, z).ok_or(format!("unknown profile {}", opts.profile))?;
        layers.insert(z, l);
    }
    let mut ids: Vec<TileId> = src
        .tiles_in(&opts.bounds, opts.zooms.clone())?
        .into_iter()
        .filter(|t| opts.zooms.contains(&t.z) && in_bounds(*t, &opts.bounds))
        .collect();
    ids.sort_unstable();
    ids.dedup();

    out.write_all(&[0u8; HEADER_LEN])?;
    let mut pos = HEADER_LEN as u64;
    let mut text = |s: &str, pos: &mut u64| -> Res<(u32, u32)> {
        let at = u32::try_from(*pos)?;
        out.write_all(s.as_bytes())?;
        *pos += s.len() as u64;
        Ok((at, u32::try_from(s.len())?))
    };
    let attribution = text(&opts.attribution, &mut pos)?;
    let notice = text(&opts.notice, &mut pos)?;

    let mut stats = CutStats::default();
    let mut entries: Vec<Entry> = Vec::new();
    let mut shared: HashMap<Vec<u8>, (u64, u32)> = HashMap::new();
    let threads = opts.threads.max(1);
    for round in ids.chunks(ROUND) {
        let per = round.len().div_ceil(threads);
        let results: Vec<Res<(Option<Vec<u8>>, ZoomStats)>> = std::thread::scope(|s| {
            let handles: Vec<_> = round
                .chunks(per)
                .map(|part| {
                    let layers = &layers;
                    s.spawn(move || {
                        part.iter()
                            .map(|t| {
                                let raw = src.mvt(*t)?.ok_or("listed tile missing")?;
                                cut_tile(&raw, layers[&t.z])
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| match h.join() {
                    Ok(v) => v,
                    Err(_) => vec![Err("cut worker panicked".into())],
                })
                .collect()
        });
        for (t, r) in round.iter().zip(results) {
            let (blob, mut st) = r.map_err(|e| format!("tile {}/{}/{}: {e}", t.z, t.x, t.y))?;
            if let Some(blob) = blob {
                let len = u32::try_from(blob.len())?;
                let reuse = if blob.len() <= SHARE_MAX {
                    shared.get(&blob).copied()
                } else {
                    None
                };
                let offset = match reuse {
                    Some((offset, _)) => {
                        st.shared += 1;
                        offset
                    }
                    None => {
                        let offset = pos;
                        out.write_all(&blob)?;
                        pos += blob.len() as u64;
                        st.bytes += blob.len() as u64;
                        if blob.len() <= SHARE_MAX {
                            shared.insert(blob, (offset, len));
                        }
                        offset
                    }
                };
                st.entries += 1;
                entries.push(Entry {
                    id: *t,
                    offset,
                    len,
                });
            }
            *stats.zooms.entry(t.z).or_default() += st;
        }
    }

    let index = pos;
    for e in &entries {
        out.write_all(&e.encode())?;
    }
    pos += (entries.len() * ENTRY_LEN) as u64;
    let header = Header {
        bounds: opts.bounds,
        min_zoom: lo,
        max_zoom: hi,
        profile: opts.profile,
        attribution,
        notice,
        tiles: u32::try_from(entries.len())?,
        index,
        len: pos,
    };
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&header.encode())?;
    out.flush()?;
    stats.bytes = pos;
    Ok(stats)
}
