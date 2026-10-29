//! LMR2: the names a region carries. A label section and a string table
//! round trip through their writer and reader; the committed Florence
//! region (cut with `--locale de`) holds the places and street names its
//! tiles name; a version 1 file still reads, without names; and a version
//! this reader does not know is refused, never misread.

use leviculum_map::labels::{
    encode_section, encode_strings, LabelError, LabelKind, LineLabel, PointLabel, Section,
    POINT_LEN, SECTION_HEAD,
};
use leviculum_map::region::{
    check_strings, Entry, Header, Region, RegionError, TileId, ENTRY_LEN, HEADER_LEN,
};
use leviculum_mapfill::fixture::Kind;

const FLORENCE: &[u8] = include_bytes!("../fixtures/firenze.lmr");

fn point(kind: LabelKind, string: u32, x: i16, y: i16) -> PointLabel {
    PointLabel {
        kind,
        rank: 7,
        min_zoom: 11,
        flags: 0,
        string,
        x,
        y,
    }
}

#[test]
fn a_label_section_round_trips() {
    let points = [
        point(LabelKind::City, 3, 0, 4095),
        point(LabelKind::Water, 1, 2048, 17),
    ];
    let lines = [LineLabel {
        feature: 41,
        kind: LabelKind::MinorRoad,
        min_zoom: 15,
        string: 0,
    }];
    // Given out of order; the writer sorts them.
    let alts = [(2, 4), (0, 2)];
    let bytes = encode_section(&points, &lines, &alts);
    assert_eq!(bytes.len(), SECTION_HEAD + 2 * POINT_LEN + 8 + 2 * 8);
    let s = Section::parse(&bytes, 5).expect("parse");
    let got: Vec<PointLabel> = s.points().map(|(_, p)| p).collect();
    assert_eq!(got, points);
    let got: Vec<(u16, LineLabel)> = s.lines().collect();
    assert_eq!(got, [(2, lines[0])]);
    assert_eq!(s.alternates().collect::<Vec<_>>(), [(0, 2), (2, 4)]);
    assert_eq!(
        (s.alternate(0), s.alternate(1), s.alternate(2)),
        (Some(2), None, Some(4))
    );

    // A string past the table, an anchor outside the tile, a reserved
    // byte, a length that does not match: each an error.
    assert_eq!(Section::parse(&bytes, 4).err(), Some(LabelError::String(4)));
    let mut b = bytes.clone();
    b[SECTION_HEAD + 8..SECTION_HEAD + 10].copy_from_slice(&4096i16.to_le_bytes());
    assert_eq!(Section::parse(&b, 5).err(), Some(LabelError::Anchor(0)));
    let mut b = bytes.clone();
    b[6] = 1;
    assert_eq!(Section::parse(&b, 5).err(), Some(LabelError::Reserved));
    let mut b = bytes.clone();
    b.push(0);
    assert_eq!(Section::parse(&b, 5).err(), Some(LabelError::Length));
    // Alternates out of order, or naming a label that is not there.
    let mut b = bytes.clone();
    let alt = bytes.len() - 16;
    b[alt..alt + 2].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(Section::parse(&b, 5).err(), Some(LabelError::Alternate(2)));

    // A kind this reader does not know is skipped, not refused: a later
    // cut may add one.
    let mut b = bytes.clone();
    b[SECTION_HEAD] = 200;
    let s = Section::parse(&b, 5).expect("an unknown kind parses");
    assert_eq!(s.points().map(|(n, _)| n).collect::<Vec<_>>(), [1]);
    assert_eq!(s.point_count(), 2);
}

#[test]
fn a_string_table_is_sorted_and_searched() {
    let (count, table) = encode_strings(["Arno", "Firenze", "Florenz", "Via dei Servi"]);
    assert_eq!(count, 4);
    check_strings(&table, count).expect("table");
    let (_, unsorted) = encode_strings(["Firenze", "Arno"]);
    assert_eq!(check_strings(&unsorted, 2), Err(RegionError::Strings(1)));
    let (_, twice) = encode_strings(["Arno", "Arno"]);
    assert_eq!(check_strings(&twice, 2), Err(RegionError::Strings(1)));
    // Cut one byte short: the last string runs past the bytes.
    assert_eq!(
        check_strings(&table[..table.len() - 1], 4),
        Err(RegionError::Strings(3))
    );

    let r = Region::from_bytes(FLORENCE).expect("open");
    let n = r.string_count();
    assert!(n > 200, "{n} strings");
    let firenze = r.find_string("Firenze").expect("search").expect("Firenze");
    assert_eq!(r.string(firenze), Ok("Firenze"));
    assert_eq!(r.find_string("Frankfurt am Main").expect("search"), None);
    let mut s = String::new();
    for i in [0, firenze, n - 1] {
        r.read_string(i, &mut s).expect("read");
        assert_eq!(Ok(s.as_str()), r.string(i));
    }
    assert_eq!(r.string(n), Err(RegionError::Strings(n)));
}

