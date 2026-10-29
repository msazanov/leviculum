//! Map labels as data: the names of places, waters, parks and streets a
//! region carries (LMR2, `docs/drafts/map-renderer-roadmap-2026-10.md`,
//! section 1, "The format step: LMR2"), and the one rule every frontend
//! places them by ([`place`]).
//!
//! A region holds one string table for all its tiles and, per tile, a
//! label section beside the geometry blob. All integers little-endian.
//!
//! ```text
//! string table
//!   u32 offsets[count + 1], from the start of the bytes; offsets[0] = 0
//!   bytes   the strings, UTF-8, sorted by their bytes, each once
//!
//! label section of one tile
//!   0  u16 points   2 u16 lines   4 u16 alternates   6 u16 0
//!   points      12 bytes each: u8 kind  u8 rank  u8 min zoom  u8 flags
//!               u32 string  i16 x  i16 y (the anchor in tile units at
//!               extent 4096, inside the tile)
//!   lines       8 bytes each: u16 feature  u8 kind  u8 min zoom  u32 string
//!               (the feature is an index into the tile's geometry blob, a
//!               line feature of its own)
//!   alternates  8 bytes each, ascending by label: u16 label (points first,
//!               then lines)  u16 0  u32 string (the name in the locale the
//!               region was cut for, where it differs from the local one)
//! ```
//!
//! A reader skips a kind it does not know and ignores flag bits it does
//! not know, so a later cut can add both without breaking it; anything
//! else out of range is an error. Sorted strings make a name searchable by
//! binary search ([`crate::Region::find_string`]).

use alloc::vec::Vec;
use core::cmp::{Ordering, Reverse};

/// Length of a section's head.
pub const SECTION_HEAD: usize = 8;
/// Length of a point label record.
pub const POINT_LEN: usize = 12;
/// Length of a line label record.
pub const LINE_LEN: usize = 8;
/// Length of an alternate name record.
pub const ALT_LEN: usize = 8;
/// The extent label anchors are given in, whatever the tile's geometry.
pub const LABEL_EXTENT: i16 = 4096;

/// What a label names. The byte values are the format's; a reader skips a
/// byte it does not know ([`LabelKind::from_wire`] is `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LabelKind {
    City,
    Town,
    /// A district: OpenStreetMap's quarter, Protomaps' `macrohood`.
    District,
    Village,
    Suburb,
    Neighbourhood,
    /// A lake, a river, a sea: the name of a water area.
    Water,
    Park,
    /// A motorway or trunk road.
    MajorRoad,
    /// A primary, secondary or tertiary road.
    SecondaryRoad,
    MinorRoad,
}

impl LabelKind {
    /// Every kind, in the order [`place`] serves them.
    pub const ALL: [LabelKind; 11] = [
        LabelKind::City,
        LabelKind::Town,
        LabelKind::District,
        LabelKind::Village,
        LabelKind::Suburb,
        LabelKind::Neighbourhood,
        LabelKind::Water,
        LabelKind::Park,
        LabelKind::MajorRoad,
        LabelKind::SecondaryRoad,
        LabelKind::MinorRoad,
    ];

    /// The format's byte.
    pub const fn wire(self) -> u8 {
        match self {
            LabelKind::City => 1,
            LabelKind::Town => 2,
            LabelKind::District => 3,
            LabelKind::Village => 4,
            LabelKind::Suburb => 5,
            LabelKind::Neighbourhood => 6,
            LabelKind::Water => 16,
            LabelKind::Park => 17,
            LabelKind::MajorRoad => 32,
            LabelKind::SecondaryRoad => 33,
            LabelKind::MinorRoad => 34,
        }
    }

    /// The kind of a format byte; `None` for one this reader does not know.
    pub fn from_wire(b: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.wire() == b)
    }

    /// A place: a city down to a neighbourhood.
    pub fn is_place(self) -> bool {
        self <= LabelKind::Neighbourhood
    }

    /// A street name, set along its line.
    pub fn is_road(self) -> bool {
        self >= LabelKind::MajorRoad
    }

    /// The order [`place`] serves labels in, lowest first: pins come before
    /// all of them, then places by class, then waters and parks, then
    /// streets, major roads first.
    pub fn priority(self) -> u8 {
        self as u8
    }
}

