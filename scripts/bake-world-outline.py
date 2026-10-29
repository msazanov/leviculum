#!/usr/bin/env python3
"""Bake Natural Earth's 1:50m land polygons and land boundary lines into
the world outline tables leviculum-map draws when no region covers the view
(leviculum-map/src/outline.rs, docs/src/concepts/loquax-map.md).

Input: ne_50m_land.geojson or ne_50m_admin_0_boundary_lines_land.geojson
of Natural Earth 5.1.2, public domain
(https://www.naturalearthdata.com/about/terms-of-use/), from
https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/
(sources and checksums in leviculum-map/fixtures/README.md). Polygons make
a table of rings, "LWO2"; line strings a table of lines, "LWL2", in the
same format, whose hole flag is always clear and whose last point is not
joined back to its first.

A point is the Web Mercator world coordinate of leviculum-map's
mercator.rs shifted right by 32 - BITS: with BITS = 18, 262,144 steps
around the world, 153 m at the equator and one pixel at zoom 10, the
outline's deepest, so a coast is never off by more than half a pixel there.
x is not wrapped, so a ring that reaches 180 E ends at the last step
instead of jumping back to 0; the drawer places a copy of the world each
side. Latitudes are clamped to the projection's edge, so Antarctica's
closing edge along 90 S lies on the bottom row. The geometry is Natural
Earth's own 1:50m simplification; this script only quantises it and drops
the repeated points the quantisation makes.

Output, "uv" an unsigned LEB128 varint:

    "LWO2" or "LWL2"  u8 BITS  uv ring or line count
    per ring: uv point count * 2 + (1 for a hole of the ring before)
              uv byte length of its deltas
              uv x, uv y of its first point
              uv x - west, uv y - north, uv east - x, uv south - y
              u8 k
              then per further point its dx and dy, zigzagged, k bits
              each, least significant bit first; a value of k ones is an
              escape and BITS + 1 bits with the value follow

k is chosen per ring for the fewest bits, the smallest k on a tie, so a
ring of short steps packs tight and one long step costs only its escape.
Baking the same input again gives the same bytes.

Usage: scripts/bake-world-outline.py IN.geojson OUT.lwo (or OUT.lwl for lines)
"""

import json
import math
import sys

BITS = 18
MAX_LAT = 85.0511287798


def project(lon, lat):
    n = 1 << BITS
    x = min(n - 1, max(0, round((lon + 180.0) / 360.0 * n)))
    lat = max(-MAX_LAT, min(MAX_LAT, lat))
    phi = math.radians(lat)
    t = math.log(math.tan(math.pi / 4 + phi / 2))
    y = min(n - 1, max(0, round((1.0 - t / math.pi) / 2.0 * n)))
    return x, y


def line_points(coords):
    out = []
    for lon, lat in coords:
        p = project(lon, lat)
        if not out or out[-1] != p:
            out.append(p)
    return out


def ring_points(coords):
    out = line_points(coords)
    # GeoJSON repeats the first point last; the drawer closes rings itself.
    while len(out) > 1 and out[-1] == out[0]:
        out.pop()
    return out


def uv(v):
    out = bytearray()
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return out


def zigzag(v):
    return v << 1 if v >= 0 else ((-v) << 1) - 1


def cost(values, k):
    esc = (1 << k) - 1
    return sum(k if v < esc else k + BITS + 1 for v in values)


def pack(values, k):
    esc = (1 << k) - 1
    acc, n, out = 0, 0, bytearray()

    def put(v, width):
        nonlocal acc, n
        acc |= v << n
        n += width
        while n >= 8:
            out.append(acc & 0xFF)
            acc >>= 8
            n -= 8

    for v in values:
        if v < esc:
            put(v, k)
        else:
            put(esc, k)
            put(v, BITS + 1)
    if n:
        out.append(acc & 0xFF)
    return out


def ring_bytes(hole, pts):
    values = []
    for (ax, ay), (bx, by) in zip(pts, pts[1:]):
        values += [zigzag(bx - ax), zigzag(by - ay)]
    k = min(range(1, BITS + 2), key=lambda k: (cost(values, k), k))
    deltas = pack(values, k)
    xs = [p[0] for p in pts]
    ys = [p[1] for p in pts]
    x, y = pts[0]
    out = bytearray()
    out += uv(len(pts) * 2 + (1 if hole else 0))
    out += uv(len(deltas))
    out += uv(x) + uv(y)
    out += uv(x - min(xs)) + uv(y - min(ys)) + uv(max(xs) - x) + uv(max(ys) - y)
    out.append(k)
    out += deltas
    return out


def main():
    src, dst = sys.argv[1], sys.argv[2]
    with open(src) as f:
        doc = json.load(f)
    rings = []
    magic = None
    for feature in doc["features"]:
        geom = feature["geometry"]
        kind = geom["type"]
        lines = kind in ("LineString", "MultiLineString")
        this = b"LWL2" if lines else b"LWO2"
        if magic not in (None, this):
            sys.exit(f"{src}: lines and polygons in one input")
        magic = this
        if lines:
            parts = [geom["coordinates"]] if kind == "LineString" else geom["coordinates"]
            for coords in parts:
                pts = line_points(coords)
                if len(pts) >= 2:
                    rings.append((False, pts))
            continue
        polys = [geom["coordinates"]] if kind == "Polygon" else geom["coordinates"]
        for poly in polys:
            for i, coords in enumerate(poly):
                pts = ring_points(coords)
                if len(pts) >= 3:
                    rings.append((i > 0, pts))
    body = bytearray(magic or b"LWO2")
    body.append(BITS)
    body += uv(len(rings))
    for hole, pts in rings:
        body += ring_bytes(hole, pts)
    with open(dst, "wb") as f:
        f.write(body)
    points = sum(len(p) for _, p in rings)
    holes = sum(1 for h, _ in rings if h)
    name = "lines" if magic == b"LWL2" else "rings"
    print(f"OUTLINE {name}={len(rings)} holes={holes} points={points} bytes={len(body)}")


if __name__ == "__main__":
    main()
