//! The map screen's call through the facade (1128): the frame Java gets
//! is loquax's own map on a pixel screen, the same pixels the window's
//! renderer draws for the view, the pins drawn in, the dark table when the
//! phone is dark or the map switch says so (1133), and a position in the store is a pin. A test binary of
//! its own: the map takes a running session's pins over the store's, and
//! the facade tests run sessions in their process.

use std::path::{Path, PathBuf};

use leviculum_core::Identity;
use leviculum_lxmf::telemetry::Location;
use leviculum_map::draw::Ink;
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::frame::rgb888;
use leviculum_mapfill::style::{MapTheme, Style};
use leviculum_ui::theme::ThemeFlag;
use loquax::mapnames::Typesetter;
use loquax::picture::{Maps, Painter};
use loquax::store::Store;
use loquax::telemetry::Fix;
use loquax::ui::history::StoreWrite;
use loquax::ui::map::MapPin;
use loquax::when::WallTime;
use loquax_android::schema::{self, MapRequest, MapShown};

/// The Florence view of the window's golden of the names: z16, 480 x 320.
const LAT: f64 = 43.7752;
const LON: f64 = 11.2660;
const W: u32 = 480;
const H: u32 = 320;
const NOW: i64 = 1_790_000_000;

/// A home with the committed Florence region in its maps, the autoload
/// off: no test reaches for the internet.
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("home");
    let maps = home.path().join("maps");
    std::fs::create_dir_all(&maps).expect("maps");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures/firenze.lmr");
    std::fs::copy(fixture, maps.join("firenze.lmr")).expect("the region");
    std::fs::write(
        home.path().join(loquax::config::CONFIG_FILE),
        "map_autoload = off\n",
    )
    .expect("config");
    home
}

/// The request for the Florence view.
fn request(home: &Path, dark: bool) -> MapRequest {
    let at = project(LatLon::from_degrees(LAT, LON));
    MapRequest {
        dir: home.display().to_string(),
        w: W,
        h: H,
        density_milli: 1000,
        dark,
        x: Some(at.x),
        y: Some(at.y),
        zoom: Some(16),
        ..MapRequest::default()
    }
}

fn frame(bytes: &[u8]) -> (MapShown, Vec<u8>) {
    let (shown, rgba) = schema::unframe(bytes).expect("a frame");
    assert!(shown.ok, "{:?}", shown.error);
    assert_eq!(rgba.len(), (shown.w * shown.h * 4) as usize);
    (shown, rgba.to_vec())
}

fn pin(peer: u8, label: &str, lat: f64, lon: f64) -> MapPin {
    MapPin {
        peer: [peer; 16],
        at: project(LatLon::from_degrees(lat, lon)),
        label: label.to_string(),
        age: Some("5m".to_string()),
        stale: false,
    }
}

fn now() -> WallTime {
    WallTime {
        unix_secs: NOW,
        offset_secs: 0,
    }
}

/// The view's picture as loquax's painter draws it, no names, no pins,
/// as RGBA.
fn picture(home: &Path, theme: MapTheme) -> Vec<u8> {
    let mut painter = Painter::new(Maps::new(Some(home.join("maps"))));
    let view = Viewport::new(project(LatLon::from_degrees(LAT, LON)), 16, W, H);
    let drawn = painter.draw(&view, Ink::Colour, Style::PC.in_theme(theme), 100);
    drawn
        .fb
        .iter()
        .flat_map(|&px| {
            use embedded_graphics::pixelcolor::raw::RawU16;
            use embedded_graphics::pixelcolor::Rgb565;
            let [r, g, b] = rgb888(Rgb565::from(RawU16::new(u16::from_be(px))));
            [r, g, b, 0xff]
        })
        .collect()
}

/// How many of the pixels of `a` and `b` are the same, per cent.
fn same_share(a: &[u8], b: &[u8]) -> usize {
    let same = a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x == y).count();
    same * 100 / (a.len() / 4)
}

