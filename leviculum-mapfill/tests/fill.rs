//! Pixel-exact checks of the filler and the stroker against small reference
//! images: `#` is a filled pixel, `.` an empty one.

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use leviculum_mapfill::fill::{fill_rings, Edge, FillError, FillRule, Filler, Scratch};
use leviculum_mapfill::stroke::{isqrt, stroke};
use leviculum_mapfill::SUBPIXEL;

/// Counts how often each pixel was painted, so overlaps show as well as gaps.
struct Canvas {
    w: u32,
    h: u32,
    hits: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            hits: vec![0; (w * h) as usize],
        }
    }

    fn picture(&self) -> String {
        let mut s = String::new();
        for y in 0..self.h {
            for x in 0..self.w {
                s.push(match self.hits[(y * self.w + x) as usize] {
                    0 => '.',
                    1 => '#',
                    _ => '2',
                });
            }
            s.push('\n');
        }
        s
    }
}

impl OriginDimensions for Canvas {
    fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }
}

impl DrawTarget for Canvas {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<BinaryColor>>>(
        &mut self,
        it: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, _) in it {
            assert!(
                p.x >= 0 && p.y >= 0 && (p.x as u32) < self.w && (p.y as u32) < self.h,
                "filler drew outside the clip at {p:?}"
            );
            self.hits[(p.y as u32 * self.w + p.x as u32) as usize] += 1;
        }
        Ok(())
    }
}

/// A point at whole pixel coordinates, in sub-pixels.
fn p(x: i32, y: i32) -> Point {
    Point::new(x * SUBPIXEL, y * SUBPIXEL)
}

struct Store {
    edges: Vec<Edge>,
    heads: Vec<u16>,
    active: Vec<u16>,
}

impl Store {
    fn new(edges: usize, rows: usize) -> Self {
        Self {
            edges: vec![Edge::default(); edges],
            heads: vec![0; rows],
            active: vec![0; edges],
        }
    }

    fn scratch(&mut self) -> Scratch<'_> {
        Scratch {
            edges: &mut self.edges,
            heads: &mut self.heads,
            active: &mut self.active,
        }
    }
}

fn draw(w: u32, h: u32, rings: &[&[Point]], rule: FillRule) -> String {
    let mut c = Canvas::new(w, h);
    let mut st = Store::new(64, h as usize);
    let clip = c.bounding_box();
    fill_rings(
        &mut st.scratch(),
        clip,
        rings,
        &mut c,
        BinaryColor::On,
        rule,
    )
    .expect("fill");
    c.picture()
}

fn both(w: u32, h: u32, rings: &[&[Point]]) -> String {
    let a = draw(w, h, rings, FillRule::EvenOdd);
    let b = draw(w, h, rings, FillRule::NonZero);
    assert_eq!(a, b, "the two rules disagree where they must not");
    a
}

