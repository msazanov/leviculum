//! `leviculum-map-tool cut | info | render`.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Parser, Subcommand};
use embedded_graphics::prelude::*;
use leviculum_map::draw::{draw, draw_tiles, Ink};
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::region::{Bounds, Region};
use leviculum_map::viewport::Viewport;
use leviculum_map_tool::cut::{cut, CutOptions, ZoomStats};
use leviculum_map_tool::pmtiles::Archive;
use leviculum_map_tool::{Res, ATTRIBUTION};
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{NoClock, Work};
use leviculum_mapfill::style::Profile;

/// See the manifest: musl's allocator made the cut kernel-bound.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(about = "Cut and inspect leviculum map region files")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Cut a region file from Protomaps PMTiles archives: one, or the parts
    /// of one build downloaded separately.
    Cut {
        #[arg(short, long)]
        output: PathBuf,
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// west,south,east,north in degrees.
        #[arg(long, allow_hyphen_values = true)]
        bbox: String,
        #[arg(long, default_value_t = 0)]
        min_zoom: u8,
        #[arg(long, default_value_t = 15)]
        max_zoom: u8,
        #[arg(long, default_value_t = leviculum_map::profile::CUT_1)]
        profile: u8,
        /// Worker threads; the output does not depend on it.
        #[arg(long)]
        threads: Option<usize>,
    },
    /// Print a region's header and per-zoom sizes, after checking it whole.
    Info { region: PathBuf },
    /// Draw one view of a region into a binary PPM.
    Render {
        region: PathBuf,
        output: PathBuf,
        #[arg(long, allow_hyphen_values = true)]
        lat: f64,
        #[arg(long, allow_hyphen_values = true)]
        lon: f64,
        #[arg(long)]
        zoom: u8,
        #[arg(long, default_value_t = 240)]
        width: u32,
        #[arg(long, default_value_t = 320)]
        height: u32,
    },
}

fn parse_bbox(s: &str) -> Res<Bounds> {
    let v: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()?;
    let [w, so, e, n] = v[..] else {
        return Err("--bbox wants west,south,east,north".into());
    };
    let e7 = |d: f64| (d * 1e7).round() as i32;
    Ok(Bounds {
        west: e7(w),
        south: e7(so),
        east: e7(e),
        north: e7(n),
    })
}

fn source_line(a: &Archive) -> String {
    let m = a.metadata().unwrap_or(serde_json::Value::Null);
    let field = |k: &str| m.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let name = field("name").unwrap_or_else(|| "a PMTiles archive".into());
    let version = field("version")
        .map(|v| format!(" {v}"))
        .unwrap_or_default();
    let osm = field("planetiler:osm:osmosisreplicationtime")
        .or_else(|| field("planetiler:osm:replicationtime"))
        .map(|t| format!(", OpenStreetMap data of {t}"))
        .unwrap_or_default();
    format!("{name}{version}{osm}")
}

fn notice(profile: u8, source: &str) -> String {
    format!(
        "Map data {ATTRIBUTION}, https://www.openstreetmap.org/copyright. \
         Available under the Open Database License (ODbL) 1.0, \
         https://opendatacommons.org/licenses/odbl/1-0/. This region file is a \
         derived database of it and is under the same licence. Low zooms may \
         carry Natural Earth data, which is in the public domain. Cut by \
         leviculum-map-tool under layer profile {profile} from {source}."
    )
}

fn print_zoom(label: &str, s: &ZoomStats) {
    println!(
        "MAPTOOL zoom={label} tiles={} entries={} empty={} shared={} bytes={} features_in={} features_out={} vertices_in={} vertices_out={} over_device={} clamped={} resimplified={}",
        s.tiles, s.entries, s.empty, s.shared, s.bytes, s.features_in, s.features_out,
        s.vertices_in, s.vertices_out, s.over_device, s.clamped, s.resimplified
    );
}

