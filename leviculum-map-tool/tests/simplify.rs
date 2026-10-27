//! Douglas-Peucker, quantisation and areas.

use leviculum_map_tool::simplify::{
    area2, douglas_peucker, quantise, quantise_path, simplify_ring,
};

#[test]
fn douglas_peucker_keeps_the_ends_and_the_corners() {
    let straight: Vec<[i32; 2]> = (0..10).map(|i| [i * 10, 0]).collect();
    assert_eq!(douglas_peucker(&straight, 1), [[0, 0], [90, 0]]);
    let corner = [[0, 0], [50, 1], [100, 0], [100, 100]];
    assert_eq!(douglas_peucker(&corner, 2), [[0, 0], [100, 0], [100, 100]]);
    // A bump exactly at the tolerance stays out, one past it stays in.
    assert_eq!(douglas_peucker(&[[0, 0], [5, 3], [10, 0]], 3).len(), 2);
    assert_eq!(douglas_peucker(&[[0, 0], [5, 4], [10, 0]], 3).len(), 3);
    assert_eq!(douglas_peucker(&[[1, 1]], 5), [[1, 1]]);
}

#[test]
fn a_ring_keeps_its_shape_and_drops_its_closing_repeat() {
    let mut sq = vec![[0, 0], [50, 0], [100, 0], [100, 100], [0, 100], [0, 0]];
    let r = simplify_ring(&sq, 4);
    assert_eq!(r, [[0, 0], [100, 0], [100, 100], [0, 100]]);
    assert_eq!(area2(&r), 20_000);
    sq.reverse();
    assert_eq!(area2(&simplify_ring(&sq, 4)), -20_000);
}

#[test]
fn quantisation_rounds_and_dedupes() {
    assert_eq!(quantise(7, 3), 1);
    assert_eq!(quantise(3, 3), 0);
    assert_eq!(quantise(4, 3), 1);
    assert_eq!(quantise(-4, 3), 0);
    assert_eq!(quantise(-5, 3), -1);
    assert_eq!(quantise(4100, 0), 4100);
    let ring = [[0, 0], [1, 1], [16, 0], [16, 16], [2, 1]];
    assert_eq!(quantise_path(&ring, 3, true), [[0, 0], [2, 0], [2, 2]]);
    assert_eq!(
        quantise_path(&[[0, 0], [1, 0], [9, 0]], 3, false),
        [[0, 0], [1, 0]]
    );
}