/// A label at a point: a place, a water, a park.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointLabel {
    pub kind: LabelKind,
    /// Population rank for a place (Protomaps' `population_rank`), 0 where
    /// the source has none: within a kind, higher ranks are placed first.
    pub rank: u8,
    /// The shallowest view zoom that shows it.
    pub min_zoom: u8,
    pub flags: u8,
    /// Index into the region's string table: the local name.
    pub string: u32,
    /// The anchor in tile units at [`LABEL_EXTENT`], inside the tile.
    pub x: i16,
    pub y: i16,
}

/// A label along a line feature of the tile: a street name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineLabel {
    /// Index of the line feature in the tile's geometry blob.
    pub feature: u16,
    pub kind: LabelKind,
    pub min_zoom: u8,
    pub string: u32,
}

/// Why a label section or a string table is not one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelError {
    /// The length does not match the counts.
    Length,
    /// A reserved field is not 0.
    Reserved,
    /// A string index past the region's table.
    String(u32),
    /// The alternates are out of order or name a label that is not there.
    Alternate(u16),
    /// The anchor of point `n` is outside its tile.
    Anchor(u16),
    /// Line label `n` names a feature its tile does not hold as a line.
    Feature(u16),
    /// A string table's offsets are out of order or past its bytes, or a
    /// string is not UTF-8.
    Table,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// One tile's label section, checked, read in place.
#[derive(Clone, Copy, Debug)]
pub struct Section<'a> {
    bytes: &'a [u8],
    points: u16,
    lines: u16,
    alts: u16,
}

impl<'a> Section<'a> {
    /// Check `bytes` against the counts it starts with and against a
    /// region of `strings` strings.
    pub fn parse(bytes: &'a [u8], strings: u32) -> Result<Self, LabelError> {
        if bytes.len() < SECTION_HEAD {
            return Err(LabelError::Length);
        }
        let (points, lines, alts) = (u16_at(bytes, 0), u16_at(bytes, 2), u16_at(bytes, 4));
        if u16_at(bytes, 6) != 0 {
            return Err(LabelError::Reserved);
        }
        let len = SECTION_HEAD
            + usize::from(points) * POINT_LEN
            + usize::from(lines) * LINE_LEN
            + usize::from(alts) * ALT_LEN;
        if bytes.len() != len {
            return Err(LabelError::Length);
        }
        let s = Self {
            bytes,
            points,
            lines,
            alts,
        };
        let known = |i: u32| match i < strings {
            true => Ok(()),
            false => Err(LabelError::String(i)),
        };
        for (n, p) in s.raw_points().enumerate() {
            known(p.string)?;
            if !(0..LABEL_EXTENT).contains(&p.x) || !(0..LABEL_EXTENT).contains(&p.y) {
                return Err(LabelError::Anchor(n as u16));
            }
        }
        for l in s.raw_lines() {
            known(l.3)?;
        }
        let mut prev: Option<u16> = None;
        for (label, string) in s.alternates() {
            known(string)?;
            let labels = u32::from(points) + u32::from(lines);
            if prev.is_some_and(|p| p >= label) || u32::from(label) >= labels {
                return Err(LabelError::Alternate(label));
            }
            prev = Some(label);
        }
        for i in 0..usize::from(alts) {
            if u16_at(bytes, s.alt_at(i) + 2) != 0 {
                return Err(LabelError::Reserved);
            }
        }
        Ok(s)
    }

    fn line_at(&self, i: usize) -> usize {
        SECTION_HEAD + usize::from(self.points) * POINT_LEN + i * LINE_LEN
    }

    fn alt_at(&self, i: usize) -> usize {
        self.line_at(usize::from(self.lines)) + i * ALT_LEN
    }

