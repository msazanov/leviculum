# loquax: the map

Part of the [loquax design record](loquax.md). Lew, 2026-10-27: the vector
map is part of loquax. Messenger and map belong together because telemetry
travels over the same LXMF channel as the messages, and the user wants to
see the positions inside the messenger. The map comes to the desktop
loquax, its terminal frontend and the planned iced GUI, not only to the
board. [A messenger whose core runs anywhere](handheld-messenger.md), "The
map", already calls the map a product of its own and a view inside the
messenger, on the desktop and embedded. This page plans it in slices.

**Status: decided 2026-10-27 (Lew), all five points of section 7 as
proposed.** Slice (a), positions in the model shown as text, is built
(2026-10-27, leviculum 1059, commit `230c5e4d`); the rest of the page is
not. The measurement it rests on is
`docs/drafts/map-measurement-2026-10.md`; the decisions it rests on are the
toolkit ([A GUI toolkit for the desktop and the M9](gui-toolkit.md),
option B) and the vector path with our own filler (handheld-messenger.md,
"The map", 2026-10-25). Tracked under #232 (the messenger core) and #474
(the map).

## What loquax receives of telemetry today

Since slice (a), every position. The engine passes an incoming message's
fields on (`Message::fields`, `leviculum-lxmf/src/message.rs:84`):
`RouterEvent::MessageReceived` becomes `OutboxEvent::Received` with the
fields beside id, source, title, body and the verification
(`loquax/src/engine.rs:420`, `loquax/src/outbox.rs:158`). The core reads
the two telemetry fields into positions (`telemetry::read`,
`loquax/src/telemetry.rs:130`), the screen model keeps each peer's latest
and hands every one to the store (`take_telemetry`,
`loquax/src/ui.rs:2003`), and the store keeps the fields whole in its
`fields` column (`loquax/src/store.rs:724`) and the positions in a table of
their own (`SCHEMA_V2`, `loquax/src/store.rs:144`). The sender's timestamp
is still not carried: `OutboxEvent`, the screen's `Message` and every type
the model folds derive `Eq`, which an `f64` cannot, so carrying it is a
change of its own and not the one line of plumbing the fields were.

