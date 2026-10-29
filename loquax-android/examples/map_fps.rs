//! Host frame rate of the phone's map frame (1131): a portrait 1080 x 2400
//! screen at a density of 2.75, Frankfurt at z15, drawn through the
//! facade's own call ([`loquax_android::map::render`]) with no margin, a
//! quarter screen and half a screen on each side, while a finger moves (no
//! names) and at rest (names set). Each frame is panned 40 pixels from the
//! last, as a finger drags. A measuring tool for the reviewer and the
//! commit message, not a gate.
//!
//! ```sh
//! cargo run --release -p loquax-android --example map_fps -- HOME
//! ```
//!
//! `HOME` holds `maps/` with the region, Frankfurt with its names (LMR2,
//! cut as `loquax/examples/map_bench.rs` says), and a `config` with
//! `map_autoload = off`. One thread, the median of 21 frames after one
//! warm-up.

use std::path::PathBuf;
use std::time::Instant;

use leviculum_map::mercator::{project, LatLon};
use loquax::when::WallTime;
use loquax_android::schema::{self, MapRequest};

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
    println!("| margin | frame | moving | frame ms | fps |");
    println!("|---|---|---|---|---|");
    for (name, mx, my) in [
        ("none", 0, 0),
        ("quarter", w / 4, h / 4),
        ("half", w / 2, h / 2),
    ] {
        for moving in [true, false] {
            let mut ask = MapRequest {
                dir: home.display().to_string(),
                w,
                h,
                density_milli: 2750,
                x: Some(at.x),
                y: Some(at.y),
                zoom: Some(15),
                margin_x: mx,
                margin_y: my,
                moving,
                ..MapRequest::default()
            };
            let mut frame = |pan: i32| {
                ask.pan_x = pan;
                let bytes = loquax_android::map::render(&ask, Some(Vec::new()), now);
                let Ok((shown, rgba)) = schema::unframe(&bytes) else {
                    eprintln!("no frame");
                    std::process::exit(1);
                };
                ask.x = Some(shown.x);
                ask.y = Some(shown.y);
                std::hint::black_box(rgba.len());
                (shown.w, shown.h)
            };
            frame(0);
            let mut size = (0, 0);
            let mut ms: Vec<f64> = (0..21)
                .map(|i| {
                    let t = Instant::now();
                    size = frame(if i % 2 == 0 { 40 } else { -40 });
                    t.elapsed().as_secs_f64() * 1e3
                })
                .collect();
            ms.sort_by(f64::total_cmp);
            let median = ms[ms.len() / 2];
            println!(
                "| {name} | {}x{} | {moving} | {median:.1} | {:.1} |",
                size.0,
                size.1,
                1e3 / median
            );
        }
    }
}
