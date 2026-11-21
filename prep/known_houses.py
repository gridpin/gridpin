#!/usr/bin/env python3
"""Known houses of a sheet from Overture places: the houses an Overture place stands at, by its own address.

A cityless "Długa 2" names a street that exists in hundreds of towns; the competitors pick the house the world knows
(an office, a museum, a shop at that address). This reads the same signal from Overture places (a source apart from the
truth of the sets, Wikidata): a house of the build input is known when a place within 50 m writes the same number and a
word of the street name (>3 letters) in its address line. The engine orders equal cityless homonyms with a known house
first (city_tail `known_houses`, rules class 28; query.rs), and a line without a place goes to the house with the most
notable places (a church, a museum, an office, a school: NOTABLE), then the most places (query.rs known_elsewhere).

    python3 prep/known_houses.py PL data/build_pl.csv.gz eval/reference/pl/known_houses.tsv.gz
        [--places data/pl_places.parquet]   # the cache; fetched from S3 when missing

Output: gzip TSV `<street>|<commune>\\t<number>:<places>:<notable>:<near> …` (the places an address stands at, counted,
those of a NOTABLE kind, and the NOTABLE places within the radius whatever their address), keys normalized as the engine
normalizes a display name
(prep/normalize.NORM), numbers as the sheet writes them (numero + rep). Release and licences: the places theme of the
pinned Overture release, CDLA-Permissive-2.0 / Apache-2.0 / CC0-1.0 per source (counted from `sources[].dataset` of the
data itself, 14.11.2026).
"""
import argparse
import gzip
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from duck import connect_tuned  # noqa: E402
from normalize import NORM  # noqa: E402

RELEASE = {"PL": "2026-10-21.0"}  # ANSWERS §318
BBOX = {"PL": (14.0, 24.2, 49.0, 54.9)}
RADIUS_M = 50.0
# Kinds of place (Overture `basic_category`) that make a house known beyond its street: worship, memory, the state,
# learning, care, culture, transport. Chosen by meaning, not fitted to a set; a private school, a kindergarten, a shop, a
# bank or a hairdresser is not notable (Poland, 14.11.2026: the house a cityless Wikidata line means hosts a church, a
# museum or an office; its homonym with more places hosts shops).
NOTABLE = (
    "christian_place_of_worship", "jewish_place_of_worship", "muslim_place_of_worship", "buddhist_place_of_worship",
    "hindu_place_of_worship", "place_of_worship", "religious_organization", "museum", "historic_site", "monument",
    "castle", "government_office", "government_department", "community_and_government", "courthouse",
    "police_station", "fire_station", "embassy", "library", "elementary_school", "middle_school", "high_school",
    "college_university", "hospital", "cultural_center", "theatre_venue", "art_gallery", "stadium_arena",
    "train_station", "airport", "zoo", "cemetery", "national_park",
)
CELL = 0.001  # degrees; 50 m stays within the 3x3 neighbourhood


def fetch_places(con, cc: str, out: pathlib.Path) -> None:
    xmin, xmax, ymin, ymax = BBOX[cc]
    s3 = f"s3://overturemaps-us-west-2/release/{RELEASE[cc]}/theme=places/type=place/*.parquet"
    con.execute(f"""COPY (
        SELECT id, names.primary AS name, basic_category AS cat,
               (bbox.ymin + bbox.ymax) / 2 AS lat, (bbox.xmin + bbox.xmax) / 2 AS lon,
               addresses[1].freeform AS street_line,
               list_transform(sources, s -> s.dataset) AS datasets,
               list_transform(sources, s -> s.license) AS licenses
        FROM read_parquet('{s3}', hive_partitioning=1)
        WHERE bbox.xmin BETWEEN {xmin} AND {xmax} AND bbox.ymin BETWEEN {ymin} AND {ymax}
          AND names.primary IS NOT NULL AND addresses[1].country = '{cc}'
          AND addresses[1].freeform IS NOT NULL
    ) TO '{out}' (FORMAT parquet)""")


