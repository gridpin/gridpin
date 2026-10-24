#!/usr/bin/env python3
"""US interpolation layer from TIGER/Line address ranges (US Census Bureau, public domain).

The point sources of the US sheet (prep/overture.py, US_GREEN_DATASETS) cover 64.5 % of the
housing units. Everywhere else the only nationwide, redistributable address data is the Census
Bureau's TIGER/Line ADDRFEAT layer: one record per road edge with the house-number range of each
side (LFROMHN..LTOHN, RFROMHN..RTOHN), the ZIP of each side and the edge geometry. This script
turns those ranges into *range points* in the canonical row format of the sheet builder:

* a range is sampled along its edge at most every ``--step-m`` metres (both ends included), the
  house number grows linearly with the distance and keeps the parity of the side;
* every range point carries the suffix ``RANGE_MARKER`` ("~"). The engine (query.rs, find_house)
  interpolates between two marked neighbours whatever their number gap, answers "interp" when a
  query hits a marked number exactly, and never prints the marker; real houses always win;
* a ZIP whose point sources already hold at least ``--min-green`` addresses is skipped, and so is
  every range point in a grid cell (``CELL_DEG``, about 500 m) that already holds
  ``--min-green-cell`` point addresses: there the point data is complete and the ranges would only
  add noise (single-building ZIPs in Manhattan are green by area, not by their own ZIP count);
* the city is the USPS place name of the ZIP (GeoNames postal codes, CC BY 4.0), the state comes
  from the county FIPS of the file. Rows whose ZIP is unknown to both tables are dropped and
  counted (``rows_no_zip``);
* the locality code reuses the code of the point sheet when a same-named green locality lies
  within ``NEAR_KM`` of the point; otherwise a new code continues the numbering.

Usage:
  python3 prep/us_tiger.py <addrfeat_zip_dir> <geonames_postal_US.txt> <us_norm.parquet>
                           <out_parquet> [--min-green N] [--step-m M] [--work DIR]

Input ZIP files are read in place through GDAL's /vsizip/ (DuckDB spatial); nothing is unpacked.
The result is a Parquet file with the canon columns (see CANON_COLUMNS) that
prep/export_build.py can read after a UNION with the point sheet (see Makefile, target `us`).
"""
from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
import time

from duck import connect_tuned
from overture import NORM, TITLE

CODE = pathlib.Path(__file__).resolve().parent.parent
RANGE_MARKER = "~"
# Grid-style numbers reach six digits (Utah, Wisconsin); longer values are coding artefacts.
MAX_HOUSE_NUMBER = 999_999
NEAR_KM = 100.0
CELL_DEG = 0.005  # coverage grid: about 550 m north-south, 400-500 m east-west in the US
CANON_COLUMNS = ("id", "numero", "rep", "nom_voie", "nom_voie_norm", "code_postal",
                 "code_postal_display", "code_insee", "nom_commune", "nom_commune_norm",
                 "provincia_norm", "street_key", "lon", "lat")

# County FIPS -> state code (50 states and DC; territories are not in the contour, ANSWERS §140).
STATE_FIPS = {
    "01": "AL", "02": "AK", "04": "AZ", "05": "AR", "06": "CA", "08": "CO", "09": "CT", "10": "DE",
    "11": "DC", "12": "FL", "13": "GA", "15": "HI", "16": "ID", "17": "IL", "18": "IN", "19": "IA",
    "20": "KS", "21": "KY", "22": "LA", "23": "ME", "24": "MD", "25": "MA", "26": "MI", "27": "MN",
    "28": "MS", "29": "MO", "30": "MT", "31": "NE", "32": "NV", "33": "NH", "34": "NJ", "35": "NM",
    "36": "NY", "37": "NC", "38": "ND", "39": "OH", "40": "OK", "41": "OR", "42": "PA", "44": "RI",
    "45": "SC", "46": "SD", "47": "TN", "48": "TX", "49": "UT", "50": "VT", "51": "VA", "53": "WA",
    "54": "WV", "55": "WI", "56": "WY",
}

