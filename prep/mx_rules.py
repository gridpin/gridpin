#!/usr/bin/env python3
"""Generate the Mexican rule pack: common rules + Spanish street types written before the name, their
abbreviations, the state names and abbreviations of the tail grammar (mx_overlay.json) + the commune_regions
table (where each city name lies in each state, from the GeoNames place reference prep/mx_geonames_reference.py).

Same mechanism and format as the United States and Canadian packs (prep/us_rules.py); rules are data, the engine
gets no per-country branch. The street-type list was chosen by counting the first words of the 30.7 M Mexican
street names of Overture 2026-10-21.0 (CALLE 76 %, AVENIDA 9 %, PRIVADA 6 %, CERRADA, ANDADOR, BOULEVARD, CIRCUITO,
PROLONGACION, CALLEJON, PEATONAL, CALZADA, RETORNO, CARRETERA, EJE, PERIFERICO, DIAGONAL, PASAJE). The type comes
first, as in Spain: `street_type_last` stays off. The state key is the ISO 3166-2:MX code (prep/mx_states.py);
`region_names` carries the names and the abbreviations people write ("Jal.", "N.L.", "CDMX", "Edo. Mex.").
"col" is both a junk word (the colonia marker "Col. Roma Norte") and the code of Colima: the engine never treats a
region segment as junk, so a bare "Col" segment stays Colima (tools/test_mx_rules.py names the exception).
Freshness: `python3 prep/mx_rules.py --check`.
"""
import argparse
from pathlib import Path

from us_rules import check_pack, expected_pack, generate_pack

CODE = Path(__file__).resolve().parent.parent
MARKER = b"# Generated Mexican additions from mx_overlay.json.\n"
REFERENCE = CODE / "eval/reference/mx/geonames-places.json.gz"


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--common", type=Path, default=CODE / "rules")
    ap.add_argument("--overlay", type=Path, default=CODE / "rules/mx_overlay.json")
    ap.add_argument("--output", type=Path, default=CODE / "rules/mx")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--reference", type=Path, default=REFERENCE)
    args = ap.parse_args(argv)
    families = len(expected_pack(args.common, args.overlay, MARKER, args.reference))
    if args.check:
        problems = check_pack(args.common, args.overlay, args.output, MARKER, args.reference)
        if problems:
            print("\n".join(problems))
            return 1
        print(f"Mexican rule pack current: {families} families")
    else:
        size = generate_pack(args.common, args.overlay, args.output, MARKER, args.reference)
        print(f"Mexican rule pack generated: {families} families, {size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
