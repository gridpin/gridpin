"""Captions of place for a folder of photos, offline: the GPS point in each photo's EXIF, the nearest address on a
GridPin sheet, and a CSV beside the photos (and, if asked, an .xmp sidecar per photo).

Nothing leaves the machine, and the photos are never written to: the captions go into new files next to them.

    gridpin-photos ~/Pictures/2026-paris --sheet fr          # writes ~/Pictures/2026-paris/gridpin_places.csv
"""
from __future__ import annotations

import argparse
import csv
import html
import os
import sys
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Sequence, Tuple

import exifread
import gridpin

__all__ = ["COLUMNS", "STATUSES", "caption", "photo_point", "run"]

# Formats exifread reads a GPS block from (checked on real files: see the package README).
EXTENSIONS = {".jpg", ".jpeg", ".tif", ".tiff", ".heic", ".heif", ".png", ".webp"}
# Beyond this distance from the nearest house the photo was not taken on that street: the caption names the place only.
MAX_STREET_M = 150.0
COLUMNS = ("file", "lat", "lon", "status", "label", "street", "housenumber", "place", "region", "postcode", "distance_m")
STATUSES = {
    "ok": "the nearest address is close enough to name the street",
    "place_only": "the nearest house is farther than the limit, or the answer is weak: only the place is named",
    "no_geotag": "the photo has no GPS position",
    "null_island": "the GPS position is 0, 0: a camera without a fix writes that",
    "no_address": "no address of the sheet near the point (another country, sea, wilderness)",
    "unreadable": "the file could not be read as a photo",
}


def _degrees(values: Any, ref: Any) -> Optional[float]:
    """An EXIF GPS value (three rationals: degrees, minutes, seconds) and its N/S/E/W reference as decimal degrees."""
    try:
        parts = [float(v.num) / float(v.den) if v.den else float("nan") for v in values.values]
        if len(parts) != 3 or any(p != p for p in parts):
            return None
        deg = parts[0] + parts[1] / 60 + parts[2] / 3600
    except (AttributeError, TypeError, ValueError, ZeroDivisionError):
        return None
    sign = -1 if str(ref).strip().upper() in ("S", "W") else 1
    return sign * deg


def photo_point(path: Path) -> Tuple[str, Optional[Tuple[float, float]]]:
    """("ok", (lat, lon)), or a status from STATUSES and None."""
    try:
        with path.open("rb") as f:
            tags = exifread.process_file(f, details=False, extract_thumbnail=False)
    except Exception:  # noqa: BLE001 - any parse error of a broken file is the same verdict
        return "unreadable", None
    lat_t, lon_t = tags.get("GPS GPSLatitude"), tags.get("GPS GPSLongitude")
    if lat_t is None or lon_t is None:
        return "no_geotag", None
    lat = _degrees(lat_t, tags.get("GPS GPSLatitudeRef", "N"))
    lon = _degrees(lon_t, tags.get("GPS GPSLongitudeRef", "E"))
    if lat is None or lon is None or abs(lat) > 90 or abs(lon) > 180:
        return "no_geotag", None
    if abs(lat) < 1e-6 and abs(lon) < 1e-6:
        return "null_island", None
    return "ok", (lat, lon)


def caption(hit: Optional[Dict[str, Any]], max_street_m: float = MAX_STREET_M) -> Dict[str, Any]:
    """The caption cells of one answer of `reverse`: the street only when the house is near and the answer not weak."""
    if not hit:
        return {"status": "no_address"}
    distance = hit.get("distance_m")
    near = distance is not None and distance <= max_street_m and not hit.get("weak")
    place, region = hit.get("commune") or "", hit.get("region") or ""
    street = (hit.get("street") or "") if near else ""
    number = (hit.get("housenumber") or "") if near else ""
    where = ", ".join(p for p in (" ".join(p for p in (number, street) if p), place, region) if p)
    return {"status": "ok" if near else "place_only", "label": where, "street": street, "housenumber": number,
            "place": place, "region": region, "postcode": (hit.get("postcode") or "") if near else "",
            "distance_m": "" if distance is None else f"{distance:.0f}"}


