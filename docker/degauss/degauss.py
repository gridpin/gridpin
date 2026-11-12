"""A DeGAUSS-compatible entry point: `docker run --rm -v $PWD:/tmp <image> my_file.csv [threshold|all]`.

Reads /tmp/<file>.csv (an `address` column), geocodes it with GridPin, and writes
/tmp/<file>_gridpin_<version>_score_threshold_<threshold>.csv with the DeGAUSS geocoder's columns added:
matched_street, matched_city, matched_state, matched_zip, precision, score, lat, lon, geocode_result.

Semantics differ where the engines differ, and are not bent to look alike: `precision` is `range` for a house point
and for an interpolated number (DeGAUSS: interpolated on a street segment), `street`, `intersection`, `zip` (a place
read from a postcode alone) or `city`; `score` is GridPin's calibrated confidence 0..1, not the DeGAUSS score.
`geocode_result`: `geocoded` (range or street, score at or over the threshold), `imprecise_geocode` (otherwise; lat
and lon are emptied unless the threshold is `all`), `po_box`, `non_address_text` (blank, "foreign", "verify",
"unknown"). The institutional-address list of Cincinnati has no counterpart.
"""
import csv
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

GRIDPIN = os.environ.get("GRIDPIN_BIN", "/usr/local/bin/gridpin")
SHEET = os.environ.get("GRIDPIN_SHEET", "/data/sheet.bin")
VERSION = os.environ.get("GRIDPIN_VERSION", "0.9")
COLUMNS = ["matched_street", "matched_city", "matched_state", "matched_zip", "precision", "score", "lat", "lon",
           "geocode_result"]
PO_BOX = re.compile(r"\b(p\.?\s*o\.?\s*box|post office box)\b", re.I)


def precision_of(row):
    p = row.get("gp_precision", "")
    if p in ("house", "interp", "near"):
        return "range"
    if p in ("street", "intersection"):
        return p
    if p == "city":
        return "zip" if row.get("gp_match_level") == "Postal" else "city"
    return ""


def degauss_row(address, row, threshold):
    a = (address or "").strip()
    if not a or a.lower() in ("foreign", "verify", "unknown"):
        return dict.fromkeys(COLUMNS, "") | {"geocode_result": "non_address_text"}
    if PO_BOX.search(a):
        return dict.fromkeys(COLUMNS, "") | {"geocode_result": "po_box"}
    precision = precision_of(row)
    score = row.get("gp_confidence", "")
    good = precision in ("range", "street") and score != "" and (threshold == "all" or float(score) >= float(threshold))
    street = " ".join(x for x in (row.get("gp_housenumber", ""), row.get("gp_street", "")) if x)
    out = {"matched_street": street, "matched_city": row.get("gp_commune", ""), "matched_state": row.get("gp_region", ""),
           "matched_zip": row.get("gp_postcode", ""), "precision": precision, "score": score,
           "lat": row.get("gp_lat", ""), "lon": row.get("gp_lon", ""),
           "geocode_result": "geocoded" if good else "imprecise_geocode"}
    if not good and threshold != "all":
        out["lat"] = out["lon"] = ""
    return out


def main(argv):
    if not argv:
        sys.exit("usage: <file>.csv [score threshold, default 0.5 | all]")
    src = Path(argv[0]) if Path(argv[0]).is_absolute() else Path("/tmp") / argv[0]
    if not src.exists() and Path(argv[0]).exists():
        src = Path(argv[0])
    threshold = argv[1] if len(argv) > 1 else "0.5"
    with open(src, newline="") as f:
        rows = list(csv.DictReader(f))
    if rows and "address" not in rows[0]:
        sys.exit("the input needs an `address` column")
    with tempfile.TemporaryDirectory() as tmp:
        q, a = Path(tmp) / "q.csv", Path(tmp) / "a.csv"
        with open(q, "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["address"])
            w.writerows([[r.get("address", "")] for r in rows])
        subprocess.run([GRIDPIN, "batch", SHEET, str(q), str(a), "--column", "address"], check=True,
                       stderr=subprocess.DEVNULL)
        with open(a, newline="") as f:
            answers = list(csv.DictReader(f))
    out = src.with_name(f"{src.stem}_gridpin_{VERSION}_score_threshold_{threshold}.csv")
    with open(out, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0].keys()) + COLUMNS if rows else COLUMNS)
        w.writeheader()
        for r, ans in zip(rows, answers):
            w.writerow(r | degauss_row(r.get("address"), ans, threshold))
    print(f"wrote {out.name}")


if __name__ == "__main__":
    main(sys.argv[1:])
