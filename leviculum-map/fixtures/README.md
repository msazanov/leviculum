# Fixtures

`firenze.lmr`: a region file (`LMR1`, layer profile 1) of central
Florence, zooms 11 to 15, six tiles (z15/17408/11943 and z15/17409/11943,
and the one tile above them at each of z14 to z11), 187,654 bytes. Cut with

```sh
leviculum-map-tool cut -o firenze.lmr firenze.pmtiles \
    --bbox 11.2505,43.7728,11.2700,43.7785 --min-zoom 11 --max-zoom 15
```

from the Protomaps sample archive
`https://pmtiles.io/protomaps(vector)ODbL_firenze.pmtiles` (6,601,156
bytes, sha256 `7190f3d807a62f4f012b574007c96b809f6842f45a6b0c508639331fc68fd30a`;
Protomaps Basemap schema 3.7.1, planetiler 2024-08-01, OSM replication of
2024-08-07). Re-cutting gives the same bytes.

**© OpenStreetMap contributors**, <https://www.openstreetmap.org/copyright>.
The data is available under the Open Database License (ODbL) 1.0,
<https://opendatacommons.org/licenses/odbl/1-0/>. This file is a derived
database of it and is under the same licence, not under the AGPL of the
code around it; it carries this notice inside itself as well. Pictures
drawn from it are produced works and carry the same attribution.

## The world outline

`../data/ne_50m_land.lwo` is not a test fixture but the table the crate
draws where no region covers the view (`src/outline.rs`): Natural Earth's
1:50m land polygons, version 5.1.2, 1,422 rings and 59,239 points in
159,357 bytes. Baked with

```sh
scripts/bake-world-outline.py ne_50m_land.geojson leviculum-map/data/ne_50m_land.lwo
```

from
`https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_50m_land.geojson`
(1,636,166 bytes, sha256
`e874b27a51d146452be360cafb3cc50c86001074a67d534113e6534682f9826b`). Baking
again gives the same bytes (sha256
`a154e60ef141e605a707441bb11215efcb97b5a013da08acc8ddfca90c0e2579`).

**Natural Earth** is in the public domain
(<https://www.naturalearthdata.com/about/terms-of-use/>); no permission is
needed and none is claimed. The map names it on the view anyway, as the
attribution line "Natural Earth", the way it names OpenStreetMap over a
region. Chosen over the world file ratatui's `Map` shape draws, whose
licence is unsettled (`docs/src/concepts/loquax-map.md`, section 7).
