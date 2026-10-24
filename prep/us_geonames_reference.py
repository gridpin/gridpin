#!/usr/bin/env python3
"""Build the United States place reference from the GeoNames US dump.

Input: data/geonames_us.txt (GeoNames country dump, 19 tab-separated columns:
geonameid, name, asciiname, alternatenames, latitude, longitude, feature class,
feature code, country code, cc2, admin1 code, admin2 code, admin3 code, admin4 code,
population, elevation, dem, timezone, modification date).

Output: eval/reference/us/geonames-places.json.gz with
  "states": the ADM1 rows of the 50 states and the District of Columbia;
  "places": populated places (feature class P) with population >= 1000, plus every
            capital and administrative seat (PPLC, PPLA, PPLA2, PPLA3) whatever its
            population, inside those 51 regions. Each place carries id, name,
            asciiname, admin1, fcode, pop, lat, lon and up to ten Latin alternate names.
  "source": counts and the newest modification date of the dump, so the file states
            where its numbers come from.

The municipality guard of the US rule pack (prep/us_rules.py) is generated from this
file, so it is checked in and rebuilt only on purpose: without flags the script writes
it, `--check` proves that the checked-in file still follows from the dump byte for byte.
"""
import argparse
import gzip
import json
import re
import unicodedata
from pathlib import Path

CODE = Path(__file__).resolve().parent.parent
DUMP = CODE / "data/geonames_us.txt"
REFERENCE = CODE / "eval/reference/us/geonames-places.json.gz"

MIN_POPULATION = 1000
SEAT_CODES = frozenset({"PPLC", "PPLA", "PPLA2", "PPLA3"})
MAX_ALTERNATES = 10
# The 50 states and the District of Columbia: GeoNames admin1 codes of the contour.
REGIONS = frozenset("""
AK AL AR AZ CA CO CT DC DE FL GA HI IA ID IL IN KS KY LA MA MD ME MI MN MO MS MT NC ND NE
NH NJ NM NV NY OH OK OR PA RI SC SD TN TX UT VA VT WA WI WV WY
""".split())
LATIN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9 .'\-]*$")


def normalize(value):
    letters = "".join(c for c in unicodedata.normalize("NFD", value.lower())
                      if unicodedata.category(c) != "Mn")
    return re.sub(r"[^a-z0-9]+", " ", letters).strip()


def latin_alternates(field, name, asciiname):
    """Latin alternate spellings: ASCII after diacritics are dropped, starting with an
    upper-case letter or digit (GeoNames romanisations of non-Latin names are written
    in lower case), not the name itself, distinct after normalisation. At most ten are
    kept; forms that extend or shorten the name by whole words ("New York" for "New York
    City", "City of Saint Paul") come first, the rest follow in dump order."""
    own = normalize(asciiname)
    seen = {normalize(name), own}
    related, other = [], []
    for raw in field.split(","):
        raw = raw.strip()
        folded = "".join(c for c in unicodedata.normalize("NFD", raw)
                         if unicodedata.category(c) != "Mn")
        key = normalize(folded)
        if not LATIN.match(folded) or folded[0].islower() or not key or key in seen:
            continue
        seen.add(key)
        padded = f" {key} "
        (related if f" {own} " in padded or padded in f" {own} " else other).append(folded)
    return (related + other)[:MAX_ALTERNATES]


def build(dump=DUMP):
    states, places = {}, []
    rows, newest = 0, ""
    with open(dump, encoding="utf-8") as handle:
        for line in handle:
            rows += 1
            cols = line.rstrip("\n").split("\t")
            if len(cols) != 19:
                raise ValueError(f"{dump.name}:{rows}: expected 19 columns, got {len(cols)}")
            newest = max(newest, cols[18])
            fclass, fcode, admin1 = cols[6], cols[7], cols[10]
            if admin1 not in REGIONS:
                continue
            if fcode == "ADM1":
                states[admin1] = cols[1]
                continue
            population = int(cols[14] or 0)
            if fclass != "P" or (population < MIN_POPULATION and fcode not in SEAT_CODES):
                continue
            places.append({
                "id": int(cols[0]),
                "name": cols[1],
                "asciiname": cols[2],
                "admin1": admin1,
                "fcode": fcode,
                "pop": population,
                "lat": float(cols[4]),
                "lon": float(cols[5]),
                "alternates": latin_alternates(cols[3], cols[1], cols[2]),
            })
    if set(states) != REGIONS:
        raise ValueError(f"ADM1 rows missing for: {sorted(REGIONS - set(states))}")
    places.sort(key=lambda p: (p["admin1"], p["asciiname"], p["id"]))
    return {
        "source": {
            "file": dump.name,
            "rows": rows,
            "modified_max": newest,
            "min_population": MIN_POPULATION,
            "seat_codes": sorted(SEAT_CODES),
            "max_alternates": MAX_ALTERNATES,
            "places": len(places),
        },
        "states": [{"code": code, "name": states[code]} for code in sorted(states)],
        "places": places,
    }


def encode(reference):
    text = json.dumps(reference, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    return gzip.compress(text.encode("utf-8"), compresslevel=9, mtime=0)


def load(path=REFERENCE):
    return json.loads(gzip.decompress(path.read_bytes()))


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dump", type=Path, default=DUMP)
    ap.add_argument("--output", type=Path, default=REFERENCE)
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args(argv)
    reference = build(args.dump)
    raw = encode(reference)
    summary = (f"{reference['source']['places']} places, {len(reference['states'])} states, "
               f"{len(raw)} bytes gzip")
    if args.check:
        if not args.output.is_file() or args.output.read_bytes() != raw:
            print(f"stale or missing: {args.output}")
            return 1
        print(f"US place reference current: {summary}")
        return 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    if not args.output.is_file() or args.output.read_bytes() != raw:
        args.output.write_bytes(raw)
    print(f"US place reference written: {summary}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
