#!/usr/bin/env python3
"""BAN CSV -> normalized Parquet + stats + reproducible evaluation sample.

Usage:   python3 prep/normalize.py   (from the code/ directory)
Input:   data/adresses-france.csv.gz (national BAN export, ';' delimiter)
Output:  data/ban_norm.parquet, data/stats.json, and a 20k-row evaluation sample
"""
import json
import pathlib
import sys
import time

import duckdb  # noqa: F401 (typing)
from duck import connect_tuned

CODE = pathlib.Path(__file__).resolve().parent.parent
RAW = CODE / "data" / "adresses-france.csv.gz"
OUT_PARQUET = CODE / "data" / "ban_norm.parquet"
OUT_STATS = CODE / "data" / "stats.json"
OUT_EVAL = CODE / "eval" / "eval_set.parquet"

REQUIRED = {"id", "numero", "rep", "nom_voie", "code_postal", "code_insee",
            "nom_commune", "lon", "lat"}

# lowercase + strip diacritics + punctuation to spaces + collapse whitespace
NORM = ("trim(regexp_replace(regexp_replace(replace(strip_accents(lower({col})), 'đ', 'd'),"
        " '[-''’`./,;()ʻʼ‘]', ' ', 'g'), ' +', ' ', 'g'))")  # đ→d: strip_accents does not cover it


def main() -> None:
    if not RAW.exists():
        sys.exit(f"missing {RAW} — download BAN first")
    t0 = time.time()
    con = connect_tuned()

    con.execute(f"""
        CREATE VIEW raw AS
        SELECT * FROM read_csv('{RAW}', delim=';', header=true,
                               sample_size=200000, ignore_errors=true)
    """)
    cols = {r[0] for r in con.execute("DESCRIBE raw").fetchall()}
    missing = REQUIRED - cols
    if missing:
        sys.exit(f"CSV is missing expected columns: {missing}; present: {sorted(cols)}")

    total_raw = con.execute("SELECT count(*) FROM raw").fetchone()[0]

    norm_voie = NORM.format(col="nom_voie")
    norm_commune = NORM.format(col="nom_commune")
    con.execute(f"""
        CREATE TABLE ban AS
        SELECT
            id,
            TRY_CAST(numero AS INTEGER)              AS numero,
            nullif(lower(trim(rep)), '')             AS rep,
            nom_voie,
            {norm_voie}                              AS nom_voie_norm,
            CAST(code_postal AS VARCHAR)             AS code_postal,
            CAST(code_insee AS VARCHAR)              AS code_insee,
            nom_commune,
            {norm_commune}                           AS nom_commune_norm,
            {norm_voie} || '|' || CAST(code_insee AS VARCHAR) AS street_key,
            CAST(lon AS DOUBLE)                      AS lon,
            CAST(lat AS DOUBLE)                      AS lat
        FROM raw
        WHERE nom_voie IS NOT NULL AND trim(nom_voie) <> ''
          AND lon IS NOT NULL AND lat IS NOT NULL
          AND TRY_CAST(numero AS INTEGER) IS NOT NULL
    """)

    kept = con.execute("SELECT count(*) FROM ban").fetchone()[0]
    stats = {
        "date": time.strftime("%Y-%m-%d %H:%M"),
        "rows_raw": total_raw,
        "rows_kept": kept,
        "rows_dropped": total_raw - kept,
        "communes": con.execute("SELECT count(DISTINCT code_insee) FROM ban").fetchone()[0],
        "streets": con.execute("SELECT count(DISTINCT street_key) FROM ban").fetchone()[0],
        "postcodes": con.execute("SELECT count(DISTINCT code_postal) FROM ban").fetchone()[0],
        "lat_min_max": con.execute("SELECT round(min(lat),3), round(max(lat),3) FROM ban").fetchone(),
        "lon_min_max": con.execute("SELECT round(min(lon),3), round(max(lon),3) FROM ban").fetchone(),
        "houses_per_street_avg": round(kept / max(1, con.execute(
            "SELECT count(DISTINCT street_key) FROM ban").fetchone()[0]), 1),
    }

    con.execute(f"COPY ban TO '{OUT_PARQUET}' (FORMAT parquet, COMPRESSION zstd)")

    OUT_EVAL.parent.mkdir(parents=True, exist_ok=True)
    con.execute(f"""
        COPY (SELECT * FROM ban USING SAMPLE reservoir(20000 ROWS) REPEATABLE (42))
        TO '{OUT_EVAL}' (FORMAT parquet, COMPRESSION zstd)
    """)

    stats["parquet_mb"] = round(OUT_PARQUET.stat().st_size / 1e6, 1)
    stats["seconds"] = round(time.time() - t0, 1)
    OUT_STATS.write_text(json.dumps(stats, ensure_ascii=False, indent=2))
    # provenance manifest -> SEC_META of the sheet (v6)
    import datetime
    ban = CODE / "data" / "adresses-france.csv.gz"
    manifest = {
        "country": "fr",
        "layer": "addresses",
        "license": "Licence Ouverte 2.0 (etalab)",
        "sources": "BAN - Base Adresse Nationale (national registry of France)",
        "source_release": datetime.date.fromtimestamp(ban.stat().st_mtime).isoformat()
                          if ban.exists() else "",
        "attribution": "(c) DINUM / IGN - Base Adresse Nationale, Licence Ouverte 2.0",
    }
    (CODE / "data" / "fr_manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2))
    print(json.dumps(stats, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
