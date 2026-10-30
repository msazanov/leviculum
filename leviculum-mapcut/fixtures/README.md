# Fixtures

`oltrarno.pmtiles`: a PMTiles v3 archive of six tiles of the Protomaps
sample of Florence, the map source the autoload's tests serve on a
loopback port (1089): z15 17409/11946, 17409/11947, 17410/11946,
17410/11947 and the two z14 tiles above them, south of the Arno, picked
among the sample's smallest so the file stays small (67,158 bytes, sha256
`80cdb27334e8d93fe0e00190c777d814076626b6c60ce0fa3522934c07c1aff0`). The
tiles are the sample's bytes, gzip MVT, untouched; the metadata keeps only
its text fields. Made with

```sh
leviculum-map-tool extract -o oltrarno.pmtiles firenze.pmtiles \
    --bbox 11.2615,43.7405,11.2825,43.7545 --min-zoom 14 --max-zoom 15
```

from `https://pmtiles.io/protomaps(vector)ODbL_firenze.pmtiles`
(6,601,156 bytes, sha256
`7190f3d807a62f4f012b574007c96b809f6842f45a6b0c508639331fc68fd30a`; see
`leviculum-map/fixtures/README.md`). Extracting again gives the same
bytes.

**© OpenStreetMap contributors**, <https://www.openstreetmap.org/copyright>.
The data is available under the Open Database License (ODbL) 1.0,
<https://opendatacommons.org/licenses/odbl/1-0/>. This file is a derived
database of it and is under the same licence, not under the AGPL of the
code around it.
