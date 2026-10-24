#!/usr/bin/env python3
"""Generate the United States rule pack: common rules + English street types (us_overlay.json)
+ the commune_regions table (where each city name lies in each state, from the GeoNames place
reference), optionally + a municipality guard built from the same reference (--with-guard).

Same mechanism as the Spanish and Brazilian packs (prep/es_rules.py, prep/br_rules.py): rules
are data, the engine gets no per-country branch. Build: `make us`; freshness:
`python3 prep/us_rules.py --check`. The reference comes from prep/us_geonames_reference.py.

The guard is off by default for the first sheet: the engine rejects an answer whose commune is
named in the query but absent from the guard, and the GeoNames reference covers about 88 percent
of the sheet's city groups (places under 1 000 inhabitants, "X Township", "City of X" are missing).
It is switched on in a quality cycle once the city list is derived from the sheet itself.
"""
import argparse
import gzip
import json
import re
import unicodedata
from collections import defaultdict
from pathlib import Path

from es_rules import FAMILIES, expected_pack as common_pack

CODE = Path(__file__).resolve().parent.parent
MARKER = b"# Generated United States additions from us_overlay.json.\n"
REFERENCE = CODE / "eval/reference/us/geonames-places.json.gz"
GUARD_HEADER = b"# Generated from GeoNames populated places and states (names, alternates, admin1).\n"
MILLION = 1_000_000
REGIONS_HEADER = (b"# Generated from GeoNames populated places: normalized name (name and ASCII name; ASCII alternates"
                  b" for places of a million people)"
                  b" -> \"STATE lat lon\" per place, \";\"-joined. Read by the engine's city tail (query.rs"
                  b" commune_region_points): an answer for \"<city>, <STATE>\" stays within 75 km of one of"
                  b" these points.\n")

# Unit designators of a US address line: the words after them are a unit payload, not a place.
DISTRICT_MARKERS = "apt suite ste unit bldg building fl floor rm room lot trlr spc"
# Leading words of place names written both in full and abbreviated. Every place is keyed
# under the full form; the abbreviated form points to it (alias:), so a query and a sheet
# spelling the name differently still meet on one key.
PLACE_WORDS = {"st": "saint", "ste": "sainte", "ft": "fort", "mt": "mount"}
# English street types that already live in the common tables (not repeated by the overlay).
COMMON_ENGLISH_TYPES = ("avenue", "boulevard", "place", "square")
# Type words that must not open a street range in the guard: "St" also opens the names of
# the Saint places ("St Louis", "St Petersburg"), and a range starting there would hide them.
NOT_STREET_MARKERS = frozenset({"st"})


def normalize(value):
    letters = "".join(c for c in unicodedata.normalize("NFD", value.lower())
                      if unicodedata.category(c) != "Mn")
    return re.sub(r"[^a-z0-9]+", " ", letters).strip()


def canonical(name):
    """Normalised place name with an abbreviated first word written in full."""
    words = normalize(name).split()
    if len(words) > 1 and words[0] in PLACE_WORDS:
        words[0] = PLACE_WORDS[words[0]]
    return " ".join(words)


def abbreviated(name):
    """Alias forms of a canonical name: its first word abbreviated, when it has a short form."""
    words = name.split()
    if len(words) > 1:
        for short, full in PLACE_WORDS.items():
            if words[0] == full:
                yield " ".join([short] + words[1:])


def guard_table(overlay, reference):
    cities = defaultdict(set)
    for place in reference["places"]:
        for form in [place["name"], place["asciiname"], *place["alternates"]]:
            key = canonical(form)
            if key:
                cities[key].add(place["admin1"])
    states = {normalize(state["name"]): state["code"] for state in reference["states"]}
    markers = [word for family in ("street_types_latin", "street_types_extra")
               for word in overlay.get(family, []) if word not in NOT_STREET_MARKERS]
    table = {
        "regions": " ".join(sorted(states.values())),
        "street_markers": " ".join(markers + list(COMMON_ENGLISH_TYPES)),
        "district_markers": DISTRICT_MARKERS,
    }
    table.update({"city:" + name: " ".join(sorted(codes)) for name, codes in cities.items()})
    table.update({"state:" + name: code for name, code in states.items()})
    for name in cities:
        for short in abbreviated(name):
            table["alias:" + short] = name
    return table


