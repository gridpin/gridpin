#!/usr/bin/env python3
"""Generate the New Zealand rule pack: common rules + English street types and the NZ Post abbreviations
(nz_overlay.json) + the commune_regions table (where each place name lies in each region, from the GeoNames place
reference prep/nz_geonames_reference.py).

Same mechanism and format as the United States and Australian packs (prep/us_rules.py, prep/au_rules.py); rules are
data, the engine gets no per-country branch. The street-type list was chosen by counting the last words of the 2.43 M
New Zealand street names of Overture 2026-10-21.0 (types seen at least 50 times): the source writes every type in full,
people abbreviate it. A postal code has four digits and is not in the source (LINZ NZ Addresses), so the sheet keeps
none; the closing four digits of a query are still read as the code, not as a second house number (ANSWERS §364).
The first sheet reads no region name in the query: `regions` holds the ISO codes only, the form is `comma`, and there
are no `region_names`, so a city named in the tail is never dropped as a region word (plan §5, nz_plan_20261117).
Freshness: `python3 prep/nz_rules.py --check`.
"""
import argparse
from pathlib import Path

from us_rules import check_pack, expected_pack, generate_pack

CODE = Path(__file__).resolve().parent.parent
MARKER = b"# Generated New Zealand additions from nz_overlay.json.\n"
REFERENCE = CODE / "eval/reference/nz/geonames-places.json.gz"  # GeoNames places (prep/nz_geonames_reference.py)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--common", type=Path, default=CODE / "rules")
    ap.add_argument("--overlay", type=Path, default=CODE / "rules/nz_overlay.json")
    ap.add_argument("--output", type=Path, default=CODE / "rules/nz")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--reference", type=Path, default=REFERENCE)
    args = ap.parse_args(argv)
    families = len(expected_pack(args.common, args.overlay, MARKER, args.reference))
    if args.check:
        problems = check_pack(args.common, args.overlay, args.output, MARKER, args.reference)
        if problems:
            print("\n".join(problems))
            return 1
        print(f"New Zealand rule pack current: {families} families")
    else:
        size = generate_pack(args.common, args.overlay, args.output, MARKER, args.reference)
        print(f"New Zealand rule pack generated: {families} families, {size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
