#!/usr/bin/env python3
"""Australia's SEIFA 2021 by Statistical Area Level 2, for `gridpin attach --table` (plan T1.6).

Reads `Statistical Area Level 2, Indexes, SEIFA 2021.xlsx` (Australian Bureau of Statistics, Table 1: the four
indexes, score and decile) with the standard library and writes `seifa2021_sa2.csv` and its passport. The codes are
SA2 of the ASGS Edition 3 (2021) boundaries: a layer of another edition is refused by the passport's year.

    python3 prep/seifa_au.py <the .xlsx> <out_dir>

Licence: Creative Commons Attribution 4.0 (abs.gov.au, "Copyright and creative commons", read 2026-11-19).
"""
from __future__ import annotations

import argparse
import csv
import json
import pathlib
import re
import sys
import xml.etree.ElementTree as ET
import zipfile

NS = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"
# column of Table 1 → output name
COLUMNS = {
    "A": "SA2_CODE21",
    "C": "irsd_score", "D": "irsd_decile",
    "E": "irsad_score", "F": "irsad_decile",
    "G": "ier_score", "H": "ier_decile",
    "I": "ieo_score", "J": "ieo_decile",
    "K": "usual_resident_population",
}


def sheet_rows(path: pathlib.Path, sheet: str):
    """The cells of one worksheet, row by row, as {column letter: text}."""
    z = zipfile.ZipFile(path)
    shared = ["".join(t.text or "" for t in si.iter(f"{NS}t"))
              for si in ET.fromstring(z.read("xl/sharedStrings.xml")).findall(f"{NS}si")]
    for row in ET.fromstring(z.read(f"xl/worksheets/{sheet}.xml")).iter(f"{NS}row"):
        out = {}
        for c in row.findall(f"{NS}c"):
            v = c.find(f"{NS}v")
            if v is None:
                continue
            col = re.match(r"[A-Z]+", c.get("r")).group()
            out[col] = shared[int(v.text)] if c.get("t") == "s" else v.text
        yield out


def build(xlsx: pathlib.Path, out_dir: pathlib.Path) -> dict:
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / "seifa2021_sa2.csv"
    n = 0
    with open(path, "w", newline="") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(COLUMNS.values())
        for r in sheet_rows(xlsx, "sheet2"):  # Table 1
            code = r.get("A", "")
            if not re.fullmatch(r"\d{9}", code):
                continue
            w.writerow([r.get(c, "") for c in COLUMNS])
            n += 1
    take = {}
    for col in list(COLUMNS.values())[1:]:
        take[col] = {"as": col, "unit": "decile 1-10" if col.endswith("decile") else
                     ("persons" if col.startswith("usual") else "score, mean 1000"), "missing": ["-", ""]}
    passport = {
        "table": "seifa2021_sa2",
        "file": path.name,
        "prefix": "depr",
        "key": {"column": "SA2_CODE21", "kind": "au_sa2", "vintage": "2021", "width": 9, "grain": "fine"},
        "take": take,
        "edition": "SEIFA 2021 (released 2023-04-27), ASGS Edition 3",
        "publisher": "Australian Bureau of Statistics",
        "source_url": "https://www.abs.gov.au/statistics/people/people-and-communities/"
                      "socio-economic-indexes-areas-seifa-australia/latest-release",
        "read": "2026-11-19",
        "licence": "CC BY 4.0",
        "attribution": "Australian Bureau of Statistics, Socio-Economic Indexes for Areas (SEIFA) 2021, CC BY 4.0",
        "coverage": "complete",
        "max_absent_share": 0.02,
        "caveat": "A relative index of the Statistical Area Level 2 the address lies in; not a measure of the "
                  "household or the person at the address. Areas with few residents have no score.",
    }
    (out_dir / "seifa2021_sa2.table.json").write_text(json.dumps(passport, indent=1) + "\n")
    return {"areas": n, "bytes": path.stat().st_size}


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("xlsx", type=pathlib.Path)
    p.add_argument("out", type=pathlib.Path)
    a = p.parse_args(argv)
    print(json.dumps(build(a.xlsx, a.out)), file=sys.stderr)


if __name__ == "__main__":
    main()
