#!/usr/bin/env python3
"""Build the Mexican place reference from the GeoNames MX dump.

Same builder as the United States and Canada (prep/us_geonames_reference.py): populated places with
population >= 1000 plus every capital and administrative seat, with up to ten Latin alternate names, and the
ADM1 rows of the 32 states. GeoNames numbers the states in its own order ("14"); the reference carries the
sheet's three-letter codes (prep/mx_states.py).

Output: eval/reference/mx/geonames-places.json.gz, checked in; `--check` proves it still follows from
data/geonames_mx.txt byte for byte.
"""
import argparse
from pathlib import Path

from mx_states import GEONAMES_ADMIN1_TO_STATE
from us_geonames_reference import build, encode

CODE = Path(__file__).resolve().parent.parent
DUMP = CODE / "data/geonames_mx.txt"
REFERENCE = CODE / "eval/reference/mx/geonames-places.json.gz"
REGIONS = frozenset(GEONAMES_ADMIN1_TO_STATE.values())


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dump", type=Path, default=DUMP)
    ap.add_argument("--output", type=Path, default=REFERENCE)
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args(argv)
    reference = build(args.dump, REGIONS, GEONAMES_ADMIN1_TO_STATE)
    raw = encode(reference)
    summary = (f"{reference['source']['places']} places, {len(reference['states'])} states, "
               f"{len(raw)} bytes gzip")
    if args.check:
        if not args.output.is_file() or args.output.read_bytes() != raw:
            print(f"stale or missing: {args.output}")
            return 1
        print(f"MX place reference current: {summary}")
        return 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(raw)
    print(f"MX place reference written: {summary}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
