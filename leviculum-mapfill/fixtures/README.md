# Fixtures

`firenze-z15.lmf`: the geometry of the four z15 tiles a 240x320 view
centred on Santa Maria del Fiore touches (z15/17408/11943, 17408/11944,
17409/11943, 17409/11944), cut by `measure extract` into the flat format
`src/fixture.rs` describes. Areas (earth, landuse, natural, water,
buildings) and lines (roads, transit, physical_line); point features are
not in it. 6,600 features, 55,183 vertices, 278,236 bytes.

Source: the Protomaps sample archive
`https://pmtiles.io/protomaps(vector)ODbL_firenze.pmtiles` (Protomaps
Basemap schema 3.7.1, planetiler 2024-08-01, OSM replication of
2024-08-07), the tiles fetched by HTTP range requests as
`docs/drafts/map-measurement-2026-10.md` describes.

**© OpenStreetMap contributors**, <https://www.openstreetmap.org/copyright>.
The data is available under the Open Database License (ODbL) 1.0,
<https://opendatacommons.org/licenses/odbl/1-0/>. This file is a derived
database of it and is under the same licence, not under the AGPL of the
code around it. Pictures drawn from it are produced works and carry the
same attribution.
