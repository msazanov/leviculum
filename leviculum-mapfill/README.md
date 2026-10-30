# leviculum-mapfill

The polygon fill the vector map needs and `embedded-graphics` 0.8 lacks
(#474, #293). `no_std`, no `alloc`, no float; the caller lends every
buffer.

- `fill`: scanline fill with integer edges and per-row buckets. Holes,
  even-odd and nonzero, pixel-centre sampling (shared edges neither
  overlap nor gap). Spans go to any `DrawTarget` through `fill_solid`.
- `stroke`: wide lines as quads through the filler, square caps,
  unioned under nonzero.
- `fixture`: the flat i16 geometry format, read in place from flash.
- `render`: one view from a fixture: clear, project and cull, fill,
  stroke, each phase timed by a caller's `Clock`.
- `style`: two flat colour tables (areas, lines) and the two layer
  profiles of the first measurement (`full`, `stripped`).
- `frame`: an Rgb565 framebuffer stored in the ST7789's byte order.

The measurement it exists for is
`docs/drafts/map-measurement-2026-10.md`, section "Device measurement,
prepared 2026-10-25".

## The API in one example

```rust
use embedded_graphics::prelude::*;
use leviculum_mapfill::fill::{Edge, FillRule, Filler, Scratch, SUBPIXEL};

let mut edges = [Edge::EMPTY; 64];
let mut active = [0u16; 64];
let mut heads = [0u16; 320];
let mut scratch = Scratch { edges: &mut edges, heads: &mut heads, active: &mut active };
let mut f = Filler::new(&mut scratch, target.bounding_box());
let p = |x: i32, y: i32| Point::new(x * SUBPIXEL, y * SUBPIXEL);
f.ring(&[p(10, 10), p(90, 10), p(90, 60), p(10, 60)])?;   // outer
f.ring(&[p(30, 30), p(30, 40), p(70, 40), p(70, 30)])?;   // hole
let stats = f.fill(&mut target, color, FillRule::NonZero)?;
```

Coordinates are sub-pixels, 1/16 pixel (`SUBPIXEL`). A pixel is inside
when its centre is.

## Host harness: `measure/`

Its own workspace. It decodes tiles through `leviculum-map-tool`'s MVT
decoder, the tree's only one, which it carried itself until the region
tool needed it too (#474, 1062); `extract` still writes the same bytes.

```sh
cd leviculum-mapfill/measure
cargo build --release
B=target/x86_64-unknown-linux-musl/release/leviculum-mapfill-measure
# tiles fetched as in docs/drafts/map-measurement-2026-10.md, "Exact commands"
$B extract /tmp/map-measure/tiles ../fixtures/firenze-z15.lmf
$B render ../fixtures/firenze-z15.lmf /tmp/map-measure/mapfill
```

`render` writes `view-full.png` and `view-stripped.png` and prints the
counts and host times as `MAPFILL host ...` lines.

## Device binary: `m9/`

Bare measurement firmware for the ThinkNode M9, not a node: nothing on
the air. Its own workspace on the esp-rs toolchain, like `leviculum-esp`.

```sh
source /home/lew/export-esp.sh
cd leviculum-mapfill/m9
cargo build --release              # opt-level 3
cargo build --profile release-s    # opt-level "s"
```

Flash and listen (the reviewer's probe path, as `/home/lew/rig-run/m9-flash-run.sh`;
espflash writes its default bootloader and partition table, so the vendor
image comes back only from the backup under `/home/lew/firmware-backups/`):

```sh
PORT=/dev/serial/by-id/usb-1a86_USB_Serial-if00-port0
espflash flash --chip esp32s3 -p "$PORT" --non-interactive --baud 230400 \
    target/xtensa-esp32s3-none-elf/release/mapfill_m9
python3 /home/lew/rig-run/m9-listen4.py "$PORT" 115200 120 /tmp/mapfill-m9.raw /tmp/mapfill-m9.log
```

What the UART (115200 8N1) says, every ten seconds, a complete cycle:

```text
MAPFILL boot sha=<sha> opt=3 cycle=1 fixture_bytes=278236 features=6600 psram_bytes=8388608 fb_sram=0x3fc... fb_psram=0x3c...
MAPFILL profile=full fb=sram phase=clear cycles=... ms=... rep=1
MAPFILL profile=full fb=sram phase=project_clip cycles=... ms=... rep=1
MAPFILL profile=full fb=sram phase=fill cycles=... ms=... rep=1
MAPFILL profile=full fb=sram phase=stroke cycles=... ms=... rep=1
MAPFILL profile=full fb=sram phase=total cycles=... ms=... bytes=... rep=1
MAPFILL profile=full fb=sram phase=counts features=6600 culled=3851 vertices=55183 fill_edges=13747 ... overflows=0 rep=1
MAPFILL profile=full fb=sram phase=flush cycles=... ms=... bytes=153600 spi_mhz=40 rep=1
... rep=2, rep=3, then profile=stripped, then fb=psram ...
MAPFILL cycle=1 done
```

The `counts` lines must equal the host's (`tests/fixture.rs` pins
them); a difference means the device ran other code than the host. A
`psram_bytes=0` boot line and `fb=psram skipped=absent` mean the octal
PSRAM did not come up. The panel shows the last frame drawn; its
orientation (MADCTL 0x00, portrait) is unverified on this panel.

## Data

`fixtures/firenze-z15.lmf` is OpenStreetMap data, see
`fixtures/README.md`.