fn pic(rows: &[&str]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

#[test]
fn square() {
    let ring = [p(1, 1), p(5, 1), p(5, 4), p(1, 4)];
    assert_eq!(
        both(6, 5, &[&ring]),
        pic(&["......", ".####.", ".####.", ".####.", "......"])
    );
}

#[test]
fn square_with_hole() {
    // The hole runs the other way, as MVT writes interior rings.
    let outer = [p(0, 0), p(6, 0), p(6, 6), p(0, 6)];
    let hole = [p(2, 2), p(2, 4), p(4, 4), p(4, 2)];
    assert_eq!(
        both(6, 6, &[&outer, &hole]),
        pic(&["######", "######", "##..##", "##..##", "######", "######"])
    );
}

#[test]
fn self_touching_ring() {
    // One ring through the shared corner (3, 3) twice: two squares.
    let ring = [
        p(0, 0),
        p(3, 0),
        p(3, 3),
        p(6, 3),
        p(6, 6),
        p(3, 6),
        p(3, 3),
        p(0, 3),
    ];
    assert_eq!(
        both(6, 6, &[&ring]),
        pic(&["###...", "###...", "###...", "...###", "...###", "...###"])
    );
}

#[test]
fn degenerate_rings_fill_nothing() {
    let there_and_back = [p(1, 1), p(4, 4), p(1, 1)];
    let collinear = [p(0, 2), p(2, 2), p(5, 2)];
    let single = [p(3, 3)];
    let empty = pic(&[".....", ".....", ".....", ".....", "....."]);
    assert_eq!(both(5, 5, &[&there_and_back]), empty);
    assert_eq!(both(5, 5, &[&collinear]), empty);
    assert_eq!(both(5, 5, &[&single]), empty);
    assert_eq!(both(5, 5, &[]), empty);
}

#[test]
fn diagonal_samples_pixel_centres() {
    // The pixel (x, y) is in when x + 0.5 + y + 0.5 < 6; a centre exactly
    // on the hypotenuse, x + y = 5, is on a right edge and so out.
    let tri = [p(0, 0), p(6, 0), p(0, 6)];
    assert_eq!(
        both(6, 6, &[&tri]),
        pic(&["#####.", "####..", "###...", "##....", "#.....", "......"])
    );
}

#[test]
fn edges_beyond_the_clip() {
    // Off the top left, and a ring whose right side lies wholly right of
    // the clip, so the filler never stores its closing edge.
    let top_left = [p(-3, -2), p(3, -2), p(3, 2), p(-3, 2)];
    let right = [p(4, -10), p(100, -10), p(100, 3), p(4, 3)];
    assert_eq!(
        both(6, 5, &[&top_left, &right]),
        pic(&["###.##", "###.##", "....##", "......", "......"])
    );
    // A clip that does not start at the origin.
    let mut c = Canvas::new(6, 5);
    let mut st = Store::new(16, 5);
    let clip = Rectangle::new(Point::new(1, 1), Size::new(3, 2));
    let all = [p(-5, -5), p(20, -5), p(20, 20), p(-5, 20)];
    fill_rings(
        &mut st.scratch(),
        clip,
        &[&all],
        &mut c,
        BinaryColor::On,
        FillRule::NonZero,
    )
    .expect("fill");
    assert_eq!(
        c.picture(),
        pic(&["......", ".###..", ".###..", "......", "......"])
    );
}

#[test]
fn fill_rules_differ_on_overlap() {
    // Two squares, same orientation, overlapping in the middle.
    let a = [p(0, 0), p(4, 0), p(4, 4), p(0, 4)];
    let b = [p(2, 2), p(6, 2), p(6, 6), p(2, 6)];
    assert_eq!(
        draw(6, 6, &[&a, &b], FillRule::EvenOdd),
        pic(&["####..", "####..", "##..##", "##..##", "..####", "..####"])
    );
    assert_eq!(
        draw(6, 6, &[&a, &b], FillRule::NonZero),
        pic(&["####..", "####..", "######", "######", "..####", "..####"])
    );
    // A same-direction inner ring is a hole only under even-odd.
    let outer = [p(0, 0), p(6, 0), p(6, 6), p(0, 6)];
    let inner = [p(2, 2), p(4, 2), p(4, 4), p(2, 4)];
    assert_eq!(
        draw(6, 6, &[&outer, &inner], FillRule::EvenOdd),
        pic(&["######", "######", "##..##", "##..##", "######", "######"])
    );
    assert_eq!(
        draw(6, 6, &[&outer, &inner], FillRule::NonZero),
        pic(&["######", "######", "######", "######", "######", "######"])
    );
}

#[test]
fn shared_edges_neither_overlap_nor_gap() {
    // A square cut along both diagonals into four triangles, each filled
    // on its own: every pixel painted exactly once.
    let (w, h) = (16u32, 16u32);
    let mut c = Canvas::new(w, h);
    let mut st = Store::new(16, h as usize);
    let clip = c.bounding_box();
    let m = Point::new(8 * SUBPIXEL + 5, 8 * SUBPIXEL - 3);
    let corners = [p(0, 0), p(16, 0), p(16, 16), p(0, 16)];
    for i in 0..4 {
        let tri = [corners[i], corners[(i + 1) % 4], m];
        fill_rings(
            &mut st.scratch(),
            clip,
            &[&tri],
            &mut c,
            BinaryColor::On,
            FillRule::EvenOdd,
        )
        .expect("fill");
    }
    assert!(c.hits.iter().all(|&n| n == 1), "{}", c.picture());
}

#[test]
fn overflow_is_reported_and_draws_nothing() {
    let mut c = Canvas::new(6, 5);
    let mut st = Store::new(1, 5);
    let ring = [p(1, 1), p(5, 1), p(5, 4), p(1, 4)];
    let clip = c.bounding_box();
    let r = fill_rings(
        &mut st.scratch(),
        clip,
        &[&ring],
        &mut c,
        BinaryColor::On,
        FillRule::EvenOdd,
    );
    assert_eq!(r, Err(FillError::Overflow));
    assert!(c.hits.iter().all(|&n| n == 0));
}

#[test]
fn wide_lines() {
    // One pixel wide from x 1.5 to 4.5, square caps of half a pixel: the
    // quad runs from x 1 to 5.
    let mut c = Canvas::new(7, 5);
    let mut st = Store::new(64, 5);
    let clip = c.bounding_box();
    let mut s = st.scratch();
    let mut f = Filler::new(&mut s, clip);
    let half = SUBPIXEL / 2;
    let line = [
        Point::new(SUBPIXEL + half, 2 * SUBPIXEL + half),
        Point::new(4 * SUBPIXEL + half, 2 * SUBPIXEL + half),
    ];
    let stats = stroke(&mut f, &line, SUBPIXEL, &mut c, BinaryColor::On).expect("stroke");
    assert_eq!(
        c.picture(),
        pic(&[".......", ".......", ".####..", ".......", "......."])
    );
    assert_eq!(stats.pixels, 4);
    // Three pixels wide around a right-angle joint: the caps close it.
    let mut c = Canvas::new(7, 7);
    let mut st = Store::new(64, 7);
    let mut s = st.scratch();
    let mut f = Filler::new(&mut s, c.bounding_box());
    let corner = [
        Point::new(half, 3 * SUBPIXEL + half),
        Point::new(3 * SUBPIXEL + half, 3 * SUBPIXEL + half),
        Point::new(3 * SUBPIXEL + half, 7 * SUBPIXEL),
    ];
    stroke(&mut f, &corner, 3 * SUBPIXEL, &mut c, BinaryColor::On).expect("stroke");
    assert_eq!(
        c.picture(),
        pic(&[".......", ".......", "#####..", "#####..", "#####..", "..###..", "..###.."])
    );
}

#[test]
fn stroke_refills_when_the_scratch_runs_out() {
    // Room for one quad at a time: every segment still lands, and where two
    // caps overlap the pixel is painted twice (`2`), harmless for an opaque
    // colour. With room for all quads the same line paints each pixel once.
    let mut c = Canvas::new(10, 3);
    let mut st = Store::new(4, 3);
    let mut s = st.scratch();
    let mut f = Filler::new(&mut s, c.bounding_box());
    let y = SUBPIXEL + SUBPIXEL / 2;
    let line: Vec<Point> = (0..5)
        .map(|i| Point::new(2 * i * SUBPIXEL + SUBPIXEL / 2, y))
        .collect();
    stroke(&mut f, &line, SUBPIXEL, &mut c, BinaryColor::On).expect("stroke");
    assert_eq!(
        c.picture(),
        pic(&["..........", "##2#2#2##.", ".........."])
    );
    let mut c = Canvas::new(10, 3);
    let mut st = Store::new(64, 3);
    let mut s = st.scratch();
    let mut f = Filler::new(&mut s, c.bounding_box());
    stroke(&mut f, &line, SUBPIXEL, &mut c, BinaryColor::On).expect("stroke");
    assert_eq!(
        c.picture(),
        pic(&["..........", "#########.", ".........."])
    );
}

#[test]
fn isqrt_is_floor_sqrt() {
    for v in [
        0u64,
        1,
        2,
        3,
        4,
        15,
        16,
        17,
        99,
        100,
        1 << 40,
        (1 << 40) - 1,
        u64::MAX,
    ] {
        let r = isqrt(v);
        assert!(u128::from(r) * u128::from(r) <= u128::from(v));
        assert!(u128::from(r + 1) * u128::from(r + 1) > u128::from(v));
    }
}
