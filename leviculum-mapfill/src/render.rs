//! One view from a fixture: project and cull, fill the areas, stroke the
//! lines, each phase timed by a caller-supplied clock.
//!
//! The same function runs in the host harness and on the device, so the
//! two measure the same code. The clock is read three times per feature;
//! on the ESP32-S3 that is the `CCOUNT` register, one instruction.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;

use crate::fill::{FillRule, FillStats, Filler, Scratch};
use crate::fixture::{Feature, Fixture, Kind};
use crate::stroke::stroke;
use crate::style::{area_color, line_style, Profile, BACKGROUND};

/// A free-running tick counter; differences are taken wrapping.
pub trait Clock {
    fn now(&mut self) -> u32;
}

/// A clock that never moves, for runs that time only the whole.
pub struct NoClock;

impl Clock for NoClock {
    fn now(&mut self) -> u32 {
        0
    }
}

/// Ticks spent per phase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Phases {
    /// Clearing the frame to the background.
    pub clear: u64,
    /// Projecting vertices to sub-pixels and culling features outside.
    pub project_clip: u64,
    /// Filling areas.
    pub fill: u64,
    /// Stroking lines.
    pub stroke: u64,
}

impl Phases {
    /// All phases together.
    pub fn total(&self) -> u64 {
        self.clear + self.project_clip + self.fill + self.stroke
    }
}

/// What one view drew.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Features the profile includes.
    pub features: u32,
    /// Of those, wholly outside the view.
    pub culled: u32,
    /// Vertices projected.
    pub vertices: u32,
    /// Area fills.
    pub fill: FillStats,
    /// Line strokes.
    pub stroke: FillStats,
    /// Features skipped because the work or edge storage was too small.
    pub overflows: u32,
    /// Most vertices one feature held.
    pub max_feature_vertices: u32,
    pub phases: Phases,
}

/// Per-feature storage for projected vertices: the points, and where each
/// ring ends in them.
pub struct Work<'a> {
    pub points: &'a mut [Point],
    pub ring_ends: &'a mut [u16],
}

fn project(v: i32, shift: i8) -> i32 {
    if shift >= 0 {
        v << shift
    } else {
        v >> -shift
    }
}

/// Sub-pixel bounding box, (left, top, right, bottom), all inclusive.
type Bbox = (i32, i32, i32, i32);

/// Project `f` into `w`: the number of rings, the bounding box and the
/// number of vertices, or `None` when it does not fit.
fn project_feature(f: &Feature<'_>, shift: i8, w: &mut Work<'_>) -> Option<(usize, Bbox, usize)> {
    let (mut n, mut nr) = (0usize, 0usize);
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for ring in f.rings() {
        if n + ring.len() > w.points.len() || nr >= w.ring_ends.len() {
            return None;
        }
        for (x, y) in ring.points() {
            let p = Point::new(
                project(f.offset.x + i32::from(x), shift),
                project(f.offset.y + i32::from(y), shift),
            );
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
            w.points[n] = p;
            n += 1;
        }
        w.ring_ends[nr] = n as u16;
        nr += 1;
    }
    Some((nr, (x0, y0, x1, y1), n))
}

/// Draw `fx` under `profile` into `target`, which is cleared first.
///
/// `scratch.heads` must hold at least the view's height in rows. The
/// clip is the target's bounding box.
pub fn render<D, C>(
    fx: &Fixture<'_>,
    profile: Profile,
    target: &mut D,
    scratch: &mut Scratch<'_>,
    work: &mut Work<'_>,
    clock: &mut C,
) -> Result<RenderStats, D::Error>
where
    D: DrawTarget<Color = Rgb565>,
    C: Clock,
{
    let mut st = RenderStats::default();
    let clip = target.bounding_box();
    let t = clock.now();
    target.clear(BACKGROUND)?;
    let mut prev = clock.now();
    st.phases.clear += u64::from(prev.wrapping_sub(t));
    let sub = crate::fill::SUBPIXEL;
    let (vl, vt) = (clip.top_left.x * sub, clip.top_left.y * sub);
    let (vr, vb) = (
        vl + clip.size.width as i32 * sub,
        vt + clip.size.height as i32 * sub,
    );
    let mut filler = Filler::new(scratch, clip);
    for f in fx.features() {
        if !profile.includes(f.class) {
            continue;
        }
        st.features += 1;
        let projected = project_feature(&f, fx.shift(), work);
        let pad = match f.kind {
            Kind::Area => 0,
            Kind::Line => line_style(f.class).1,
        };
        let visible = projected.map(|(_, (x0, y0, x1, y1), n)| {
            n > 0 && x1 + pad >= vl && x0 - pad < vr && y1 + pad >= vt && y0 - pad < vb
        });
        let now = clock.now();
        st.phases.project_clip += u64::from(now.wrapping_sub(prev));
        prev = now;
        let Some((nr, _, n)) = projected else {
            st.overflows += 1;
            continue;
        };
        st.vertices += n as u32;
        st.max_feature_vertices = st.max_feature_vertices.max(n as u32);
        if visible != Some(true) {
            st.culled += 1;
            continue;
        }
        let mut start = 0usize;
        match f.kind {
            Kind::Area => {
                let mut ok = true;
                for &end in &work.ring_ends[..nr] {
                    let end = usize::from(end);
                    if filler.ring(&work.points[start..end]).is_err() {
                        ok = false;
                        break;
                    }
                    start = end;
                }
                if ok {
                    st.fill += filler.fill(target, area_color(f.class), FillRule::NonZero)?;
                } else {
                    filler.clear();
                    st.overflows += 1;
                }
                let now = clock.now();
                st.phases.fill += u64::from(now.wrapping_sub(prev));
                prev = now;
            }
            Kind::Line => {
                let (color, width) = line_style(f.class);
                for &end in &work.ring_ends[..nr] {
                    let end = usize::from(end);
                    st.stroke +=
                        stroke(&mut filler, &work.points[start..end], width, target, color)?;
                    start = end;
                }
                let now = clock.now();
                st.phases.stroke += u64::from(now.wrapping_sub(prev));
                prev = now;
            }
        }
    }
    Ok(st)
}
