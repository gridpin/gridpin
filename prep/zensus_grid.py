#!/usr/bin/env python3
"""Germany's Zensus 2022 results by grid cell, for `gridpin attach --table` (plan T1.5).

Reads the archives of https://www.zensus2022.de/DE/Ergebnisse-des-Zensus/gitterzellen.html (one archive per theme,
each with a CSV per cell size: `GITTER_ID_100m;x_mp_100m;y_mp_100m;<value>;werterlaeuternde_Zeichen`, a decimal
comma, "–" for a value kept secret or not available) and writes one table per cell size with the themes side by
side, and its passport:

    python3 prep/zensus_grid.py <dir with the archives> <out_dir>

    zensus2022_100m.csv.gz   key cell:de100m (CRS3035RES100mN<north>E<east>)
    zensus2022_1km.csv.gz    key cell:eu1km  (CRS3035RES1000mN<north>E<east>, the European grid)

A cell is in a file only when the theme has a value or a mark for it: a cell without residents is in no file, so the
tables are `partial` and a cell absent from them is reported, not refused. Licence: Datenlizenz Deutschland –
Namensnennung – Version 2.0 (named in each archive's Datensatzbeschreibung, read 2026-11-19).
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

# (archive, file stem, value column, output name, unit)
THEMES = [
    ("Zensus2022_Bevoelkerungszahl", "Zensus2022_Bevoelkerungszahl", "Einwohner", "population", "persons"),
    ("Durchschnittsalter_in_Gitterzellen", "Zensus2022_Durchschnittsalter", "Durchschnittsalter", "mean_age",
     "years"),
    ("Eigentuemerquote_in_Gitterzellen", "Zensus2022_Eigentuemerquote", "Eigentuemerquote", "owner_occupied_pct",
     "percent of flats"),
    ("Leerstandsquote_in_Gitterzellen", "Zensus2022_Leerstandsquote", "Leerstandsquote", "vacancy_pct",
     "percent of flats"),
    ("Zensus2022_Durchschn_Nettokaltmiete", "Zensus2022_Durchschn_Nettokaltmiete", "durchschnMieteQM",
     "net_rent_eur_m2", "EUR per m2 a month"),
]
SECRET = "–"  # the Zensus mark of a value kept secret or not available
SIZES = {"100m": "cell:de100m", "1km": "cell:eu1km"}


def read_theme(src: pathlib.Path, archive: str, stem: str, column: str, size: str) -> dict[str, str]:
    with zipfile.ZipFile(src / f"{archive}.zip") as z:
        with z.open(f"{stem}_{size}-Gitter.csv") as f:
            r = csv.DictReader(io.TextIOWrapper(f, encoding="utf-8-sig"), delimiter=";")
            out = {}
            for row in r:
                v = row[column].strip()
                out[row[f"GITTER_ID_{size}"]] = v if v == SECRET else v.replace(",", ".")
            return out


def build(src: pathlib.Path, out_dir: pathlib.Path) -> dict:
    out_dir.mkdir(parents=True, exist_ok=True)
    counts = {}
    for size, kind in SIZES.items():
        themes = [(name, read_theme(src, a, s, c, size)) for a, s, c, name, _ in THEMES]
        keys = sorted(set().union(*(t.keys() for _, t in themes)))
        path = out_dir / f"zensus2022_{size}.csv.gz"
        with gzip.open(path, "wt", encoding="utf-8", newline="") as f:
            w = csv.writer(f, lineterminator="\n")
            w.writerow(["GRD_ID"] + [n for n, _ in themes])
            for k in keys:
                w.writerow([k] + [t.get(k, "") for _, t in themes])
        passport = {
            "table": f"zensus2022_{size}",
            "file": path.name,
            "prefix": "zensus",
            "key": {"column": "GRD_ID", "kind": kind, "vintage": None},
            "take": {n: {"as": n, "unit": u, "missing": [SECRET]} for _, _, _, n, u in THEMES},
            "edition": "Zensus 2022 (reference day 2022-05-15), grid cells",
            "publisher": "Statistische Ämter des Bundes und der Länder",
            "source_url": "https://www.zensus2022.de/DE/Ergebnisse-des-Zensus/gitterzellen.html",
            "read": "2026-11-19",
            "licence": "Datenlizenz Deutschland – Namensnennung – Version 2.0 (dl-de/by-2-0)",
            "attribution": "Statistische Ämter des Bundes und der Länder, Zensus 2022, dl-de/by-2-0, "
                           "www.govdata.de/dl-de/by-2-0",
            "coverage": "partial",
            "caveat": "A value of the grid cell the address lies in; a cell absent from the table has no residents "
                      "in the census. Not a value of the household or the person at the address.",
        }
        (out_dir / f"zensus2022_{size}.table.json").write_text(
            json.dumps(passport, indent=1, ensure_ascii=False) + "\n")
        counts[size] = {"cells": len(keys), "bytes": path.stat().st_size}
    return counts


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("src", type=pathlib.Path)
    p.add_argument("out", type=pathlib.Path)
    a = p.parse_args(argv)
    print(json.dumps(build(a.src, a.out)), file=sys.stderr)


if __name__ == "__main__":
    main()
