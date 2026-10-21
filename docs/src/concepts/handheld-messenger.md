# A messenger whose core runs anywhere

A real messenger runs on the handheld. It sends and receives LXMF
messages, keeps contacts and history, and shows on a map where other
nodes are, from their telemetry, the way Columba does on Android today.
Lew decided on 2026-10-21 that it is written once, as a core, and
reused: on the ThinkNode M9 ([The ThinkNode M9](thinknode-m9.md)), on
the desktop, and later on Android. The map uses OpenStreetMap data and
existing libraries, not a renderer of our own. This page records that
decision, what exists to build it from, the reviewer's proposals for
the core/frontend cut and for the map, and what is open.

## Decided

- **The features:** send and receive LXMF messages, contacts, history,
  and the positions of other nodes on a map, taken from their telemetry.
- **One core, several frontends:** the M9, the desktop, later Android.
- **The map** is built from OpenStreetMap material with existing
  libraries. We do not write our own renderer.
- **Columba is the reference for scope and protocol behaviour**: what a
  messenger does and how it behaves on the wire. It is not a reference
  for how the code is designed.

## What exists today

- **`lnmsg`, the terminal messenger.** It sends (direct, propagated, or
  `auto` with the fallback to a propagation node) and drains the
  mailbox. It has no TUI yet ([lnmsg](lnmsg.md)). Its frontend already
  talks to the backend only through `Outbox` (`lnmsg/src/outbox.rs:170`),
  but the crate is `std`. It depends on `leviculum-std` and tokio
  (`lnmsg/Cargo.toml:24-28`).
- **#232's rule:** the messenger's core must be `std`-free, not merely
  terminal-free, because the next frontend after the TUI is firmware. The
  shape #232 asks for is a `no_std + alloc` core crate, a store behind a
  trait the core owns, effects as data, and a cross-compile gate. That
  core crate does not exist yet. [lnmsg:
  architecture](lnmsg-architecture.md), section 4, has the terminal-free
  requirement it extends.
- **`lnomad`'s core/frontend boundary (#219).** It is the same
  requirement for the browser, and still a refactor to come: model,
  update and view share `lnomad/src/tui.rs`, which is about 12,000 lines.
- **`leviculum-lxmf`, `no_std + alloc`** (`leviculum-lxmf/src/lib.rs:36`):
  - delivery (`LxmfRouter`, `leviculum-lxmf/src/router.rs:462`);
  - propagation nodes, both the client and the node
    (`PropagationNode`, `leviculum-lxmf/src/propagation_node.rs:819`);
  - stamps (`CooperativeStamper`, `leviculum-lxmf/src/stamp.rs:269`).
- **Telemetry.** [Telemetry](telemetry.md) settles positions as LXMF
  fields (`FIELD_TELEMETRY`, `FIELD_TELEMETRY_STREAM`) and the
  collector exchange. The codec is `Telemetry`
  (`leviculum-lxmf/src/telemetry.rs:176`) with its `Location`
  (`leviculum-lxmf/src/telemetry.rs:56`).
- **The C surface, `leviculum-ffi`**, is the path to Android
  (`leviculum-ffi/src/lib.rs:1-12`, [C API](../c-api/overview.md)). Today
  it covers identities, destinations, links, requests and resources, and
  no LXMF.
- **`embedded-graphics` on the T114's ST7789.** The T114 paints its
  status screen through `embedded-graphics`
  (`leviculum-nrf/screen/Cargo.toml:10`) into a monochrome framebuffer
  (`MonoFb`, `leviculum-nrf/screen/src/lib.rs:377`), which the ST7789
  backend scales onto a 240x135 panel (`leviculum-nrf/src/st7789.rs:3`).
  That is a status screen, not a colour UI. It shows that the drawing
  library works on our firmware.

## The core/frontend cut

*The reviewer's proposal, not yet decided.*

**The core owns:**

- conversations, contacts and delivery state;
- telemetry positions;
- persistence, behind a trait;
- the LXMF and propagation node machinery.

It is `no_std + alloc` and tested on the host.

**Frontends own only rendering and input:**

- `lnmsg`'s terminal;
- the M9's display and keyboard;
- an Android app over the C surface.

Nothing a frontend draws is decided inside it.

## The map

*The reviewer's proposal, not yet decided, with its open measurement.*

The reviewer knows of no off-the-shelf vector map renderer for embedded
targets. The candidate building blocks are:

- **PMTiles**, an archive that holds a whole tile set in one file;
- **Mapbox Vector Tiles**, the payload inside each tile;
- the Rust readers for both;
- **`embedded-graphics`** for drawing.

The likely shape: a desktop tool cuts an OSM extract, simplifies lines
and polygons for each zoom level, and writes a compact tile set to the
device's flash. The device draws polylines, its own GNSS position, and
the telemetry positions of other nodes.

The measured precedent is raster, not vector: Meshtastic's device UI
on the M9 reads PNG raster tiles from the microSD card at
`/maps/<style>/{z}/{x}/{y}.png` ([The ThinkNode M9](thinknode-m9.md),
"Storage"). The vector idea stays the first-step measurement below,
compared against that raster path.

**The first step is a measurement, not code.** Measure, for one city at
three zoom levels:

- the flash the tiles need;
- the RAM used while rendering one 240x320 view;
- the time it takes to draw that view.

Whether we use an existing library or our own format is decided on
those numbers, not before.

## Order of work

*The reviewer's proposal.*

1. The community port is visible and reviewed.
2. Bring-up and radio at the rig ([The ThinkNode M9](thinknode-m9.md),
   "Bring-up plan").
3. The messenger core is taken out of `lnmsg` and tested on the host.
4. The M9 frontend, with messages.
5. Positions on a map.
6. The map itself.

## Open

- **The licence text for the map data.** OSM data is ODbL: which notice
  ships with the tiles, and where the device shows it ([Licensing and
  third-party notices](licensing-and-notices.md)).
- **How other nodes' positions age out of the map.**
- **Whether the M9 frontend and the terminal share a text layout
  engine.**

## Not yet built

All of it: the `no_std` core crate, a firmware frontend, LXMF on the C
surface, the tile tool, the tile format, and the map view.

## See also

- [The ThinkNode M9](thinknode-m9.md), the first device this is for.
- [lnmsg: a terminal LXMF messenger](lnmsg.md) and [lnmsg:
  architecture](lnmsg-architecture.md).
- [Telemetry](telemetry.md), for the positions on the map.
