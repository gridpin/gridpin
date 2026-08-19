#!/usr/bin/env python3
"""OSM adapter: country .osm.pbf extract -> canonical Parquet.

Third adapter type (gap-filling layer; ODbL license — store separately).
Takes address nodes + building outlines (centroid over outline points).
Locality: addr:city → addr:place → addr:district → addr:suburb.

Usage: python3 prep/osm.py MC data/monaco-latest.osm.pbf   (run from code/)
"""
import datetime
import json
import os
import pathlib
import sys
import time

import duckdb  # noqa: F401 (types)
from duck import connect_tuned

CODE = pathlib.Path(__file__).resolve().parent.parent

NORM = ("trim(regexp_replace(regexp_replace(replace(strip_accents(lower({col})), 'đ', 'd'),"
        " '[-''’`./,;()ʻʼ‘]', ' ', 'g'), ' +', ' ', 'g'))")  # đ→d: strip_accents does not strip it


def write_canon(con, tile, out_parquet):
    """Final step: build canon from the bs+prov tables (already in the connection) ->
    norm parquet; returns the kept row count. Split out of main so the final step can be
    replayed from a bs/prov checkpoint without re-reading the pbf. code_insee is computed
    via dense_rank over DISTINCT (commune, geo cell) keys, NOT over all rows — otherwise
    the window sort holds tens of millions of rows in memory and fails on large countries."""
    norm_voie = NORM.format(col="nom_voie")
    norm_commune = NORM.format(col="nom_commune")
    con.execute(f"""
        CREATE TABLE canon AS
        WITH typed AS (
            SELECT b.nom_voie,
                   {norm_voie} AS nom_voie_norm,
                   -- first digit group ANYWHERE: letter-prefixed house numbers
                   -- (e.g. "k1801", "vl27") must still yield a numeric part, not NULL
                   TRY_CAST(regexp_extract(b.number_raw, '[0-9]+') AS INTEGER) AS numero,
                   nullif(lower(regexp_replace(
                       CASE WHEN regexp_matches(b.number_raw, '^[0-9]')
                            THEN regexp_extract(b.number_raw, '^[0-9]+(.*)$', 1)
                            ELSE regexp_extract(b.number_raw, '^([^0-9]{1,4})', 1) END,
                       '[\\s\\-/\\.ʻʼ‘]', '', 'g')), '')                AS rep,
                   regexp_extract(b.postcode_raw, '[0-9]+')             AS code_postal,
                   b.postcode_raw, b.nom_commune,
                   {norm_commune} AS nom_commune_norm,
                   {NORM.format(col='pr.provincia')} AS provincia_norm,
                   b.lat, b.lon
            FROM bs b JOIN prov pr ON pr.rid = b.rid
            WHERE b.nom_voie IS NOT NULL AND b.nom_commune <> ''
        ),
        flt AS (
            -- garbage filter: absurd house numbers (parse artifacts), one-letter street names
            SELECT * FROM typed
            WHERE numero IS NOT NULL AND numero BETWEEN 1 AND 99999
              AND length(nom_voie_norm) >= 2
        ),
        keys AS (  -- DISTINCT (commune, 2-deg geo cell): the window is cheap on these ~1e5 keys
            SELECT DISTINCT nom_commune_norm,
                   coalesce(CAST(round(lat/2) AS INTEGER), 999999) AS bla2,
                   coalesce(CAST(round(lon/2) AS INTEGER), 999999) AS blo2
            FROM flt
        ),
        keyed AS (  -- 2-deg geo cell splits same-name communes; '{tile}' prefix is globally unique
            SELECT nom_commune_norm, bla2, blo2,
                   'L' || '{tile}' || lpad(CAST(dense_rank() OVER (
                       ORDER BY nom_commune_norm, bla2, blo2) AS VARCHAR), 6, '0') AS code_insee
            FROM keys
        ),
        loc AS (
            SELECT f.*, k.code_insee
            FROM flt f JOIN keyed k
              ON k.nom_commune_norm = f.nom_commune_norm
             AND k.bla2 = coalesce(CAST(round(f.lat/2) AS INTEGER), 999999)
             AND k.blo2 = coalesce(CAST(round(f.lon/2) AS INTEGER), 999999)
        )
        SELECT numero, rep, nom_voie, nom_voie_norm,  -- no id column: unused, and a row-level window would waste temp space
               coalesce(code_postal, '0') AS code_postal,
               postcode_raw AS code_postal_display,
               code_insee, nom_commune, nom_commune_norm, provincia_norm,
               nom_voie_norm || '|' || code_insee AS street_key,
               lon, lat
        FROM loc
    """)
    for _t in ("tg", "nearest", "bs", "prov", "street_alias", "osm_keys"):
        try:  # bs may be a VIEW when resuming from a checkpoint, others are TABLEs; drop either kind
            con.execute(f"DROP TABLE IF EXISTS {_t}")
        except Exception:
            con.execute(f"DROP VIEW IF EXISTS {_t}")
    kept = con.execute("SELECT count(*) FROM canon").fetchone()[0]
    con.execute(f"COPY canon TO '{out_parquet}' (FORMAT parquet, COMPRESSION zstd)")
    return kept


