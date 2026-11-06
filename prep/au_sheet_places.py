#!/usr/bin/env python3
"""Every locality of the Australian sheet with its state, added to the GeoNames place reference.

GeoNames knows about a quarter of the localities of G-NAF (3 697 names of 14 994), and the engine checks a
city against its state only for the names in the table: "Old Eyre Hwy, Cook SA" answered Cook in the capital
territory, 1 800 km away. The source carries no state, but the postal code gives it (Australia Post ranges).
One entry per locality id: its name, the state of most of its addresses and the mean of their points.

Input: data/au_norm.parquet. Output: eval/reference/au/places.json.gz, checked in; `--check` proves it still
follows from both inputs when the parquet is on disk.
"""
import argparse
import gzip
import json
import sys
from pathlib import Path

CODE = Path(__file__).resolve().parents[1]
GEONAMES = CODE / "eval/reference/au/geonames-places.json.gz"
NORM = CODE / "data/au_norm.parquet"
OUTPUT = CODE / "eval/reference/au/places.json.gz"

# (low, high, state) by postal code; the first range that holds a code decides. Australia Post, the postcode
# ranges of the states and territories; the external territories (OT) stand first, the capital's enclaves next.
RANGES = (
    (6798, 6799, "OT"), (2899, 2899, "OT"), (2540, 2540, "OT"),
    (200, 299, "ACT"), (2600, 2618, "ACT"), (2900, 2920, "ACT"),
    (800, 999, "NT"),
    (1000, 2999, "NSW"),
    (3000, 3999, "VIC"), (8000, 8999, "VIC"),
    (4000, 4999, "QLD"), (9000, 9999, "QLD"),
    (5000, 5999, "SA"),
    (6000, 6999, "WA"),
    (7000, 7999, "TAS"),
)


def state_of(code):
    """The state of a postal code, or None when no range holds it."""
    if not code.isdigit():
        return None
    number = int(code)
    return next((state for low, high, state in RANGES if low <= number <= high), None)


def sheet_places(norm):
    import duckdb

    con = duckdb.connect()
    con.execute("SET enable_progress_bar=false")
    rows = con.execute(
        """select code_insee, min(nom_commune) AS place, code_postal, count(*) AS n, avg(lat) AS lat, avg(lon) AS lon
           from read_parquet(?) where code_insee <> '' and code_postal <> ''
           group by code_insee, code_postal""",
        [str(norm)],
    ).fetchall()
    by_locality = {}
    for locality, name, code, count, lat, lon in rows:
        state = state_of(code)
        if state is None:
            continue
        entry = by_locality.setdefault(locality, {"name": name, "states": {}, "n": 0, "lat": 0.0, "lon": 0.0})
        entry["states"][state] = entry["states"].get(state, 0) + count
        entry["lat"] += lat * count
        entry["lon"] += lon * count
        entry["n"] += count
    places = []
    for locality in sorted(by_locality):
        entry = by_locality[locality]
        state = min(entry["states"], key=lambda s: (-entry["states"][s], s))
        places.append({
            "name": entry["name"], "asciiname": entry["name"], "admin1": state,
            "lat": round(entry["lat"] / entry["n"], 4), "lon": round(entry["lon"] / entry["n"], 4),
            "pop": 0, "alternates": [],
        })
    return places


def build(geonames, norm):
    document = json.loads(gzip.decompress(geonames.read_bytes()))
    document["places"] = document["places"] + sheet_places(norm)
    return document


def encode(document):
    raw = json.dumps(document, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return gzip.compress(raw, 9, mtime=0)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--geonames", type=Path, default=GEONAMES)
    ap.add_argument("--norm", type=Path, default=NORM)
    ap.add_argument("--output", type=Path, default=OUTPUT)
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args(argv)
    raw = encode(build(args.geonames, args.norm))
    if args.check:
        if not args.output.is_file() or args.output.read_bytes() != raw:
            print(f"stale or missing: {args.output}")
            return 1
        print("AU places current")
        return 0
    args.output.write_bytes(raw)
    print(f"AU places written: {len(raw)} bytes gzip")
    return 0


if __name__ == "__main__":
    sys.exit(main())
