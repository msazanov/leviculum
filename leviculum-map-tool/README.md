# leviculum-map-tool

Makes region files for `leviculum-map` (#474,
`docs/src/concepts/loquax-map.md`, section 4). A desktop tool; no package
ships it.

```sh
# A box of the Protomaps daily build, with go-pmtiles' `pmtiles extract`:
pmtiles extract https://build.protomaps.com/20261026.pmtiles city.pmtiles \
    --bbox=8.47,50.01,8.80,50.23 --maxzoom=15
leviculum-map-tool cut -o city.lmr city.pmtiles --bbox 8.47,50.01,8.80,50.23
leviculum-map-tool info city.lmr
leviculum-map-tool render city.lmr view.ppm --lat 50.1109 --lon 8.6821 --zoom 15
```

`cut` takes several archives as one source, for a region fetched in parts
of one build. Its output depends on the input alone, not on the thread
count. It prints one `MAPTOOL zoom=` line per zoom (tiles, bytes, features
and vertices in and out) and a `MAPTOOL cut` total, then checks the file it
wrote.

The MVT decoder (`mvt`) is the tree's only one; the measurement harness
`leviculum-mapfill/measure` reads its tiles through it.