# TIGER FULLNAME abbreviations (Census feature-name standard). Whole tokens only; the point
# sources mostly spell the words out ("North 15th Avenue"), and the engine rewrites both ways.
TYPE_WORDS = {
    "Aly": "Alley", "Ave": "Avenue", "Blvd": "Boulevard", "Byp": "Bypass", "Cir": "Circle",
    "Cres": "Crescent", "Ct": "Court", "Cv": "Cove", "Dr": "Drive", "Expy": "Expressway",
    "Ext": "Extension", "Exd": "Extended", "Fwy": "Freeway", "Holw": "Hollow", "Hts": "Heights",
    "Hwy": "Highway", "Jct": "Junction", "Ln": "Lane", "Mnr": "Manor", "Mt": "Mount",
    "Mtn": "Mountain", "Pkwy": "Parkway", "Pl": "Place", "Plz": "Plaza", "Pt": "Point",
    "Rd": "Road", "Rdg": "Ridge", "Rte": "Route", "Sq": "Square", "Sta": "Station", "St": "Street",
    "Ter": "Terrace", "Trce": "Trace", "Trl": "Trail", "Tpke": "Turnpike", "Vly": "Valley",
    "Vw": "View", "Xing": "Crossing", "Cnty": "County", "Co": "County", "Twp": "Township",
}
# Direction words only at the start or the end of the name ("N Main St", "15th Ave NW").
DIRECTION_WORDS = {
    "N": "North", "S": "South", "E": "East", "W": "West", "NE": "Northeast", "NW": "Northwest",
    "SE": "Southeast", "SW": "Southwest",
}


def expand_name(name: str) -> str:
    """Spell out the Census abbreviations of one FULLNAME ("Co Rd 47 S Exd" -> "County Road 47 South Extended")."""
    toks = name.split()
    out = []
    last = len(toks) - 1
    # ... or right before a trailing type word ("Co Rd 47 S Exd").
    tail = last - 1 if last > 0 and toks[last] in TYPE_WORDS else last
    for i, tok in enumerate(toks):
        if tok in DIRECTION_WORDS and i in (0, last, tail):
            out.append(DIRECTION_WORDS[tok])
        elif tok in TYPE_WORDS:
            out.append(TYPE_WORDS[tok])
        else:
            out.append(tok)
    return " ".join(out)


def _parse_args(argv: list[str] | None) -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("zip_dir", type=pathlib.Path)
    p.add_argument("geonames_postal", type=pathlib.Path)
    p.add_argument("us_norm", type=pathlib.Path)
    p.add_argument("out_parquet", type=pathlib.Path)
    p.add_argument("--min-green", type=int, default=50,
                   help="skip ZIPs with at least this many point addresses (default 50)")
    p.add_argument("--min-green-cell", type=int, default=10,
                   help="skip range points in grid cells with at least this many point addresses (default 10)")
    p.add_argument("--step-m", type=float, default=200.0,
                   help="largest distance between two range points of one side (default 200 m)")
    p.add_argument("--work", type=pathlib.Path, default=None,
                   help="directory for the per-county point files (default <out>.points/)")
    p.add_argument("--limit", type=int, default=0, help="process only the first N files (tests)")
    p.add_argument("--manifest", type=pathlib.Path, default=None,
                   help="rewrite this sheet manifest with the range layer in its provenance")
    return p.parse_args(argv)


