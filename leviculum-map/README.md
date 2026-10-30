# leviculum-map

The map core every frontend draws through (#474,
`docs/src/concepts/loquax-map.md`, sections 3 and 4). `no_std` with
`alloc`; the feature `std` adds only a `Storage` for a file.

- `region`: the region file (`LMR1`, versions 1 and 2), a tile pyramid of
  `leviculum-mapfill`'s flat geometry behind an index, opened through a
  `Storage` and searched in place; version 2 adds one string table and a
  label section per tile. Truncated or corrupt bytes are an error, never a
  panic.
- `labels`: the label sections and the string table, and the one rule
  every frontend places names by (`place`).
- `mercator`: Web Mercator between degrees x 10^7 and a `u32` world
  coordinate (2^32 units around the world, 9.3 mm at the equator).
- `viewport`: centre, zoom, size; pan, fit to points, the screen position
  of a world point, and the tiles a view needs with their placement. x
  wraps across the antimeridian.
- `profile`: the layer profile per zoom the cut applied (profile 1).
- `pins`: pins and labels as data, placed in a view, and a hit test.
- `draw`: a view's tiles drawn class by class across the tiles into any
  `DrawTarget`, through `leviculum-mapfill`.
- `style`: `leviculum-mapfill`'s colour tables, re-exported.

```rust
let region = Region::from_bytes(bytes)?;          // or Region::open(file)
let view = Viewport::new(project(LatLon::from_e6(lat, lon)), 15, 240, 320);
let tiles = region.view(&view)?;                   // or read_view + draw_tiles
let stats = draw(&tiles, Profile::Full, &mut frame, &mut scratch, &mut work, &mut NoClock)?;
let pins = place(&view, &[Pin { at, label: "Bob", stale: false }]);
```

Region files are made by `leviculum-map-tool cut`. `just map-core-no-std`
holds the crate to `no_std` on the firmware triple.

## Data

`fixtures/firenze.lmr` is OpenStreetMap data, see `fixtures/README.md`.