    fn raw_points(&self) -> impl Iterator<Item = RawPoint> + 'a {
        let b = self.bytes;
        (0..usize::from(self.points)).map(move |i| {
            let at = SECTION_HEAD + i * POINT_LEN;
            RawPoint {
                kind: b[at],
                rank: b[at + 1],
                min_zoom: b[at + 2],
                flags: b[at + 3],
                string: u32_at(b, at + 4),
                x: u16_at(b, at + 8) as i16,
                y: u16_at(b, at + 10) as i16,
            }
        })
    }

    /// (feature, kind byte, min zoom, string) of each line label.
    fn raw_lines(&self) -> impl Iterator<Item = (u16, u8, u8, u32)> + 'a {
        let b = self.bytes;
        let first = self.line_at(0);
        (0..usize::from(self.lines)).map(move |i| {
            let at = first + i * LINE_LEN;
            (u16_at(b, at), b[at + 2], b[at + 3], u32_at(b, at + 4))
        })
    }

    /// How many point labels the section holds, known kinds or not: the
    /// label numbers of the lines start here.
    pub fn point_count(&self) -> u16 {
        self.points
    }

    /// The point labels of kinds this reader knows, each with its label
    /// number.
    pub fn points(&self) -> impl Iterator<Item = (u16, PointLabel)> + 'a {
        self.raw_points().enumerate().filter_map(|(n, p)| {
            Some((
                n as u16,
                PointLabel {
                    kind: LabelKind::from_wire(p.kind)?,
                    rank: p.rank,
                    min_zoom: p.min_zoom,
                    flags: p.flags,
                    string: p.string,
                    x: p.x,
                    y: p.y,
                },
            ))
        })
    }

    /// The line labels of kinds this reader knows, each with its label
    /// number.
    pub fn lines(&self) -> impl Iterator<Item = (u16, LineLabel)> + 'a {
        let first = self.points;
        self.raw_lines()
            .enumerate()
            .filter_map(move |(n, (feature, kind, min_zoom, string))| {
                Some((
                    first + n as u16,
                    LineLabel {
                        feature,
                        kind: LabelKind::from_wire(kind)?,
                        min_zoom,
                        string,
                    },
                ))
            })
    }

    /// Every (label number, string) alternate, ascending.
    pub fn alternates(&self) -> impl Iterator<Item = (u16, u32)> + 'a {
        let b = self.bytes;
        let first = self.alt_at(0);
        (0..usize::from(self.alts)).map(move |i| {
            let at = first + i * ALT_LEN;
            (u16_at(b, at), u32_at(b, at + 4))
        })
    }

    /// The alternate name of label `n`, by binary search.
    pub fn alternate(&self, n: u16) -> Option<u32> {
        let (mut lo, mut hi) = (0usize, usize::from(self.alts));
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let at = self.alt_at(mid);
            match u16_at(self.bytes, at).cmp(&n) {
                Ordering::Equal => return Some(u32_at(self.bytes, at + 4)),
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
            }
        }
        None
    }
}

struct RawPoint {
    kind: u8,
    rank: u8,
    min_zoom: u8,
    flags: u8,
    string: u32,
    x: i16,
    y: i16,
}

/// A label section's bytes: the writer's half, beside the reader. The
/// alternates are sorted here; label numbers count the points first.
pub fn encode_section(points: &[PointLabel], lines: &[LineLabel], alts: &[(u16, u32)]) -> Vec<u8> {
    let mut alts = alts.to_vec();
    alts.sort_unstable();
    let mut b = Vec::with_capacity(
        SECTION_HEAD + points.len() * POINT_LEN + lines.len() * LINE_LEN + alts.len() * ALT_LEN,
    );
    for n in [points.len(), lines.len(), alts.len(), 0] {
        b.extend_from_slice(&(n as u16).to_le_bytes());
    }
    for p in points {
        b.extend_from_slice(&[p.kind.wire(), p.rank, p.min_zoom, p.flags]);
        b.extend_from_slice(&p.string.to_le_bytes());
        b.extend_from_slice(&p.x.to_le_bytes());
        b.extend_from_slice(&p.y.to_le_bytes());
    }
    for l in lines {
        b.extend_from_slice(&l.feature.to_le_bytes());
        b.extend_from_slice(&[l.kind.wire(), l.min_zoom]);
        b.extend_from_slice(&l.string.to_le_bytes());
    }
    for (label, string) in alts {
        b.extend_from_slice(&label.to_le_bytes());
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&string.to_le_bytes());
    }
    b
}