def regions_table(reference):
    """Normalized city name -> sorted "STATE lat lon" entries, one per GeoNames place and spelling
    (the engine normalizes the query the same way: lowercase letters and digits)."""
    points = defaultdict(set)
    for place in reference["places"]:
        # The name and its ASCII form; alternates are mostly foreign transliterations, except for
        # the largest cities, whose everyday names are alternates ("New York" for "New York City"):
        # there the ASCII alternates are keyed too (population from the reference).
        forms = {place["name"], place["asciiname"]}
        if place["pop"] >= MILLION:
            forms.update(alt for alt in place["alternates"] if alt.isascii())
        for form in forms:
            key = normalize(form)
            if key and not key.isdigit():
                points[key].add(f'{place["admin1"]} {place["lat"]:.4f} {place["lon"]:.4f}')
    return {name: ";".join(sorted(entries)) for name, entries in points.items()}


def expected_pack(common, overlay, marker=MARKER, reference=REFERENCE, with_guard=False):
    pack = common_pack(common, overlay, marker)
    document = json.loads(gzip.decompress(reference.read_bytes()))
    regions = regions_table(document)
    pack["commune_regions.tsv"] = REGIONS_HEADER + "".join(
        f"{key}\t{regions[key]}\n" for key in sorted(regions)).encode()
    if not with_guard:
        return pack
    additions = json.loads(overlay.read_text(encoding="utf-8"))
    table = guard_table(additions, document)
    pack["municipality_guard.tsv"] = GUARD_HEADER + "".join(
        f"{key}\t{table[key]}\n" for key in sorted(table)).encode()
    return pack


def check_pack(common, overlay, output, marker=MARKER, reference=REFERENCE, with_guard=False):
    expected = expected_pack(common, overlay, marker, reference, with_guard)
    problems = [f"unexpected family: {name}" for name in sorted(
        {p.name for p in output.glob("*.tsv")} - expected.keys())]
    for name, raw in expected.items():
        path = output / name
        if not path.is_file() or path.read_bytes() != raw:
            problems.append(f"stale or missing: {name}")
    return problems


def generate_pack(common, overlay, output, marker=MARKER, reference=REFERENCE, with_guard=False):
    pack = expected_pack(common, overlay, marker, reference, with_guard)
    if output.resolve() == common.resolve():
        raise ValueError("output must not replace the common rules")
    extra = {p.name for p in output.glob("*.tsv")} - pack.keys()
    if extra:
        raise ValueError(f"unexpected output families: {sorted(extra)}")
    output.mkdir(parents=True, exist_ok=True)
    for name, raw in pack.items():
        path = output / name
        if not path.is_file() or path.read_bytes() != raw:
            path.write_bytes(raw)
    return sum(map(len, pack.values()))


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--common", type=Path, default=CODE / "rules")
    ap.add_argument("--overlay", type=Path, default=CODE / "rules/us_overlay.json")
    ap.add_argument("--output", type=Path, default=CODE / "rules/us")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--reference", type=Path, default=REFERENCE)
    ap.add_argument("--with-guard", action="store_true", help="add municipality_guard.tsv from the reference")
    args = ap.parse_args(argv)
    families = len(expected_pack(args.common, args.overlay, MARKER, args.reference, args.with_guard))
    if args.check:
        problems = check_pack(args.common, args.overlay, args.output, MARKER, args.reference, args.with_guard)
        if problems:
            print("\n".join(problems))
            return 1
        print(f"United States rule pack current: {families} families")
    else:
        size = generate_pack(args.common, args.overlay, args.output, MARKER, args.reference, args.with_guard)
        print(f"United States rule pack generated: {families} families, {size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
