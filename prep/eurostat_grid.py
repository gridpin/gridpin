#!/usr/bin/env python3
"""Eurostat's census grid 2021 (1 km, EU regulation 1799/2018), for `gridpin attach --table` (plan T1.5).

Reads `ESTAT_Census_2021_V3.csv` inside `Eurostat_Census-GRID_2021_V3.zip`
(https://ec.europa.eu/eurostat/web/gisco/geodata/population-distribution/population-grids, version of 30 May 2026)
without unpacking it and writes `estat_census_grid2021.csv.gz` with the passport. The cell code (`GRD_ID`,
`CRS3035RES1000mN<north>E<east>`) is what `cell:eu1km` computes from a point. -8888 (confidential) and -9999 (not
available) are the publisher's marks; rows "CC_unallocated" have no cell and never match.

    python3 prep/eurostat_grid.py <the .zip> <out_dir>

Re-use: "Copyright European Union 2026. The source needs to be indicated" (read.me of the archive); Eurostat's
copyright notice (read 2026-11-19) excludes commercial re-use of data for countries outside the EU, EFTA and the
candidate countries. The countries of the grid are counted and printed.
"""
from __future__ import annotations

import argparse
import csv
import gzip
import io
import json
import pathlib
import sys
import zipfile

MEMBER = "Eurostat_Census-GRID_2021_V3/ESTAT_Census_2021_V3.csv"
TAKE = {"T": ("population", "persons"), "Y_LT15": ("age_under_15", "persons"), "Y_1564": ("age_15_64", "persons"),
        "Y_GE65": ("age_65_over", "persons"), "EMP": ("employed", "persons"),
        "CHG_OUT": ("moved_from_abroad_last_year", "persons")}


def build(zip_path: pathlib.Path, out_dir: pathlib.Path) -> dict:
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / "estat_census_grid2021.csv.gz"
    countries: dict[str, int] = {}
    n = 0
    with zipfile.ZipFile(zip_path) as z, z.open(MEMBER) as raw, \
            gzip.open(path, "wt", encoding="utf-8", newline="") as out:
        r = csv.DictReader(io.TextIOWrapper(raw, encoding="utf-8"))
        w = csv.writer(out, lineterminator="\n")
        w.writerow(["GRD_ID", *TAKE, "CNTR_ID"])
        for row in r:
            w.writerow([row["GRD_ID"], *(row[c] for c in TAKE), row["CNTR_ID"]])
            for c in row["CNTR_ID"].split("-"):
                countries[c] = countries.get(c, 0) + 1
            n += 1
    passport = {
        "table": "estat_census_grid2021",
        "file": path.name,
        "prefix": "grid",
        "key": {"column": "GRD_ID", "kind": "cell:eu1km", "vintage": None},
        "take": {c: {"as": a, "unit": u, "missing": ["-8888", "-9999"]} for c, (a, u) in TAKE.items()},
        "edition": "Eurostat census grid 2021, version 3 (2026-05-30)",
        "publisher": "Eurostat (data of the national statistical institutes)",
        "source_url": "https://ec.europa.eu/eurostat/web/gisco/geodata/population-distribution/population-grids",
        "read": "2026-11-19",
        "licence": "re-use with the source indicated (Eurostat copyright notice); modifications stated",
        "attribution": "Source: Eurostat, Census 2021 population grid (version 3, 30 May 2026)",
        "coverage": "partial",
        "caveat": "A value of the 1 km grid cell the address lies in, after the publishers' disclosure control; not "
                  "a value of the household or the person at the address.",
    }
    (out_dir / "estat_census_grid2021.table.json").write_text(json.dumps(passport, indent=1) + "\n")
    return {"cells": n, "bytes": path.stat().st_size, "countries": dict(sorted(countries.items()))}


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("zip", type=pathlib.Path)
    p.add_argument("out", type=pathlib.Path)
    a = p.parse_args(argv)
    print(json.dumps(build(a.zip, a.out)), file=sys.stderr)


if __name__ == "__main__":
    main()