/// A string table's bytes from strings already sorted and unique.
pub fn encode_strings<'s>(sorted: impl IntoIterator<Item = &'s str>) -> (u32, Vec<u8>) {
    let strings: Vec<&str> = sorted.into_iter().collect();
    let mut b = Vec::new();
    let mut at = 0u32;
    b.extend_from_slice(&at.to_le_bytes());
    for s in &strings {
        at += s.len() as u32;
        b.extend_from_slice(&at.to_le_bytes());
    }
    for s in &strings {
        b.extend_from_slice(s.as_bytes());
    }
    (strings.len() as u32, b)
}

/// A rectangle in the caller's unit, `x0..x1` by `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Rect {
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }

    /// Grown by `by` on every side.
    pub fn grown(&self, by: i32) -> Rect {
        Rect {
            x0: self.x0 - by,
            y0: self.y0 - by,
            x1: self.x1 + by,
            y1: self.y1 + by,
        }
    }
}

/// A label a frontend wants placed: what it is, and the boxes its text
/// covers in the frontend's unit (one for a horizontal name, squares along
/// the baseline for a turned one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub kind: LabelKind,
    pub rank: u8,
    /// The string it shows: two candidates of one string are one name.
    pub string: u32,
    /// A tie breaker stable under a pan: the anchor's world position, or
    /// the feature's.
    pub at: (u32, u32),
    pub boxes: Vec<Rect>,
}

/// What [`place`] holds every candidate to, beside free boxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Space every label keeps around it, in the caller's unit.
    pub margin: i32,
    /// At most one place, water or park label per block of this size, by
    /// the block its first box starts in (the terminal's density rule); 0
    /// for none.
    pub block: (i32, i32),
    /// Two labels of one street at least this far apart (Chebyshev, first
    /// box to first box); a place, water or park name is set once per view.
    pub repeat: i32,
}

/// The order [`place`] serves candidates in: by kind's priority, higher
/// rank first, then by string and position, so the order never depends on
/// which tile a label came from or the order tiles were read.
pub fn order(c: &[Candidate]) -> Vec<usize> {
    let mut out: Vec<usize> = (0..c.len()).collect();
    out.sort_by_key(|&i| {
        let c = &c[i];
        (c.kind.priority(), Reverse(c.rank), c.string, c.at)
    });
    out
}

/// The one placement rule (`docs/drafts/map-renderer-roadmap-2026-10.md`,
/// section 1, "Placement"): greedy in [`order`], a label is placed when
/// its boxes, grown by the margin, cover no box of `taken` (the pins and
/// their labels, placed first and never covered) and no label placed
/// before it, and the density and repeat rules allow it; else it is
/// dropped. Returns the indices of the placed candidates in the order
/// placed. Pure, so a frame and a test agree, and run at rest only: while
/// a view moves, its labels ride along in the picture.
pub fn place(candidates: &[Candidate], taken: &[Rect], rule: Rule) -> Vec<usize> {
    let mut boxes: Vec<Rect> = taken.to_vec();
    let mut blocks: Vec<(i32, i32)> = Vec::new();
    let mut placed: Vec<usize> = Vec::new();
    for i in order(candidates) {
        let c = &candidates[i];
        let Some(first) = c.boxes.first() else {
            continue;
        };
        let same = placed
            .iter()
            .map(|&p| &candidates[p])
            .filter(|p| p.string == c.string);
        let repeated = match c.kind.is_road() {
            false => same.count() > 0,
            true => same
                .filter_map(|p| p.boxes.first())
                .any(|b| (b.x0 - first.x0).abs().max((b.y0 - first.y0).abs()) < rule.repeat),
        };
        if repeated {
            continue;
        }
        let block = (rule.block.0 > 0 && rule.block.1 > 0 && !c.kind.is_road()).then(|| {
            (
                first.x0.div_euclid(rule.block.0),
                first.y0.div_euclid(rule.block.1),
            )
        });
        if block.is_some_and(|b| blocks.contains(&b)) {
            continue;
        }
        let free = c
            .boxes
            .iter()
            .all(|b| boxes.iter().all(|t| !t.grown(rule.margin).overlaps(b)));
        if !free {
            continue;
        }
        boxes.extend(c.boxes.iter().copied());
        blocks.extend(block);
        placed.push(i);
    }
    placed
}
