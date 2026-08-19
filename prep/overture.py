#!/usr/bin/env python3
"""Overture adapter: country from the S3 addresses layer -> canonical Parquet.

Second adapter type (bulk layer) alongside the registry adapter (normalize.py/BAN).
Communes without an official government code get a synthetic id L000001..
(keyed by normalized name; same-named localities merge — a known limitation).

Usage: python3 prep/overture.py NL [geonames_country.txt]   (run from code/)
The optional second argument is a GeoNames dump for the same country
(download.geonames.org/export/dump/RS.zip): administrative seats
(PPLC/PPLA/PPLA2/PPLA3) are taken from it as an umbrella city (locality
alias) when the source has no hierarchy (e.g. only neighbourhoods, no
parent city). GeoNames is CC BY 4.0 (permissive), unlike OSM (ODbL,
copyleft): the permissive layer must not pick up share-alike terms.
"""
import json
import pathlib
import sys
import time

import duckdb  # noqa: F401 (types)
from duck import connect_tuned

CODE = pathlib.Path(__file__).resolve().parent.parent
from duck import OVERTURE_RELEASE as RELEASE  # single source

# Per-country upstream provider + license for Overture ADDRESSES. Verified against the
# official Overture attribution page (docs.overturemaps.org/attribution/, 2026-07) — NOT a
# blanket CDLA. FR's manifest is built in prep/normalize.py (Etalab 2.0).
ADDRESS_LICENSE = {
    "it": {"provider": "ANNCSU (Archivio Nazionale dei Numeri Civici delle Strade Urbane)",
           "license": "CC BY 4.0"},
    "nl": {"provider": "Nationaal Georegister (BAG / Kadaster)",
           "license": "Public Domain Mark 1.0 (PDM 1.0)"},
    "rs": {"provider": "Republicki geodetski zavod (RGZ), data.gov.rs",
           "license": "data.gov.rs Terms of use"},
    # FR is built by prep/normalize.py; MC (smoke) is OSM/ODbL and set in prep/osm.py
    "fr": {"provider": "Base Adresse Nationale (BAN), adresse.data.gouv.fr",
           "license": "Licence Ouverte / Open Licence 2.0 (Etalab 2.0)"},
}
S3 = f"s3://overturemaps-us-west-2/release/{RELEASE}/theme=addresses/type=*/*.parquet"

NORM = ("trim(regexp_replace(regexp_replace(replace(strip_accents(lower({col})), 'đ', 'd'),"
        " '[-''’`./,;()ʻʼ‘]', ' ', 'g'), ' +', ' ', 'g'))")  # đ→d: strip_accents does not strip it


def address_manifest(cc: str, geonames: bool = False) -> dict:
    """Provenance manifest for an address sheet (per-country license, NOT one blanket CDLA).

    Pure function of the single source of truth (ADDRESS_LICENSE + OVERTURE_RELEASE) so the
    manifest regenerator can call it WITHOUT the heavy S3 pull."""
    lic = ADDRESS_LICENSE.get(cc.lower())
    if not lic:
        sys.exit(f"STOP: no verified license for country {cc} — add it to ADDRESS_LICENSE "
                 "(prep/overture.py), checked against docs.overturemaps.org/attribution/")
    sources_str = f"Overture Maps addresses via OpenAddresses; upstream: {lic['provider']} ({lic['license']})"
    attribution = f"{lic['provider']} — {lic['license']}; via Overture Maps / OpenAddresses"
    if geonames:  # settlement aliases (Serbia)
        sources_str += "; settlement names: GeoNames (CC BY 4.0)"
        attribution += "; settlement names © GeoNames (CC BY 4.0)"
    return {
        "country": cc.lower(),
        "layer": "addresses",
        "license": lic["license"],
        "sources": sources_str,
        "source_release": RELEASE,
        "attribution": attribution,
    }


