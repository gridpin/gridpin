"""The CEP layer of Brazil: every eight-digit CEP of the sheet's source to its street, town, state and a point.

    python3 prep/br_cep_layer.py OUT.tsv.gz [--source data/br_norm.parquet] [--release 2026-08-19.0]

The free sheet of Brazil keeps the first five digits of a CEP (its key, PRINCIPLES 2.8-twelve), so "01310-100" alone
finds nothing there. This layer is a separate file beside the sheet (the sheet is not changed, ANSWERS §264, §357):
`gridpin serve br.bin --postcode-layer br_cep.tsv.gz` answers a query that is only a CEP, and the ViaCEP-shaped
route `/ws/<cep>/json/`.

One row per (CEP, street, town) of the source: a street CEP ("01310-100") names its street; a town CEP ("96230-000",
the whole town) has no street. A code ending in -000 is a town code only when no street holds half of its houses: in
large cities -000 is also a street's code ("04551-000" is Rua Olimpíadas, checked against ViaCEP 17.11.2026). The point is the house nearest to the middle of the houses of that row, so it lies on
the street. The state comes from the official CEP ranges of the Correios by the first five digits (the source has no
state); the district (bairro) is not in the source and stays empty.

The file: one header line `#gridpin postcode layer 1` with key=value fields, then tab-separated rows sorted by the
code (digits only), the street with most houses first: code, street, district, city, region, lat, lon, houses.
"""
import argparse
import gzip
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from duck import connect_tuned  # noqa: E402

# Correios: the ranges of the first five CEP digits of each state (Faixas de CEP por UF). Several states have two
# ranges (DF, GO, AM). Checked against ViaCEP's `uf` on a sample (code/eval/evidence/k6_20261116/).
UF_RANGES = [
    (1000, 19999, "SP"), (20000, 28999, "RJ"), (29000, 29999, "ES"), (30000, 39999, "MG"),
    (40000, 48999, "BA"), (49000, 49999, "SE"), (50000, 56999, "PE"), (57000, 57999, "AL"),
    (58000, 58999, "PB"), (59000, 59999, "RN"), (60000, 63999, "CE"), (64000, 64999, "PI"),
    (65000, 65999, "MA"), (66000, 68899, "PA"), (68900, 68999, "AP"), (69000, 69299, "AM"),
    (69300, 69399, "RR"), (69400, 69899, "AM"), (69900, 69999, "AC"), (70000, 72799, "DF"),
    (72800, 72999, "GO"), (73000, 73699, "DF"), (73700, 76799, "GO"), (76800, 76999, "RO"),
    (77000, 77999, "TO"), (78000, 78899, "MT"), (79000, 79999, "MS"), (80000, 87999, "PR"),
    (88000, 89999, "SC"), (90000, 99999, "RS"),
]


def uf_of(code: str) -> str:
    """The state of an eight-digit CEP by its first five digits; '' outside every range."""
    if len(code) != 8 or not code.isdigit():
        return ""
    head = int(code[:5])
    for lo, hi, uf in UF_RANGES:
        if lo <= head <= hi:
            return uf
    return ""


def clean(text) -> str:
    return " ".join(str(text or "").replace("\t", " ").split())


# Plain string, not an f-string: the regular expression's {5} must reach DuckDB as written (PRINCIPLES 2.7).
ROWS_SQL = """
WITH src AS (
    SELECT replace(code_postal_display, '-', '') AS code, nom_voie, nom_commune AS city, lat, lon
    FROM read_parquet(?)
    WHERE regexp_full_match(code_postal_display, '[0-9]{5}-[0-9]{3}') AND lat IS NOT NULL AND lon IS NOT NULL
), town_codes AS (
    -- a -000 code whose biggest street holds less than half of its houses names a whole town
    SELECT code FROM (
        SELECT code, max(n) AS top, sum(n) AS total
        FROM (SELECT code, nom_voie, count(*) AS n FROM src WHERE code LIKE '%000' GROUP BY code, nom_voie)
        GROUP BY code
    ) WHERE top * 2 < total
), a AS (
    SELECT code, CASE WHEN code IN (SELECT code FROM town_codes) THEN '' ELSE nom_voie END AS street, city, lat, lon
    FROM src
), c AS (
    SELECT *, avg(lat) OVER w AS alat, avg(lon) OVER w AS alon, count(*) OVER w AS n
    FROM a WINDOW w AS (PARTITION BY code, street, city)
)
SELECT code, street, city,
       arg_min({'lat': lat, 'lon': lon}, (lat - alat) * (lat - alat) + (lon - alon) * (lon - alon)) AS p,
       any_value(n) AS n
FROM c GROUP BY code, street, city
ORDER BY code, n DESC, street, city
"""


def build(out: pathlib.Path, source: str, release: str) -> dict:
    con = connect_tuned()
    rows = con.execute(ROWS_SQL, [source]).fetchall()
    counts = {"rows": 0, "codes": set(), "street_rows": 0, "town_rows": 0, "no_state": 0}
    tmp = out.with_name(out.name + ".part")
    with gzip.open(tmp, "wt", encoding="utf-8", compresslevel=9) as f:
        f.write(f"#gridpin postcode layer 1\tcountry=BR\trelease={release}\tsource=Overture addresses (OpenAddresses, "
                f"CNEFE)\tcolumns=code,street,district,city,region,lat,lon,houses\n")
        for code, street, city, p, n in rows:
            uf = uf_of(code)
            counts["rows"] += 1
            counts["codes"].add(code)
            counts["street_rows" if street else "town_rows"] += 1
            counts["no_state"] += not uf
            f.write("\t".join([code, clean(street), "", clean(city), uf, f"{p['lat']:.7f}", f"{p['lon']:.7f}",
                               str(n)]) + "\n")
    tmp.replace(out)
    counts["codes"] = len(counts["codes"])
    return counts


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out", type=pathlib.Path)
    ap.add_argument("--source", default=str(HERE.parent / "data" / "br_norm.parquet"))
    ap.add_argument("--release", default="unknown", help="the Overture release of the source, written in the header")
    args = ap.parse_args(argv)
    counts = build(args.out, args.source, args.release)
    print(f"{args.out}: {counts['rows']} rows, {counts['codes']} codes ({counts['street_rows']} street rows, "
          f"{counts['town_rows']} town rows), {counts['no_state']} rows without a state", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
