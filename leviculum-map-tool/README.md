# leviculum-map-tool

Makes region files for `leviculum-map` (#474,
`docs/src/concepts/loquax-map.md`, section 4). A desktop tool; no package
ships it.

```sh
# A box of the Protomaps daily build, read by HTTP range requests:
leviculum-map-tool cut -o city.lmr https://build.protomaps.com/20261028.pmtiles \
    --bbox 8.47,50.01,8.80,50.23
# or from an archive on disk (go-pmtiles' `pmtiles extract` makes one):
leviculum-map-tool cut -o city.lmr city.pmtiles --bbox 8.47,50.01,8.80,50.23
# a box of an archive as a smaller archive, tiles untouched (test fixtures):
leviculum-map-tool extract -o small.pmtiles city.pmtiles --bbox 8.67,50.10,8.69,50.12
# names in German beside the local ones where they differ:
leviculum-map-tool cut -o city.lmr city.pmtiles --bbox 8.47,50.01,8.80,50.23 --locale de
leviculum-map-tool info city.lmr
leviculum-map-tool labels city.lmr --zoom 15
leviculum-map-tool render city.lmr view.ppm --lat 50.1109 --lon 8.6821 --zoom 15
```

`cut` takes several archives as one source, for a region fetched in parts
of one build. Its output depends on the input alone, not on the thread
count. It prints one `MAPTOOL zoom=` line per zoom (tiles, bytes, features
and vertices in and out, point and line labels and their bytes) and a
`MAPTOOL cut` total with the string table, then checks the file it wrote.
It writes version 2 (LMR2), with the names of places, waters, parks,
rivers and streets; `labels` prints those of one zoom, a line each.

The cut itself, the PMTiles reader and the MVT decoder live in
`leviculum-mapcut`, which loquax's map autoload calls as well (1089); this
crate is the command line over it. The MVT decoder is the tree's only one;
the measurement harness `leviculum-mapfill/measure` reads its tiles
through it.
