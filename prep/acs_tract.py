#!/usr/bin/env python3
"""ACS 5-year indicators by census tract, for `gridpin attach --table` (plan T1.4).

Reads the table-based summary files of the American Community Survey (U.S. Census Bureau,
https://www2.census.gov/programs-surveys/acs/summary_file/<year>/table-based-SF/data/5YRData/, pipe-delimited,
one file per table, every geography) and writes one CSV of tracts: GEOID (11 digits), counts, medians and shares
with their margins of error, and the passport `acs5_<year>_tract.table.json` beside it.

    python3 prep/acs_tract.py <dir with acsdt5y<year>-<table>.dat> <out_dir> --year 2024

Shares are computed here from a numerator and a denominator. The margin of a share follows the Census Bureau's
formula for a proportion (ACS General Handbook 2020, ch. 8): MOE(p) = sqrt(MOE(x)^2 - p^2 MOE(y)^2) / y, with a plus
under the root when the difference is negative. A margin of a sum of cells is the root of the sum of squares.
Special values the Bureau writes in place of a number (-666666666 and others, ACS notes on annotations) are kept as
they are and named `missing` in the passport, so `attach` reports them as suppressed.

Census tracts of these files follow the 2020 boundaries (the ACS 2020-2024 tabulates 2020 tracts).
"""
from __future__ import annotations

import argparse
import gzip
import json
import math
import pathlib
import sys

# The Bureau's marks of a value that is not a number (estimates and margins).
SPECIAL = ["-666666666", "-999999999", "-888888888", "-555555555", "-333333333", "-222222222"]

# (output name, unit, table, estimate cell, margin cell)
DIRECT = [
    ("population", "persons", "b01003", "B01003_E001", "B01003_M001"),
    ("median_age", "years", "b01002", "B01002_E001", "B01002_M001"),
    ("median_household_income", "USD", "b19013", "B19013_E001", "B19013_M001"),
    ("housing_units", "units", "b25001", "B25001_E001", "B25001_M001"),
    ("median_home_value", "USD", "b25077", "B25077_E001", "B25077_M001"),
    ("median_gross_rent", "USD", "b25064", "B25064_E001", "B25064_M001"),
]
# (output name, table, numerator cells, denominator cell)
SHARES = [
    ("vacant_share", "b25002", ["B25002_E003"], "B25002_E001"),
    ("owner_occupied_share", "b25003", ["B25003_E002"], "B25003_E001"),
    ("bachelor_or_higher_share", "b15003", ["B15003_E022", "B15003_E023", "B15003_E024", "B15003_E025"],
     "B15003_E001"),
    ("below_poverty_share", "b17001", ["B17001_E002"], "B17001_E001"),
]
TRACT = "1400000US"


def read_table(path: pathlib.Path, cells: set[str]) -> dict[str, dict[str, str]]:
    out: dict[str, dict[str, str]] = {}
    with open(path, encoding="utf-8") as f:
        head = f.readline().rstrip("\n").split("|")
        at = {c: head.index(c) for c in cells | {c.replace("_E", "_M") for c in cells}}
        for line in f:
            if not line.startswith(TRACT):
                continue
            parts = line.rstrip("\n").split("|")
            out[parts[0][len(TRACT):]] = {c: parts[i] for c, i in at.items()}
    return out


def number(v: str | None) -> float | None:
    if v is None or v in SPECIAL or v == "":
        return None
    try:
        return float(v)
    except ValueError:
        return None


def share(num: list[tuple[float, float]], den: tuple[float, float]) -> tuple[str, str]:
    """A share and its margin from (estimate, margin) pairs; empty when the denominator is zero."""
    y, my = den
    if y <= 0:
        return "", ""
    x = sum(e for e, _ in num)
    mx = math.sqrt(sum(m * m for _, m in num))
    p = x / y
    under = mx * mx - p * p * my * my
    if under < 0:
        under = mx * mx + p * p * my * my
    return f"{p:.4f}", f"{math.sqrt(under) / y:.4f}"


def build(src: pathlib.Path, out_dir: pathlib.Path, year: int) -> dict:
    tables: dict[str, set[str]] = {}
    for _, _, t, e, _ in DIRECT:
        tables.setdefault(t, set()).add(e)
    for _, t, num, den in SHARES:
        tables.setdefault(t, set()).update(num + [den])
    data = {t: read_table(src / f"acsdt5y{year}-{t}.dat", cells) for t, cells in tables.items()}
    geoids = sorted(set().union(*(d.keys() for d in data.values())))
    columns = ["GEOID"]
    for name, *_ in DIRECT:
        columns += [name, f"{name}_moe"]
    for name, *_ in SHARES:
        columns += [name, f"{name}_moe"]
    out_dir.mkdir(parents=True, exist_ok=True)
    csv_path = out_dir / f"acs5_{year}_tract.csv.gz"
    with gzip.open(csv_path, "wt", encoding="utf-8", newline="") as f:
        f.write(",".join(columns) + "\n")
        for g in geoids:
            row = [g]
            for _, _, t, e, m in DIRECT:
                cell = data[t].get(g, {})
                row += [cell.get(e, ""), cell.get(m, "")]
            for _, t, num, den in SHARES:
                cell = data[t].get(g, {})
                pairs = [(number(cell.get(c)), number(cell.get(c.replace("_E", "_M")))) for c in num]
                d = (number(cell.get(den)), number(cell.get(den.replace("_E", "_M"))))
                if any(v is None for pair in pairs + [d] for v in pair):
                    row += ["", ""]
                else:
                    row += list(share(pairs, d))
            f.write(",".join(row) + "\n")
    take = {}
    for name, unit, *_ in DIRECT:
        take[name] = {"as": name, "unit": unit, "moe": f"{name}_moe", "missing": SPECIAL}
    for name, *_ in SHARES:
        take[name] = {"as": name, "unit": "share 0..1", "moe": f"{name}_moe", "missing": SPECIAL}
    passport = {
        "table": f"acs5_{year}_tract",
        "file": csv_path.name,
        "prefix": "acs",
        "key": {"column": "GEOID", "kind": "us_tract", "vintage": "2020", "width": 11, "grain": "fine"},
        "take": take,
        "edition": f"ACS 5-year {year - 4}-{year}",
        "publisher": "U.S. Census Bureau",
        "source_url": f"https://www2.census.gov/programs-surveys/acs/summary_file/{year}/table-based-SF/data/5YRData/",
        "read": "2026-11-19",
        "licence": "U.S. federal government work (17 U.S.C. 105); the Bureau's terms forbid using the data to "
                   "identify any person, household or business",
        "coverage": "complete",
        "max_absent_share": 0.02,
        "caveat": "A value of the census tract the address lies in, an estimate with its margin of error; not a "
                  "value of the household or the person at the address.",
    }
    (out_dir / f"acs5_{year}_tract.table.json").write_text(json.dumps(passport, indent=1) + "\n")
    return {"tracts": len(geoids), "csv": str(csv_path), "bytes": csv_path.stat().st_size}


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("src", type=pathlib.Path)
    p.add_argument("out", type=pathlib.Path)
    p.add_argument("--year", type=int, required=True)
    a = p.parse_args(argv)
    print(json.dumps(build(a.src, a.out, a.year)), file=sys.stderr)


if __name__ == "__main__":
    main()
