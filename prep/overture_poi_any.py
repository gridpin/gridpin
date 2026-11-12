"""A POI layer for any country from an Overture places extract: the sheet (`--poi`) and its categories table.

    python3 prep/overture_poi_any.py <cc>_poi_raw__<release>.parquet OUT_DIR <cc> [--min-confidence 0.5]

Writes `<cc>_poi_places.csv` (the friendly table of `gridpin build --places`: name, lat, lon, group = the locality,
aliases = the brand) and `<cc>_poi_cat.csv.gz` (name, lat, lon, category: Overture `basic_category`; the field
`categories.primary` of the 2025 schema is gone in the 2026 releases), read by `gridpin-pro batch --near` and
`--append building` (`gp_rdi`). Licence: Overture places are a mix of permissive licences (CDLA-Permissive-2.0,
Apache-2.0, CC0-1.0), named per row in the extract's `source_license`.
"""
import argparse
import csv
import gzip
import pathlib
import sys

import duckdb


def cut255(s):
    """A display name within 255 UTF-8 bytes (the names section stores a one-byte length), cut at a character."""
    return s.encode("utf-8")[:255].decode("utf-8", "ignore").rstrip()


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("parquet")
    p.add_argument("out_dir")
    p.add_argument("cc")
    p.add_argument("--min-confidence", type=float, default=0.5)
    a = p.parse_args(argv)
    out = pathlib.Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    rows = duckdb.connect().execute(
        """SELECT name_primary, lat, lon, coalesce(nullif(locality, ''), ?) AS grp, coalesce(brand_name, '') AS brand,
                  coalesce(categorie, '') AS cat
           FROM read_parquet(?) WHERE confidence >= ? AND length(name_primary) BETWEEN 2 AND 120
             AND coalesce(operating_status, 'open') = 'open'
           ORDER BY grp, confidence DESC, name_primary""",
        [a.cc.upper(), a.parquet, a.min_confidence],
    ).fetchall()
    cc = a.cc.lower()
    with open(out / f"{cc}_poi_places.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["name", "lat", "lon", "group", "code", "aliases"])
        for name, lat, lon, grp, brand, _ in rows:
            w.writerow([cut255(name.replace("|", " ")), f"{lat:.7f}", f"{lon:.7f}", cut255(grp), "",
                        cut255(brand.replace("|", " ")) if brand and brand != name else ""])
    with gzip.open(out / f"{cc}_poi_cat.csv.gz", "wt", newline="") as f:
        w = csv.writer(f)
        w.writerow(["name", "lat", "lon", "category"])
        for name, lat, lon, _, _, cat in rows:
            if cat:
                w.writerow([name, f"{lat:.7f}", f"{lon:.7f}", cat])
    print(f"{len(rows)} objects -> {out}/{cc}_poi_places.csv, {cc}_poi_cat.csv.gz", file=sys.stderr)


if __name__ == "__main__":
    main()
