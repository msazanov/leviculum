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
use crate::style::{
    area_color, line_style, line_width, LineWidths, Profile, BACKGROUND, LINE_WIDTHS,
};

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

/// Where a feature's tile units land in the target: `(offset + v)` is
/// shifted by `shift` (left when positive), then `origin` sub-pixels are
/// added. A fixture drawn on its own is placed at its own shift and the
/// origin zero; the map core places each tile of a view at its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub shift: i8,
    pub origin: Point,
}

/// Project `f` into `w`: the number of rings, the bounding box and the
/// number of vertices, or `None` when it does not fit.
fn project_feature(
    f: &Feature<'_>,
    at: Placement,
    w: &mut Work<'_>,
) -> Option<(usize, Bbox, usize)> {
    let (mut n, mut nr) = (0usize, 0usize);
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for ring in f.rings() {
        if n + ring.len() > w.points.len() || nr >= w.ring_ends.len() {
            return None;
        }
        for (x, y) in ring.points() {
            let p = Point::new(
                project(f.offset.x + i32::from(x), at.shift) + at.origin.x,
                project(f.offset.y + i32::from(y), at.shift) + at.origin.y,
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

/// How a pass inks what it draws: the line widths per class, and whether
/// areas are filled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// Each line class's width; 0 leaves the class out.
    pub widths: LineWidths,
    /// `Some`: lines only, all in this colour, and no area filled: the
    /// terminal's braille backend, which has one colour a cell and no fill.
    pub mono: Option<Rgb565>,
}

impl Look {
    /// The colour tables at the fixtures' widths.
    pub const DEFAULT: Self = Self {
        widths: LINE_WIDTHS,
        mono: None,
    };
}

impl Look {
    /// The same look with every line `percent` per cent as wide: 200 on a
    /// screen of twice the ordinary density, where a picture drawn at the
    /// screen's own pixels would otherwise show every road half as thick
    /// to the eye. 100 is this look unchanged.
    pub fn scaled(self, percent: u32) -> Self {
        let scale = |w: i32| {
            let wide = i64::from(w) * i64::from(percent) / 100;
            i32::try_from(wide).unwrap_or(i32::MAX)
        };
        Self {
            widths: self.widths.map(scale),
            ..self
        }
    }
}

impl Default for Look {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// One frame being drawn feature by feature: the filler, the view in
/// sub-pixels, the clock's last reading and the counts so far.
///
/// [`render`] is one pass over one fixture. A caller that draws several
/// fixtures into one frame (the map core, one per tile) opens one pass and
/// hands it the features in the order they are to be painted.
pub struct Pass<'s, 'a> {
    filler: Filler<'s, 'a>,
    view: Bbox,
    prev: u32,
    look: Look,
    /// What the pass drew so far.
    pub stats: RenderStats,
}

impl<'s, 'a> Pass<'s, 'a> {
    /// Clear `target` to the background and open a pass clipped to it.
    ///
    /// `scratch.heads` must hold at least the target's height in rows.
    pub fn begin<D, C>(
        target: &mut D,
        scratch: &'s mut Scratch<'a>,
        clock: &mut C,
    ) -> Result<Self, D::Error>
    where
        D: DrawTarget<Color = Rgb565>,
        C: Clock,
    {
        let mut stats = RenderStats::default();
        let clip = target.bounding_box();
        let t = clock.now();
        target.clear(BACKGROUND)?;
        let prev = clock.now();
        stats.phases.clear += u64::from(prev.wrapping_sub(t));
        let sub = crate::fill::SUBPIXEL;
        let (vl, vt) = (clip.top_left.x * sub, clip.top_left.y * sub);
        let (vr, vb) = (
            vl + clip.size.width as i32 * sub,
            vt + clip.size.height as i32 * sub,
        );
        Ok(Self {
            filler: Filler::new(scratch, clip),
            view: (vl, vt, vr, vb),
            prev,
            look: Look::DEFAULT,
            stats,
        })
    }

    /// The same pass inked by `look` instead of [`Look::DEFAULT`].
    pub fn with_look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    /// Draw one feature placed at `at`, if `profile` includes its class
    /// and any of it is in view.
    pub fn feature<D, C>(
        &mut self,
        f: &Feature<'_>,
        at: Placement,
        profile: Profile,
        target: &mut D,
        work: &mut Work<'_>,
        clock: &mut C,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = Rgb565>,
        C: Clock,
    {
        // A class the look leaves out is skipped as one the profile does:
        // a line class at width 0, every area in the lines-only look.
        let width = match f.kind {
            Kind::Area if self.look.mono.is_some() => return Ok(()),
            Kind::Area => 0,
            Kind::Line => line_width(&self.look.widths, f.class),
        };
        if !profile.includes(f.class) || (f.kind == Kind::Line && width == 0) {
            return Ok(());
        }
        let st = &mut self.stats;
        let (vl, vt, vr, vb) = self.view;
        st.features += 1;
        let projected = project_feature(f, at, work);
        let pad = width;
        let visible = projected.map(|(_, (x0, y0, x1, y1), n)| {
            n > 0 && x1 + pad >= vl && x0 - pad < vr && y1 + pad >= vt && y0 - pad < vb
        });
        let now = clock.now();
        st.phases.project_clip += u64::from(now.wrapping_sub(self.prev));
        self.prev = now;
        let Some((nr, _, n)) = projected else {
            st.overflows += 1;
            return Ok(());
        };
        st.vertices += n as u32;
        st.max_feature_vertices = st.max_feature_vertices.max(n as u32);
        if visible != Some(true) {
            st.culled += 1;
            return Ok(());
        }
        let filler = &mut self.filler;
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
                st.phases.fill += u64::from(now.wrapping_sub(self.prev));
                self.prev = now;
            }
            Kind::Line => {
                let color = self.look.mono.unwrap_or(line_style(f.class).0);
                for &end in &work.ring_ends[..nr] {
                    let end = usize::from(end);
                    st.stroke += stroke(filler, &work.points[start..end], width, target, color)?;
                    start = end;
                }
                let now = clock.now();
                st.phases.stroke += u64::from(now.wrapping_sub(self.prev));
                self.prev = now;
            }
        }
        Ok(())
    }
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
    let mut pass = Pass::begin(target, scratch, clock)?;
    let at = Placement {
        shift: fx.shift(),
        origin: Point::zero(),
    };
    for f in fx.features() {
        pass.feature(&f, at, profile, target, work, clock)?;
    }
    Ok(pass.stats)
}