Before slice (a) the fields were dropped at that hand-over, and a telemetry
report, whose body and title are empty ([Telemetry](telemetry.md), "A
reporting message carries no text"), arrived as an empty block in the
peer's conversation with its position lost.

What the core reads them with, all in `leviculum-lxmf`, `no_std + alloc`:

- the field numbers, `FIELD_TELEMETRY` and `FIELD_TELEMETRY_STREAM`
  (`leviculum-lxmf/src/constants.rs:61`, `leviculum-lxmf/src/constants.rs:62`);
- the Telemeter codec, `Telemetry` (`leviculum-lxmf/src/telemetry.rs:176`)
  with `Telemetry::decode_field_value`
  (`leviculum-lxmf/src/telemetry.rs:325`), and its `Location` in the wire's
  scaled integers (`leviculum-lxmf/src/telemetry.rs:56`);
- the stream decoder for a collector's rows, `decode_stream_field_value`
  (`leviculum-lxmf/src/telemetry.rs:381`);
- the request reader, `telemetry_request_timebase`
  (`leviculum-lxmf/src/telemetry.rs:688`), whose command id is a
  documented assumption (`COMMAND_TELEMETRY_REQUEST`,
  `leviculum-lxmf/src/telemetry.rs:678`).

## 1. What the user sees

**In the conversation.** A peer with a known position carries it in the
header, at 120 columns on the header's second row beside how the peer is
reached: `at 43.77310 N 11.25600 E · ±5 m · 4 min ago`. At 80 columns the
header shows only the age, `pos 4 min`, so the reader knows there is one.
The detail view (`Enter` on a message, slice 2 of [keys, mouse and
layout](loquax-keys-and-layout.md)) of a telemetry message lists every
sensor the codec decoded: position with altitude, speed, bearing and
accuracy where given, the fix time, battery, link and temperature. A
telemetry message is not drawn as an empty block. It is one line in the
conversation, `◎ position 43.77310 N 11.25600 E, 14:05`, so the exchange
still shows when a report arrived, and the rows of one peer's reports
fold into the latest one when they follow each other. As built, the `14:05` is the
block's own arrival time beside the line, as every block carries it; the
fix's own time is in the header's age and the detail view. A run folds
only within one day, and not across a name note or the `new` rule.

**The age** is always shown and always counts from the fix time
(`Location::last_update`), not from when the message arrived: a report
that waited at a propagation node for an hour is an hour old. Accuracy is
shown only when the sender gave one; nothing is invented for a missing
field, the same rule as the producer's ([Telemetry](telemetry.md), "No
fix, no position").

**The map view** is a palette action, `map`, with a key the keymap table
of [keys, mouse and layout](loquax-keys-and-layout.md) assigns. It
takes the conversation pane, so the list stays and `Esc` returns. It shows
every peer with a position, our own if we have one, a pin per peer with
its name by the display rule of [the mailbox page](loquax-mailbox.md)
(petname, else announced name, always with the short hash; slice 5), and
the age beside each name. A stale fix (below) is drawn dimmed with its age.
Selecting a pin, by `Tab` through the pins or by click, opens that peer's
conversation. The view fits all pins on open; `+`, `-` and the arrows zoom
and pan, `0` fits again.

## 2. The data path

**In.** Two fields carry positions, both read with the codec above:

- `FIELD_TELEMETRY` (0x02), one node's own readings, whose source is the
  message's signed source.
- `FIELD_TELEMETRY_STREAM` (0x03), a collector's rows, each
  `[source_hash, timestamp, packed_telemetry, appearance]` ([Telemetry](telemetry.md),
  "The collector exchange"). Here the row's source is who measured, and
  the message's source only who delivered.

The format is Sideband's, its `Telemeter` class in `sense.py`, with
`Location.pack` as the position's layout and `Telemeter.from_packed` as
the reader that skips a sensor it does not know (Sideband `2000d81`, cited
by symbol for the reason [Telemetry](telemetry.md) gives). Columba's
`TelemeterCodec.kt` reads and writes the same two sensors, time and
location (Columba `0930293`). So a position from Sideband or Columba, and
from one of our boards, is the same bytes, and loquax reads all three with
one decoder. A SID mapped to nil is a sensor without a reading, accepted.

**The cut.** The engine passes `message.fields` on in
`OutboxEvent::Received`; the core (not the frontend) decodes the
telemetry fields and turns each reading with a location into a position
event. A blob that does not decode is kept raw and named in `tui.log`,
never dropped silently, the archive rule of `lntd` ([Telemetry](telemetry.md),
"What a receiving node owes").

**Stored where.** Slice 6's SQLite store fills its `fields` column with
the raw msgpack and gains one table, through the schema version
(`SCHEMA_VERSION` 1 to 2, a migration that only adds):

```sql
CREATE TABLE positions(
    identity BLOB NOT NULL,      -- our own address, as on every table
    peer BLOB NOT NULL,          -- who measured
    via BLOB NOT NULL,           -- who delivered; differs for a stream row
    message INTEGER REFERENCES messages(seq) ON DELETE SET NULL,
    fix_time INTEGER NOT NULL,   -- Location.last_update, unix seconds
    received_at INTEGER NOT NULL,
    lat_e6 INTEGER NOT NULL, lon_e6 INTEGER NOT NULL,
    alt_e2 INTEGER, speed_e2 INTEGER, bearing_e2 INTEGER, accuracy_e2 INTEGER,
    telemetry BLOB NOT NULL,     -- the raw Telemeter, for what we cannot decode yet
    PRIMARY KEY (identity, peer, fix_time));
```

The wire's integers are stored as they are, so no rounding happens between
the wire and the disk. As built, `received_at` may be NULL: a report
that arrives before the shell first told the screen the time has no
receive time to store, and inventing one would be the placeholder the
telemetry page forbids. A fix the wire gives no accuracy for (the field
is always packed, so zero is the only absence it can say) is stored as 0
and shown without one. The key `(peer, fix_time)` is Sideband's dedup rule
(exact source and timestamp), so the same fix arriving directly and again
in a collector's stream is one row. The map reads the latest row per peer;
the table keeps the track, which a later slice can draw. Retention follows
the messages: kept until the reader deletes them, with the conversation or
by a palette action `forget positions`.

**A future fix** is clamped for ordering, kept for display, the ingress
rule of [Time and clocks](time-and-clocks.md#ingress-clamp-for-semantics-keep-for-display):
a fix stamped ahead of our clock sorts at receive time and shows its claimed
time with a mark. Without the clamp, one wrong clock would pin a peer's
pin at "now" forever.

**Stale.** Decided 2026-10-27 (Lew): a fix is drawn as current up to 2 h old and dimmed
after that, with its age always beside it. Two hours is twice the station
profile's heartbeat (`max_interval_ms`, 60 min,
`leviculum-nrf/telemetry-policy/src/lib.rs:190`; the tracker's is 15 min,
`leviculum-nrf/telemetry-policy/src/lib.rs:175`), so a board that misses
one report stays current and one that misses two goes dim. A fix is never
removed for its age alone; it goes with a delete. This closes
handheld-messenger.md's open "how other nodes' positions age out of the
map".

**Asking a peer for its position** is a later step, not in slices (a) to
(e). The request is an LXMF message whose `FIELD_COMMANDS` carries
`TELEMETRY_REQUEST` with a timebase ([Telemetry](telemetry.md), "A request
from the target triggers one report"); sending one is a palette action
`request position` on the open conversation. Answering one needs a
position of our own and the user's per-peer switch of section 7, so it
waits for that.
Its command id must first be pinned against a captured Sideband request,
as the codec's own comment says.

## 3. The renderer split

One map core, three frontends. The core does what is the same problem on
every screen; a frontend only turns the core's output into its pixels or
cells and its input into the core's events, the rule of "The core/frontend
cut" on handheld-messenger.md.

| crate | what | `no_std` | used by |
|---|---|---|---|
| `leviculum-mapfill` (exists) | scanline filler, wide lines, the flat geometry reader, the render pipeline into any `DrawTarget` | yes, no `alloc` | the core, all three frontends |
| `leviculum-map` (new) | the region file reader, Web Mercator projection, the viewport (centre, zoom, pan, fit), tile selection and feature culling, the layer profile per zoom, the style tables, pins and labels as data | yes, `alloc` | all three frontends |
| `leviculum-map-tool` (new, `std`) | cuts a region file from an OSM extract (section 4); prost and the MVT decoder stay here, out of the shipping graph, as `leviculum-mapfill/measure/` keeps them today | no | the desktop, by hand |
| loquax core | positions in the model, the map view's state (open, viewport, selected pin), the events | yes, `alloc` | all three frontends |

The frontends:

- **The TUI** (`loquax`, ratatui). The core renders the view into an
  Rgb565 or Rgb888 frame through `leviculum-mapfill`, and the frontend
  hands that frame to the terminal: through `ratatui-image`'s graphics
  protocols where the terminal has one, else as Unicode half-blocks, else
  as a braille line drawing (the terminal options below). Pins and names
  are drawn as text cells over the picture, not into it, so they stay
  sharp and clickable on every backend.
- **The iced GUI.** The same frame, at the window's size, as an iced
  image widget; pins and labels as iced text over it. Panning by drag and
  zoom by wheel are input mapped to the same core events.
- **The M9.** The same pipeline straight into the panel's
  `embedded-graphics` target, as the measurement firmware does
  (`leviculum-mapfill/m9/`), with mousefood's cells for the labels.

One drawing path for all three is the point: the measured renderer is the
renderer, and a style change shows on every screen at once. The desktop
does not need a second, richer renderer to look acceptable; at desktop
sizes the pixel count grows (a 1280x800 view is 13 times the M9's), and
the host is fast enough for that by the measurement (1.25 ms for the full
240x320 view, Table "Host numbers").

## 4. Tiles

**Format.** One region file, read by all three frontends. It is a tile
pyramid of the flat i16 geometry that `leviculum-mapfill` already reads in
place (the `.lmf` fixture layout, one per tile) behind a directory index,
already cut to the per-zoom layer profile: z15 and z14 without
`buildings` and `pois`, the region zooms with `landuse` and `natural`
thinned and their geometry simplified ("Simplifications the numbers ask
for", items 1 and 2). Pre-decoded geometry is what the device measured
(177 ms full, 90 ms stripped, 31 ms flush); decoding MVT on the M9 was the
cost the measurement found to dominate, and the region file removes it.
The desktop could read MVT, but one format for all three is one thing to
test.

**Where it comes from.** `leviculum-map-tool cut` reads an OSM extract,
either an OSM PBF (Geofabrik's regional downloads) or a Protomaps
PMTiles archive, for a bounding box or a named region, and writes the
region file. Attribution and licence travel inside the file: the data is
ODbL ("© OpenStreetMap contributors"), every frontend shows the line on
the map view, and the region file's notice is what the open licence item
of handheld-messenger.md asks for.

**How a user gets one, zero config.** The 90 % rule (Lew, 2026-10-25; zero
config, #471) says the map works without a step. Decided 2026-10-27
(Lew):

1. Without any region file, the map view still works: pins on a plain
   grid with the coastlines of the world at low detail. ratatui's canvas
   carries such a world outline (`Map`, 5,125 points in its high
   resolution) for the TUI. Its source is the world file of
   gnuplotting.org, whose own licence is to be checked before the core
   copies the data for the GUI and the M9.
   Positions are useful at once, the map comes later.
2. A region is installed by one action: `loquax map add <region>` or the
   palette's `add map region`, which cuts it around a named place or
   around the positions loquax already holds, from an extract the user
   names or downloads. Nothing is downloaded without that action: a mesh
   messenger may run with no internet at all, and a download that fires
   on its own is the wrong default for a privacy tool.
3. The Debian package ships no region; a board's flash image may carry
   one cut for its deployment.

**Size per region.** Not measured. What is measured: the Protomaps sample
of Florence centre (about 5 x 5 km, z0 to z15, all layers, gzip MVT) is
6,601,156 bytes; one z15 view of it, decoded to flat geometry without
points, is 278,236 bytes for four tiles. A size per region for the cut
profile is slice (b)'s first measurement, for one city, one German
Bundesland and Germany, before the default zoom range is fixed.

## 5. Slices

In order. Each has its acceptance; where it touches a real exchange, a
phase in the 1001 cell (periculum's `regression/lnmsg_tui_chat_session.toml`,
`loquax` against the stock Python LXMF client `lxmf_node.py` on one
`lnsd`).

**(a) Positions in the model, shown as text.** The engine passes the
fields; the core decodes telemetry and stream fields into positions; the
store gains `positions` and fills `fields`; the header and the detail
view show position and age; a telemetry message is one `◎ position` line
instead of an empty block. Depends on slice 6 (history, built).
*Acceptance:* unit tests over the decode with fixtures of each producer
(a board report, Columba's location layout, a Sideband blob with a nil
sensor, a stream row, an undecodable blob kept raw); the store's migration
from version 1 with the durability tests; a `tui_snapshot` of the header
at 80 and 120 columns with a position. *1001 phase:* Bob sends a
`FIELD_TELEMETRY` message with a position and an empty body; Alice's
header shows it within 5 s with its age, and her conversation has one
position line and no empty block. The Python helper has no verb that sends
telemetry today (`lxmf_node.py` has `send` and `send_propagated`), so the
phase needs a `send_telemetry` twin of `lxmf-node`'s verb, packing a
`Telemeter` blob the way `Location.pack` does: a periculum companion order.

**(b) The map core and the region tool.** `leviculum-map` and
`leviculum-map-tool`, the region format, the size measurement of section 4,
a region of the test area committed as a small fixture. Depends on nothing
open; it can run beside (a). *Acceptance:* the cut is deterministic (same
input, byte-identical file, as `extract` is today); a host render of the
fixture region at three zooms matches pinned counts, as
`leviculum-mapfill/tests/fixture.rs` pins the fixture's; projection and
culling unit tests (antimeridian, a pin at the view's edge); a
cross-compile gate that holds `leviculum-map` to `no_std`, as `just
loquax-core-no-std` holds the core. *1001
phase:* none, no exchange.

**(c) The TUI map view.** The `map` action, the pins with names and ages,
fit, zoom and pan, the three drawing backends, the attribution line.
Depends on (a) for positions and (b) for the core. *Acceptance:* a
`tui_snapshot` of the map view in the half-block and braille backends
(graphics protocols cannot be snapshotted as cells; their choice is a unit
test over the backend ladder, as `choose_backend` has today); pins
positioned by projection tests; clicking a pin opens the conversation.
*1001 phase:* after (a)'s phase, Alice opens the map and her frame shows
Bob's pin with his announced name.

**(d) The iced GUI map.** The map view in the GUI. Depends on (b), and on
the iced frontend itself, which is not built. *Acceptance:* the GUI's own
snapshot test of the view over the fixture region. *1001 phase:* the same
as (c) once the cell can drive the GUI; until then none.

**(e) The M9 map.** The view on the panel with the region on flash.
Depends on (b), the M9's stage two (its keyboard UI, [The ThinkNode
M9](thinknode-m9.md)) and the M9 frontend of the messenger core.
*Acceptance:* the rig measurement repeated with the region file in place of
the fixture, frame and flush time against the device rows of the draft;
the orientation (MADCTL) settled. *1001 phase:* none; its exchange is a
hardware cell.

**Parallel.** (a) and (b) are independent and can run on two lanes at
once. (c) needs both. (d) and (e) need (b) and their own frontends, and
neither waits for the other.

## 6. The terminal's graphics

What the TUI can draw a map with, in the order the backend ladder tries.

**The kitty graphics protocol.** An APC escape that sends pixel data
(PNG or raw RGB) and places it on a cell rectangle; it can keep an image on
the terminal and only move it, which suits a map that redraws on a pan.
Spoken by kitty, WezTerm, Ghostty and Konsole by their own documentation,
not checked here. The crate is
`ratatui-image` 11.0.6 (MIT), already in the tree behind
`leviculum-ui-ratatui`'s `images` feature
(`leviculum-ui-ratatui/Cargo.toml:34`), built without its default
`chafa-dyn` so no C library comes in; `lnomad` draws pictures through it.
Its terminal query (`Pictures::detect`,
`leviculum-ui-ratatui/src/images.rs:109`) picks the protocol at start and
falls back to half-blocks.

**Sixel.** The DEC bitmap format, palette-based (so a map's flat colours
fit it well), redrawn whole on every change. Spoken by foot, xterm (with
`-ti vt340` or its resource), mlterm, WezTerm and Konsole by their own
documentation; only foot is checked here. `ratatui-image` encodes it through `icy_sixel` 0.5.0
(MIT OR Apache-2.0), pure Rust. Checked on the coder host: foot 1.21.0's
`foot.ini(5)` has `sixel` default `yes`, its `foot-ctlseqs(7)` lists the
kitty keyboard flags (query, push, pop, set) and no kitty graphics escape.
So a terminal that speaks the kitty keyboard protocol need not speak kitty
graphics: foot speaks the one and not the other, and draws the map by
sixel. Lew's terminal is foot (the reviewer checked on his machine,
2026-10-27), which is why the ladder must not assume kitty graphics. The
`KEYS` line in `tui.log` names the keyboard, and slice (c) adds a
`GRAPHICS backend=` line beside it, so `tui.log` names the backend on
any machine.

**Half-blocks.** `▀` with a foreground and a background colour gives two
pixels per cell, in truecolor or 256 colours: the 80 x 24 layout's
conversation pane, about 52 x 20 cells, gives a map of about 52 x 40
pixels. Coarse, but filled areas and roads
are recognisable, and every modern terminal draws it. `ratatui-image`
already falls back to it.

**Braille dots.** `⣿` and its kin give 2 x 4 dots per cell in one
colour per cell: about 104 x 80 dots in the same pane, the sharpest line
drawing a terminal without graphics can do, but no filled areas and one
colour per cell. ratatui's own `Canvas` with `Marker::Braille` draws it
(ratatui 0.30.2, MIT, in the tree), and its `Map` shape is the world
outline of section 4. It is the backend for a terminal with neither
graphics nor 256 colours, and for `NO_COLOR`: roads and coastlines as
lines, pins as text.

The ladder, decided 2026-10-27 (Lew): kitty or sixel where the query finds one, else
half-blocks in colour, else braille. It is the ladder `lnomad`'s pictures
already climb, plus braille at the bottom.

## 7. Decided

Lew, 2026-10-27, accepted the five proposals of this section as written.
Each alternative stays as one line, rejected, so the reasoning stays.

- **Decision 6: the vector path holds.** The device measurement says yes
  for the stripped profile: 90 ms to draw plus 31 ms to flush on the M9,
  177 ms plus 31 ms with buildings
  (`docs/drafts/map-measurement-2026-10.md`, "Device rows, measured
  2026-10-26"). The PNG raster fallback is dropped, and the region format
  of section 4 is the one format for the desktop and the board.
  *Rejected:* the PNG raster tiles of Meshtastic's layout, a second format
  to cut, ship and test that the measurement does not need.
- **TUI graphics: the ladder of section 6.** Kitty or sixel, then
  half-blocks, then braille. Lew's terminal is foot, which draws by sixel
  and speaks no kitty graphics, so the ladder must not assume kitty.
  *Rejected:* braille only, which needs no `ratatui-image` in loquax but
  gives no filled areas.
- **No default region in the package.** The world outline until the user
  adds one, one explicit action to add one, no download of loquax's own
  (section 4). *Rejected:* a small default region in the package, which
  would have to be chosen for someone.
- **Our own position is not sent by default.** loquax sends no position
  until the user switches it on per peer, and answers no telemetry
  request until then. A messenger that broadcasts where its user is
  because nobody changed a default is the failure this avoids; the cost
  is that two loquax users see each other on the map only after one step
  each. *Rejected:* sending by default.
- **Stale threshold 2 h** (section 2): dimmed after that, never removed
  for age alone. *Rejected:* removing a fix when it ages out.

## See also

- [A messenger whose core runs anywhere](handheld-messenger.md), "The map"
- [Telemetry](telemetry.md), the format and the collector exchange
- [A GUI toolkit for the desktop and the M9](gui-toolkit.md)
- [loquax: keys, mouse and layout](loquax-keys-and-layout.md), the slices
  this page's slices follow
- [loquax: conversation storage](loquax-storage.md)
- [The ThinkNode M9](thinknode-m9.md)