def county_points_sql(zip_path: pathlib.Path, step_m: float) -> str:
    """Range points of one county file: side, numbers, ZIP, parity and the sampled geometry."""
    shp = zip_path.name[:-4] + ".shp"
    fips = zip_path.name.split("_")[2]
    src = f"ST_Read('/vsizip/{zip_path}/{shp}')"
    # One row per side with a numeric range; the number grows with the fraction of the edge
    # length in the digitising direction, as the Census documents the from/to numbers.
    sides = f"""
        SELECT TLID, FULLNAME, 'L' AS side, TRY_CAST(LFROMHN AS BIGINT) AS hn_from,
               TRY_CAST(LTOHN AS BIGINT) AS hn_to, ZIPL AS zip, PARITYL AS parity, geom
        FROM {src}
        UNION ALL
        SELECT TLID, FULLNAME, 'R', TRY_CAST(RFROMHN AS BIGINT), TRY_CAST(RTOHN AS BIGINT),
               ZIPR, PARITYR, geom
        FROM {src}"""
    # Edge length in metres through an equal-area projection of the lower 48 (EPSG:5070): the
    # spheroid length returned NaN for every edge of 1 583 county files (those declaring the
    # NAD83 CRS on the geometry, Los Angeles and San Diego among them) and the NaN filter then
    # dropped half the country silently (22.10.2026). The projected length agrees with the
    # vertex-wise haversine sum on the files where both exist; Alaska and Hawaii are only
    # distorted, and the length only sets the number of samples per edge.
    length = ("ST_Length(ST_Transform(ST_GeomFromWKB(ST_AsWKB(geom)), 'EPSG:4269', 'EPSG:5070', "
              "always_xy := true))")
    return f"""
        WITH sides AS ({sides}),
        ranges AS (
            SELECT *, {length} AS len_m,
                   greatest(1, CAST(ceil({length} / {step_m}) AS INTEGER)) AS n
            FROM sides
            WHERE hn_from IS NOT NULL AND hn_to IS NOT NULL
              AND hn_from BETWEEN 0 AND {MAX_HOUSE_NUMBER} AND hn_to BETWEEN 0 AND {MAX_HOUSE_NUMBER}
              AND FULLNAME IS NOT NULL AND trim(FULLNAME) <> '' AND NOT ST_IsEmpty(geom)
              AND {length} IS NOT NULL AND NOT isnan({length}) AND {length} > 0
        ),
        pts AS (
            SELECT TLID, FULLNAME, side, zip, parity, len_m, k, n, hn_from, hn_to,
                   hn_from + (hn_to - hn_from) * k / n AS raw_hn,
                   ST_LineInterpolatePoint(geom, k / n) AS pt
            FROM ranges, generate_series(0, n) AS g(k)
        )
        SELECT '{fips}' AS county_fips, TLID AS tlid, FULLNAME AS fullname, side, zip,
               k AS seq, n AS seq_count, len_m,
               -- keep the parity of the side's own numbers (E / O); mixed sides stay linear
               CASE WHEN parity IN ('E', 'O') AND k > 0 AND k < n
                         AND CAST(round(raw_hn) AS BIGINT) % 2 <> hn_from % 2
                    THEN CAST(round(raw_hn) AS BIGINT) + CASE WHEN hn_to >= hn_from THEN 1 ELSE -1 END
                    ELSE CAST(round(raw_hn) AS BIGINT) END AS numero,
               ST_X(pt) AS lon, ST_Y(pt) AS lat
        FROM pts
        WHERE pt IS NOT NULL"""


def extract_points(con, zip_dir: pathlib.Path, work: pathlib.Path, step_m: float,
                   limit: int = 0) -> dict:
    """Write one Parquet of range points per county file into `work`; skip files already done."""
    files = sorted(zip_dir.glob("tl_*_addrfeat.zip"))
    if limit:
        files = files[:limit]
    work.mkdir(parents=True, exist_ok=True)
    done = 0
    failed: list[str] = []
    t0 = time.time()
    for i, zp in enumerate(files, 1):
        out = work / (zp.stem + ".parquet")
        if out.exists():
            continue
        tmp = out.with_suffix(".parquet.tmp")
        try:
            con.execute(f"COPY ({county_points_sql(zp, step_m)}) TO '{tmp}' (FORMAT parquet)")
        except Exception as exc:  # a truncated or corrupt download: named, not silently skipped
            failed.append(f"{zp.name}: {str(exc).splitlines()[0][:120]}")
            continue
        tmp.replace(out)
        done += 1
        if i % 100 == 0:
            print(f"  {i}/{len(files)} counties, {time.time() - t0:.0f} s", flush=True)
    return {"files": len(files), "files_converted": done, "files_failed": failed}


