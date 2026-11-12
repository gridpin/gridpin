"""A world sheet of populated places from Overture divisions: the friendly table of `gridpin build --places`.

    python3 prep/overture_world_places.py divisions_places__<release>.parquet world_places.csv [--min-population N]

Rows: localities (towns, villages) of every country: name, lat, lon, group (the region, ISO 3166-2, which begins with the
country code), aliases (other names, `|`). LICENCE: Overture divisions are almost all OpenStreetMap (ODbL-1.0; on
2026-10-21.0 5 481 156 of 5 537 138 source records), so the sheet built from this table is ODbL: it carries that
licence and is never merged into a country sheet; it opens beside one (`--also-after`).
"""
import argparse
import csv
import re
import sys
import unicodedata

import duckdb

LICENSE = "ODbL-1.0 (Overture Maps divisions, from OpenStreetMap; CC-BY-4.0 parts: Esri Community Maps, LINZ)"


def rows(parquet, min_population=0):
    con = duckdb.connect()
    # the friendly table answers `code` like a name, so the country stays out of it (it is the head of the region code);
    # one name per region: homonyms of one region keep the most populous (the table refuses two equal names in a group)
    q = f"""SELECT name, lat, lon, coalesce(region, country, '') AS grp, '' AS code,
                   array_to_string(list_distinct(list_filter(coalesce(names_common, []), x -> x <> name)), '|') AS aliases
            FROM read_parquet(?) WHERE subtype = 'locality' AND name IS NOT NULL AND length(name) BETWEEN 1 AND 80
              AND coalesce(population, 0) >= ?
            QUALIFY row_number() OVER (PARTITION BY lower(strip_accents(name)), coalesce(region, country, '')
                                       ORDER BY coalesce(population, 0) DESC, id) = 1
            ORDER BY grp, coalesce(population, 0) DESC, name"""
    return con.execute(q, [parquet, min_population]).fetchall()


def norm(text):
    """As the engine reads a name (norm.rs): lower case, marks stripped, đ folded, punctuation to space."""
    t = "".join(c for c in unicodedata.normalize("NFD", text.lower()) if not unicodedata.combining(c))
    t = t.replace("đ", "d")
    return " ".join(re.sub(r"[^\w]+", " ", t).split())


def unique(data):
    """One name per group as the engine reads it: a later row whose name is taken is dropped, an alias taken is
    left out (the rows come most populous first within a group)."""
    taken, out = set(), []
    for name, lat, lon, grp, code, aliases in data:
        key = (norm(name), grp)
        if not key[0] or key in taken:
            continue
        taken.add(key)
        kept = []
        for a in (aliases or "").split("|"):
            k = (norm(a), grp)
            if a and k[0] and k not in taken:
                taken.add(k)
                kept.append(a.replace("|", " "))
        out.append((name, lat, lon, grp, code, "|".join(kept)[:2000]))
    return out


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("parquet")
    p.add_argument("out")
    p.add_argument("--min-population", type=int, default=0)
    a = p.parse_args(argv)
    data = unique(rows(a.parquet, a.min_population))
    with open(a.out, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["name", "lat", "lon", "group", "code", "aliases"])
        for name, lat, lon, grp, code, aliases in data:
            # aliases are names; a stray separator inside a name would split it
            w.writerow([name.replace("|", " "), f"{lat:.6f}", f"{lon:.6f}", grp, code, aliases])
    print(f"{len(data)} places -> {a.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
