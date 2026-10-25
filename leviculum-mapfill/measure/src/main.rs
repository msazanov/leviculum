//! Host harness of the map measurement (#474).
//!
//! `extract <tiles> <fixture>` decodes the z15 Protomaps tiles a 240x320
//! view centred on Santa Maria del Fiore touches (`<tiles>/15_<x>_<y>.mvt.gz`,
//! fetched as in `docs/drafts/map-measurement-2026-10.md`) and writes the
//! flat fixture the device binary carries in flash.
//!
//! `render <fixture> <outdir>` draws the fixture with `leviculum-mapfill`
//! under both layer profiles into an Rgb565 frame, writes
//! `<outdir>/view-<profile>.png`, and prints one `MAPFILL` line per
//! number: the counts, one phase-timed run and the median of 50 untimed
//! runs.

use std::io::Read;
use std::path::Path;
use std::time::Instant;

use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::fixture::{write, Fixture, Kind};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{render, Clock, NoClock, RenderStats, Work};
use leviculum_mapfill::style::{Class, Profile};
use prost::Message;
use tinymvt::geometry::GeometryDecoder;
use tinymvt::vector_tile::Tile;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const W: u32 = 240;
const H: u32 = 320;
const ZOOM: u8 = 15;
/// Santa Maria del Fiore, the first measurement's view centre.
const CENTRE: (f64, f64) = (43.77310, 11.25600);
const EXTENT: i64 = 4096;
const TILE_PX: i64 = 256;

/// Capacities the device binary uses as well (`m9/src/main.rs`).
const EDGES: usize = 4096;
const POINTS: usize = 4096;
const RINGS: usize = 1024;

fn class_of(layer: &str, kind: &str) -> Option<(Kind, Class)> {
    use Class::*;
    Some(match layer {
        "earth" => (Kind::Area, Earth),
        "landuse" => (
            Kind::Area,
            match kind {
                "garden" | "grass" | "park" | "playground" | "pitch" | "greenfield" => Park,
                "pedestrian" => Pedestrian,
                _ => Urban,
            },
        ),
        "natural" => (Kind::Area, if kind == "sand" { Sand } else { Park }),
        "water" => (Kind::Area, Water),
        "buildings" => (Kind::Area, Building),
        "roads" => (
            Kind::Line,
            match kind {
                "highway" | "major_road" | "medium_road" => MajorRoad,
                "path" => Path,
                _ => MinorRoad,
            },
        ),
        "transit" => (Kind::Line, Rail),
        "physical_line" => (Kind::Line, Waterway),
        _ => return None,
    })
}

/// The view's top-left corner in global tile units at [`ZOOM`].
fn view_origin_units() -> (i64, i64) {
    let (lat, lon) = CENTRE;
    let n = f64::from(1u32 << ZOOM) * TILE_PX as f64;
    let lr = lat.to_radians();
    let px = (lon + 180.0) / 360.0 * n - f64::from(W) / 2.0;
    let py = (1.0 - (lr.tan() + 1.0 / lr.cos()).ln() / std::f64::consts::PI) / 2.0 * n
        - f64::from(H) / 2.0;
    let k = (EXTENT / TILE_PX) as f64;
    ((px * k).round() as i64, (py * k).round() as i64)
}

fn i16_of(v: i32) -> Res<i16> {
    i16::try_from(v).map_err(|_| format!("tile coordinate {v} beyond i16").into())
}

fn extract(tiles: &Path, out: &Path) -> Res<()> {
    let (ox, oy) = view_origin_units();
    let mut names: Vec<(u32, u32, std::path::PathBuf)> = Vec::new();
    for e in std::fs::read_dir(tiles)? {
        let p = e?.path();
        let Some(n) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let f: Vec<&str> = n.trim_end_matches(".mvt.gz").split('_').collect();
        if f.len() == 3 && f[0] == ZOOM.to_string() {
            names.push((f[1].parse()?, f[2].parse()?, p.clone()));
        }
    }
    names.sort();
    let mut offsets = Vec::new();
    let mut feats: Vec<write::Feature> = Vec::new();
    let (mut verts, mut skipped) = (0usize, 0usize);
    for (ti, (tx, ty, p)) in names.iter().enumerate() {
        let gz = std::fs::read(p)?;
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut raw)?;
        let tile = Tile::decode(&raw[..])?;
        offsets.push((
            i32::try_from(i64::from(*tx) * EXTENT - ox)?,
            i32::try_from(i64::from(*ty) * EXTENT - oy)?,
        ));
        for l in &tile.layers {
            if i64::from(l.extent.unwrap_or(4096)) != EXTENT {
                return Err(format!("layer {} extent {:?}", l.name, l.extent).into());
            }
            let kidx = l.keys.iter().position(|k| k == "pmap:kind");
            for f in &l.features {
                let mut kind = "";
                if let Some(ki) = kidx {
                    for c in f.tags.chunks(2) {
                        if c[0] as usize == ki {
                            kind = l
                                .values
                                .get(c[1] as usize)
                                .and_then(|v| v.string_value.as_deref())
                                .unwrap_or("");
                        }
                    }
                }
                let Some((k, class)) = class_of(&l.name, kind) else {
                    skipped += 1;
                    continue;
                };
                let mut d = GeometryDecoder::new(&f.geometry);
                let rings: Vec<Vec<[i32; 2]>> = match (f.r#type, k) {
                    (Some(3), Kind::Area) => d.decode_polygons()?.into_iter().flatten().collect(),
                    (Some(2), Kind::Line) => d.decode_linestrings()?,
                    _ => {
                        skipped += 1;
                        continue;
                    }
                };
                let mut rs = Vec::new();
                for r in rings {
                    verts += r.len();
                    rs.push(
                        r.iter()
                            .map(|p| Ok((i16_of(p[0])?, i16_of(p[1])?)))
                            .collect::<Res<Vec<_>>>()?,
                    );
                }
                feats.push(write::Feature {
                    kind: k,
                    class,
                    tile: u8::try_from(ti)?,
                    rings: rs,
                });
            }
        }
    }
    // Draw order: by class, stable, so tile order holds within a class.
    feats.sort_by_key(|f| f.class);
    let bytes = write::fixture(W as u16, H as u16, 0, &offsets, &feats);
    std::fs::write(out, &bytes)?;
    println!(
        "MAPFILL extract tiles={} features={} vertices={verts} skipped={skipped} bytes={}",
        names.len(),
        feats.len(),
        bytes.len()
    );
    Fixture::parse(&bytes).map_err(|e| format!("written fixture does not parse: {e:?}"))?;
    Ok(())
}