fn cmd_cut(
    inputs: &[PathBuf],
    output: &Path,
    bbox: &str,
    zooms: (u8, u8),
    profile: u8,
    threads: Option<usize>,
) -> Res<()> {
    let t = Instant::now();
    let archives = inputs
        .iter()
        .map(|p| Archive::open(p))
        .collect::<Res<Vec<_>>>()?;
    let opts = CutOptions {
        bounds: parse_bbox(bbox)?,
        zooms: zooms.0..=zooms.1,
        profile,
        attribution: ATTRIBUTION.to_owned(),
        notice: notice(profile, &source_line(&archives[0])),
        threads: threads
            .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
            .unwrap_or(1),
    };
    let file = BufWriter::new(File::create(output)?);
    let stats = cut(archives.as_slice(), &opts, file)?;
    let cut_s = t.elapsed().as_secs_f64();
    for (z, s) in &stats.zooms {
        print_zoom(&z.to_string(), s);
    }
    let region = Region::open(File::open(output)?).map_err(|e| format!("{e:?}"))?;
    region
        .verify()
        .map_err(|e| format!("written region does not verify: {e:?}"))?;
    let mut input_bytes = 0;
    for p in inputs {
        input_bytes += std::fs::metadata(p)?.len();
    }
    println!(
        "MAPTOOL cut inputs={} input_bytes={input_bytes} region_bytes={} entries={} threads={} seconds={cut_s:.2}",
        inputs.len(),
        stats.bytes,
        region.header().tiles,
        opts.threads
    );
    Ok(())
}

fn cmd_info(path: &Path) -> Res<()> {
    let region = Region::open(File::open(path)?).map_err(|e| format!("{e:?}"))?;
    region.verify().map_err(|e| format!("{e:?}"))?;
    let h = region.header();
    println!(
        "MAPTOOL info bytes={} zooms={}..={} profile={} tiles={} bounds={},{},{},{}",
        h.len,
        h.min_zoom,
        h.max_zoom,
        h.profile,
        h.tiles,
        h.bounds.west,
        h.bounds.south,
        h.bounds.east,
        h.bounds.north
    );
    println!("MAPTOOL attribution {}", region.attribution());
    println!("MAPTOOL notice {}", region.notice());
    let mut per = std::collections::BTreeMap::<u8, (u64, u64)>::new();
    let mut seen = std::collections::HashSet::new();
    for n in 0..h.tiles {
        let e = region.entry(n).map_err(|e| format!("{e:?}"))?;
        let z = per.entry(e.id.z).or_default();
        z.0 += 1;
        if seen.insert(e.offset) {
            z.1 += u64::from(e.len);
        }
    }
    for (z, (n, bytes)) in per {
        println!("MAPTOOL info zoom={z} entries={n} blob_bytes={bytes}");
    }
    Ok(())
}

fn cmd_render(path: &Path, out: &Path, at: LatLon, zoom: u8, w: u32, h: u32) -> Res<()> {
    let region = Region::open(File::open(path)?).map_err(|e| format!("{e:?}"))?;
    let view = Viewport::new(project(at), zoom, w, h);
    let mut read = Vec::new();
    region
        .read_view(&view, &mut read)
        .map_err(|e| format!("{e:?}"))?;
    let tiles = draw_tiles(&read).map_err(|e| format!("{e:?}"))?;
    let mut fb = vec![0u16; (w * h) as usize];
    let mut edges = vec![Edge::default(); 4096];
    let mut active = vec![0u16; 4096];
    let mut heads = vec![0u16; h as usize];
    let mut points = vec![Point::zero(); 4096];
    let mut ring_ends = vec![0u16; 1024];
    let mut frame = Frame::new(&mut fb, w, h).ok_or("frame")?;
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut work = Work {
        points: &mut points,
        ring_ends: &mut ring_ends,
    };
    let t = Instant::now();
    let st = draw(
        &tiles,
        Profile::Full,
        Ink::Colour,
        &mut frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    )?;
    let ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "MAPTOOL render tiles={} features={} culled={} vertices={} overflows={} fill_pixels={} stroke_pixels={} ms={ms:.3}",
        tiles.len(), st.features, st.culled, st.vertices, st.overflows, st.fill.pixels, st.stroke.pixels
    );
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    frame.rgb888_into(&mut ppm);
    std::fs::write(out, ppm)?;
    Ok(())
}

fn main() -> Res<()> {
    match Cli::parse().cmd {
        Cmd::Cut {
            inputs,
            output,
            bbox,
            min_zoom,
            max_zoom,
            profile,
            threads,
        } => cmd_cut(
            &inputs,
            &output,
            &bbox,
            (min_zoom, max_zoom),
            profile,
            threads,
        ),
        Cmd::Info { region } => cmd_info(&region),
        Cmd::Render {
            region,
            output,
            lat,
            lon,
            zoom,
            width,
            height,
        } => cmd_render(
            &region,
            &output,
            LatLon::from_degrees(lat, lon),
            zoom,
            width,
            height,
        ),
    }
}