#[test]
fn the_florence_region_names_its_places_and_streets() {
    let r = Region::from_bytes(FLORENCE).expect("open");
    r.verify().expect("verify");
    let l = r.header().labels.expect("a version 2 file");
    assert_eq!(l.sections, 6, "every tile of the fixture has names");

    // z11: Florence, its towns and villages, and the German name where it
    // differs; no locality, hamlet or neighbourhood at a zoom that has
    // none of them.
    let z11 = TileId {
        z: 11,
        x: 1088,
        y: 746,
    };
    let s = r.labels(z11).expect("labels").expect("a section");
    let named: Vec<(LabelKind, &str, Option<&str>)> = s
        .points()
        .map(|(n, p)| {
            let alt = s.alternate(n).map(|a| r.string(a).expect("alt"));
            (p.kind, r.string(p.string).expect("name"), alt)
        })
        .collect();
    assert_eq!(
        named[0],
        (LabelKind::City, "Firenze", Some("Florenz")),
        "{named:?}"
    );
    assert!(named.iter().all(|(k, ..)| k.is_place()), "{named:?}");
    assert!(named
        .iter()
        .any(|(k, n, _)| *k == LabelKind::Town && *n == "Fiesole"));
    assert_eq!(s.lines().count(), 0, "no street names below z14");

    // z15: street names, each a line feature of its own tile.
    let z15 = TileId {
        z: 15,
        x: 17408,
        y: 11943,
    };
    let s = r.labels(z15).expect("labels").expect("a section");
    let fx = r.fixture(z15).expect("tile").expect("geometry");
    let features: Vec<Kind> = fx.features().map(|f| f.kind).collect();
    let streets: Vec<&str> = s
        .lines()
        .map(|(_, line)| {
            assert_eq!(features[usize::from(line.feature)], Kind::Line);
            assert!(line.kind.is_road() && line.min_zoom >= 14);
            r.string(line.string).expect("name")
        })
        .collect();
    assert!(streets.contains(&"Via del Proconsolo"), "{streets:?}");
}

/// The Florence region written as version 1: the same tiles under a
/// 64-byte header, no names.
fn as_version_1(v2: &Region<&[u8]>) -> Vec<u8> {
    let h = *v2.header();
    let mut out = vec![0u8; HEADER_LEN];
    let text = |s: &str, out: &mut Vec<u8>| {
        let at = out.len() as u32;
        out.extend_from_slice(s.as_bytes());
        (at, s.len() as u32)
    };
    let attribution = text(v2.attribution(), &mut out);
    let notice = text(v2.notice(), &mut out);
    let mut entries = Vec::new();
    let mut buf = Vec::new();
    for n in 0..h.tiles {
        let e = v2.entry(n).expect("entry");
        v2.read_tile(&e, &mut buf).expect("tile");
        entries.push(Entry {
            offset: out.len() as u64,
            ..e
        });
        out.extend_from_slice(&buf);
    }
    let index = out.len() as u64;
    for e in &entries {
        out.extend_from_slice(&e.encode());
    }
    let header = Header {
        attribution,
        notice,
        index,
        len: out.len() as u64,
        labels: None,
        ..h
    };
    out[..HEADER_LEN].copy_from_slice(&header.encode());
    out
}

#[test]
fn a_version_1_file_still_reads_without_names() {
    let v2 = Region::from_bytes(FLORENCE).expect("open");
    let bytes = as_version_1(&v2);
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 1);
    let v1 = Region::from_bytes(&bytes).expect("a version 1 file opens");
    v1.verify().expect("verify");
    assert_eq!(v1.header().labels, None);
    assert_eq!(v1.string_count(), 0);
    for n in 0..v2.header().tiles {
        let id = v2.entry(n).expect("entry").id;
        assert!(v1.labels(id).expect("labels").is_none());
        let (a, b) = (v1.fixture(id).expect("v1"), v2.fixture(id).expect("v2"));
        let count = |f: Option<leviculum_mapfill::fixture::Fixture<'_>>| f.map(|f| f.len());
        assert_eq!(count(a), count(b), "tile {id:?}");
    }
    assert_eq!(v1.find_string("Firenze"), Ok(None));
}

#[test]
fn a_version_this_reader_does_not_know_is_refused() {
    let mut b = FLORENCE.to_vec();
    b[4] = 3;
    assert_eq!(Region::from_bytes(&b).err(), Some(RegionError::Version(3)));
    // A version 1 reader's check is the same field: it sees 2 and stops,
    // where reading on would take the label index for tiles. Here a v2
    // file claiming version 1 is caught by its header length.
    let mut b = FLORENCE.to_vec();
    b[4] = 1;
    assert_eq!(
        Region::from_bytes(&b).err(),
        Some(RegionError::Header("header length"))
    );
    // The label index must end where the tile index starts.
    let r = Region::from_bytes(FLORENCE).expect("open");
    let mut h = *r.header();
    let mut b = FLORENCE.to_vec();
    if let Some(l) = h.labels.as_mut() {
        l.index -= ENTRY_LEN as u64;
    }
    b[..h.header_len()].copy_from_slice(&h.encode());
    assert_eq!(
        Region::from_bytes(&b).err(),
        Some(RegionError::Header("label index"))
    );
}
