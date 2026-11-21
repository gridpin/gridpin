#!/usr/bin/env python3
"""Generate the Polish rule pack: common rules + Polish street types, their abbreviations and the titles in street names
(pl_overlay.json) + the commune_regions table (where each place name lies in each voivodeship, from the GeoNames place
reference prep/pl_geonames_reference.py) + the known_houses table (the houses an Overture place stands at,
prep/known_houses.py, ANSWERS §330) + the unfolded_words table ("lodz" -> "łodz", prep/unfolded_words.py).

Same mechanism and format as the United States pack (prep/us_rules.py); rules are data, the engine gets no per-country
branch. The type stands first and the source writes it inconsistently (survey 2026-11-13, 8.6 M names of Overture
2026-10-21.0): no type word for most names, "ulica X" 122 827, "ul. X" 72 612, "Aleja X" 33 693, "Plac X" 17 169; titles
abbreviated or in full ("gen." 40 594, "generała" 13 809). A postal code is five digits written "00-950": the hyphen
after the second digit (`postal_digits 5`, `postal_split 2`). "sk" (skwer) is left out: it spells the region code SK.
Freshness: `python3 prep/au_rules.py --check`.
"""
import argparse
from pathlib import Path

from us_rules import check_pack, expected_pack, generate_pack

CODE = Path(__file__).resolve().parent.parent
MARKER = b"# Generated Polish additions from pl_overlay.json.\n"
REFERENCE = CODE / "eval/reference/pl/geonames-places.json.gz"  # GeoNames places of the PL dump (prep/pl_geonames_reference.py)
# the houses an Overture place stands at (prep/known_houses.py; ANSWERS §330): class 28, cityless homonyms
KNOWN = CODE / "eval/reference/pl/known_houses.tsv.gz"
# words of the sheet keys with "ł" by the form written without it (prep/unfolded_words.py): class 29
UNFOLDED = CODE / "eval/reference/pl/unfolded_words.tsv.gz"


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--common", type=Path, default=CODE / "rules")
    ap.add_argument("--overlay", type=Path, default=CODE / "rules/pl_overlay.json")
    ap.add_argument("--output", type=Path, default=CODE / "rules/pl")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--reference", type=Path, default=REFERENCE)
    ap.add_argument("--known", type=Path, default=KNOWN)
    ap.add_argument("--unfolded", type=Path, default=UNFOLDED)
    args = ap.parse_args(argv)
    families = len(expected_pack(args.common, args.overlay, MARKER, args.reference, known=args.known, unfolded=args.unfolded))
    if args.check:
        problems = check_pack(args.common, args.overlay, args.output, MARKER, args.reference, known=args.known, unfolded=args.unfolded)
        if problems:
            print("\n".join(problems))
            return 1
        print(f"Polish rule pack current: {families} families")
    else:
        size = generate_pack(args.common, args.overlay, args.output, MARKER, args.reference, known=args.known, unfolded=args.unfolded)
        print(f"Polish rule pack generated: {families} families, {size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