/// The frame through the facade with one pin: the window's renderer's
/// pixels for the view, its names and its pin, exactly
/// (`loquax::screenmap::render` called here as the window's code calls
/// it); the map under them `loquax::picture`'s; the pin's marker at its
/// place; pixel for pixel the golden.
#[test]
fn the_frame_is_the_shared_renderers_with_names_and_a_pin() {
    let home = home();
    let pins = vec![pin(1, "alice", 43.7752, 11.2640)];
    let bytes =
        loquax_android::map::render(&request(home.path(), false), Some(pins.clone()), now());
    let (shown, rgba) = frame(&bytes);
    assert_eq!((shown.w, shown.h, shown.zoom), (W, H, 16));
    assert_eq!(shown.pins, 1);
    assert_eq!(shown.attribution, "© OpenStreetMap contributors");

    let mut painter = Painter::new(Maps::new(Some(home.path().join("maps"))));
    let mut type_ = Typesetter::new();
    let view = Viewport::new(project(LatLon::from_degrees(LAT, LON)), 16, W, H);
    let direct = loquax::screenmap::render(
        &mut painter,
        &mut type_,
        &view,
        Style::PC,
        1.0,
        &pins,
        None,
        true,
    );
    assert_eq!(rgba, direct.picture.rgba, "the shared renderer's pixels");
    assert!(
        direct.picture.names.iter().any(|n| n.starts_with("Via ")),
        "{:?}",
        direct.picture.names
    );

    let plain = picture(home.path(), MapTheme::Light);
    let share = same_share(&rgba, &plain);
    assert!(
        (90..100).contains(&share),
        "the map under the names and the pin is the picture's: {share} %"
    );

    let placed = &direct.placed[0];
    let (x, y) = (placed.x as usize, placed.y as usize);
    let at = (y * W as usize + x) * 4;
    assert_eq!(&rgba[at..at + 3], &[0x20, 0x50, 0xc0], "the marker's blue");

    same_as("map-florence-z16.png", &rgba, W, H);
}

/// The phone in its dark setting draws the map's dark table: the frame
/// is the dark picture under its names, and not the light one.
#[test]
fn a_dark_phone_draws_the_dark_table() {
    let home = home();
    let (_, light) = frame(&loquax_android::map::render(
        &request(home.path(), false),
        Some(Vec::new()),
        now(),
    ));
    let (_, dark) = frame(&loquax_android::map::render(
        &request(home.path(), true),
        Some(Vec::new()),
        now(),
    ));
    let dark_picture = picture(home.path(), MapTheme::Dark);
    let share = same_share(&dark, &dark_picture);
    assert!(share >= 90, "dark under its names: {share} %");
    let share = same_share(&dark, &light);
    assert!(share < 50, "not the light table: {share} %");
}

/// The map's own switch (1133): with the phone dark, a toggle draws the
/// light table and stores it as the config's `map_theme`, the other keys
/// kept; the choice wins over the phone from then on, for a renderer that
/// reads the config afresh too; "follow the phone" stores `auto` and the
/// phone decides again.
#[test]
fn the_map_switch_wins_over_the_phone_until_it_follows_again() {
    let home = home();
    let draw = |request: &MapRequest| {
        frame(&loquax_android::map::render(
            request,
            Some(Vec::new()),
            now(),
        ))
    };
    let dark_picture = picture(home.path(), MapTheme::Dark);
    let light_picture = picture(home.path(), MapTheme::Light);

    let (shown, rgba) = draw(&request(home.path(), true));
    assert!(shown.dark && !shown.theme_chosen, "{shown:?}");
    assert!(same_share(&rgba, &dark_picture) >= 90);

    let toggle = MapRequest {
        toggle_theme: true,
        ..request(home.path(), true)
    };
    let (shown, rgba) = draw(&toggle);
    assert!(!shown.dark && shown.theme_chosen, "{shown:?}");
    assert_eq!(shown.notice, None);
    assert!(same_share(&rgba, &light_picture) >= 90, "light at once");
    let config = loquax::config::load(home.path()).expect("the config");
    assert_eq!(config.map_theme, Some(ThemeFlag::Light));
    assert_eq!(config.map_autoload, Some(false), "the other keys kept");

    for phone_dark in [true, false] {
        let (shown, rgba) = draw(&request(home.path(), phone_dark));
        assert!(!shown.dark && shown.theme_chosen, "phone dark {phone_dark}");
        assert!(same_share(&rgba, &light_picture) >= 90);
    }

    let (shown, _) = draw(&MapRequest {
        toggle_theme: true,
        ..request(home.path(), false)
    });
    assert!(shown.dark && shown.theme_chosen, "toggled back to dark");
    assert_eq!(
        loquax::config::load(home.path())
            .expect("the config")
            .map_theme,
        Some(ThemeFlag::Dark)
    );

    let (shown, rgba) = draw(&MapRequest {
        follow_phone: true,
        ..request(home.path(), false)
    });
    assert!(
        !shown.dark && !shown.theme_chosen,
        "follows the light phone"
    );
    assert!(same_share(&rgba, &light_picture) >= 90);
    assert_eq!(
        loquax::config::load(home.path())
            .expect("the config")
            .map_theme,
        Some(ThemeFlag::Auto)
    );
    let (shown, _) = draw(&request(home.path(), true));
    assert!(shown.dark && !shown.theme_chosen, "and the dark one");
}