def assert_no_copyleft(cc, sources):
    """License gate: the permissive layer must not contain a copyleft source
    (OSM/ODbL) — otherwise the artifact would be bound by share-alike terms."""
    bad = [s for s in sources if any(
        t in s["license"].lower()
        for t in ("odbl", "share-alike", "share alike", "copyleft"))]
    if bad:
        raise SystemExit(
            f"LICENSE GATE: permissive layer {cc} would pull in a copyleft source "
            f"{[s['name'] for s in bad]} — share-alike contamination. Use a permissive source.")


def main() -> None:
    if len(sys.argv) not in (2, 3):
        sys.exit("usage: prep/overture.py <COUNTRY_CODE> [geonames.txt]")
    cc = sys.argv[1].upper()
    geonames_file = sys.argv[2] if len(sys.argv) == 3 else None
    out_parquet = CODE / "data" / f"{cc.lower()}_norm.parquet"

    # layer provenance + license gate: sources are declared explicitly with the REAL
    # per-country upstream license, not a blanket string
    _lic = ADDRESS_LICENSE.get(cc.lower(), {"provider": "Overture data providers", "license": "open/permissive"})
    sources = [{"name": f"Overture Maps addresses — upstream {_lic['provider']}",
                "release": RELEASE, "license": _lic["license"]}]
    if geonames_file:
        sources.append({"name": "GeoNames — populated places (admin seats)",
                        "file": str(geonames_file), "license": "CC-BY-4.0"})
    assert_no_copyleft(cc, sources)

    t0 = time.time()
    con = connect_tuned()
    con.execute("INSTALL httpfs; LOAD httpfs; INSTALL spatial; LOAD spatial;")
    con.execute("SET s3_region='us-west-2';")
    con.execute("SET http_timeout=120000;")   # the network can be flaky
    con.execute("SET http_retries=8;")

    # network stage: fetch only the target country (pruned via row-group statistics)
    con.execute(f"""
        CREATE TABLE src AS
        SELECT id,
               street                                   AS nom_voie,
               number                                   AS number_raw,
               coalesce(postcode, '')                   AS postcode_raw,
               coalesce(address_levels[-1].value, '')   AS nom_commune,  -- last level = locality (NL: the only level; IT: region/province/commune)
               CASE WHEN len(address_levels) >= 2
                    THEN coalesce(address_levels[-2].value, '') ELSE '' END AS provincia,
               st_x(geometry)                           AS lon,
               st_y(geometry)                           AS lat
        FROM read_parquet('{S3}')
        WHERE country = '{cc}' AND street IS NOT NULL AND number IS NOT NULL
    """)
    total = con.execute("SELECT count(*) FROM src").fetchone()[0]

    if geonames_file:
        # Umbrella city from GeoNames (CC BY 4.0, permissive — NOT OSM/ODbL):
        # nearest administrative seat (PPLC/PPLA/PPLA2/PPLA3) in a 0.1-deg grid, ±3 cells.
        # Names are packed multi-script (e.g. "Beograd" plus its Cyrillic form), keeping
        # only Latin/Cyrillic — local forms live in alternatenames (the main name is
        # often the English one).
        script_re = r"^[\p{Latin}\p{Cyrillic} .'-]+$"
        con.execute(f"""
            CREATE TABLE places AS
            SELECT array_to_string(list_distinct(list_filter(
                       list_concat([column01, column02], string_split(column03, ',')),
                       x -> x IS NOT NULL AND length(x) BETWEEN 2 AND 40
                            AND regexp_full_match(x, $re)
                            AND NOT (upper(x) = x AND length(x) <= 4))), '|') AS nm,
                   CAST(column04 AS DOUBLE) AS lat, CAST(column05 AS DOUBLE) AS lon,
                   coalesce(TRY_CAST(column14 AS BIGINT), 0) AS pop,  -- GeoNames col 14 = population
                   column07 AS code,
                   CAST(round(CAST(column04 AS DOUBLE)*10) AS BIGINT) AS bla,
                   CAST(round(CAST(column05 AS DOUBLE)*10) AS BIGINT) AS blo
            FROM read_csv('{geonames_file}', delim='\t', header=false,
                          quote='', all_varchar=true)
            WHERE column06 = 'P' AND column07 IN ('PPLC','PPLA','PPLA2','PPLA3')
        """, {"re": script_re})
        # Memory-friendly: each umbrella is a separate CREATE TABLE so the window's
        # working memory is released between steps (two windows in one query can OOM).
        con.execute("""CREATE TABLE addr AS
            SELECT *, row_number() OVER () AS rid,
                   CAST(round(lat*10) AS BIGINT) AS bla, CAST(round(lon*10) AS BIGINT) AS blo
            FROM src""")
        # 1st umbrella: distance-weighted significance — a large city wins its own core
        # over sub-centres (population ≈ 0), while standalone towns keep their own name.
        con.execute("""CREATE TABLE u1 AS SELECT rid, nm FROM (
                SELECT a.rid, p.nm, row_number() OVER (PARTITION BY a.rid
                    ORDER BY ((a.lat-p.lat)*(a.lat-p.lat)+((a.lon-p.lon)*cos(radians(a.lat)))*((a.lon-p.lon)*cos(radians(a.lat))))
                             /(1+ln(1+p.pop))) AS rn
                FROM addr a JOIN places p ON p.bla BETWEEN a.bla-3 AND a.bla+3
                                         AND p.blo BETWEEN a.blo-3 AND a.blo+3
                WHERE a.provincia='') WHERE rn=1""")
        # 2nd umbrella: nearest MAJOR city (PPLC/PPLA/PPLA2), so a district address also
        # matches queries that use the parent city name ("Novi Beograd" vs "Beograd").
        con.execute("""CREATE TABLE u2 AS SELECT rid, nm FROM (
                SELECT a.rid, p.nm, row_number() OVER (PARTITION BY a.rid
                    ORDER BY (a.lat-p.lat)*(a.lat-p.lat)+((a.lon-p.lon)*cos(radians(a.lat)))*((a.lon-p.lon)*cos(radians(a.lat)))) AS rn
                FROM addr a JOIN places p ON p.bla BETWEEN a.bla-3 AND a.bla+3
                                         AND p.blo BETWEEN a.blo-3 AND a.blo+3
                                         AND p.code IN ('PPLC','PPLA','PPLA2')
                WHERE a.provincia='') WHERE rn=1""")
        # provincia = district (u1) + major city (u2, unless identical): "Novi Beograd|Beograd"
        con.execute("""CREATE TABLE src2 AS
            SELECT a.* EXCLUDE (provincia, rid, bla, blo),
                   CASE WHEN a.provincia<>'' THEN a.provincia
                        ELSE concat_ws('|', u1.nm, nullif(u2.nm, u1.nm)) END AS provincia
            FROM addr a LEFT JOIN u1 ON u1.rid=a.rid LEFT JOIN u2 ON u2.rid=a.rid""")
        con.execute("DROP TABLE src; ALTER TABLE src2 RENAME TO src;")
        con.execute("DROP TABLE addr; DROP TABLE u1; DROP TABLE u2;")

    norm_voie = NORM.format(col="nom_voie")
    norm_commune = NORM.format(col="nom_commune")
    # The commune ALIAS comes ONLY from the GeoNames umbrella (city level). The Overture
    # province (address_levels[-2]) is too broad as an alias — the "Roma" province spans
    # hundreds of communes, so province-as-alias misroutes city-name queries. The
    # province stays in the KEY (code_insee) to split same-named communes.
    prov_alias = NORM.format(col="provincia") if geonames_file else "''"
    # For Italy the commune key also includes a 0.5-deg geo cell, and the ORIGINAL justification
    # for it — "the Overture province is almost always empty for IT" — is FALSE. Measured against
    # the source on 2026-07-29 (release 2026-06-17.0): every one of the 25 898 743 Italian address
    # rows carries a province, across exactly 107 distinct values, Italy's real province count;
    # all rows have exactly 3 address_levels, so [-2] is always the province, never a region.
    #
    # The cell was nevertheless KEPT, because removing it was tried and it lost on measurement.
    # The plain `nom_commune_norm, provincia` key does remove roughly 1 800 spurious splits of
    # communes that straddle a cell boundary — but accuracy went DOWN, and a control rebuild with
    # the old key on the same refreshed data isolated the key, not the data, as the cause. The
    # regressions land in large cities the cell used to split: de-fragmenting them makes them
    # heavier in ranking, and they start winning over the smaller commune named in the query.
    #
    # The hoped-for gain did not materialise either: namesake resolution did not improve at all.
    # The reason is that the province is in the KEY but is not SEARCHABLE — with no GeoNames
    # umbrella `provincia_norm` is '' (see prov_alias above), so a province token in the query
    # cannot pick one same-named commune over another. What resolves those today is the runtime
    # geographic qualifier, not the key. Making the province an alias is the open question, and it
    # carries its own risk the comment above already names: the "Roma" province spans hundreds of
    # communes. Until that is answered, the cell earns its place empirically.
    insee_order = "nom_commune_norm, provincia"
    if cc == "IT":
        insee_order = "nom_commune_norm, provincia, floor(lat*2), floor(lon*2)"
    # title-case ALL-CAPS names (Italy): Via Giuseppe Mazzini
    title = ("CASE WHEN {col} = upper({col}) AND len({col}) > 3 THEN "
             "array_to_string(list_transform(string_split(lower({col}), ' '), "
             "x -> upper(x[1]) || x[2:]), ' ') ELSE {col} END")
    con.execute(f"""
        CREATE TABLE canon AS
        WITH base AS (
            SELECT id, {title.format(col='nom_voie')} AS nom_voie,
                   {norm_voie}  AS nom_voie_norm,
                   TRY_CAST(regexp_extract(number_raw, '^[0-9]+') AS INTEGER) AS numero,
                   nullif(lower(trim(regexp_replace(
                       regexp_extract(number_raw, '^[0-9]+(.*)$', 1),
                       '^[\\s/\\-]+', ''))), '')        AS rep,
                   regexp_extract(postcode_raw, '[0-9]+') AS code_postal,
                   postcode_raw,
                   {title.format(col='nom_commune')} AS nom_commune,
                   {norm_commune} AS nom_commune_norm,
                   provincia,
                   lon, lat
            FROM src
            WHERE nom_commune <> '' AND lon IS NOT NULL AND lat IS NOT NULL
        ),
        loc AS (
            -- the locality key includes the province: same-named communes
            -- do not merge
            SELECT *, 'L' || lpad(CAST(dense_rank() OVER (ORDER BY {insee_order}) AS VARCHAR), 6, '0') AS code_insee
            FROM base
            WHERE numero IS NOT NULL AND nom_voie_norm <> ''
        )
        SELECT id, numero, rep, nom_voie, nom_voie_norm,
               coalesce(code_postal, '0') AS code_postal,
               postcode_raw AS code_postal_display,
               code_insee, nom_commune, nom_commune_norm,
               {prov_alias} AS provincia_norm,
               nom_voie_norm || '|' || code_insee AS street_key,
               lon, lat
        FROM loc
    """)
    kept = con.execute("SELECT count(*) FROM canon").fetchone()[0]
    con.execute(f"COPY canon TO '{out_parquet}' (FORMAT parquet, COMPRESSION zstd)")

    stats = {
        "country": cc,
        "release": RELEASE,
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

    # provenance manifest -> SEC_META of the sheet (v6): flat EN strings, the two identity
    # keys (country, layer) are mandatory. Overture ADDRESSES are NOT one blanket CDLA license
    # each national source carries its own, per the Overture attribution
    # page (docs.overturemaps.org/attribution/, verified 2026-07). Distributed via OpenAddresses.
    manifest = address_manifest(cc, geonames=bool(geonames_file))
    (CODE / "data" / f"{cc.lower()}_manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
