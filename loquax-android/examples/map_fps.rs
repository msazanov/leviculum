//! Host frame rate of the phone's map frame (1131, 1134): a portrait
//! 1080 x 2400 screen at a density of 2.75, Frankfurt at z15, drawn
//! through the facade's own call ([`loquax_android::map::render`]) with a
//! quarter screen of margin on each side, as the app asks.
//!
//! Rows: a pan with the tile cache warm (each frame 40 pixels from the
//! last, the finger down), the frame at rest where that pan started (the
//! names placed), a pan that uncovers a new tile column every frame (256
//! pixels a frame), a cold frame at rest (the cache dropped, every tile
//! drawn and the names placed), a cold frame while the finger is down (at
//! most two tiles a worker drawn, the rest owed), and a pinch from z14.3
//! to z15.7 in steps of 0.07 with the finger down, then at rest. Each with
//! every core the cache takes and with one. The cache's memory after the
//! run is printed last.
//!
//! ```sh
//! cargo run --release -p loquax-android --example map_fps -- HOME
//! ```
//!
//! `HOME` holds `maps/` with the region, Frankfurt with its names (LMR2,
//! cut as `loquax/examples/map_bench.rs` says), and a `config` with
//! `map_autoload = off`. The median of the frames after one warm-up. A
//! measuring tool for the reviewer and the commit message, not a gate.

use std::path::PathBuf;
use std::time::Instant;

use leviculum_map::mercator::{project, LatLon};
use loquax::when::WallTime;
use loquax_android::schema::{self, MapRequest, MapShown};

fn main() {
    let Some(home) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: map_fps HOME");
        std::process::exit(2);
    };
    let (w, h) = (1080u32, 2400u32);
    let at = project(LatLon::from_degrees(50.1109, 8.6821));
    let now = WallTime {
        unix_secs: 1_790_000_000,
        offset_secs: 0,
    };
    let base = MapRequest {
        dir: home.display().to_string(),
        w,
        h,
        density_milli: 2750,
        x: Some(at.x),
        y: Some(at.y),
        zoom_milli: Some(15_000),
        margin_x: w / 4,
        margin_y: h / 4,
        ..MapRequest::default()
    };
    let draw = |ask: &MapRequest| -> MapShown {
        let bytes = loquax_android::map::render(ask, Some(Vec::new()), now);
        let Ok((shown, rgba)) = schema::unframe(&bytes) else {
            eprintln!("no frame");
            std::process::exit(1);
        };
        std::hint::black_box(rgba.len());
        shown
    };
    let timed = |ask: &MapRequest| -> (f64, MapShown) {
        let t = Instant::now();
        let shown = draw(ask);
        (t.elapsed().as_secs_f64() * 1e3, shown)
    };
    let median = |mut ms: Vec<f64>| {
        ms.sort_by(f64::total_cmp);
        ms[ms.len() / 2]
    };
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    println!("| frame | workers | frame ms | fps | tiles drawn a frame | owed |");
    println!("|---|---|---|---|---|---|");
    for threads in [workers, 1] {
        let _ = draw(&base);
        loquax_android::map::tune_tiles(&home, Some(threads), false);
        let stats = || loquax_android::map::tile_stats(&home).unwrap_or_default();
        let row = |name: &str, ms: f64, drawn: f64, owed: u32| {
            println!(
                "| {name} | {threads} | {ms:.1} | {:.0} | {drawn:.1} | {owed} |",
                1e3 / ms
            );
        };
        // A warm pan: back and forth over cached tiles.
        let mut ask = MapRequest {
            moving: true,
            ..base.clone()
        };
        let _ = draw(&ask);
        let before = stats().rendered;
        let mut ms = Vec::new();
        for i in 0..21 {
            ask.pan_x = if i % 2 == 0 { 40 } else { -40 };
            let (t, shown) = timed(&ask);
            (ask.x, ask.y) = (Some(shown.x), Some(shown.y));
            ms.push(t);
        }
        let drawn = (stats().rendered - before) as f64 / 21.0;
        row("pan, cache warm", median(ms), drawn, 0);
        // At rest where the pan started, the tiles cached: the names
        // placed.
        let rest = MapRequest {
            moving: false,
            pan_x: 0,
            ..ask.clone()
        };
        let ms: Vec<f64> = (0..5).map(|_| timed(&rest).0).collect();
        row("at rest, cache warm, names placed", median(ms), 0.0, 0);
        // A pan that uncovers a tile column a frame.
        let before = stats().rendered;
        let (mut ms, mut owed) = (Vec::new(), 0);
        ask.pan_x = 256;
        for _ in 0..21 {
            let (t, shown) = timed(&ask);
            (ask.x, ask.y) = (Some(shown.x), Some(shown.y));
            owed = owed.max(u32::from(shown.pending));
            ms.push(t);
        }
        let drawn = (stats().rendered - before) as f64 / 21.0;
        row("pan, a new tile column a frame", median(ms), drawn, owed);
        // Cold: the cache dropped before each frame, at rest and moving.
        for moving in [false, true] {
            let cold = MapRequest {
                moving,
                ..base.clone()
            };
            let before = stats().rendered;
            let (mut ms, mut owed) = (Vec::new(), 0);
            for _ in 0..7 {
                loquax_android::map::tune_tiles(&home, None, true);
                let (t, shown) = timed(&cold);
                owed = owed.max(u32::from(shown.pending));
                ms.push(t);
            }
            let drawn = (stats().rendered - before) as f64 / 7.0;
            let name = match moving {
                false => "cold, at rest, names placed",
                true => "cold, finger down",
            };
            row(name, median(ms), drawn, owed);
        }
        // A pinch from z14.3 to z15.7, the finger down, then at rest.
        let start = MapRequest {
            zoom_milli: Some(14_300),
            ..base.clone()
        };
        let _ = draw(&start);
        let before = stats().rendered;
        let mut ms = Vec::new();
        let mut owed = 0;
        let mut pinch = MapRequest {
            moving: true,
            ..start.clone()
        };
        for z in (14_370..=15_700).step_by(70) {
            pinch.zoom_milli = Some(z);
            let (t, shown) = timed(&pinch);
            owed += u32::from(shown.pending);
            ms.push(t);
        }
        let n = ms.len() as f64;
        let drawn = (stats().rendered - before) as f64 / n;
        let mean = ms.iter().sum::<f64>() / n;
        row("pinch z14.3 to z15.7 (median)", median(ms), drawn, owed);
        println!(
            "| pinch, mean | {threads} | {mean:.1} | {:.0} | | |",
            1e3 / mean
        );
        let rest = MapRequest {
            moving: false,
            ..pinch
        };
        let (t, shown) = timed(&rest);
        row(
            &format!(
                "at rest after the pinch, z{:.2}",
                f64::from(shown.zoom_milli) / 1e3
            ),
            t,
            0.0,
            u32::from(shown.pending),
        );
    }
    if let Some((tiles, bytes)) = loquax_android::map::tile_memory(&home) {
        println!(
            "\ncache: {tiles} tiles, {:.1} MB of the budget's {} MB",
            bytes as f64 / f64::from(1u32 << 20),
            loquax::tiles::BUDGET_MB
        );
    }
}