/// A choice stored before the app draws its first map wins over the
/// phone: phone dark, `map_theme = light`, the light table.
#[test]
fn a_stored_map_theme_wins_over_the_phone() {
    let home = home();
    loquax::config::save(home.path(), "map_theme", "light").expect("stored");
    let (shown, rgba) = frame(&loquax_android::map::render(
        &request(home.path(), true),
        Some(Vec::new()),
        now(),
    ));
    assert!(!shown.dark && shown.theme_chosen, "{shown:?}");
    assert!(same_share(&rgba, &picture(home.path(), MapTheme::Light)) >= 90);
}

/// A tap on a pin's marker selects it and names it; the selected pin is
/// drawn red. A pan moves the centre by the pixels at the zoom.
#[test]
fn a_tap_names_the_pin_and_a_pan_moves_the_view() {
    let home = home();
    let pins = vec![pin(7, "bob", 43.7752, 11.2640)];
    let first = request(home.path(), false);
    let (shown, _) = frame(&loquax_android::map::render(
        &first,
        Some(pins.clone()),
        now(),
    ));
    assert_eq!(shown.tapped, None);
    let mut view = Viewport::new(project(LatLon::from_degrees(LAT, LON)), 16, W, H);
    let marker = leviculum_map::pins::place(
        &view,
        &[leviculum_map::pins::Pin {
            at: pins[0].at,
            label: "bob",
            stale: false,
        }],
    )[0];
    let tap = MapRequest {
        tap_x: Some(marker.x as i32),
        tap_y: Some(marker.y as i32),
        ..first.clone()
    };
    let (shown, rgba) = frame(&loquax_android::map::render(
        &tap,
        Some(pins.clone()),
        now(),
    ));
    assert_eq!(shown.tapped.as_deref(), Some("07".repeat(16).as_str()));
    assert_eq!(shown.tapped_name.as_deref(), Some("bob · 5m"));
    assert_eq!(shown.selected, shown.tapped);
    let at = (marker.y as usize * W as usize + marker.x as usize) * 4;
    assert_eq!(&rgba[at..at + 3], &[0xd0, 0x30, 0x30], "the selected red");

    let pan = MapRequest {
        pan_x: 40,
        pan_y: -20,
        ..first
    };
    let (shown, _) = frame(&loquax_android::map::render(&pan, Some(pins), now()));
    view.pan(40, -20);
    assert_eq!((shown.x, shown.y), (view.centre.x, view.centre.y));
}