def build_canon(con, work: pathlib.Path, geonames_postal: pathlib.Path, us_norm: pathlib.Path,
                out_parquet: pathlib.Path, min_green: int, min_green_cell: int = 10) -> dict:
    """Join the range points with the ZIP place names and the point sheet; write the canon rows."""
    stats: dict = {}
    con.execute(f"CREATE VIEW pts AS SELECT * FROM read_parquet('{work}/*.parquet')")
    stats["range_points"] = con.execute("SELECT count(*) FROM pts").fetchone()[0]
    # GeoNames postal codes: country, postal_code, place_name, admin_name1, admin_code1, ...
    con.execute(f"""CREATE TABLE zip_place AS
        SELECT column01 AS zip, any_value(column02) AS place, any_value(column04) AS state
        FROM read_csv('{geonames_postal}', delim='\t', header=false, quote='', all_varchar=true)
        WHERE column00 = 'US' GROUP BY 1""")
    # ZIPs already complete in the point sheet, and the green localities with their centroids.
    con.execute(f"""CREATE TABLE green_zip AS
        SELECT code_postal AS zip, count(*) AS n FROM '{us_norm}'
        WHERE code_postal <> '' AND code_postal <> '0' GROUP BY 1 HAVING count(*) >= {min_green}""")
    con.execute(f"""CREATE TABLE green_loc AS
        SELECT code_insee, any_value(nom_commune_norm) AS nom_commune_norm,
               avg(lat) AS lat, avg(lon) AS lon FROM '{us_norm}' GROUP BY 1""")
    stats["green_zips_skipped"] = con.execute("SELECT count(*) FROM green_zip").fetchone()[0]
    con.execute(f"""CREATE TABLE green_cell AS
        SELECT CAST(floor(lat / {CELL_DEG}) AS INTEGER) AS gy, CAST(floor(lon / {CELL_DEG}) AS INTEGER) AS gx,
               count(*) AS n FROM '{us_norm}' GROUP BY 1, 2 HAVING count(*) >= {min_green_cell}""")
    stats["green_cells_skipped"] = con.execute("SELECT count(*) FROM green_cell").fetchone()[0]
    max_code = con.execute("SELECT max(CAST(substr(code_insee, 2) AS INTEGER)) FROM green_loc").fetchone()[0]

    # Spell out the abbreviations once per distinct name (about a million names).
    names = [r[0] for r in con.execute("SELECT DISTINCT fullname FROM pts").fetchall()]
    con.execute("CREATE TABLE name_map (fullname VARCHAR, nom_voie VARCHAR)")
    con.executemany("INSERT INTO name_map VALUES (?, ?)", [(n, expand_name(n)) for n in names])
    stats["distinct_names"] = len(names)

    con.execute(f"""CREATE TABLE base AS
        SELECT p.*, m.nom_voie, z.place AS nom_commune_src, z.state AS zip_state,
               {NORM.format(col='m.nom_voie')} AS nom_voie_norm,
               {NORM.format(col='z.place')} AS nom_commune_norm,
               coalesce(z.state, '') AS state
        FROM pts p
        JOIN name_map m USING (fullname)
        LEFT JOIN zip_place z ON z.zip = p.zip
        WHERE p.zip IS NOT NULL AND p.zip <> ''
          AND p.zip NOT IN (SELECT zip FROM green_zip)
          AND NOT EXISTS (SELECT 1 FROM green_cell c
                          WHERE c.gy = CAST(floor(p.lat / {CELL_DEG}) AS INTEGER)
                            AND c.gx = CAST(floor(p.lon / {CELL_DEG}) AS INTEGER))""")
    stats["rows_zip_empty"] = con.execute(
        "SELECT count(*) FROM pts WHERE zip IS NULL OR zip = ''").fetchone()[0]
    stats["rows_in_green_zip"] = con.execute(
        "SELECT count(*) FROM pts WHERE zip IN (SELECT zip FROM green_zip)").fetchone()[0]
    stats["rows_in_green_cell"] = con.execute(f"""SELECT count(*) FROM pts p
        WHERE p.zip NOT IN (SELECT zip FROM green_zip) AND EXISTS (SELECT 1 FROM green_cell c
              WHERE c.gy = CAST(floor(p.lat / {CELL_DEG}) AS INTEGER)
                AND c.gx = CAST(floor(p.lon / {CELL_DEG}) AS INTEGER))""").fetchone()[0]
    stats["rows_zip_unknown"] = con.execute(
        "SELECT count(*) FROM base WHERE nom_commune_src IS NULL").fetchone()[0]
    con.execute("DELETE FROM base WHERE nom_commune_src IS NULL OR nom_voie_norm = ''")

    # Locality code: the nearest same-named green locality within NEAR_KM, else a new code per
    # (name, state) after the last green code.
    con.execute(f"""CREATE TABLE loc_key AS
        SELECT nom_commune_norm, state, any_value(nom_commune_src) AS nom_commune,
               avg(lat) AS lat, avg(lon) AS lon
        FROM base GROUP BY 1, 2""")
    con.execute(f"""CREATE TABLE loc_code AS
        WITH cand AS (
            SELECT k.nom_commune_norm, k.state, g.code_insee,
                   2 * 6371 * asin(sqrt(pow(sin(radians(g.lat - k.lat) / 2), 2)
                       + cos(radians(k.lat)) * cos(radians(g.lat))
                       * pow(sin(radians(g.lon - k.lon) / 2), 2))) AS km
            FROM loc_key k JOIN green_loc g USING (nom_commune_norm)
        ),
        best AS (
            SELECT nom_commune_norm, state, arg_min(code_insee, km) AS code_insee, min(km) AS km
            FROM cand GROUP BY 1, 2
        )
        SELECT k.nom_commune_norm, k.state, k.nom_commune,
               CASE WHEN b.km <= {NEAR_KM} THEN b.code_insee END AS green_code
        FROM loc_key k LEFT JOIN best b USING (nom_commune_norm, state)""")
    con.execute(f"""CREATE TABLE loc_final AS
        SELECT nom_commune_norm, state, nom_commune,
               coalesce(green_code, 'L' || lpad(CAST({max_code} + dense_rank()
                   OVER (ORDER BY CASE WHEN green_code IS NULL THEN nom_commune_norm END,
                                  CASE WHEN green_code IS NULL THEN state END) AS VARCHAR), 6, '0'))
                   AS code_insee
        FROM loc_code""")
    stats["localities_reused"] = con.execute(
        "SELECT count(*) FROM loc_code WHERE green_code IS NOT NULL").fetchone()[0]
    stats["localities_new"] = con.execute(
        "SELECT count(*) FROM loc_code WHERE green_code IS NULL").fetchone()[0]

    con.execute(f"""COPY (
        SELECT 'tiger:' || b.tlid || ':' || b.side || ':' || b.seq AS id,
               CAST(b.numero AS INTEGER) AS numero, '{RANGE_MARKER}' AS rep,
               {TITLE.format(col='b.nom_voie')} AS nom_voie, b.nom_voie_norm,
               b.zip AS code_postal, b.zip AS code_postal_display,
               l.code_insee, l.nom_commune, b.nom_commune_norm,
               '' AS provincia_norm,
               b.nom_voie_norm || '|' || l.code_insee AS street_key,
               b.lon, b.lat
        FROM base b JOIN loc_final l USING (nom_commune_norm, state)
        WHERE b.numero BETWEEN 0 AND {MAX_HOUSE_NUMBER}
    ) TO '{out_parquet}' (FORMAT parquet, COMPRESSION zstd)""")
    stats["rows_out"] = con.execute(f"SELECT count(*) FROM '{out_parquet}'").fetchone()[0]
    stats["streets_out"] = con.execute(
        f"SELECT count(DISTINCT street_key) FROM '{out_parquet}'").fetchone()[0]
    return stats


