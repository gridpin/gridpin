#!/usr/bin/env python3
"""Build the Polish place reference from the GeoNames PL dump.

Same builder as the United States, Canada, Mexico and Australia (prep/us_geonames_reference.py): populated places with
population >= 1000 plus every capital and administrative seat, with up to ten Latin alternate names, and the
ADM1 rows of the sixteen voivodeships. GeoNames numbers the voivodeships 72-87; the reference carries the
sheet codes (prep/pl_voivodeships.py).

Output: eval/reference/pl/geonames-places.json.gz, checked in; `--check` proves it still follows from
data/geonames_pl.txt byte for byte.
"""
import argparse
from pathlib import Path

from pl_voivodeships import GEONAMES_ADMIN1_TO_VOIVODESHIP as GEONAMES_ADMIN1_TO_STATE
from us_geonames_reference import build, encode

CODE = Path(__file__).resolve().parent.parent
DUMP = CODE / "data/geonames_pl.txt"
REFERENCE = CODE / "eval/reference/pl/geonames-places.json.gz"
REGIONS = frozenset(GEONAMES_ADMIN1_TO_STATE.values())


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dump", type=Path, default=DUMP)
    ap.add_argument("--output", type=Path, default=REFERENCE)
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args(argv)
    reference = build(args.dump, REGIONS, GEONAMES_ADMIN1_TO_STATE)
    raw = encode(reference)
    summary = (f"{reference['source']['places']} places, {len(reference['states'])} voivodeships, "
               f"{len(raw)} bytes gzip")
    if args.check:
        if not args.output.is_file() or args.output.read_bytes() != raw:
            print(f"stale or missing: {args.output}")
            return 1
        print(f"PL place reference current: {summary}")
        return 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(raw)
    print(f"PL place reference written: {summary}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