/// A position in the store is a pin on the map, with no session running:
/// the first frame fits it and selects it, named by the display rule.
#[test]
fn a_position_in_the_store_makes_a_pin() {
    let home = home();
    let identity = Identity::generate(&mut rand_core::OsRng);
    let key: [u8; 64] = identity.public_key_bytes();
    loquax::home::client::save_client(home.path(), &key).expect("client");
    let address = loquax::home::lxmf_address(&key).expect("address");
    let peer = [0x42; 16];
    {
        let mut store = Store::open_in(home.path(), address).expect("store");
        let fix = Fix {
            peer,
            via: peer,
            message: None,
            received_at: Some(NOW - 60),
            location: Location {
                latitude_e6: 43_775_200,
                longitude_e6: 11_264_000,
                altitude_e2: 0,
                speed_e2: 0,
                bearing_e2: 0,
                accuracy_e2: 500,
                last_update: NOW - 90,
            },
            telemetry: Vec::new(),
        };
        store
            .write(&[StoreWrite::Position(fix)])
            .expect("the position");
    }
    let first = MapRequest {
        x: None,
        y: None,
        zoom: None,
        ..request(home.path(), false)
    };
    let bytes = loquax_android::render_map(&schema::encode(&first));
    let (shown, _) = frame(&bytes);
    assert_eq!(shown.pins, 1, "{shown:?}");
    assert_eq!(shown.selected.as_deref(), Some("42".repeat(16).as_str()));
    assert!(shown.zoom > 11, "one pin fits deep: {}", shown.zoom);
}

/// How long a phone's whole screen takes on this host, for the report:
/// 1080 x 2400 at 2.75, the Florence view, one pin.
#[test]
fn a_phone_screen_frame_takes() {
    let home = home();
    let at = project(LatLon::from_degrees(LAT, LON));
    let phone = MapRequest {
        w: 1080,
        h: 2400,
        density_milli: 2750,
        x: Some(at.x),
        y: Some(at.y),
        zoom: Some(17),
        ..request(home.path(), false)
    };
    let pins = vec![pin(1, "alice", 43.7752, 11.2640)];
    for round in 0..3 {
        let started = std::time::Instant::now();
        let (shown, _) = frame(&loquax_android::map::render(
            &phone,
            Some(pins.clone()),
            now(),
        ));
        eprintln!(
            "MAPFRAME 1080x2400 round={round} ms={} total_ms={}",
            shown.ms,
            started.elapsed().as_millis()
        );
    }
}

/// Hold `rgba` to the PNG golden `name` under `tests/golden`, pixel for
/// pixel; a picture that moved is left in the temp dir.
fn same_as(name: &str, rgba: &[u8], w: u32, h: u32) {
    let rgb: Vec<u8> = rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let encode = || {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut writer = e.write_header().expect("header");
        writer.write_image_data(&rgb).expect("data");
        writer.finish().expect("finish");
        out
    };
    let keep = || {
        let dir = std::env::temp_dir().join("loquax-android-golden");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let kept = dir.join(name);
        std::fs::write(&kept, encode()).expect("write");
        kept
    };
    let Ok(bytes) = std::fs::read(&path) else {
        panic!("no golden {name}; this run's is {}", keep().display());
    };
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .expect("png");
    let mut want = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut want).expect("frame");
    want.truncate(info.buffer_size());
    if (info.width, info.height) != (w, h) || want != rgb {
        let differ = want
            .chunks(3)
            .zip(rgb.chunks(3))
            .filter(|(a, b)| a != b)
            .count();
        panic!(
            "{name} moved: {differ} pixels differ; this run's is {}",
            keep().display()
        );
    }
}

/// The smallest shift by which every pixel column of `rgba`, `w` wide,
/// repeats: the copies of the world side by side.
fn period(rgba: &[u8], w: usize) -> Option<usize> {
    let h = rgba.len() / 4 / w;
    let px = |x: usize, y: usize| &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
    (1..w).find(|&p| (0..w - p).all(|x| (0..h).all(|y| px(x, y) == px(x + p, y))))
}