def _xmp(row: Dict[str, Any]) -> str:
    """An XMP sidecar with the place in the IPTC and Photoshop fields photo managers read."""
    e = lambda s: html.escape(str(s or ""), quote=True)  # noqa: E731
    return ('<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>\n'
            '<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">\n'
            '<rdf:Description rdf:about="" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"\n'
            ' xmlns:Iptc4xmpCore="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/"\n'
            f' photoshop:City="{e(row.get("place"))}" photoshop:State="{e(row.get("region"))}"\n'
            f' Iptc4xmpCore:Location="{e(" ".join(p for p in (row.get("housenumber"), row.get("street")) if p))}"/>\n'
            '</rdf:RDF></x:xmpmeta>\n<?xpacket end="w"?>\n')


def photos(folder: Path, recursive: bool) -> List[Path]:
    walk = folder.rglob("*") if recursive else folder.glob("*")
    return sorted(p for p in walk if p.is_file() and p.suffix.lower() in EXTENSIONS)


def run(folder: Path, sheet: str, out: Optional[Path] = None, xmp: bool = False, recursive: bool = False,
        max_street_m: float = MAX_STREET_M) -> Dict[str, int]:
    """Caption every photo of `folder`; returns how many got each status. Photos are only read."""
    g = gridpin.Geocoder(str(gridpin.sheets.open_path(sheet)))
    rows: List[Dict[str, Any]] = []
    for path in photos(folder, recursive):
        status, point = photo_point(path)
        row: Dict[str, Any] = {"file": str(path.relative_to(folder)), "status": status}
        if point:
            row.update(lat=f"{point[0]:.6f}", lon=f"{point[1]:.6f}")
            hits = g.reverse(point[0], point[1], 1)
            row.update(caption(hits[0] if hits else None, max_street_m))
        rows.append(row)
        if xmp and row["status"] in ("ok", "place_only"):
            sidecar = path.with_name(path.name + ".xmp")
            if not sidecar.exists():  # never overwrite a sidecar a photo manager wrote
                sidecar.write_text(_xmp(row), encoding="utf-8")
    target = out or folder / "gridpin_places.csv"
    tmp = target.with_name(f".{target.name}.{os.getpid()}.part")
    with tmp.open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=COLUMNS, extrasaction="ignore", lineterminator="\n")
        w.writeheader()
        w.writerows(rows)
    os.replace(tmp, target)
    counts: Dict[str, int] = {s: 0 for s in STATUSES}
    for r in rows:
        counts[r["status"]] += 1
    return counts


def main(argv: Optional[Iterable[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="gridpin-photos", description=__doc__.splitlines()[0])
    p.add_argument("folder", type=Path)
    p.add_argument("--sheet", required=True, help='a country code ("fr", downloaded once) or the path of a .bin file')
    p.add_argument("--out", type=Path, help="the CSV to write (default: gridpin_places.csv in the folder)")
    p.add_argument("--xmp", action="store_true", help="also write <photo>.xmp beside each placed photo (never over one)")
    p.add_argument("--recursive", action="store_true", help="also the photos of the subfolders")
    p.add_argument("--max-street-m", type=float, default=MAX_STREET_M,
                   help="name the street only when the nearest house is this close (default %(default)s m)")
    a = p.parse_args(list(argv) if argv is not None else None)
    if not a.folder.is_dir():
        p.error(f"{a.folder} is not a folder")
    counts = run(a.folder, a.sheet, a.out, a.xmp, a.recursive, a.max_street_m)
    total = sum(counts.values())
    print(f"{total} photos: " + ", ".join(f"{n} {s}" for s, n in counts.items() if n), file=sys.stderr)
    return 0


def _entry() -> None:  # console script
    sys.exit(main())


def describe_statuses() -> Sequence[str]:
    return [f"{k}: {v}" for k, v in STATUSES.items()]
