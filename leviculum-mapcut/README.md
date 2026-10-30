# leviculum-mapcut

Cuts region files for `leviculum-map` from Protomaps PMTiles archives
(#474, `docs/src/concepts/loquax-map.md`, section 4). The one cut path:
`leviculum-map-tool cut` and loquax's map autoload (1089) both call it.

- `pmtiles`: a PMTiles v3 archive read by ranges, from a file or (feature
  `http`) a server, only the directories and tiles a box needs.
- `http`: the server side by HTTP range requests (ureq on rustls with the
  webpki roots), and the newest Protomaps daily build found by date.
- `mvt`, `simplify`, `cut`: vector tiles to the flat region format under a
  layer profile, deterministic.
- `load`: a box cut into a region under a size cap that lowers the
  deepest zoom instead of asking.
- `serve`: a loopback HTTP server with range requests, the map source of
  the tests, which never touch the network.

`std` only; the `no_std` cores (`leviculum-map`, the loquax core) never
link it.