/// The phone shows one world at most (Lew 2026-10-29, 1131): a portrait
/// screen asked for z0, where the world was drawn five times across a
/// thin band, gets z3, the world 2048 pixels wide over its 1080, no
/// column repeated; zooming out stops there; the first view with no pin
/// and no region is that world, centred.
#[test]
fn the_phone_never_shows_the_world_twice() {
    let home = tempfile::tempdir().expect("home");
    std::fs::write(
        home.path().join(loquax::config::CONFIG_FILE),
        "map_autoload = off\n",
    )
    .expect("config");
    let ask = MapRequest {
        dir: home.path().display().to_string(),
        w: 1080,
        h: 2400,
        density_milli: 1000,
        x: Some(1 << 31),
        y: Some(1 << 31),
        zoom: Some(0),
        moving: true,
        ..MapRequest::default()
    };
    let (shown, rgba) = frame(&loquax_android::map::render(&ask, Some(Vec::new()), now()));
    assert_eq!((shown.zoom, shown.min_zoom), (3, 3));
    assert_eq!(period(&rgba, 1080), None);
    let out = MapRequest {
        zoom_by: -2,
        about_x: 540,
        about_y: 1200,
        ..ask.clone()
    };
    let (shown, _) = frame(&loquax_android::map::render(&out, Some(Vec::new()), now()));
    assert_eq!(shown.zoom, 3, "zooming out stops at one world");
    let first = MapRequest {
        x: None,
        y: None,
        zoom: None,
        ..ask
    };
    let (shown, _) = frame(&loquax_android::map::render(
        &first,
        Some(Vec::new()),
        now(),
    ));
    assert_eq!((shown.zoom, shown.y), (3, 1 << 31));
}

/// The first view with no pin is the newest region, the Florence fixture
/// here, fitted whole, and not the world.
#[test]
fn the_first_view_without_pins_is_the_installed_region() {
    let home = home();
    let first = MapRequest {
        x: None,
        y: None,
        zoom: None,
        ..request(home.path(), false)
    };
    let (shown, _) = frame(&loquax_android::map::render(
        &first,
        Some(Vec::new()),
        now(),
    ));
    assert!(shown.zoom >= 14, "z{}", shown.zoom);
    let at = leviculum_map::mercator::unproject(leviculum_map::World {
        x: shown.x,
        y: shown.y,
    });
    assert!((at.lat_e7 - 437_756_000).abs() < 100_000, "{at:?}");
}

/// A frame with margins covers them (1131): the Florence view drawn with
/// half a screen past each edge is twice the screen each way, and each
/// margin holds exactly the pixels a frame panned half a screen that way
/// shows, so a finger that pans reveals drawn map and no dark strip. The
/// margins are echoed for Java to place the frame by, and a tap is still
/// in the screen's pixels.
#[test]
fn a_frame_with_margins_covers_them() {
    let home = home();
    let (mx, my) = (W / 2, H / 2);
    let wide = MapRequest {
        margin_x: mx,
        margin_y: my,
        moving: true,
        ..request(home.path(), false)
    };
    let (shown, big) = frame(&loquax_android::map::render(&wide, Some(Vec::new()), now()));
    assert_eq!((shown.w, shown.h), (2 * W, 2 * H));
    assert_eq!((shown.margin_x, shown.margin_y), (mx, my));
    assert!(big.chunks(4).all(|px| px[3] == 0xff), "every pixel set");
    let bw = (2 * W) as usize;
    for (dx, dy) in [
        (-(mx as i32), 0),
        (mx as i32, 0),
        (0, -(my as i32)),
        (0, my as i32),
        (mx as i32, my as i32),
    ] {
        let panned = MapRequest {
            pan_x: dx,
            pan_y: dy,
            moving: true,
            ..request(home.path(), false)
        };
        let (_, small) = frame(&loquax_android::map::render(
            &panned,
            Some(Vec::new()),
            now(),
        ));
        // The panned screen is the big frame's window at the margin plus
        // the pan.
        let (x0, y0) = ((mx as i32 + dx) as usize, (my as i32 + dy) as usize);
        let window: Vec<u8> = (0..H as usize)
            .flat_map(|y| {
                let at = ((y0 + y) * bw + x0) * 4;
                big[at..at + W as usize * 4].to_vec()
            })
            .collect();
        assert_eq!(same_share(&window, &small), 100, "pan {dx},{dy}");
    }
    // A tap on a pin's marker, in the screen's pixels.
    let pins = vec![pin(7, "bob", 43.7752, 11.2640)];
    let view = Viewport::new(project(LatLon::from_degrees(LAT, LON)), 16, W, H);
    let marker = leviculum_map::pins::place(
        &view,
        &[leviculum_map::pins::Pin {
            at: pins[0].at,
            label: "bob",
            stale: false,
        }],
    )[0];
    let _ = loquax_android::map::render(&wide, Some(pins.clone()), now());
    let tap = MapRequest {
        tap_x: Some(marker.x as i32),
        tap_y: Some(marker.y as i32),
        ..wide
    };
    let (shown, _) = frame(&loquax_android::map::render(&tap, Some(pins), now()));
    assert_eq!(shown.tapped.as_deref(), Some("07".repeat(16).as_str()));
}