def main() -> None:
    if len(sys.argv) not in (3, 4, 5):
        sys.exit("usage: prep/osm.py <COUNTRY_CODE> <file.osm.pbf> [geonames.txt] [overture_poi.parquet]")
    cc = sys.argv[1].upper()
    pbf = pathlib.Path(sys.argv[2])
    # Tile mode (GRIDPIN_TILE=1..9): build large countries in batches from regional extracts
    # to fit in RAM. The prefix keeps code_insee globally unique across tiles when merging
    # the norm parquets (insee is 8 bytes: "L" + tile + 6 digits). Empty → single-pass mode.
    tile = os.environ.get("GRIDPIN_TILE", "")
    # GRIDPIN_NO_OLDNAME=1 — build WITHOUT old_name (to diagnose or avoid old_name collisions).
    old_arr = "" if os.environ.get("GRIDPIN_NO_OLDNAME", "") else ", tags['old_name']"
    # Optional GeoNames file → umbrella-city aliases: multiple spellings of a city
    # (e.g. toshkent/tashkent) collapse into one alias, and sub-city neighbourhoods
    # become findable by city name. Without it behavior is unchanged.
    geonames_file = sys.argv[3] if len(sys.argv) >= 4 else None
    # Optional permissive Overture places layer (CDLA-Permissive): house addresses for
    # regions where OSM is sparse. Merging permissive data INTO the ODbL artifact is
    # allowed; the attribution note is recorded in the stats output.
    overture_poi = sys.argv[4] if len(sys.argv) >= 5 else None
    out_parquet = CODE / "data" / f"{cc.lower()}_norm.parquet"
    t0 = time.time()
    con = connect_tuned()
    con.execute("INSTALL spatial; LOAD spatial;")

    # address nodes
    con.execute(f"""
        CREATE TABLE a_nodes AS
        SELECT tags, lat, lon FROM st_readosm('{pbf}')
        WHERE kind = 'node' AND tags['addr:housenumber'] IS NOT NULL
    """)
    # address buildings (frugal: do NOT materialize every node in the country —
    # only nodes referenced by address outlines; for large countries this is the
    # difference between hundreds of millions of rows and tens of millions)
    con.execute(f"""
        CREATE TABLE a_w AS
        SELECT id, tags, refs FROM st_readosm('{pbf}')
        WHERE kind = 'way' AND tags['addr:housenumber'] IS NOT NULL
    """)
    con.execute("CREATE TABLE wref AS SELECT DISTINCT unnest(refs) AS ref FROM a_w")
    con.execute(f"""
        CREATE TABLE wnodes AS
        SELECT id, lat, lon FROM st_readosm('{pbf}')
        WHERE kind = 'node' AND id IN (SELECT ref FROM wref)
    """)
    con.execute("""
        CREATE TABLE a_ways AS
        WITH pts AS (
            SELECT u.id, n.lat, n.lon
            FROM (SELECT id, unnest(refs) AS ref FROM a_w) u
            JOIN wnodes n ON n.id = u.ref
        )
        SELECT any_value(w.tags) AS tags, avg(pts.lat) AS lat, avg(pts.lon) AS lon
        FROM pts JOIN a_w w ON w.id = pts.id
        GROUP BY pts.id
    """)
    con.execute("DROP TABLE wnodes; DROP TABLE wref; DROP TABLE a_w;")

    # Relation addresses (building multipolygons: malls, museums, campuses). st_readosm
    # returns relations with refs (member ids, member type not given). The centroid is
    # computed ONLY over nodes of member ways (buildings are way members) and as a MEDIAN:
    # node and way ids in OSM overlap numerically, so a stray misidentified node cannot
    # skew the median. Merged into a_ways — downstream stages treat them the same.
    con.execute(f"""
        CREATE TABLE a_r AS
        SELECT id, tags, refs FROM st_readosm('{pbf}')
        WHERE kind = 'relation' AND tags['addr:housenumber'] IS NOT NULL
    """)
    con.execute("CREATE TABLE rref AS SELECT id AS rel, unnest(refs) AS ref FROM a_r")
    con.execute(f"""
        CREATE TABLE rways AS SELECT id, refs FROM st_readosm('{pbf}')
        WHERE kind = 'way' AND id IN (SELECT ref FROM rref)
    """)
    con.execute("CREATE TABLE rwref AS SELECT id AS way, unnest(refs) AS ref FROM rways")
    con.execute(f"""
        CREATE TABLE rnodes AS SELECT id, lat, lon FROM st_readosm('{pbf}')
        WHERE kind = 'node' AND id IN (SELECT ref FROM rwref)
    """)
    con.execute("""
        INSERT INTO a_ways
        WITH pts AS (
            SELECT rr.rel, n.lat, n.lon
            FROM rref rr JOIN rwref wr ON wr.way = rr.ref JOIN rnodes n ON n.id = wr.ref
        )
        SELECT any_value(r.tags) AS tags, median(p.lat) AS lat, median(p.lon) AS lon
        FROM pts p JOIN a_r r ON r.id = p.rel GROUP BY p.rel
    """)
    con.execute("DROP TABLE a_r; DROP TABLE rref; DROP TABLE rways; DROP TABLE rwref; DROP TABLE rnodes;")

    # Coverage enrichment from the same OSM extract (as Nominatim does): named features
    # WITHOUT house numbers → district-level points. Mappers often add names long before
    # addresses, so without this such places would be missing entirely.
    # Taken: highway/landuse=residential/building=apartments/amenity/shop/tourism + name
    # (outlines → centroid) plus place/amenity/shop/office/tourism/leisure NODES (points),
    # with a synthetic house number "1". Names are EXPLODED across variants: name +
    # name:ru + name:uz + alt_name (Nominatim indexes all of them; without the variants,
    # queries in another script miss).
    # Named POIs (shop/amenity/tourism/office/leisure) as "streets" help sparse coverage,
    # but in dense coverage they are noise: shop names leak into the street field on
    # reverse lookups. So POIs are dropped for RU and kept elsewhere; real streets
    # (highway/residential/building) and districts (place=*) are always kept.
    poi = cc != "RU"
    way_poi = (" OR tags['amenity'] IS NOT NULL OR tags['shop'] IS NOT NULL"
               " OR tags['tourism'] IS NOT NULL") if poi else ""
    node_poi = (" OR tags['amenity'] IS NOT NULL OR tags['shop'] IS NOT NULL"
                " OR tags['office'] IS NOT NULL OR tags['tourism'] IS NOT NULL"
                " OR tags['leisure'] IS NOT NULL") if poi else ""
    con.execute(f"""
        CREATE TABLE feat_w AS
        SELECT id, tags, refs FROM st_readosm('{pbf}')
        WHERE kind = 'way' AND tags['name'] IS NOT NULL AND tags['addr:housenumber'] IS NULL
          AND (tags['highway'] IS NOT NULL OR tags['landuse'] = 'residential'
               OR tags['building'] IN ('apartments', 'residential'){way_poi})
    """)
    con.execute("CREATE TABLE fref AS SELECT DISTINCT unnest(refs) AS ref FROM feat_w")
    con.execute(f"""
        CREATE TABLE fnodes AS
        SELECT id, lat, lon FROM st_readosm('{pbf}')
        WHERE kind = 'node' AND id IN (SELECT ref FROM fref)
    """)
    con.execute("""
        CREATE TABLE feat_centroids AS
        WITH pts AS (
            SELECT u.id, n.lat, n.lon
            FROM (SELECT id, unnest(refs) AS ref FROM feat_w) u
            JOIN fnodes n ON n.id = u.ref
        )
        SELECT any_value(w.tags) AS tags, avg(pts.lat) AS lat, avg(pts.lon) AS lon
        FROM pts JOIN feat_w w ON w.id = pts.id GROUP BY pts.id
    """)
    con.execute(f"""
        CREATE TABLE feat_all AS
        SELECT tags, lat, lon FROM feat_centroids
        UNION ALL
        SELECT tags, lat, lon FROM st_readosm('{pbf}')
        WHERE kind = 'node' AND tags['name'] IS NOT NULL AND tags['addr:housenumber'] IS NULL
          AND (tags['place'] IN ('neighbourhood','quarter','suburb','hamlet','isolated_dwelling'){node_poi})
    """)
    # explode name variants → (nm, lat, lon)
    con.execute(f"""
        CREATE TABLE a_streets AS
        SELECT unnest(list_distinct(list_filter(
            [tags['name'], tags['name:ru'], tags['name:uz'], tags['alt_name']{old_arr}],
            x -> x IS NOT NULL AND length(x) BETWEEN 2 AND 80))) AS nm, lat, lon
        FROM feat_all
    """)
    # Geo-anchored name-alias map: each (name, variant) pair carries the ~0.33-deg cell
    # of its source way, so the stitching below stays local (±1 cell ≈ ±35 km). Without
    # the anchor, a name-only join would apply variants of one street to ALL same-named
    # streets country-wide, cloning houses onto phantom streets and polluting reverse
    # lookups. Built from feat_centroids (centroids already computed) — saves a pbf read.
    con.execute(f"""
        CREATE TABLE street_alias AS
        SELECT DISTINCT lat_norm, cyr, gla, glo FROM (
            SELECT {NORM.format(col="tags['name']")} AS lat_norm,
                   unnest(list_distinct(list_filter(
                       [tags['name:ru'], tags['name:uz'], tags['alt_name']{old_arr}],
                       x -> x IS NOT NULL AND length(x) BETWEEN 2 AND 80))) AS cyr,
                   CAST(round(lat*3) AS BIGINT) AS gla,
                   CAST(round(lon*3) AS BIGINT) AS glo
            FROM feat_centroids
            WHERE tags['name'] IS NOT NULL
              AND (tags['highway'] IS NOT NULL OR tags['name:ru'] IS NOT NULL)
        ) WHERE lat_norm <> '' AND {NORM.format(col='cyr')} <> lat_norm
    """)
    con.execute("DROP TABLE fnodes; DROP TABLE fref; DROP TABLE feat_w; DROP TABLE feat_centroids; DROP TABLE feat_all;")
    # Anti-join BY NAME VARIANT: drop variants already present in real addresses (so the
    # synthetic house "1" cannot pollute a street that has real houses); a variant in
    # another script (e.g. a Cyrillic name for a Latin addr:street) is NEW and is kept —
    # that is what lets cross-script queries reach these streets.
    con.execute("""CREATE TABLE a_streets2 AS
        SELECT s.* FROM a_streets s
        WHERE lower(s.nm) NOT IN (
            SELECT DISTINCT lower(tags['addr:street'])
            FROM (SELECT tags FROM a_nodes UNION ALL SELECT tags FROM a_ways)
            WHERE tags['addr:street'] IS NOT NULL)""")
    con.execute("DROP TABLE a_streets; ALTER TABLE a_streets2 RENAME TO a_streets;")
    total = con.execute("SELECT (SELECT count(*) FROM a_nodes) + (SELECT count(*) FROM a_ways) + (SELECT count(*) FROM a_streets)").fetchone()[0]

    # localities for addresses without a city tag: nearest place in a 0.1-deg grid, ±1 cell
    con.execute(f"""
        CREATE TABLE places AS
        SELECT tags['name'] AS nm, lat, lon,
               CAST(round(lat*10) AS BIGINT) AS bla, CAST(round(lon*10) AS BIGINT) AS blo
        FROM st_readosm('{pbf}')
        WHERE kind = 'node' AND tags['place'] IN ('city','town','village')
          AND tags['name'] IS NOT NULL
    """)

    # Name stitching: map latin street name → its Cyrillic variant (name:ru/name:uz/
    # alt_name) from OSM highway features. Real houses sit under the latin addr:street
    # ("Chaykovskiy ko'chasi"), so a query in the other script does not match and degrades
    # to a district point. Via this map, every real house is duplicated under the Cyrillic
    # street name with the SAME geometry and number — house-level results for cross-script
    # queries. (the map is built above from feat_centroids, geo-anchored)

    # Checkpoint: dump all pbf-derived tables (the product of 8 pbf scans) to parquet
    # BEFORE the heavy joins (nearest/u1/u2 are where OOM happens). If a join or the
    # final step fails, it can be replayed from the checkpoint in minutes without
    # re-reading the pbf. Enabled with GRIDPIN_CK=1.
    if os.environ.get("GRIDPIN_CK"):
        ckdir = CODE / "data" / "ck_pbf"
        ckdir.mkdir(exist_ok=True)
        for _t in ("a_nodes", "a_ways", "a_streets", "places", "street_alias"):
            _p = ckdir / f"{cc.lower()}_{_t}.parquet"
            con.execute(f"COPY {_t} TO '{_p}' (FORMAT parquet, COMPRESSION zstd)")
        print(f"[ck] checkpoint saved: pbf tables → data/{ckdir.name}/ (joins now read from it)", flush=True)
    norm_voie = NORM.format(col="nom_voie")
    norm_commune = NORM.format(col="nom_commune")
    norm_alias = NORM.format(col="a.st")  # normalize addr:street for name stitching
    # optional permissive Overture layer: house addresses of the same shape (street, number, city, coords)
    ov_union = ""
    if overture_poi and pathlib.Path(overture_poi).exists():
        # Anti-join: take Overture POIs ONLY for OSM gaps — where OSM does not already
        # know this (house, name) pair. Otherwise the venue's POI coordinate competes
        # with the OSM house point and degrades accuracy. The OSM house key is the
        # stitched (Cyrillic) street name plus the house number.
        con.execute(f"""CREATE TABLE osm_keys AS
            SELECT DISTINCT {NORM.format(col="sa.cyr")} AS s, a.num AS n
            FROM (SELECT tags['addr:street'] AS st, tags['addr:housenumber'] AS num
                  FROM (SELECT tags FROM a_nodes UNION ALL SELECT tags FROM a_ways)
                  WHERE tags['addr:street'] IS NOT NULL AND tags['addr:housenumber'] IS NOT NULL) a
            JOIN street_alias sa ON {NORM.format(col="a.st")} = sa.lat_norm""")
        norm_ov = NORM.format(col="o.nom_voie")
        ov_union = f"""
            UNION ALL
            SELECT o.nom_voie, o.number_raw, o.postcode_raw, o.city_tag, o.lat, o.lon
            FROM read_parquet('{overture_poi}') o
            WHERE NOT EXISTS (
                SELECT 1 FROM osm_keys k
                WHERE k.n = o.number_raw
                  AND (k.s LIKE '%' || {norm_ov} || '%' OR {norm_ov} LIKE '%' || k.s || '%'))"""

    # addresses with rid + grid cells: real addresses (tags) + a_streets enrichment (nm, synthetic house "1")
    print("[stage] building tg (addresses + grid) — pbf reading done", flush=True)
    con.execute(f"""CREATE TABLE tg AS
        WITH raw AS (
            SELECT coalesce(tags['addr:street'], tags['addr:place']) AS nom_voie,
                   tags['addr:housenumber'] AS number_raw,
                   coalesce(tags['addr:postcode'], '') AS postcode_raw,
                   coalesce(tags['addr:city'], tags['addr:place'],
                            tags['addr:district'], tags['addr:suburb'], '') AS city_tag,
                   lat, lon FROM (SELECT tags, lat, lon FROM a_nodes
                                  UNION ALL SELECT tags, lat, lon FROM a_ways)
            UNION ALL
            SELECT nm, '1', '', '', lat, lon FROM a_streets  -- enrichment: district-level
            UNION ALL
            -- stitching: real house under the Cyrillic street name, same geometry + number
            SELECT DISTINCT sa.cyr, a.number_raw, a.postcode_raw, a.city_tag, a.lat, a.lon
            FROM (SELECT tags['addr:street'] AS st,
                         tags['addr:housenumber'] AS number_raw,
                         coalesce(tags['addr:postcode'], '') AS postcode_raw,
                         coalesce(tags['addr:city'], tags['addr:place'],
                                  tags['addr:district'], tags['addr:suburb'], '') AS city_tag,
                         lat, lon
                  FROM (SELECT tags, lat, lon FROM a_nodes
                        UNION ALL SELECT tags, lat, lon FROM a_ways)
                  WHERE tags['addr:street'] IS NOT NULL) a
            JOIN street_alias sa ON {norm_alias} = sa.lat_norm
                AND abs(CAST(round(a.lat*3) AS BIGINT) - sa.gla) <= 1
                AND abs(CAST(round(a.lon*3) AS BIGINT) - sa.glo) <= 1{ov_union})
        SELECT row_number() OVER () AS rid, nom_voie, number_raw, postcode_raw, city_tag,
               lat, lon,
               CAST(round(lat*10) AS BIGINT) AS bla, CAST(round(lon*10) AS BIGINT) AS blo
        FROM raw""")
    # commune: city_tag, or the nearest OSM place within a ±1-cell window (≈ ±11 km)
    print("[stage] building nearest (closest settlement, spatial join)", flush=True)
    con.execute("""CREATE TABLE nearest AS SELECT rid, nm FROM (
        SELECT t.rid, p.nm, row_number() OVER (PARTITION BY t.rid
            ORDER BY (t.lat-p.lat)*(t.lat-p.lat) + ((t.lon-p.lon)*cos(radians(t.lat)))*((t.lon-p.lon)*cos(radians(t.lat))),
                     p.nm, p.lat, p.lon) AS rn  -- stable tie-break → deterministic output
        FROM tg t JOIN places p ON p.bla BETWEEN t.bla-1 AND t.bla+1
                               AND p.blo BETWEEN t.blo-1 AND t.blo+1
        WHERE t.city_tag = '') WHERE rn = 1""")
    print("[stage] building bs (street base)", flush=True)
    con.execute("""CREATE TABLE bs AS
        SELECT t.rid, t.nom_voie, t.number_raw, t.postcode_raw,
               CASE WHEN t.city_tag <> '' THEN t.city_tag ELSE coalesce(n.nm, '') END AS nom_commune,
               t.lat, t.lon, t.bla, t.blo
        FROM tg t LEFT JOIN nearest n ON n.rid = t.rid""")

    # Umbrella city from GeoNames: multiple spellings of a city collapse into one alias,
    # and sub-city neighbourhoods become findable by city name. provincia carries the
    # commune aliases.
    if geonames_file:
        script_re = r"^[\p{Latin}\p{Cyrillic} .'-]+$"
        con.execute(f"""CREATE TABLE gpl AS SELECT
            array_to_string(list_distinct(list_filter(
                list_concat([column01, column02], string_split(column03, ',')),
                x -> x IS NOT NULL AND length(x) BETWEEN 2 AND 40
                     AND regexp_full_match(x, $re)
                     AND NOT (upper(x) = x AND length(x) <= 4))), '|') AS nm,
            coalesce(TRY_CAST(column14 AS BIGINT), 0) AS pop, column07 AS code,
            CAST(round(CAST(column04 AS DOUBLE)*10) AS BIGINT) AS bla,
            CAST(round(CAST(column05 AS DOUBLE)*10) AS BIGINT) AS blo,
            CAST(column04 AS DOUBLE) AS lat, CAST(column05 AS DOUBLE) AS lon
            FROM read_csv('{geonames_file}', delim='\t', header=false, quote='', all_varchar=true)
            WHERE column06 = 'P' AND column07 IN ('PPLC','PPLA','PPLA2','PPLA3')""", {"re": script_re})
        print("[stage] building u1/u2/prov (GeoNames umbrella, spatial join)", flush=True)
        con.execute("""CREATE TABLE u1 AS SELECT rid, nm FROM (
            SELECT b.rid, p.nm, row_number() OVER (PARTITION BY b.rid
                ORDER BY ((b.lat-p.lat)*(b.lat-p.lat)+((b.lon-p.lon)*cos(radians(b.lat)))*((b.lon-p.lon)*cos(radians(b.lat))))/(1+ln(1+p.pop)),
                         p.nm, p.lat, p.lon) rn  -- stable tie-break → deterministic output
            FROM bs b JOIN gpl p ON p.bla BETWEEN b.bla-3 AND b.bla+3
                                 AND p.blo BETWEEN b.blo-3 AND b.blo+3) WHERE rn=1""")
        con.execute("""CREATE TABLE u2 AS SELECT rid, nm FROM (
            SELECT b.rid, p.nm, row_number() OVER (PARTITION BY b.rid
                ORDER BY (b.lat-p.lat)*(b.lat-p.lat)+((b.lon-p.lon)*cos(radians(b.lat)))*((b.lon-p.lon)*cos(radians(b.lat))),
                         p.nm, p.lat, p.lon) rn  -- stable tie-break → deterministic output
            FROM bs b JOIN gpl p ON p.bla BETWEEN b.bla-3 AND b.bla+3
                                 AND p.blo BETWEEN b.blo-3 AND b.blo+3
                                 AND p.code IN ('PPLC','PPLA','PPLA2')) WHERE rn=1""")
        con.execute("""CREATE TABLE prov AS
            SELECT b.rid, concat_ws('|', u1.nm, nullif(u2.nm, u1.nm)) AS provincia
            FROM bs b LEFT JOIN u1 ON u1.rid=b.rid LEFT JOIN u2 ON u2.rid=b.rid""")
    else:
        con.execute("CREATE TABLE prov AS SELECT rid, '' AS provincia FROM bs")

    kept = write_canon(con, tile, out_parquet)
    # provenance manifest -> SEC_META of the sheet (v6); OSM-derived layers are
    # ODbL and never enter the public distribution (Monaco smoke is the exception:
    # a public test artifact with full attribution)
    manifest = {
        "country": cc.lower(),
        "layer": "addresses",
        "license": "ODbL-1.0",
        "sources": "OpenStreetMap contributors",
        "source_release": datetime.date.fromtimestamp(pbf.stat().st_mtime).isoformat(),
        "attribution": "(c) OpenStreetMap contributors, ODbL 1.0",
    }
    (CODE / "data" / f"{cc.lower()}_manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2))

    src_label = f"OSM ({pbf.name}) — ODbL! keep this layer separate"
    if ov_union:
        src_label += " + Overture places (CDLA-Permissive-2.0) — Overture attribution required"
    stats = {
        "country": cc,
        "source": src_label,
        "rows_src": total,
        "rows_kept": kept,
        "rows_dropped": total - kept,
        "localities": con.execute("SELECT count(DISTINCT code_insee) FROM canon").fetchone()[0],
        "streets": con.execute("SELECT count(DISTINCT street_key) FROM canon").fetchone()[0],
        "parquet_mb": round(out_parquet.stat().st_size / 1e6, 1),
        "seconds": round(time.time() - t0, 1),
    }
    (CODE / "data" / f"{cc.lower()}_stats.json").write_text(json.dumps(stats, ensure_ascii=False, indent=2))
    print(json.dumps(stats, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
