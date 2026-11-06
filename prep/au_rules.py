#!/usr/bin/env python3
"""Generate the Australian rule pack: common rules + English street types and the Australia Post abbreviations
(au_overlay.json) + the commune_regions table (where each locality name lies in each state, from the GeoNames place
reference prep/au_geonames_reference.py).

Same mechanism and format as the United States pack (prep/us_rules.py); rules are data, the engine gets no per-country
branch. The street-type list was chosen by counting the last words of the 15.7 M Australian street names of Overture
2026-10-21.0: the source writes every type in full, people abbreviate it. A postal code has four digits, and the
Northern Territory ones start with a zero that people drop ("Darwin NT 800"): `postal_pad_zero`.
Freshness: `python3 prep/au_rules.py --check`.
"""
import argparse
from pathlib import Path

from us_rules import check_pack, expected_pack, generate_pack

CODE = Path(__file__).resolve().parent.parent
MARKER = b"# Generated Australian additions from au_overlay.json.\n"
REFERENCE = CODE / "eval/reference/au/places.json.gz"  # GeoNames and every locality of the sheet (prep/au_sheet_places.py)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--common", type=Path, default=CODE / "rules")
    ap.add_argument("--overlay", type=Path, default=CODE / "rules/au_overlay.json")
    ap.add_argument("--output", type=Path, default=CODE / "rules/au")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--reference", type=Path, default=REFERENCE)
    args = ap.parse_args(argv)
    families = len(expected_pack(args.common, args.overlay, MARKER, args.reference))
    if args.check:
        problems = check_pack(args.common, args.overlay, args.output, MARKER, args.reference)
        if problems:
            print("\n".join(problems))
            return 1
        print(f"Australian rule pack current: {families} families")
    else:
        size = generate_pack(args.common, args.overlay, args.output, MARKER, args.reference)
        print(f"Australian rule pack generated: {families} families, {size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
