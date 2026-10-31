#!/usr/bin/env python3
"""Generate the Canadian rule pack: common rules + English and French street types (ca_overlay.json)
+ the commune_regions table (where each city name lies in each province, from the GeoNames place
reference prep/ca_geonames_reference.py).

Same mechanism and format as the United States pack (prep/us_rules.py); rules are data, the engine
gets no per-country branch. The street-type list was chosen by counting the first and last words of
the 417 708 distinct Canadian street names of Overture 2026-10-21.0. "ste" is not a junk word here:
in Canada it opens Sainte- city names. `postal_digits` 6 / `postal_split` 3 satisfy the tail grammar
but match nothing: a Canadian postal code is alphanumeric ("K1A 0B1") and is read by
`postal_pattern` "A9A 9A9" instead (query.rs pattern_postal), which sets the code aside -- the
sheet keeps no numeric postcode for it.
Freshness: `python3 prep/ca_rules.py --check`.
"""
import argparse
from pathlib import Path

from us_rules import check_pack, expected_pack, generate_pack

CODE = Path(__file__).resolve().parent.parent
MARKER = b"# Generated Canadian additions from ca_overlay.json.\n"
REFERENCE = CODE / "eval/reference/ca/geonames-places.json.gz"


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--common", type=Path, default=CODE / "rules")
    ap.add_argument("--overlay", type=Path, default=CODE / "rules/ca_overlay.json")
    ap.add_argument("--output", type=Path, default=CODE / "rules/ca")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--reference", type=Path, default=REFERENCE)
    args = ap.parse_args(argv)
    families = len(expected_pack(args.common, args.overlay, MARKER, args.reference))
    if args.check:
        problems = check_pack(args.common, args.overlay, args.output, MARKER, args.reference)
        if problems:
            print("\n".join(problems))
            return 1
        print(f"Canadian rule pack current: {families} families")
    else:
        size = generate_pack(args.common, args.overlay, args.output, MARKER, args.reference)
        print(f"Canadian rule pack generated: {families} families, {size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