/// A text that came with a position (Sideband's telemetry beside a body,
/// Lew's device test of 1132): its bubble carries the text and the
/// position line's words, and the map request a tap on it makes, the fix
/// and the sender's pin, opens the map centred there at the region's
/// street zoom with that pin selected. The same text without the field is
/// a plain bubble with nothing to open.
#[test]
fn a_tap_on_a_text_with_a_position_opens_the_map_at_it() {
    use leviculum_lxmf::telemetry::Telemetry;
    use loquax::outbox::OutboxEvent;
    use loquax::ui::{update, UiEvent, UiModel};

    let home = home();
    let peer = [0x42; 16];
    let fix = Location {
        latitude_e6: 43_775_200,
        longitude_e6: 11_264_000,
        altitude_e2: 0,
        speed_e2: 0,
        bearing_e2: 0,
        accuracy_e2: 500,
        last_update: NOW - 90,
    };
    let telemetry = Telemetry {
        time: Some(NOW - 90),
        location: Some(fix),
        ..Telemetry::default()
    };
    let mut model = UiModel::<loquax_android::session::Line>::new(&[]);
    update(&mut model, UiEvent::Clock(now()));
    for (id, fields) in [
        (
            1u8,
            vec![(
                leviculum_lxmf::constants::FIELD_TELEMETRY,
                telemetry.encode_field_value(),
            )],
        ),
        (2, Vec::new()),
    ] {
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::Received {
                message_id: [id; 32],
                source: peer,
                title: Vec::new(),
                body: b"Brandenburger Tor, Testposition".to_vec(),
                fields,
                verified: true,
            }),
        );
    }
    let rows = loquax_android::session::rows(&model, 0);
    assert_eq!(rows.len(), 2, "{rows:?}");
    let with = &rows[0];
    assert_eq!(with.body, "Brandenburger Tor, Testposition");
    assert_eq!(
        (with.lat_e6, with.lon_e6),
        (Some(43_775_200), Some(11_264_000))
    );
    assert_eq!(with.place.as_deref(), Some("43.77520 N 11.26400 E"));
    assert_eq!(with.accuracy.as_deref(), Some("±5 m"));
    assert_eq!(with.fix_at, Some(NOW - 90));
    assert_eq!(
        with.stale_at,
        Some(NOW - 90 + loquax::telemetry::STALE_AFTER_SECS)
    );
    let without = &rows[1];
    assert_eq!(without.body, "Brandenburger Tor, Testposition");
    assert_eq!((without.lat_e6, without.place.as_ref()), (None, None));

    // The tap: what ChatActivity hands MapActivity, as MapView asks.
    let tap = MapRequest {
        x: None,
        y: None,
        zoom: None,
        at_lat_e6: with.lat_e6,
        at_lon_e6: with.lon_e6,
        selected: Some(loquax::address::to_hex(&peer)),
        ..request(home.path(), false)
    };
    let (shown, _) = frame(&loquax_android::map::render(
        &tap,
        Some(model.map_pins()),
        now(),
    ));
    let at = project(LatLon::from_e6(43_775_200, 11_264_000));
    assert_eq!((shown.x, shown.y), (at.x, at.y), "centred on the fix");
    // The Florence fixture's deepest zoom, where its streets are named.
    assert!(shown.zoom >= 15, "a street zoom: {}", shown.zoom);
    assert_eq!(
        shown.selected.as_deref(),
        Some("42".repeat(16).as_str()),
        "the sender's pin selected"
    );
    assert_eq!(shown.pins, 1, "{shown:?}");
}