def main(argv: list[str] | None = None) -> None:
    args = _parse_args(argv)
    work = args.work or args.out_parquet.with_suffix(".points")
    t0 = time.time()
    con = connect_tuned()
    con.execute("INSTALL spatial; LOAD spatial;")
    stats = {"source": "TIGER/Line ADDRFEAT (US Census Bureau, public domain)",
             "zip_dir": str(args.zip_dir), "step_m": args.step_m, "min_green": args.min_green,
             "min_green_cell": args.min_green_cell, "cell_deg": CELL_DEG}
    stats.update(extract_points(con, args.zip_dir, work, args.step_m, args.limit))
    # A county file that reads but yields no range point is a conversion defect, not data: every
    # county has address ranges. Named and fatal, after half the country went missing silently.
    empty = [f.name for f in sorted(work.glob("*.parquet"))
             if con.execute(f"SELECT count(*) FROM '{f}'").fetchone()[0] == 0]
    stats["files_empty"] = empty
    if empty:
        raise SystemExit("STOP: county files converted to zero range points (conversion defect):\n  "
                         + "\n  ".join(empty[:20]) + (f"\n  … {len(empty)} in all" if len(empty) > 20 else ""))
    if stats["files_failed"]:
        raise SystemExit("STOP: county files that could not be read (re-download them):\n  "
                         + "\n  ".join(stats["files_failed"]))
    stats.update(build_canon(con, work, args.geonames_postal, args.us_norm, args.out_parquet,
                             args.min_green, args.min_green_cell))
    stats["seconds"] = round(time.time() - t0, 1)
    if args.manifest:
        from overture import address_manifest
        manifest = address_manifest("us", geonames=True, ranges=True)
        args.manifest.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    stats_path = args.out_parquet.with_suffix(".stats.json")
    stats_path.write_text(json.dumps(stats, indent=2) + "\n")
    print(json.dumps(stats, indent=2))


if __name__ == "__main__":
    main()
