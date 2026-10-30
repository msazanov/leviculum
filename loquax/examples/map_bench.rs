//! Host draw time of the map per profile (1099): the PC profile (casings,
//! round joins, dashed paths) at 1280x800 and 1920x1080 and the M9 profile
//! at 320x240, Frankfurt at z15, through the painter every frontend draws
//! with. A measuring tool for the reviewer
//! and the commit messages, not a gate: the gates count edges, spans and
//! pixels (`leviculum-map/tests/golden.rs`), never wall clocks.
//!
//! ```sh
//! cargo run --release -p loquax --features picture --example map_bench -- DIR
//! ```
//!
//! `DIR` holds the region (`frankfurt.lmr`, cut with
//! `--bbox 8.47,50.01,8.80,50.23 --min-zoom 0 --max-zoom 15`) and nothing
//! else. One thread, the median of 21 draws after one warm-up. With a
//! second argument, a directory, each view is also written there as a PPM,
//! in the light table and in the dark one.

use std::path::PathBuf;
use std::time::Instant;

use leviculum_map::draw::Ink;
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::style::{MapTheme, Style};
use loquax::picture::{Maps, Painter};

fn main() {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: map_bench DIR");
        std::process::exit(2);
    };
    let out = std::env::args_os().nth(2).map(PathBuf::from);
    let mut painter = Painter::new(Maps::new(Some(dir)));
    let centre = project(LatLon::from_degrees(50.1109, 8.6821));
    println!("| view | profile | size | draw ms |");
    println!("|---|---|---|---|");
    let rows = [
        (1280, 800, "pc", Style::PC),
        (1920, 1080, "pc", Style::PC),
        (320, 240, "m9", Style::M9),
    ];
    for (w, h, profile, style) in rows {
        let view = Viewport::new(centre, 15, w, h);
        let mut draw = || {
            let drawn = painter.draw(&view, Ink::Colour, style, 100);
            std::hint::black_box(drawn.fb.len());
        };
        draw();
        let mut ms: Vec<f64> = (0..21)
            .map(|_| {
                let t = Instant::now();
                draw();
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        println!(
            "| Frankfurt z15 | {profile} | {w}x{h} | {:.2} |",
            ms[ms.len() / 2]
        );
        let Some(out) = &out else {
            continue;
        };
        for (theme, name) in [(MapTheme::Light, "light"), (MapTheme::Dark, "dark")] {
            let mut fb = painter
                .draw(&view, Ink::Colour, style.in_theme(theme), 100)
                .fb;
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            if let Some(frame) = Frame::new(&mut fb, w, h) {
                frame.rgb888_into(&mut ppm);
            }
            let path = out.join(format!("frankfurt-z15-{profile}-{w}x{h}-{name}.ppm"));
            if let Err(e) = std::fs::write(&path, ppm) {
                eprintln!("{}: {e}", path.display());
            }
        }
    }
}
