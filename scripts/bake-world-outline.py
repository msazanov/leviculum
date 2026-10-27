#!/usr/bin/env python3
"""Bake Natural Earth's 1:110m land polygons into the world outline table
leviculum-map draws when no region covers the view
(leviculum-map/src/outline.rs, docs/src/concepts/loquax-map.md).

Input: ne_110m_land.geojson of Natural Earth 5.1.2, public domain
(https://www.naturalearthdata.com/about/terms-of-use/), from
https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_110m_land.geojson
(sha256 9e0729ee253ca7d7a5c4ae9395fb1902264c5377c52e224d13dd85010e2835d9).

Output, all integers little-endian u16:

    "LWO1"  ring count
    per ring: point count (bit 15 set: a hole of the ring before),
              west, north, east, south of its points,
              then x, y per point

x and y are the Web Mercator world coordinate of leviculum-map's
mercator.rs shifted right by 16: 65,536 steps around the world, 611 m at
the equator. x is not wrapped, so a ring that reaches 180 E ends at 65535
instead of jumping back to 0; the drawer places a copy of the world each
side. Latitudes are clamped to the projection's edge, so Antarctica's
closing edge along 90 S lies on the bottom row. The geometry is Natural
Earth's own 1:110m simplification; this script only quantises it and drops
the repeated points the quantisation makes.

Usage: scripts/bake-world-outline.py IN.geojson OUT.lwo
"""

import json
import math
import struct
import sys

MAX_LAT = 85.0511287798


def project(lon, lat):
    x = min(65535, max(0, round((lon + 180.0) / 360.0 * 65536.0)))
    lat = max(-MAX_LAT, min(MAX_LAT, lat))
    phi = math.radians(lat)
    t = math.log(math.tan(math.pi / 4 + phi / 2))
    y = min(65535, max(0, round((1.0 - t / math.pi) / 2.0 * 65536.0)))
    return x, y


def ring_points(coords):
    out = []
    for lon, lat in coords:
        p = project(lon, lat)
        if not out or out[-1] != p:
            out.append(p)
    # GeoJSON repeats the first point last; the drawer closes rings itself.
    while len(out) > 1 and out[-1] == out[0]:
        out.pop()
    return out


def main():
    src, dst = sys.argv[1], sys.argv[2]
    with open(src) as f:
        doc = json.load(f)
    rings = []
    for feature in doc["features"]:
        geom = feature["geometry"]
        polys = [geom["coordinates"]] if geom["type"] == "Polygon" else geom["coordinates"]
        for poly in polys:
            for i, coords in enumerate(poly):
                pts = ring_points(coords)
                if len(pts) >= 3:
                    rings.append((i > 0, pts))
    body = bytearray(b"LWO1")
    body += struct.pack("<H", len(rings))
    for hole, pts in rings:
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        body += struct.pack("<H", len(pts) | (0x8000 if hole else 0))
        body += struct.pack("<HHHH", min(xs), min(ys), max(xs), max(ys))
        for x, y in pts:
            body += struct.pack("<HH", x, y)
    with open(dst, "wb") as f:
        f.write(body)
    points = sum(len(p) for _, p in rings)
    print(f"OUTLINE rings={len(rings)} points={points} bytes={len(body)}")


if __name__ == "__main__":
    main()
