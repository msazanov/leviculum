# The map's first measurement, host side (#474)

Draft, 2026-10-25. The first step that
`docs/src/concepts/handheld-messenger.md` ("The map") asks for before
any map code: can one 240x320 view of OpenStreetMap vector data be
decoded and drawn within the ESP32-S3R8's budget (512 KB SRAM, 8 MB
PSRAM, no GPU) with existing libraries and no renderer of our own.
This is the host half. Every number below was measured on the host,
except the ESP32-S3 times, which are an estimate scaled by CoreMark and
are labelled as such. No workspace crate changed and nothing was added
to any shipping crate; the throwaway crates live under
`/tmp/map-measure/` and their sources are in the appendix.

## Conclusion

Memory fits with room. Time is borderline, and the drawing side has a gap.
Decoding unmodified Protomaps tiles one at a time, clipping them to a
240x320 view and drawing them with `embedded-graphics` peaks at about
2.0 MiB at z15 (heap, the largest raw tile and an Rgb565 framebuffer
together), 1.2 MiB at z13 and 1.5 MiB for half a z10 region view: a
quarter of the 8 MB PSRAM at worst. The time is borderline. Scaled by
CoreMark, a full redraw is about 0.53 s at z15 and 0.44 s at z13
before any PSRAM penalty, which the scaling cannot see and which only
makes it slower. That is acceptable for a redraw when a position
arrives and too slow for panning. The simplification that fixes it is
the one the concept page already sketches: a desktop tool cuts the
tile set. Dropping `buildings` and `pois` alone takes the z15 view
from 461 KB to 61 KB of gzip, from 2.0 MiB to 0.36 MiB and to about
73 ms estimated. Region zooms (z10) need `landuse` and `natural`
thinned, since they are 80 % of the drawn segments there. Two library
findings shape the next step. First, no MVT or PMTiles crate is
`no_std`, but the prost-generated MVT message types are
`core`+`alloc` clean and the geometry walk is about forty lines, so
the decoder is prost plus the `.proto`, not a renderer of our own.
`mvt-reader` is disqualified: it allocates every ring with the whole
feature's capacity and peaked at 250 MB on one z13 tile. Second,
`embedded-graphics` 0.8 cannot fill an arbitrary polygon. Outlines
work but show tile seams, as the z10 view does. Filled areas need a
scanline filler, which is the only piece of renderer we would write.
The raster path (Meshtastic's PNG tiles on the microSD) was not
measured here. It is the comparison the concept page still owes.

## Data and attribution

Source: the Protomaps sample archive
`https://pmtiles.io/protomaps(vector)ODbL_firenze.pmtiles` (6,601,156
bytes, PMTiles v3, gzip MVT tiles, z0 to z15, 92 tiles, bounds lon
11.2211 to 11.2875, lat 43.7451 to 43.7893, Protomaps Basemap schema
3.7.1, built by planetiler 2024-08-01 from the OSM replication of
2024-08-07). Only the archive header, the root directory and the 12
tiles three views touch were fetched by HTTP range requests (no key,
no bulk download). Fetched: 16,384 bytes of header and root
directory, plus 961,429 bytes of tiles. The tiles are 960 KB on disk
under `/tmp/map-measure/tiles/`, not in the repo.

Attribution, as the archive's metadata states it: **© OpenStreetMap
contributors**, <https://www.openstreetmap.org/copyright>. The data is
under the Open Database License (ODbL) 1.0. Protomaps Basemap layers
are derived from OpenStreetMap and Natural Earth (public domain).

View centre: Santa Maria del Fiore, 43.77310 N 11.25600 E. A view is
240x320 pixels with 256-pixel tiles in Web Mercator (a Protomaps
style draws these tiles at 512 px, so our z15 corresponds to their
z14 in apparent density). The three zooms are a town centre (z15), a
town (z13) and a region (z10). At z10 the extract has only the
southern two of the four tiles the view touches (z10/543/372 and
z10/544/372 are absent), so **the z10 rows describe half a view**.
A full region view is about twice as much, but that was not measured.

## Crates

Checked in the downloaded sources: `#![no_std]` or a `std` feature in
`src/lib.rs`, and the licence in `Cargo.toml`.

| Crate | Version | Role | `no_std` | Licence |
|---|---|---|---|---|
| `pmtiles` | 0.25.0 | PMTiles reader (async, tokio, HTTP/S3/mmap backends) | no | MIT OR Apache-2.0 |
| `pmtiles2` | 0.3.2 | PMTiles reader, sync on `std::io` | no | MIT |
| `mvt-reader` | 2.5.0 | MVT to `geo-types` features with property maps | no (uses `std` HashMap) | MIT |
| `geozero` | 0.15.1 (`with-mvt`) | prost MVT types, geometry streamed into a `GeomProcessor` | no | MIT OR Apache-2.0 |
| `tinymvt` | 0.3.0 | prost MVT types and a `GeometryDecoder` to `Vec<[i32;2]>` rings | no `#![no_std]`. `vector_tile.rs` uses only `::core` and `::prost::alloc`, `geometry.rs` no `std::`, one `std::` in `tag.rs` | MIT |
| `prost` | 0.14.4 | protobuf runtime under all three MVT crates | yes, with `alloc` (`default-features = false`) | Apache-2.0 |
| `flate2` / `miniz_oxide` | 1.1.10 / 0.8.9 | gzip inflate | `miniz_oxide` yes, with `alloc` | MIT OR Apache-2.0 / MIT OR Zlib OR Apache-2.0 |
| `embedded-graphics` | 0.8.2 | drawing (already in `leviculum-nrf`, feature `display`) | yes, no allocator | MIT OR Apache-2.0 |

`embedded-graphics` 0.8.2 primitives are arc, circle, ellipse, line,
polyline, rectangle, rounded rectangle, sector and triangle. None of
them fills an arbitrary polygon.

## Table 1: bytes per view

| View | Tiles present | gzip bytes (disk) | raw MVT bytes | largest raw tile |
|---|---|---|---|---|
| z15 town centre | 4 of 4 | 460,776 | 883,826 | 295,932 |
| z13 town | 4 of 4 | 299,713 | 405,501 | 141,846 |
| z10 region (half) | 2 of 4 | 200,940 | 277,560 | 178,611 |
| z15, stripped | 4 of 4 | 61,097 | 94,352 | 30,578 |
| z13, stripped | 4 of 4 | 187,459 | 250,942 | 94,714 |

"Stripped" means the `buildings`, `pois` and `physical_point` layers
removed and the tile re-encoded with prost and gzip at the default
level. z10 has no buildings, so its stripped figures are the same as
unstripped (202,366 gzip bytes after re-encoding). For flash: the
whole extract, about 5.3 by 4.9 km at z0 to z15, is 6.6 MB. On the M9
that goes on the microSD card, not into the 16 MB NOR.

## Table 2: decoders, host

Peak heap is the peak of a counting `GlobalAlloc` (requested sizes,
no allocator overhead) above its level before the step, for one tile
at a time, worst tile of the view. Time is the median of 50 runs for
the whole view, release build, single thread.

| View | Features | `mvt-reader` peak / time | `geozero` peak / time | inflate whole view |
|---|---|---|---|---|
| z15 | 17,128 | 9,621,616 B / 10.53 ms | 1,667,544 B / 3.48 ms | 1.83 ms |
| z13 | 2,610 | **250,207,497 B** / 3.98 ms | 737,195 B / 1.58 ms | 1.03 ms |
| z10 (half) | 1,724 | 39,997,935 B / 2.11 ms | 1,086,427 B / 1.02 ms | 0.80 ms |
| z15 stripped | 1,286 | 5,745,670 B / 1.18 ms | 181,460 B / 0.42 ms | 0.17 ms |
| z13 stripped | 2,165 | 249,731,587 B / 2.60 ms | 537,639 B / 1.04 ms | 0.59 ms |

Inflating the largest tile peaks at 371,996 B (z15), 217,910 B (z13)
and 254,675 B (z10), the output buffer included.

Why `mvt-reader` explodes: `parse_geometry`
(`mvt-reader-2.5.0/src/lib.rs:362`, again at `:380` and `:409`)
starts every coordinate run with
`Vec::with_capacity(geometry_data.len())`, the command count of the
whole feature. A multipolygon with R rings and G command integers
therefore holds about R x G x 8 bytes. The z13 tiles carry a few
features with thousands of rings. Stripping buildings does not cure
it, so it is not one layer's problem.

## Table 3: the render pipeline, host

The pipeline: for each tile in turn, `tinymvt`/prost decodes the
tile; each feature's geometry is decoded, projected to view pixels,
clipped (integer Cohen-Sutherland) against 240x320 and drawn with
`embedded_graphics::primitives::Line` into an Rgb565 framebuffer
(153,600 B, allocated before measuring); then the tile is dropped.
Polygons are drawn as outlines. "Device RAM" is the peak heap, plus
the largest raw tile (held while decoding), plus the framebuffer.

| View | Peak heap | Device RAM | decode+clip | decode+clip+draw | + inflate | lit px |
|---|---|---|---|---|---|---|
| z15 | 1,667,544 B | 2,117,076 B (2.02 MiB) | 4.93 ms | 5.87 ms | 7.70 ms | 35,252 |
| z13 | 970,119 B | 1,265,565 B (1.21 MiB) | 3.83 ms | 5.44 ms | 6.47 ms | 45,010 |
| z10 (half) | 1,255,432 B | 1,587,643 B (1.51 MiB) | 2.86 ms | 3.95 ms | 4.76 ms | 24,267 |
| z15 stripped | 192,357 B | 376,535 B (0.36 MiB) | 0.70 ms | 0.89 ms | 1.06 ms | 15,063 |
| z13 stripped | 616,594 B | 864,908 B (0.82 MiB) | 2.28 ms | 3.14 ms | 3.73 ms | 32,048 |

The heap figures are from a 64-bit host. On the 32-bit target the
`Vec` and `String` headers are half the size and the element data is
the same, so the device heap is somewhat lower. The ESP-IDF
allocator's per-block overhead, which the counter does not see, pulls
the other way.

What a view draws, after clipping ("segments >=1px" are the segments
whose clipped ends land on different pixels, the ones `Line` actually
rasterises):

| View | Points in view | Lines in view | Polygons in view | Segments in view | Segments >=1px | Vertices decoded |
|---|---|---|---|---|---|---|
| z15 | 4,953 | 873 | 2,345 | 20,742 | 18,912 | 55,183 |
| z13 | 310 | 4,856 | 1,588 | 44,495 | 36,487 | 113,993 |
| z10 (half) | 46 | 1,279 | 942 | 42,533 | 29,470 | 104,702 |
| z15 stripped | 2 | 873 | 120 | 3,118 | 2,929 | 13,470 |
| z13 stripped | 28 | 4,856 | 428 | 19,718 | 17,169 | 62,448 |

The layers that dominate, from the per-layer output:

- z15: `pois` are 10,514 of 17,128 features (61 %), 4,947 of them
  points in view. `buildings` are 15,983 of 18,912 drawn segments
  (85 %). `roads` are 1,931.
- z13: `buildings` 19,318, `roads` 11,386 and `landuse` 4,089 of
  36,487 drawn segments.
- z10: `landuse` 13,747 and `natural` 9,707 of 29,470 drawn segments
  (80 %). `roads` are 4,208.

Vertex counts differ between `geozero` (72,301 at z15) and the
pipeline (55,183) because the pipeline does not count point features
as vertices. The feature counts agree across all three decoders.

The drawn views were inspected as images (`/tmp/map-measure/view-*.png`).
The stripped z15 view shows the Florence street grid. The z10 view
shows the absent northern half as black, and a horizontal and a
vertical line where polygon outlines follow the tile clip edges.
Those are the tile seams that outline-only drawing exposes.

## Table 4: ESP32-S3 estimate and budget

**Estimate, not measurement.** Scale factor: this host's single-thread
CoreMark divided by one ESP32-S3 core's.

- Host: AMD Ryzen 9 7950X (VM, 10 vCPUs), CoreMark 1.0 (eembc/coremark
  `1f483d5`, GCC 14.2.0 `-O2`, `PORT_DIR=linux`): **45,641.26**
  iterations/s.
- ESP32-S3: datasheet v2.2, section CPU: 1329.92 CoreMark for two
  cores at 240 MHz, so **664.96** for one core.
- Factor: **68.6**.

| View | Host total | ESP32-S3 estimate | Device RAM | Against 8 MB PSRAM |
|---|---|---|---|---|
| z15 | 7.70 ms | ~530 ms | 2.02 MiB | fits with room, time borderline |
| z13 | 6.47 ms | ~440 ms | 1.21 MiB | fits with room, time borderline |
| z10 (half) | 4.76 ms | ~330 ms (full view: roughly double, unmeasured) | 1.51 MiB | fits; time too slow for a full view |
| z15 stripped | 1.06 ms | ~73 ms | 0.36 MiB | fits with room, fast enough |
| z13 stripped | 3.73 ms | ~256 ms | 0.82 MiB | fits with room, borderline |

What the factor does not see, all of it pointing towards slower on
the device:

- CoreMark runs from cache and internal SRAM. Tile and decoded data
  of 0.4 to 2 MiB live in the octal PSRAM, which is reached through
  the data cache.
- The host pipeline projects in `f64`. The S3 FPU is single
  precision, so `f64` is software there. A device port would project
  in fixed-point `i32` from the tile's integer coordinates, which is
  cheap and what the estimate implicitly assumes.
- Moving the 153,600-byte frame to the ST7789 is not included. At an
  assumed 40 MHz SPI clock that is 153,600 x 8 / 40e6 = 31 ms of
  arithmetic, not a measurement.

## Simplifications the numbers ask for

1. Per-zoom layer profile, cut on the desktop. Z15 and z14: drop
   `buildings` and `pois` (-87 % gzip, -88 % heap, -86 % time).
   Region zooms: drop or generalise `landuse` and `natural`.
2. Coarser geometry for the region zooms. A 240x320 view cannot show
   29,470 distinct segments; an extent of 512 instead of 4096 and
   Douglas-Peucker at one pixel would collapse them. This was not
   measured.
3. One tile in memory at a time, as measured. Keeping the decoded
   view for panning multiplies the heap by the tile count (4 to 6).
4. A scanline polygon filler, if areas are to be filled, or
   outlines only. Pre-tiled rasters avoid both questions. They are the
   comparison still owed.

## Device measurement, prepared 2026-10-25

Lew decided on 2026-10-25 (20:45): the vector map gets a minimal filler
of our own on `embedded-graphics`, measured on the device before any
product code; PNG tiles are the documented fallback only. This section
is the preparation of that measurement: the filler, the host harness and
the M9 binary are built, the host numbers are below, and the device rows
are empty until the reviewer has flashed the binary.

### What was built

- **`leviculum-mapfill`**, a workspace crate, `no_std`, no `alloc`, no
  float. A scanline polygon filler with integer edges (16.16 fixed point,
  sub-pixel input of 1/16 pixel) and per-row buckets, holes, even-odd and
  nonzero, pixel-centre sampling, spans into any `DrawTarget` through
  `fill_solid`; wide lines as quads through the filler under nonzero; a
  flat i16 fixture format read in place; one render pipeline (clear,
  project and cull, fill, stroke) timed per phase by a caller's clock, so
  host and device run the same code. The reference read first is the
  112-line `fill.rs` of dfyz/osm-renderer (MIT, 02bd330a), not copied: it
  marks every pixel an edge touches and fills even-odd only, ours samples
  centres and carries the winding.
- **The host harness**, `leviculum-mapfill/measure/` (its own workspace,
  so prost stays out of the shipping graph). `extract` decodes the four
  z15 tiles of the Duomo view with `tinymvt` and writes
  `leviculum-mapfill/fixtures/firenze-z15.lmf`; re-running it on the same
  tiles gives a byte-identical file. `render` draws the fixture under both
  profiles into an Rgb565 frame and writes one PNG per profile.
- **The M9 binary**, `leviculum-mapfill/m9/` (its own workspace on the
  esp-rs toolchain; esp-hal 1.2.1, the community port's pin), described
  in `leviculum-mapfill/README.md` with its flash command and the UART
  lines to expect.

The fixture: 6,600 features (areas and lines; the 10,528 point features
are left out), 55,183 vertices, 278,236 bytes. The vertex count is the
first measurement's "Vertices decoded" for z15, so the fixture holds the
same geometry the outline pipeline drew. Draw order is by class: earth,
urban landuse, parks, pedestrian areas, sand, water, buildings, then
waterways, paths, minor roads, major roads, rail. Two flat colour tables,
one for areas and one for lines, with line widths of 1 to 4 pixels.
`stripped` drops buildings, the only one of the draft's three stripped
layers left once points are out.

The pictures are `view-full.png` and `view-stripped.png`, outside the
repository (under the bridge, `mapfill-997/`). The stripped view shows
the street grid over the urban landuse, the Arno in the south-west
corner and no tile seams: shared edges neither overlap nor leave a gap,
which `tests/fill.rs` checks pixel by pixel.

### Host numbers

Same host as Table 4 (Ryzen 9 7950X VM, single thread), release build.
"Fill" and "stroke" are what the filler handed the frame: edges kept
after the clip, spans and pixels written. A pixel painted twice counts
twice, so fill pixels exceed the 76,800 of the view where areas overlap.

| Profile | Features drawn of included | Vertices projected | Fill edges / spans / pixels | Stroke edges / spans / pixels |
|---|---|---|---|---|
| full | 2,749 of 6,600 | 55,183 | 13,747 / 16,778 / 232,728 | 5,919 / 10,922 / 21,422 |
| stripped | 518 of 1,280 | 13,470 | 831 / 2,581 / 174,195 | 5,919 / 10,922 / 21,422 |

No feature overflowed the device's capacities (4,096 edges, 4,096
vertices, 1,024 rings per feature; the largest feature has 1,051
vertices). `tests/fixture.rs` pins every number in this table.

| Profile | Total, median of 50 | clear | project+clip | fill | stroke |
|---|---|---|---|---|---|
| full | 1.246 ms | 0.046 ms | 0.325 ms | 0.712 ms | 0.395 ms |
| stripped | 0.511 ms | 0.002 ms | 0.145 ms | 0.071 ms | 0.430 ms |

The phase columns are one run timed per feature with `Instant`, three
clock reads per feature, so they add up to more than the untimed median
(1.478 and 0.647 ms). Against the outline pipeline of Table 3 this is no
decode: the fixture is already geometry. Scaled by Table 4's CoreMark
factor of 68.6, the full view would be about 85 ms and the stripped one
about 35 ms. That is an estimate with the same blind spots as Table 4
(PSRAM, the fixture read from flash through the cache), and the device
rows below replace it.

### The device binary

Built 2026-10-25 for `xtensa-esp32s3-none-elf` with esp-hal 1.2.1,
`lto = true`, `codegen-units = 1`. Section sizes from
`xtensa-esp32s3-elf-size -A`, in bytes:

| Build | `.text` | `.rodata` | `.rwtext` | `.data` | `.bss` | image (`espflash save-image`) |
|---|---|---|---|---|---|---|
| `opt-level = 3` (`--release`) | 63,301 | 291,916 | 8,992 | 7,284 | 271,268 | 391,056 |
| `opt-level = "s"` (`--profile release-s`) | 46,161 | 292,468 | 9,108 | 7,400 | 271,280 | 373,920 |

`.rodata` is mostly the fixture (278,236). `.bss` holds the SRAM frame
(153,600), the edge slots (65,536), the projected vertices (32,768) and
the active list, bucket heads and ring ends; the band-buffer fallback
was not needed, the whole frame fits in internal SRAM. The linker leaves
about 52 KiB for the stack.

### Device rows, for the reviewer

Cycles are `CCOUNT` at 240 MHz, the median of the three repetitions of
a cycle. Flush is CASET, RASET and RAMWR plus 153,600 bytes over SPI2 at
40 MHz with DMA.

| Build | Profile | Frame | clear | project+clip | fill | stroke | total | flush |
|---|---|---|---|---|---|---|---|---|
| opt 3 | full | SRAM | | | | | | |
| opt 3 | full | PSRAM | | | | | | |
| opt 3 | stripped | SRAM | | | | | | |
| opt 3 | stripped | PSRAM | | | | | | |
| opt s | full | SRAM | | | | | | |
| opt s | full | PSRAM | | | | | | |
| opt s | stripped | SRAM | | | | | | |
| opt s | stripped | PSRAM | | | | | | |

The `phase=counts` lines from the device must equal the host table
above; if they do not, the device ran other code.

## Exact commands

```sh
# Network check and the archive header (no key needed)
curl -sIL "https://pmtiles.io/protomaps(vector)ODbL_firenze.pmtiles"

mkdir -p /tmp/map-measure && cd /tmp/map-measure
cargo new --quiet probe && cd probe
cargo add pmtiles@0.25 --no-default-features --features http-async,reqwest-rustls,tilejson
cargo add tokio --features rt-multi-thread,macros
# src/bin/header.rs and src/bin/fetch.rs as in the appendix
cargo run -q --release --bin header > /tmp/map-measure/header.log 2>&1
cargo run -q --release --bin fetch  > /tmp/map-measure/fetch.log 2>&1

cd /tmp/map-measure && cargo new --quiet measure && cd measure
cargo add mvt-reader@2.5 flate2 prost@0.14 tinymvt@0.3 pmtiles2@0.3 embedded-graphics@0.8
cargo add geozero@0.15 --no-default-features --features with-mvt
# src/main.rs as in the appendix
cargo build --release > /tmp/map-measure/build.log 2>&1
./target/release/measure > /tmp/map-measure/measure.log 2>&1
for v in z15 z13 z10 z15-stripped; do convert /tmp/map-measure/view-$v.pgm /tmp/map-measure/view-$v.png; done

# CoreMark on the host
cd /tmp/map-measure && git clone --depth 1 https://github.com/eembc/coremark.git
cd coremark && make XCFLAGS="-O2" ITERATIONS=0 PORT_DIR=linux && cat run1.log

# ESP32-S3 CoreMark from the datasheet
curl -sL -o esp32s3-datasheet.pdf https://www.espressif.com/sites/default/files/documentation/esp32-s3_datasheet_en.pdf
pdftotext esp32s3-datasheet.pdf ds.txt && grep -n -A2 "CoreMark® score" ds.txt
```

## Appendix: the throwaway sources

`probe/src/bin/fetch.rs`:

```rust
// Fetch exactly the tiles that a 240x320 view centred on the Duomo touches,
// by HTTP range requests, and count every byte that crosses the wire.
use pmtiles::{AsyncBackend, AsyncPmTilesReader, BackendResponse, HttpBackend, PmtResult, TileCoord, reqwest::Client};
use std::sync::atomic::{AtomicUsize, Ordering};
const URL: &str = "https://pmtiles.io/protomaps(vector)ODbL_firenze.pmtiles";
static WIRE: AtomicUsize = AtomicUsize::new(0);
const CAP: usize = 5 * 1024 * 1024;
struct Counting(HttpBackend);
impl AsyncBackend for Counting {
    async fn read(&self, offset: usize, length: usize) -> PmtResult<BackendResponse> {
        let r = self.0.read(offset, length).await?;
        let n = WIRE.fetch_add(r.bytes.len(), Ordering::SeqCst) + r.bytes.len();
        assert!(n < CAP, "wire bytes over 5 MB cap");
        Ok(r)
    }
}
pub fn view_tiles(lat: f64, lon: f64, z: u8) -> Vec<(u32, u32)> {
    let n = f64::from(1u32 << z) * 256.0;
    let px = (lon + 180.0) / 360.0 * n;
    let lr = lat.to_radians();
    let py = (1.0 - (lr.tan() + 1.0 / lr.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    let (x0, x1) = (((px - 120.0) / 256.0).floor() as u32, ((px + 120.0 - 1e-9) / 256.0).floor() as u32);
    let (y0, y1) = (((py - 160.0) / 256.0).floor() as u32, ((py + 160.0 - 1e-9) / 256.0).floor() as u32);
    let mut v = vec![];
    for x in x0..=x1 { for y in y0..=y1 { v.push((x, y)); } }
    v
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (lat, lon) = (43.77310, 11.25600); // Santa Maria del Fiore
    let r = AsyncPmTilesReader::try_from_source(Counting(HttpBackend::try_from(Client::new(), URL)?)).await?;
    println!("header+root wire bytes {}", WIRE.load(Ordering::SeqCst));
    std::fs::create_dir_all("/tmp/map-measure/tiles")?;
    for z in [15u8, 13, 10] {
        for (x, y) in view_tiles(lat, lon, z) {
            let before = WIRE.load(Ordering::SeqCst);
            let t = r.get_tile(TileCoord::new(z, x, y)?).await?;
            let after = WIRE.load(Ordering::SeqCst);
            match t {
                Some(b) => {
                    std::fs::write(format!("/tmp/map-measure/tiles/{z}_{x}_{y}.mvt.gz"), &b)?;
                    println!("tile z{z} x{x} y{y} gz_bytes {} wire {}", b.len(), after - before);
                }
                None => println!("tile z{z} x{x} y{y} ABSENT"),
            }
        }
    }
    println!("total wire bytes {}", WIRE.load(Ordering::SeqCst));
    Ok(())
}
```

`measure/src/main.rs`:

```rust
// Throwaway measurement for Lew_Palm/leviculum#474: decode the vector tiles a
// 240x320 view touches, count what it would draw, and track peak heap.
use embedded_graphics::{pixelcolor::Rgb565, prelude::*, primitives::{Line, PrimitiveStyle}};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Instant;

struct Counting;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() { let c = CUR.fetch_add(l.size(), SeqCst) + l.size(); PEAK.fetch_max(c, SeqCst); }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) { unsafe { System.dealloc(p, l) }; CUR.fetch_sub(l.size(), SeqCst); }
}
#[global_allocator]
static A: Counting = Counting;
fn reset() -> usize { let c = CUR.load(SeqCst); PEAK.store(c, SeqCst); c }
fn peak_since(base: usize) -> usize { PEAK.load(SeqCst) - base }

const W: i32 = 240;
const H: i32 = 320;

struct Fb(Vec<u16>);
impl OriginDimensions for Fb { fn size(&self) -> Size { Size::new(W as u32, H as u32) } }
impl DrawTarget for Fb {
    type Color = Rgb565;
    type Error = core::convert::Infallible;
    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(&mut self, px: I) -> Result<(), Self::Error> {
        for Pixel(p, c) in px {
            if p.x >= 0 && p.y >= 0 && p.x < W && p.y < H { self.0[(p.y * W + p.x) as usize] = c.into_storage(); }
        }
        Ok(())
    }
}

/// Cohen-Sutherland in integer view pixels; returns the clipped segment or None.
fn clip(mut a: (i64, i64), mut b: (i64, i64)) -> Option<((i64, i64), (i64, i64))> {
    let (xmin, ymin, xmax, ymax) = (0i64, 0i64, (W - 1) as i64, (H - 1) as i64);
    let code = |p: (i64, i64)| ((p.0 < xmin) as u8) | (((p.0 > xmax) as u8) << 1) | (((p.1 < ymin) as u8) << 2) | (((p.1 > ymax) as u8) << 3);
    let (mut ca, mut cb) = (code(a), code(b));
    loop {
        if ca | cb == 0 { return Some((a, b)); }
        if ca & cb != 0 { return None; }
        let c = if ca != 0 { ca } else { cb };
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let p = if c & 8 != 0 { (a.0 + dx * (ymax - a.1) / dy, ymax) }
            else if c & 4 != 0 { (a.0 + dx * (ymin - a.1) / dy, ymin) }
            else if c & 2 != 0 { (xmax, a.1 + dy * (xmax - a.0) / dx) }
            else { (xmin, a.1 + dy * (xmin - a.0) / dx) };
        if c == ca { a = p; ca = code(a); } else { b = p; cb = code(b); }
    }
}

#[derive(Default, Clone)]
struct Counts { features: usize, points_in_view: usize, lines_in_view: usize, polys_in_view: usize, segs_in_view: usize, segs_drawn_ge1px: usize, vertices: usize }

fn view_origin(lat: f64, lon: f64, z: u8) -> (f64, f64) {
    let n = f64::from(1u32 << z) * 256.0;
    let lr = lat.to_radians();
    ((lon + 180.0) / 360.0 * n - 120.0, (1.0 - (lr.tan() + 1.0 / lr.cos()).ln() / std::f64::consts::PI) / 2.0 * n - 160.0)
}

fn tiles(z: u8) -> Vec<(u32, u32, Vec<u8>, usize)> {
    let mut v = vec![];
    for e in std::fs::read_dir("/tmp/map-measure/tiles").unwrap() {
        let p = e.unwrap().path();
        let n = p.file_name().unwrap().to_str().unwrap().trim_end_matches(".mvt.gz").to_string();
        let f: Vec<u32> = n.split('_').map(|s| s.parse().unwrap()).collect();
        if f[0] as u8 != z { continue; }
        let gz = std::fs::read(&p).unwrap();
        let mut raw = vec![];
        flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut raw).unwrap();
        v.push((f[1], f[2], raw, gz.len()));
    }
    v.sort_by_key(|t| (t.0, t.1));
    v
}

/// The render pipeline: one tile decoded at a time (tinymvt/prost), each
/// feature's geometry decoded, projected, clipped and drawn, then dropped.
fn render(z: u8, ts: &[(u32, u32, Vec<u8>, usize)], fb: &mut Fb, per_layer: &mut BTreeMap<String, Counts>, draw: bool) {
    use prost::Message;
    use tinymvt::{geometry::GeometryDecoder, vector_tile::Tile};
    let (ox, oy) = view_origin(43.77310, 11.25600, z);
    let style = PrimitiveStyle::with_stroke(Rgb565::WHITE, 1);
    for (tx, ty, raw, _) in ts {
        let tile = Tile::decode(&raw[..]).unwrap();
        for layer in &tile.layers {
            let ext = layer.extent.unwrap_or(4096) as f64;
            let c = per_layer.entry(layer.name.clone()).or_default();
            let px = |p: [i32; 2]| -> (i64, i64) {
                ((*tx as f64 * 256.0 + p[0] as f64 * 256.0 / ext - ox).round() as i64,
                 (*ty as f64 * 256.0 + p[1] as f64 * 256.0 / ext - oy).round() as i64)
            };
            for f in &layer.features {
                c.features += 1;
                let mut d = GeometryDecoder::new(&f.geometry);
                let mut seg_run = |ring: &[[i32; 2]], closed: bool, c: &mut Counts, fb: &mut Fb| -> bool {
                    c.vertices += ring.len();
                    let mut any = false;
                    let n = ring.len();
                    let m = if closed { n } else { n.saturating_sub(1) };
                    for i in 0..m {
                        let (a, b) = (px(ring[i]), px(ring[(i + 1) % n]));
                        if let Some((ca, cb)) = clip(a, b) {
                            any = true;
                            c.segs_in_view += 1;
                            if ca != cb {
                                c.segs_drawn_ge1px += 1;
                                if draw { let _ = Line::new(Point::new(ca.0 as i32, ca.1 as i32), Point::new(cb.0 as i32, cb.1 as i32)).into_styled(style).draw(fb); }
                            }
                        }
                    }
                    any
                };
                match f.r#type {
                    Some(1) => { for p in d.decode_points().unwrap() { let q = px(p); if q.0 >= 0 && q.1 >= 0 && q.0 < W as i64 && q.1 < H as i64 { c.points_in_view += 1; } } }
                    Some(2) => { for l in d.decode_linestrings().unwrap() { if seg_run(&l, false, c, fb) { c.lines_in_view += 1; } } }
                    Some(3) => { for poly in d.decode_polygons().unwrap() { let mut any = false; for r in &poly { any |= seg_run(r, true, c, fb); } if any { c.polys_in_view += 1; } } }
                    _ => {}
                }
            }
        }
    }
}

fn median(mut v: Vec<f64>) -> f64 { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); v[v.len() / 2] }

fn strip(ts: &[(u32, u32, Vec<u8>, usize)]) -> Vec<(u32, u32, Vec<u8>, usize)> {
    use prost::Message;
    ts.iter().map(|(x, y, r, _)| {
        let mut t = tinymvt::vector_tile::Tile::decode(&r[..]).unwrap();
        t.layers.retain(|l| !matches!(l.name.as_str(), "buildings" | "pois" | "physical_point"));
        let raw = t.encode_to_vec();
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut e, &raw).unwrap();
        let gz = e.finish().unwrap().len();
        (*x, *y, raw, gz)
    }).collect()
}

fn main() {
    const N: usize = 50;
    for (z, stripped) in [(15u8, false), (13, false), (10, false), (15, true), (13, true), (10, true)] {
        let ts = if stripped { strip(&tiles(z)) } else { tiles(z) };
        if stripped { println!("\n##### STRIPPED: buildings, pois, physical_point removed and re-encoded (prost, gzip default level)"); }
        let gz: usize = ts.iter().map(|t| t.3).sum();
        let raw: usize = ts.iter().map(|t| t.2.len()).sum();
        println!("\n=== z{z}: {} tiles present, gzip bytes {gz}, raw MVT bytes {raw}, largest raw tile {}", ts.len(), ts.iter().map(|t| t.2.len()).max().unwrap());
        for (x, y, r, g) in &ts { println!("tile z{z}/{x}/{y} gz {g} raw {}", r.len()); }

        // Gzip inflate of the largest tile: heap and time.
        let big = ts.iter().max_by_key(|t| t.2.len()).unwrap();
        let gzb = if stripped { let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()); std::io::Write::write_all(&mut e, &big.2).unwrap(); e.finish().unwrap() } else { std::fs::read(format!("/tmp/map-measure/tiles/{z}_{}_{}.mvt.gz", big.0, big.1)).unwrap() };
        let gzs: Vec<Vec<u8>> = ts.iter().map(|t| { let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()); std::io::Write::write_all(&mut e, &t.2).unwrap(); e.finish().unwrap() }).collect();
        let t: Vec<f64> = (0..N).map(|_| { let s = Instant::now(); for g in &gzs { let mut o = vec![]; flate2::read::GzDecoder::new(&g[..]).read_to_end(&mut o).unwrap(); std::hint::black_box(o); } s.elapsed().as_secs_f64() * 1e3 }).collect();
        println!("inflate whole view (all present tiles, re-gzipped at default level): median {:.3} ms", median(t));
        let b = reset(); let mut out = Vec::with_capacity(big.2.len());
        flate2::read::GzDecoder::new(&gzb[..]).read_to_end(&mut out).unwrap();
        println!("inflate largest tile: peak heap {} B (output buffer {} B incl.)", peak_since(b), out.len()); drop(out);
        let t: Vec<f64> = (0..N).map(|_| { let s = Instant::now(); let mut o = vec![]; flate2::read::GzDecoder::new(&gzb[..]).read_to_end(&mut o).unwrap(); s.elapsed().as_secs_f64() * 1e3 }).collect();
        println!("inflate largest tile: median {:.3} ms", median(t));

        // Decoder A: mvt-reader, all features of all layers, one tile at a time.
        let mut feats = 0; let mut pk = 0;
        for (_, _, r, _) in &ts {
            let b = reset();
            let rd = mvt_reader::Reader::new(r.clone()).unwrap();
            let ls = rd.get_layer_metadata().unwrap();
            let fs: Vec<_> = (0..ls.len()).map(|i| rd.get_features(i).unwrap()).collect();
            feats += fs.iter().map(|f| f.len()).sum::<usize>();
            pk = pk.max(peak_since(b)); drop(fs); drop(rd);
        }
        let t: Vec<f64> = (0..N).map(|_| { let s = Instant::now(); for (_, _, r, _) in &ts { let rd = mvt_reader::Reader::new(r.clone()).unwrap(); let n = rd.get_layer_metadata().unwrap().len(); for i in 0..n { std::hint::black_box(rd.get_features(i).unwrap()); } } s.elapsed().as_secs_f64() * 1e3 }).collect();
        println!("mvt-reader: features {feats}, peak heap per tile {pk} B, view decode median {:.3} ms", median(t));

        // Decoder B: geozero (prost Tile, geometry streamed by process_geom).
        { use geozero::mvt::{Message, Tile}; use geozero::GeomProcessor;
          struct Cnt(usize); impl GeomProcessor for Cnt { fn xy(&mut self, _: f64, _: f64, _: usize) -> geozero::error::Result<()> { self.0 += 1; Ok(()) } }
          let mut pk = 0; let mut verts = 0; let mut feats = 0;
          for (_, _, r, _) in &ts { let b = reset(); let tile = Tile::decode(&r[..]).unwrap(); let mut c = Cnt(0);
            for l in &tile.layers { for f in &l.features { feats += 1; geozero::mvt::process_geom(f, &mut c).unwrap(); } }
            verts += c.0; pk = pk.max(peak_since(b)); }
          let t: Vec<f64> = (0..N).map(|_| { let s = Instant::now(); for (_, _, r, _) in &ts { let tile = Tile::decode(&r[..]).unwrap(); let mut c = Cnt(0); for l in &tile.layers { for f in &l.features { geozero::mvt::process_geom(f, &mut c).unwrap(); } } std::hint::black_box(c.0); } s.elapsed().as_secs_f64() * 1e3 }).collect();
          println!("geozero: features {feats}, vertices {verts}, peak heap per tile {pk} B, view decode median {:.3} ms", median(t)); }

        // Decoder C + render: tinymvt, clip, draw into an Rgb565 framebuffer.
        let mut fb = Fb(vec![0; (W * H) as usize]);
        let mut per_layer = BTreeMap::new();
        let b = reset();
        render(z, &ts, &mut fb, &mut per_layer, true);
        let pk_render = peak_since(b);
        let t_count: Vec<f64> = (0..N).map(|_| { let mut pl = BTreeMap::new(); let s = Instant::now(); render(z, &ts, &mut fb, &mut pl, false); s.elapsed().as_secs_f64() * 1e3 }).collect();
        let t_draw: Vec<f64> = (0..N).map(|_| { let mut pl = BTreeMap::new(); let s = Instant::now(); render(z, &ts, &mut fb, &mut pl, true); s.elapsed().as_secs_f64() * 1e3 }).collect();
        let lit = fb.0.iter().filter(|&&p| p != 0).count();
        println!("render (tinymvt): peak heap over the view {pk_render} B (framebuffer {} B excluded, held before), decode+clip median {:.3} ms, decode+clip+draw median {:.3} ms, lit px {lit}", fb.0.len() * 2, median(t_count), median(t_draw));
        let mut tot = Counts::default();
        println!("layer | features | points in view | lines in view | polygons in view | segments in view | segments >=1px | vertices");
        for (n, c) in &per_layer {
            println!("{n} | {} | {} | {} | {} | {} | {} | {}", c.features, c.points_in_view, c.lines_in_view, c.polys_in_view, c.segs_in_view, c.segs_drawn_ge1px, c.vertices);
            tot.features += c.features; tot.points_in_view += c.points_in_view; tot.lines_in_view += c.lines_in_view; tot.polys_in_view += c.polys_in_view; tot.segs_in_view += c.segs_in_view; tot.segs_drawn_ge1px += c.segs_drawn_ge1px; tot.vertices += c.vertices;
        }
        println!("TOTAL | {} | {} | {} | {} | {} | {} | {}", tot.features, tot.points_in_view, tot.lines_in_view, tot.polys_in_view, tot.segs_in_view, tot.segs_drawn_ge1px, tot.vertices);
        // Save the drawn view for eyeballing as a PGM.
        let mut pgm = format!("P5 {W} {H} 255\n").into_bytes();
        pgm.extend(fb.0.iter().map(|&p| if p != 0 { 255u8 } else { 0 }));
        std::fs::write(format!("/tmp/map-measure/view-z{z}{}.pgm", if stripped { "-stripped" } else { "" }), pgm).unwrap();
    }
}
```
