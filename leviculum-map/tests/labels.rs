//! The label rule ([`leviculum_map::pins::labels`]) in both units a
//! frontend counts in: the terminal's cells and a GUI's pixels. The cell
//! cases are the ones the rule was written for in loquax (1066); the pixel
//! cases are the same situations at a font's sizes.

use leviculum_map::pins::{labels, Label, Mark, Metrics, Side};

const AT: Mark = Mark { x: 20, y: 5 };

fn sides(out: &[Option<Label>]) -> Vec<Option<Side>> {
    out.iter().map(|l| l.map(|l| l.side)).collect()
}

fn cells(marks: &[Mark], len: u32, selected: Option<usize>, size: (u32, u32)) -> Vec<Option<Side>> {
    sides(&labels(marks, |_| len, selected, size, Metrics::CELLS))
}

/// Two pins on one cell: the selected keeps the right, whichever comes
/// first; the other goes left.
#[test]
fn two_pins_at_one_spot_share_the_sides_the_selected_right() {
    let marks = [AT, AT];
    let size = (60, 12);
    assert_eq!(
        cells(&marks, 8, Some(0), size),
        vec![Some(Side::Right), Some(Side::Left)]
    );
    assert_eq!(
        cells(&marks, 8, Some(1), size),
        vec![Some(Side::Left), Some(Side::Right)]
    );
    assert_eq!(
        cells(&marks, 8, None, size),
        vec![Some(Side::Right), Some(Side::Left)]
    );
}

/// Three on one cell: right, left, then the row above; four take the row
/// below; a fifth is dropped while the selected never is.
#[test]
fn three_pins_at_one_spot_go_above_and_a_name_without_room_is_dropped() {
    assert_eq!(
        cells(&[AT; 3], 8, Some(2), (60, 12)),
        vec![Some(Side::Left), Some(Side::Above), Some(Side::Right)]
    );
    assert_eq!(
        cells(&[AT; 5], 8, Some(4), (60, 12)),
        vec![
            Some(Side::Left),
            Some(Side::Above),
            Some(Side::Below),
            None,
            Some(Side::Right)
        ]
    );
}

/// A neighbour's marker is not covered: the name beside it moves to the
/// other side.
#[test]
fn a_name_does_not_cover_a_neighbours_marker() {
    let next = Mark { x: 24, ..AT };
    assert_eq!(
        cells(&[AT, next], 8, Some(1), (60, 12)),
        vec![Some(Side::Left), Some(Side::Right)]
    );
}

/// Hemmed in on every side, the selected still keeps its name: right, or
/// left at the picture's right edge.
#[test]
fn the_selected_keeps_its_name_where_no_side_is_free() {
    let corner = Mark { x: 55, y: 0 };
    let below = Mark { y: 1, ..corner };
    let left = Mark { x: 50, ..corner };
    let out = cells(&[below, left, corner], 8, Some(2), (60, 2));
    assert_eq!(out[2], Some(Side::Left));
}

/// Where the text starts in cells: right after the marker and one blank,
/// left so it ends one blank before the marker, apart shifted in from the
/// right edge.
#[test]
fn the_text_starts_where_the_terminal_writes_it() {
    let out = labels(&[AT, AT, AT], |_| 8, None, (60, 12), Metrics::CELLS);
    assert_eq!(
        out,
        vec![
            Some(Label {
                side: Side::Right,
                x: 22,
                y: 5
            }),
            Some(Label {
                side: Side::Left,
                x: 11,
                y: 5
            }),
            Some(Label {
                side: Side::Above,
                x: 20,
                y: 4
            }),
        ]
    );
    let edge = Mark { x: 58, y: 5 };
    let out = labels(&[edge, edge, edge], |_| 8, None, (60, 12), Metrics::CELLS);
    // Right does not fit at the edge: left, then above and below, both
    // shifted in so they end on the picture's last cell.
    assert_eq!(out[0].map(|l| (l.side, l.x)), Some((Side::Left, 49)));
    assert_eq!(out[1].map(|l| (l.side, l.x)), Some((Side::Above, 52)));
    assert_eq!(
        out[2].map(|l| (l.side, l.x, l.y)),
        Some((Side::Below, 52, 6))
    );
}

/// A GUI's pixels: 10-pixel markers, a 4-pixel gap, 16-pixel lines.
const PX: Metrics = Metrics {
    marker: 10,
    gap: 4,
    line: 16,
};

/// The measure decides: a long name that does not fit on the right at
/// the edge goes left, a short one beside it stays right.
#[test]
fn in_pixels_the_measured_length_decides_the_side() {
    let near_edge = Mark { x: 700, y: 300 };
    let widths = [150u32, 40];
    let out = labels(
        &[near_edge, Mark { x: 100, y: 300 }],
        |i| widths[i],
        None,
        (800, 600),
        PX,
    );
    assert_eq!(
        out[0],
        Some(Label {
            side: Side::Left,
            x: 700 - 4 - 150,
            y: 300
        })
    );
    assert_eq!(
        out[1],
        Some(Label {
            side: Side::Right,
            x: 100 + 10 + 4,
            y: 300
        })
    );
}

/// Pins a few pixels apart overlap as boxes, not only on the same row:
/// the selected keeps the right, no label covers another label or any
/// marker, and a label hemmed in by its neighbours' markers is dropped.
#[test]
fn in_pixels_labels_of_close_pins_do_not_overlap() {
    let marks = [
        Mark { x: 400, y: 300 },
        Mark { x: 402, y: 305 },
        Mark { x: 398, y: 298 },
        Mark { x: 401, y: 302 },
        Mark { x: 400, y: 301 },
    ];
    let len = 60;
    let out = labels(&marks, |_| len, Some(4), (800, 600), PX);
    assert_eq!(
        out[4].map(|l| l.side),
        Some(Side::Right),
        "the selected first"
    );
    assert!(
        out.iter().any(Option::is_none),
        "a crowded label is dropped"
    );
    let boxes: Vec<(u32, u32, u32)> = out.iter().flatten().map(|l| (l.x, l.y, len)).collect();
    let markers = marks.iter().map(|m| (m.x, m.y, PX.marker));
    let apart = |a: (u32, u32, u32), b: (u32, u32, u32)| {
        a.0 + a.2 <= b.0 || b.0 + b.2 <= a.0 || a.1 + PX.line <= b.1 || b.1 + PX.line <= a.1
    };
    for (i, a) in boxes.iter().enumerate() {
        for b in boxes[i + 1..].iter().copied().chain(markers.clone()) {
            assert!(apart(*a, b), "{a:?} covers {b:?}");
        }
    }
}