/// Nanoseconds since construction, wrapping at 2^32 (4.29 s).
struct HostClock(Instant);

impl Clock for HostClock {
    fn now(&mut self) -> u32 {
        self.0.elapsed().as_nanos() as u32
    }
}

struct Buffers {
    edges: Vec<Edge>,
    heads: Vec<u16>,
    active: Vec<u16>,
    points: Vec<Point>,
    ring_ends: Vec<u16>,
    fb: Vec<u16>,
}

impl Buffers {
    fn new() -> Self {
        Self {
            edges: vec![Edge::default(); EDGES],
            heads: vec![0; H as usize],
            active: vec![0; EDGES],
            points: vec![Point::zero(); POINTS],
            ring_ends: vec![0; RINGS],
            fb: vec![0; (W * H) as usize],
        }
    }

    fn run<C: Clock>(
        &mut self,
        fx: &Fixture<'_>,
        profile: Profile,
        clock: &mut C,
    ) -> Res<RenderStats> {
        let mut frame = Frame::new(&mut self.fb, W, H).ok_or("frame")?;
        let mut scratch = Scratch {
            edges: &mut self.edges,
            heads: &mut self.heads,
            active: &mut self.active,
        };
        let mut work = Work {
            points: &mut self.points,
            ring_ends: &mut self.ring_ends,
        };
        Ok(render(
            fx,
            profile,
            &mut frame,
            &mut scratch,
            &mut work,
            clock,
        )?)
    }
}

fn write_png(fb: &mut [u16], path: &Path) -> Res<()> {
    let frame = Frame::new(fb, W, H).ok_or("frame")?;
    let mut rgb = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            let c: Rgb565 = frame.pixel(x, y).ok_or("pixel")?;
            let v = RawU16::from(c).into_inner();
            let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
            rgb.extend_from_slice(&[
                ((r << 3) | (r >> 2)) as u8,
                ((g << 2) | (g >> 4)) as u8,
                ((b << 3) | (b >> 2)) as u8,
            ]);
        }
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, W, H);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgb)?;
    Ok(())
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn render_cmd(fixture: &Path, outdir: &Path) -> Res<()> {
    let bytes = std::fs::read(fixture)?;
    let fx = Fixture::parse(&bytes).map_err(|e| format!("{e:?}"))?;
    std::fs::create_dir_all(outdir)?;
    let mut b = Buffers::new();
    println!(
        "MAPFILL fixture bytes={} features={} view={}x{}",
        bytes.len(),
        fx.len(),
        fx.view().0,
        fx.view().1
    );
    for profile in [Profile::Full, Profile::Stripped] {
        let p = profile.name();
        let mut clock = HostClock(Instant::now());
        let st = b.run(&fx, profile, &mut clock)?;
        println!(
            "MAPFILL host profile={p} features={} culled={} vertices={} max_feature_vertices={} overflows={}",
            st.features, st.culled, st.vertices, st.max_feature_vertices, st.overflows
        );
        println!(
            "MAPFILL host profile={p} fill_edges={} fill_spans={} fill_pixels={} stroke_edges={} stroke_spans={} stroke_pixels={}",
            st.fill.edges, st.fill.spans, st.fill.pixels, st.stroke.edges, st.stroke.spans, st.stroke.pixels
        );
        let ph = st.phases;
        for (name, ns) in [
            ("clear", ph.clear),
            ("project_clip", ph.project_clip),
            ("fill", ph.fill),
            ("stroke", ph.stroke),
            ("total", ph.total()),
        ] {
            println!(
                "MAPFILL host profile={p} phase={name} ms={:.3} (one timed run)",
                ns as f64 / 1e6
            );
        }
        let runs: Vec<f64> = (0..50)
            .map(|_| {
                let t = Instant::now();
                let r = b.run(&fx, profile, &mut NoClock);
                std::hint::black_box(r.is_ok());
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        println!(
            "MAPFILL host profile={p} phase=total ms={:.3} (median of 50 untimed)",
            median(runs)
        );
        let png = outdir.join(format!("view-{p}.png"));
        write_png(&mut b.fb, &png)?;
        println!("MAPFILL host profile={p} png={}", png.display());
    }
    Ok(())
}

fn main() -> Res<()> {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("extract") if a.len() == 4 => extract(Path::new(&a[2]), Path::new(&a[3])),
        Some("render") if a.len() == 4 => render_cmd(Path::new(&a[2]), Path::new(&a[3])),
        _ => Err(
            "usage: measure extract <tiles-dir> <fixture> | measure render <fixture> <outdir>"
                .into(),
        ),
    }
}