def known_houses(con, build_csv: pathlib.Path, places: pathlib.Path) -> list[tuple[str, str]]:
    street_norm = NORM.format(col="h.nom_voie")
    commune_norm = NORM.format(col="h.nom_commune")
    line_norm = NORM.format(col="p.street_line")
    notable = ", ".join(f"'{kind}'" for kind in NOTABLE)
    rows = con.execute(f"""
        WITH h AS (
            SELECT nom_voie, nom_commune, numero, rep, lat, lon,
                   cast(floor(lat / {CELL}) AS BIGINT) AS cy, cast(floor(lon / {CELL}) AS BIGINT) AS cx
            FROM read_csv('{build_csv}', header = true, all_varchar = true,
                          columns = {{'nom_voie_norm': 'VARCHAR', 'code_insee': 'VARCHAR', 'nom_commune_norm': 'VARCHAR',
                                     'code_postal': 'VARCHAR', 'code_postal_display': 'VARCHAR', 'numero': 'VARCHAR',
                                     'rep': 'VARCHAR', 'lon': 'DOUBLE', 'lat': 'DOUBLE', 'nom_voie': 'VARCHAR',
                                     'nom_commune': 'VARCHAR', 'provincia_norm': 'VARCHAR'}})
        ),
        p AS (
            SELECT street_line, cat, lat, lon, cast(floor(lat / {CELL}) AS BIGINT) + dy AS cy,
                   cast(floor(lon / {CELL}) AS BIGINT) + dx AS cx
            FROM read_parquet('{places}'), (SELECT unnest([-1, 0, 1]) AS dy), (SELECT unnest([-1, 0, 1]) AS dx)
        ),
        m AS (
            SELECT {street_norm} AS street, {commune_norm} AS commune,
                   h.numero || coalesce(h.rep, '') AS number, count(*) AS places,
                   count(*) FILTER (WHERE p.cat IN ({notable})) AS notable  -- count_if is NULL when every kind is
            FROM h JOIN p ON h.cy = p.cy AND h.cx = p.cx
            WHERE regexp_matches({line_norm}, '(^| )' || h.numero || '( |[a-z]|$)')
              AND list_bool_or(list_transform(
                    list_filter(string_split({street_norm}, ' '), w -> length(w) > 3),
                    w -> contains({line_norm}, w)))
              AND 6371000 * 2 * asin(sqrt(pow(sin(radians(p.lat - h.lat) / 2), 2)
                    + cos(radians(h.lat)) * cos(radians(p.lat)) * pow(sin(radians(p.lon - h.lon) / 2), 2))) <= {RADIUS_M}
            GROUP BY ALL
        ),
        -- a notable place within the radius whatever its address says (a church whose line names its square): the
        -- house is the one place of its street and number with such a place nearby (query.rs known_elsewhere)
        hk AS (
            SELECT {street_norm} AS street, {commune_norm} AS commune, h.numero || coalesce(h.rep, '') AS number,
                   any_value(h.lat) AS lat, any_value(h.lon) AS lon, any_value(h.cy) AS cy, any_value(h.cx) AS cx
            FROM h GROUP BY ALL
        ),
        n AS (
            SELECT hk.street, hk.commune, hk.number, count(*) AS near
            FROM hk JOIN p ON hk.cy = p.cy AND hk.cx = p.cx
            WHERE p.cat IN ({notable})
              AND 6371000 * 2 * asin(sqrt(pow(sin(radians(p.lat - hk.lat) / 2), 2)
                    + cos(radians(hk.lat)) * cos(radians(p.lat)) * pow(sin(radians(p.lon - hk.lon) / 2), 2))) <= {RADIUS_M}
            GROUP BY ALL
        ),
        a AS (
            SELECT coalesce(m.street, n.street) AS street, coalesce(m.commune, n.commune) AS commune,
                   coalesce(m.number, n.number) AS number, coalesce(m.places, 0) AS places,
                   coalesce(m.notable, 0) AS notable, coalesce(n.near, 0) AS near
            FROM m FULL OUTER JOIN n ON m.street = n.street AND m.commune = n.commune AND m.number = n.number
        )
        SELECT street || '|' || commune AS key,
               string_agg(number || ':' || places || ':' || notable || ':' || near, ' ' ORDER BY number) AS numbers
        FROM a GROUP BY key ORDER BY key
    """).fetchall()
    return rows


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("cc")
    ap.add_argument("build_csv", type=pathlib.Path)
    ap.add_argument("out", type=pathlib.Path)
    ap.add_argument("--places", type=pathlib.Path)
    args = ap.parse_args(argv)
    cc = args.cc.upper()
    places = args.places or args.build_csv.with_name(f"{cc.lower()}_places.parquet")
    con = connect_tuned()
    t = time.time()
    if not places.exists():
        fetch_places(con, cc, places)
    rows = known_houses(con, args.build_csv, places)
    body = "".join(f"{key}\t{numbers}\n" for key, numbers in rows).encode("utf-8")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes(gzip.compress(body, mtime=0))
    houses = sum(len(n.split()) for _, n in rows)
    print(f"{args.out}: {len(rows)} streets, {houses} known houses, {len(body) / 1e6:.1f} MB raw, {time.time() - t:.0f} s")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
