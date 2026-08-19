#!/usr/bin/env python3
"""Who's on First (WOF) admin data -> compact CSV + binary of region polygons (CC0).

Downloads the whosonfirst-data-admin-<cc> GitHub tarball (a few MB per country). Writes:
  1. data/wof/regions.csv — name/type/centroid/parent of regions and localities (reference).
  2. data/<cc>_admin.bin — region (admin1) POLYGONS for reverse point-in-polygon lookup:
     coordinate -> administrative region (ray casting). Loaded alongside <cc>.bin in reverse mode.

Note: WOF localities are suitable for region anchoring and reverse PIP, NOT for commune
assignment — feeding them into the `places` pool of osm.py degrades address matching.

admin.bin format (LE): "WOFA" + u32 n_reg; per region: u8 len + name; bbox i32×4 (min/max
lat,lon ×1e7); u16 n_rings; per ring: u16 n_pts + (i32 lat, i32 lon)×n_pts.

Usage: python3 prep/wof.py uz rs
"""
import csv
import glob
import json
import os
import pathlib
import struct
import sys
import tarfile
import tempfile
import urllib.request

CODE = pathlib.Path(__file__).resolve().parent.parent
TARBALL = "https://github.com/whosonfirst-data/whosonfirst-data-admin-{cc}/archive/refs/heads/master.tar.gz"
SIMPLIFY = 0.002  # ~200 m: region geometries are coarse anyway; this only guards against heavy ones


def rings_of(geom) -> list:
    """Region geometry -> list of rings [(lat,lon),...] (exteriors + holes, even-odd PIP)."""
    from shapely.geometry import shape  # lazy: only the geometry path needs shapely

    g = shape(geom).simplify(SIMPLIFY, preserve_topology=True)
    polys = list(g.geoms) if g.geom_type == "MultiPolygon" else [g]
    out = []
    for poly in polys:
        for ring in [poly.exterior, *poly.interiors]:
            out.append([(y, x) for x, y in ring.coords])  # geojson [lon,lat] → (lat,lon)
    return out


def _download_capped(url: str, dest: str, max_bytes: int = 4 * 1024**3) -> None:
    """Download `url` to `dest`, aborting if it exceeds `max_bytes`: urlretrieve has no
    size limit, so a compromised/redirected host could stream an unbounded file to disk."""
    total = 0
    with urllib.request.urlopen(url) as resp, open(dest, "wb") as out:  # noqa: S310 (fixed https URL)
        while True:
            chunk = resp.read(1 << 16)
            if not chunk:
                break
            total += len(chunk)
            if total > max_bytes:
                raise ValueError(f"download {url} exceeds {max_bytes} bytes (download cap)")
            out.write(chunk)


def _safe_extract(tgz_path: str, dest: str, max_bytes: int = 2 * 1024**3,
                  max_members: int = 200_000) -> None:
    """Extract a downloaded tarball SAFELY. `extractall` on an untrusted archive is
    unsafe: a member named `../x` or `/etc/x` writes OUTSIDE `dest` (path traversal / "tar slip"), a
    tiny archive can declare huge members (size bomb), OR carry a huge NUMBER of (even zero-size)
    entries (member/inode bomb, and getmembers() buffering them all is itself memory amplification).
    Guard all three: every member resolves inside `dest`, only regular files/dirs are extracted (no
    symlinks/devices), the cumulative declared size is capped, and the member COUNT is capped while
    iterating LAZILY (via the streaming iterator, not getmembers())."""
    dest = os.path.realpath(dest)
    total = 0
    with tarfile.open(tgz_path) as tf:
        safe = []
        for count, m in enumerate(tf, start=1):  # streaming iterator — does NOT buffer all members
            if count > max_members:
                raise ValueError(f"tarball has more than {max_members} entries (member-count bomb)")
            target = os.path.realpath(os.path.join(dest, m.name))
            if target != dest and not target.startswith(dest + os.sep):
                raise ValueError(f"tar member escapes dest (tar slip): {m.name!r}")
            if m.isfile():
                total += m.size
                if total > max_bytes:
                    raise ValueError(f"tarball exceeds {max_bytes} bytes decompressed (bomb guard)")
                safe.append(m)
            elif m.isdir():
                safe.append(m)
            # skip symlinks/hardlinks/devices
        tf.extractall(dest, members=safe)


def extract_country(cc: str, csv_rows: list) -> bytes:
    regions = []  # (name, rings)
    with tempfile.TemporaryDirectory() as tmp:
        tgz = os.path.join(tmp, f"{cc}.tar.gz")
        _download_capped(TARBALL.format(cc=cc), tgz)
        _safe_extract(tgz, tmp)
        for f in glob.glob(f"{tmp}/**/*.geojson", recursive=True):
            try:
                d = json.load(open(f))
            except Exception:
                continue
            p = d.get("properties", {})
            t, nm = p.get("wof:placetype"), p.get("wof:name")
            la, lo = p.get("geom:latitude"), p.get("geom:longitude")
            if t in ("region", "locality") and nm and la is not None and lo is not None:
                h = (p.get("wof:hierarchy") or [{}])[0]
                csv_rows.append((cc.upper(), t, nm, round(la, 6), round(lo, 6), h.get("region_id", "")))
            if t == "region" and nm and d.get("geometry"):
                try:
                    regions.append((nm, rings_of(d["geometry"])))
                except Exception:
                    pass
    # polygon binary
    buf = b"WOFA" + struct.pack("<I", len(regions))
    npts = 0
    for name, rings in regions:
        nb = name.encode("utf-8")[:255]
        pts = [pt for r in rings for pt in r]
        lats = [a for a, _ in pts] or [0]
        lons = [b for _, b in pts] or [0]
        buf += struct.pack("<B", len(nb)) + nb
        buf += struct.pack("<iiii", int(min(lats) * 1e7), int(min(lons) * 1e7),
                           int(max(lats) * 1e7), int(max(lons) * 1e7))
        buf += struct.pack("<H", len(rings))
        for r in rings:
            buf += struct.pack("<H", len(r))
            for la2, lo2 in r:
                buf += struct.pack("<ii", int(la2 * 1e7), int(lo2 * 1e7))
            npts += len(r)
    (CODE / "data" / f"{cc.lower()}_admin.bin").write_bytes(buf)
    print(f"  {cc.upper()}: {len(regions)} region polygons, {npts} points, {len(buf)/1e3:.0f} KB → {cc.lower()}_admin.bin")
    return buf


def main() -> None:
    ccs = [a.lower() for a in sys.argv[1:]] or ["uz", "rs"]
    out = CODE / "data" / "wof"
    out.mkdir(parents=True, exist_ok=True)
    csv_rows: list = []
    for cc in ccs:
        extract_country(cc, csv_rows)
    path = out / "regions.csv"
    with path.open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["cc", "placetype", "name", "lat", "lon", "region_id"])
        w.writerows(csv_rows)
    print(f"{path}: {len(csv_rows)} rows")


if __name__ == "__main__":
    main()
