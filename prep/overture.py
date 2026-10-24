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
import argparse
import json
import os
import pathlib
import re
import sys
import time

import duckdb  # noqa: F401 (types)
from duck import connect_tuned

CODE = pathlib.Path(__file__).resolve().parent.parent
from duck import OVERTURE_RELEASE as RELEASE  # default pin for the existing country sheets
from de_sources import (
    DE_COVERAGE,
    DE_RELEASE,
    DE_SOURCE_CATALOG_SHA256,
    build_land_witness,
    build_manifest as build_de_manifest,
    land_code_for_name,
    source_catalog as de_source_catalog,
    stats_witness_problems as de_stats_witness_problems,
    validate_manifest as validate_de_manifest,
)
from us_umbrella import attach_umbrella

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
    # IGN licence §4 and the CNIG CartoCiudad product catalogue, verified 2026-09-12.
    "es": {"provider": "CartoCiudad (scne.es)",
           "license": "CC BY 4.0"},
    # Overture attribution page: "IBGE ... Available under CC0. Distributed by AddressForAll"
    # (accessed 2026-09-28). IBGE itself calls the CNEFE files public without a licence text;
    # AddressForAll records the pack as an implied licence of the CC0 family. Receipts:
    # chat/br_source_audit_20260928_2332/; owner decision ANSWERS §41 (public track).
    "br": {"provider": "IBGE (Instituto Brasileiro de Geografia e Estatistica), CNEFE 2022",
           "license": "CC0 1.0"},
    # US (owner decision ANSWERS §140, licence check 2026-10-19): only the datasets with an explicit
    # right to redistribute (US_GREEN_DATASETS below), read per dataset from the primary sources —
    # the Overture attribution page disagrees with the release data (PRINCIPLES 1.3-ter).
    "us": {"provider": "National Address Database (USDOT) and 19 state and county open datasets "
                       "via OpenAddresses",
           "license": "mixed permissive: U.S. Government work (17 U.S.C. §105), NYC Open Data "
                      "(Admin Code §23-502), CC0 1.0, PDDL 1.0, CC BY 4.0, open data terms of "
                      "Los Angeles County, Oakland County, MassGIS, MnGeo, Oregon GEOHub"},
}
S3 = f"s3://overturemaps-us-west-2/release/{RELEASE}/theme=addresses/type=*/*.parquet"

# US: the release pinned by the owner (ANSWERS §140) and the two filters of the US sheet.
US_RELEASE = "2026-09-23.1"
# sources[1].dataset values with an explicit right to redistribute (every US row has exactly one
# source; survey 2026-10-19: 100 378 003 of 125 364 414 in-contour rows).
US_GREEN_DATASETS = (
    "NAD", "us/ma/statewide", "us/ca/los_angeles", "us/mn/statewide", "us/or/statewide",
    "us/ny/city_of_new_york", "us/fl/hillsborough", "us/ca/sacramento", "us/mi/oakland",
    "us/ca/fresno", "us/ga/dekalb", "us/ca/san_francisco", "us/fl/manatee", "us/id/ada",
    "us/la/city_of_new_orleans", "us/pa/lackawanna", "us/ga/clarke", "us/ca/napa", "us/ca/nevada",
    "us/wi/waupaca",
)
# The contour: 50 states and DC (address_levels[1].value); territories (VI, PR, ...) stay out.
US_STATES = (
    "AK", "AL", "AR", "AZ", "CA", "CO", "CT", "DC", "DE", "FL", "GA", "HI", "IA", "ID", "IL", "IN",
    "KS", "KY", "LA", "MA", "MD", "ME", "MI", "MN", "MO", "MS", "MT", "NC", "ND", "NE", "NH", "NJ",
    "NM", "NV", "NY", "OH", "OK", "OR", "PA", "RI", "SC", "SD", "TN", "TX", "UT", "VA", "VT", "WA",
    "WI", "WV", "WY",
)

NORM = ("trim(regexp_replace(regexp_replace(replace(strip_accents(lower({col})), 'đ', 'd'),"
        " '[-''’`./,;()ʻʼ‘]', ' ', 'g'), ' +', ' ', 'g'))")  # đ→d: strip_accents does not strip it


def release_for_country(cc: str) -> str:
    """Keep the DE evidence pin independent of the older country sheets."""
    if cc.lower() in ("es", "br"):  # BR: ANSWERS §41, same pin as ES
        return "2026-08-19.0"
    if cc.lower() == "us":  # ANSWERS §140
        return US_RELEASE
    return DE_RELEASE if cc.lower() == "de" else RELEASE


# Countries whose working storage is bounded explicitly: (build dir, temp GiB, memory).
BOUNDED_STORAGE = {
    "ES": ("es_build", 8, "3GiB"),
    # BR is ~4.7x ES; the streamed two-pass extraction keeps temp small, the cap is a guard.
    "BR": ("br_build", 12, "6GiB"),
    # US green rows are ~1.5x BR; same streamed extraction, the duplicate merge runs locally.
    "US": ("us_build", 16, "6GiB"),
}


def connect_for_country(cc: str, offline_extensions: bool = False):
    """Bound ES/BR working storage explicitly, without changing established country runs."""
    bounds = BOUNDED_STORAGE.get(cc.upper())
    if bounds is None:
        return connect_tuned(disable_extension_autoload=offline_extensions)
    build_dir, temp_gib, memory = bounds
    con = connect_tuned(
        temp_directory=CODE / "data" / build_dir / "tmp_duck",
        max_temp_directory_size_bytes=temp_gib * 1024**3,
        disable_extension_autoload=offline_extensions,
    )
    con.execute(f"SET memory_limit='{memory}'")
    return con


def s3_for_release(release: str, cc: str | None = None) -> str:
    feature_glob = "type=address/*.parquet" if (cc or "").upper() == "DE" else "type=*/*.parquet"
    return (f"s3://overturemaps-us-west-2/release/{release}/"
            f"theme=addresses/{feature_glob}")


def address_manifest(cc: str, geonames: bool = False, release: str | None = None,
                     ranges: bool = False) -> dict:
    """Provenance manifest for an address sheet (per-country license, NOT one blanket CDLA).

    Pure function of the single source of truth (ADDRESS_LICENSE + OVERTURE_RELEASE) so the
    manifest regenerator can call it WITHOUT the heavy S3 pull."""
    country = cc.lower()
    selected_release = release or release_for_country(country)
    if selected_release != release_for_country(country):
        raise SystemExit(
            f"STOP: {country.upper()} release must be exactly "
            f"{release_for_country(country)}, got {selected_release!r}")
    if country == "de":
        if geonames:
            raise SystemExit(
                "STOP: DE public route is the fixed 15-Länder address catalog; "
                "an unvalidated GeoNames source cannot be added")
        manifest = build_de_manifest(selected_release)
        validate_de_manifest(manifest, selected_release)
        return manifest
    lic = ADDRESS_LICENSE.get(country)
    if not lic:
        sys.exit(f"STOP: no verified license for country {cc} — add it to ADDRESS_LICENSE "
                 "(prep/overture.py), checked against docs.overturemaps.org/attribution/")
    sources_str = f"Overture Maps addresses via OpenAddresses; upstream: {lic['provider']} ({lic['license']})"
    attribution = f"{lic['provider']} — {lic['license']}; via Overture Maps / OpenAddresses"
    # Settlement names from GeoNames: aliases (Serbia) or the missing cities themselves (US).
    geonames_credit = "; settlement names © GeoNames (CC BY 4.0)" if geonames else ""
    if geonames:
        sources_str += "; settlement names: GeoNames (CC BY 4.0)"
        attribution += geonames_credit
    if ranges:
        # US interpolation layer (prep/us_tiger.py, ANSWERS §147): range points sampled from the
        # Census Bureau's TIGER/Line address ranges; the city of a range is the USPS place of
        # its ZIP from the GeoNames postal-code table.
        sources_str += ("; address ranges: TIGER/Line ADDRFEAT, U.S. Census Bureau (public domain); "
                        "ZIP place names: GeoNames postal codes (CC BY 4.0)")
        attribution += ("; address ranges from TIGER/Line Shapefiles, U.S. Census Bureau (public "
                        "domain, interpolated points); ZIP place names © GeoNames (CC BY 4.0)")
    result = {
        "country": country,
        "layer": "addresses",
        "license": lic["license"],
        "sources": sources_str,
        "source_release": selected_release,
        "attribution": attribution,
    }
    if country == "es":
        result["attribution"] = (
            "Obra derivada de CartoCiudad CC-BY 4.0 scne.es; "
            f"via Overture Maps / OpenAddresses, release {selected_release}; "
            "address normalization applied"
        )
    if country == "br":
        result["attribution"] = (
            "IBGE - Instituto Brasileiro de Geografia e Estatistica, CNEFE 2022 (public data; "
            "implied licence recorded by AddressForAll as CC0 family; labelled CC0 by Overture); "
            f"via Overture Maps / OpenAddresses, release {selected_release}; "
            "address normalization applied"
        )
    if country == "us":
        result["attribution"] = (
            "National Address Database (USDOT) and state and county open data, datasets: "
            + ", ".join(US_GREEN_DATASETS)
            + f"; via Overture Maps / OpenAddresses, release {selected_release}; "
            "address normalization applied; partial coverage (sources with an explicit right "
            "to redistribute)" + geonames_credit
        )
    return result


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


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("country", help="ISO alpha-2 country code")
    # Preserve the historical `CC [geonames.txt]` positional interface.
    parser.add_argument("geonames", nargs="?", help="optional GeoNames populated-places dump")
    parser.add_argument("--release", help="must equal the repository pin for this country")
    parser.add_argument(
        "--offline-extensions",
        action="store_true",
        help="LOAD pre-installed DuckDB extensions; never INSTALL/download them",
    )
    args = parser.parse_args(argv)
    if not re.fullmatch(r"[A-Za-z]{2}", args.country):
        parser.error("country must be an ISO alpha-2 code")
    return args


def _declared_sources(cc: str, release: str, geonames_file: str | None) -> list[dict]:
    if cc == "DE":
        return [
            {
                "name": f"{code}: {record['source']} — {record['provider']}",
                "release": release,
                "license": record["license"],
            }
            for code, record in sorted(de_source_catalog().items())
        ]
    lic = ADDRESS_LICENSE.get(cc.lower())
    if not lic:
        raise SystemExit(
            f"STOP: no verified license for country {cc} — add it to ADDRESS_LICENSE "
            "(prep/overture.py), checked against docs.overturemaps.org/attribution/")
    sources = [{
        "name": f"Overture Maps addresses — upstream {lic['provider']}",
        "release": release,
        "license": lic["license"],
    }]
    if geonames_file:
        sources.append({
            "name": "GeoNames — populated places (admin seats)",
            "file": str(geonames_file),
            "license": "CC-BY-4.0",
        })
    return sources


def preflight(
    cc: str,
    geonames_file: str | None,
    requested_release: str | None,
) -> tuple[str, dict, list[dict]]:
    """Validate all license/release inputs before connect/extension/S3 operations."""
    expected_release = release_for_country(cc)
    release = requested_release or expected_release
    if release != expected_release:
        raise SystemExit(
            f"STOP: {cc} release must be exactly {expected_release}, got {release!r}")
    if geonames_file and not os.path.isfile(geonames_file):
        # Found out here, not after the S3 scans that precede the first read of the dump.
        raise SystemExit(f"STOP: GeoNames dump not found: {geonames_file}")
    manifest = address_manifest(cc, geonames=bool(geonames_file), release=release)
    if cc == "DE":
        validate_de_manifest(manifest, release)
    sources = _declared_sources(cc, release, geonames_file)
    assert_no_copyleft(cc, sources)
    return release, manifest, sources


def load_extensions(con, offline_extensions: bool) -> None:
    """Offline mode is a hard no-INSTALL contract for the build orchestrator."""
    if offline_extensions:
        con.execute("LOAD httpfs; LOAD spatial;")
    else:
        con.execute("INSTALL httpfs; LOAD httpfs; INSTALL spatial; LOAD spatial;")


# Brazilian CEP: 7.5 % of the Overture rows carry 7 bare digits ("6950000") -- a São Paulo
# code that lost its leading zero somewhere upstream. Restore it and the hyphen, so the index
# and the query (norm.rs keeps "06950" of "06950-000") share one 5-digit prefix.
BR_POSTCODE_SQL = (
    "CASE WHEN regexp_full_match(postcode, '[0-9]{7,8}') "
    "THEN substr(lpad(postcode, 8, '0'), 1, 5) || '-' || substr(lpad(postcode, 8, '0'), 6, 3) "
    "ELSE coalesce(postcode, '') END"
)
# US ZIP: five digits (94.2 % of the green rows) or empty; a ZIP+4 keeps its first five digits.
US_POSTCODE_SQL = (
    "CASE WHEN regexp_full_match(postcode, '[0-9]{5}-[0-9]{4}') "
    "THEN substr(postcode, 1, 5) ELSE coalesce(postcode, '') END"
)
POSTCODE_SQL = {"BR": BR_POSTCODE_SQL, "US": US_POSTCODE_SQL}

# US city: postal_city when filled (76.8 % of the green rows), else the second address level,
# which mixes a city ("Phoenix"), a county ("Fairfax County", Louisiana "Parish"), New Jersey's
# "County, Municipality" ("Essex, Newark") and Wisconsin's "City of X". A county is not a city.
# "X Borough" stays as is: Alaska's county-equivalent boroughs all carry postal_city, while in
# Pennsylvania a borough is a municipality ("Murrysville Borough": 9 198 rows without postal_city).
# A trailing "City" belongs to the name ("Jersey City", "Kansas City"). Case is left to the engine.
_US_LEVEL2 = "address_levels[2].value"
US_CITY_SQL = (
    "coalesce(nullif(trim(postal_city), ''), "
    f"CASE WHEN regexp_matches({_US_LEVEL2}, ' (County|Parish)$') THEN '' "
    f"WHEN contains({_US_LEVEL2}, ', ') THEN trim(split_part({_US_LEVEL2}, ', ', 2)) "
    f"WHEN starts_with({_US_LEVEL2}, 'City of ') THEN substr({_US_LEVEL2}, 9) "
    f"ELSE {_US_LEVEL2} END, '')"
)


def _sql_list(values) -> str:
    return ", ".join(f"'{value}'" for value in values)


def create_src_sql(cc: str, s3: str) -> str:
    """One remote scan; DE retains lineage/state only in the temporary src table."""
    return "\n        CREATE TABLE src AS" + select_src_sql(cc, s3)


def select_src_sql(cc: str, s3: str) -> str:
    """The source projection shared by the in-memory table and the BR streamed passes."""
    if cc == "DE":
        city = "coalesce(address_levels[2].value, '')"
        province = "coalesce(address_levels[1].value, '')"
        evidence_columns = (
            ",\n               sources                                  AS source_records"
            ",\n               address_levels[1].value                   AS land_raw"
        )
        # The pinned DE F2 COUNT covers every country=DE row.  Keep that same
        # denominator in rows_src; canon records unusable street/number rows as
        # rows_dropped instead of silently changing the audited release count.
        where_clause = "country = 'DE'"
    elif cc == "US":
        city = US_CITY_SQL
        province = "coalesce(address_levels[1].value, '')"  # the state code
        evidence_columns = ""
        where_clause = (
            f"country = 'US' AND sources[1].dataset IN ({_sql_list(US_GREEN_DATASETS)}) "
            f"AND address_levels[1].value IN ({_sql_list(US_STATES)}) "
            "AND street IS NOT NULL AND number IS NOT NULL"
        )
    else:
        city = "coalesce(address_levels[-1].value, '')"
        province = (
            "CASE WHEN len(address_levels) >= 2 "
            "THEN coalesce(address_levels[-2].value, '') ELSE '' END"
        )
        evidence_columns = ""
        where_clause = f"country = '{cc}' AND street IS NOT NULL AND number IS NOT NULL"
    postcode = POSTCODE_SQL.get(cc, "coalesce(postcode, '')")
    return f"""
        SELECT id,
               street                                   AS nom_voie,
               number                                   AS number_raw,
               {postcode:<40} AS postcode_raw,
               {city}                                    AS nom_commune,
               {province}                                AS provincia,
               st_x(geometry)                           AS lon,
               st_y(geometry)                           AS lat{evidence_columns}
        FROM read_parquet('{s3}')
        WHERE {where_clause}
    """


def _write_stats(cc: str, stats: dict) -> None:
    (CODE / "data" / f"{cc.lower()}_stats.json").write_text(
        json.dumps(stats, ensure_ascii=False, indent=2), encoding="utf-8")


def _de_base_stats(release: str, manifest: dict) -> dict:
    return {
        "country": "DE",
        "release": release,
        "coverage": DE_COVERAGE,
        "source_catalog_sha256": DE_SOURCE_CATALOG_SHA256,
        "source_catalog": manifest["source_catalog"],
    }


def _de_source_evidence(con) -> dict:
    """Aggregate stable SourceItem fields locally from the retained src.sources list."""
    with_sources, without_sources, with_root_sources, without_root_sources = con.execute("""
        WITH row_evidence AS (
            SELECT source_records IS NOT NULL AND len(source_records) > 0
                       AS has_sources,
                   coalesce(
                       len(list_filter(
                           source_records,
                           source_record -> json_extract_string(
                               to_json(source_record), '$.property') = ''
                       )) > 0,
                       false
                   ) AS has_root_source
            FROM src
        )
        SELECT count(*) FILTER (WHERE has_sources),
               count(*) FILTER (WHERE NOT has_sources),
               count(*) FILTER (WHERE has_root_source),
               count(*) FILTER (WHERE NOT has_root_source)
        FROM row_evidence
    """).fetchone()
    rows = con.execute("""
        WITH source_items AS (
            SELECT coalesce(trim(cast(land_raw AS VARCHAR)), '') AS land_raw,
                   source_record
            FROM src
            CROSS JOIN UNNEST(source_records) AS observed(source_record)
        )
        SELECT land_raw,
               coalesce(trim(json_extract_string(to_json(source_record), '$.dataset')), '')
                   AS dataset,
               coalesce(trim(json_extract_string(to_json(source_record), '$.license')), '')
                   AS license,
               CASE
                   WHEN NOT json_exists(to_json(source_record), '$.license')
                       THEN 'missing'
                   WHEN json_type(json_extract(to_json(source_record), '$.license')) = 'NULL'
                       THEN 'null'
                   WHEN coalesce(
                       trim(json_extract_string(to_json(source_record), '$.license')), '') = ''
                       THEN 'empty'
                   ELSE 'value'
               END AS license_state,
               CASE
                   WHEN json_extract(to_json(source_record), '$.property') IS NULL
                       THEN '__MISSING_PROPERTY__'
                   ELSE coalesce(
                       trim(json_extract_string(to_json(source_record), '$.property')),
                       '__MISSING_PROPERTY__'
                   )
               END AS property,
               count(*) AS rows
        FROM source_items
        GROUP BY land_raw, dataset, license, license_state, property
        ORDER BY land_raw, dataset, license, license_state, property
    """).fetchall()
    items = [
        {
            "land_code": land_code_for_name(land_raw) or "",
            "land_raw": str(land_raw or "").strip(),
            "dataset": str(dataset or "").strip(),
            "license": str(license_name or "").strip(),
            "license_state": str(license_state),
            "property": str(prop or "").strip(),
            "rows": int(count),
        }
        for land_raw, dataset, license_name, license_state, prop, count in rows
    ]
    return {
        "dimensions": [
            "land_code", "land_raw", "dataset", "license", "license_state", "property"
        ],
        "rows_with_sources": int(with_sources),
        "rows_without_sources": int(without_sources),
        "rows_with_root_sources": int(with_root_sources),
        "rows_without_root_sources": int(without_root_sources),
        "source_items_total": sum(item["rows"] for item in items),
        "root_source_items_total": sum(
            item["rows"] for item in items if item["property"] == ""),
        "items": items,
    }


def _de_extract_witness(con, release: str, manifest: dict, total: int) -> dict:
    raw_land_counts = con.execute("""
        SELECT coalesce(trim(cast(land_raw AS VARCHAR)), '') AS land_raw,
               count(*) AS rows
        FROM src
        GROUP BY land_raw
        ORDER BY land_raw
    """).fetchall()
    witness = _de_base_stats(release, manifest)
    witness.update({"status": "ok", "rows_src": int(total)})
    witness.update(build_land_witness(raw_land_counts))
    try:
        witness["observed_source_evidence"] = _de_source_evidence(con)
    except Exception as exc:
        # Preserve the Land evidence and the exact source-schema failure instead of replacing
        # both with a generic exception-only stats file. The run remains fail-closed below.
        witness["status"] = "failed"
        witness["failure_stage"] = "source_fields"
        witness["errors"] = [str(exc)]
        witness["observed_source_evidence"] = {
            "status": "unreadable",
            "error": str(exc),
        }
    return witness


def _de_failed_stats(release: str, manifest: dict, stage: str, error: object) -> dict:
    stats = _de_base_stats(release, manifest)
    stats.update({
        "status": "failed",
        "failure_stage": stage,
        "errors": [str(error)],
    })
    return stats


# title-case ALL-CAPS names (Italy, Brazil): Via Giuseppe Mazzini, Rua Das Flores
TITLE = ("CASE WHEN {col} = upper({col}) AND len({col}) > 3 THEN "
         "array_to_string(list_transform(string_split(lower({col}), ' '), "
         "x -> upper(x[1]) || x[2:]), ' ') ELSE {col} END")


def base_sql(source: str = "src") -> str:
    norm_voie = NORM.format(col="nom_voie")
    norm_commune = NORM.format(col="nom_commune")
    return f"""
            SELECT id, {TITLE.format(col='nom_voie')} AS nom_voie,
                   {norm_voie}  AS nom_voie_norm,
                   TRY_CAST(regexp_extract(number_raw, '^[0-9]+') AS INTEGER) AS numero,
                   nullif(lower(trim(regexp_replace(
                       regexp_extract(number_raw, '^[0-9]+(.*)$', 1),
                       '^[\\s/\\-]+', ''))), '')        AS rep,
                   regexp_extract(postcode_raw, '[0-9]+') AS code_postal,
                   postcode_raw,
                   {TITLE.format(col='nom_commune')} AS nom_commune,
                   {norm_commune} AS nom_commune_norm,
                   provincia,
                   lon, lat
            FROM {source}
            WHERE nom_commune <> '' AND lon IS NOT NULL AND lat IS NOT NULL"""


def keys_sql(insee_order: str, source: str = "src") -> str:
    """Locality codes over DISTINCT keys: identical to dense_rank over all rows by the same
    key, without sorting every address. Only valid for a key made of the two columns."""
    if insee_order != "nom_commune_norm, provincia":
        raise ValueError("key-table codes need the plain (commune, province) key")
    return f"""
        SELECT nom_commune_norm, provincia, n,
               'L' || lpad(CAST(dense_rank() OVER (ORDER BY {insee_order}) AS VARCHAR), 6, '0') AS code_insee
        FROM (SELECT nom_commune_norm, provincia, count(*) AS n
              FROM ({base_sql(source)})
              WHERE numero IS NOT NULL AND nom_voie_norm <> ''
              GROUP BY nom_commune_norm, provincia)"""


def canon_sql(insee_order: str, prov_alias: str, keys_table: str | None = None,
              source: str = "src") -> str:
    if keys_table is None:
        # the locality key includes the province: same-named communes do not merge
        loc = f"""
            SELECT *, 'L' || lpad(CAST(dense_rank() OVER (ORDER BY {insee_order}) AS VARCHAR), 6, '0') AS code_insee
            FROM base
            WHERE numero IS NOT NULL AND nom_voie_norm <> ''"""
    else:
        loc = f"""
            SELECT b.*, k.code_insee
            FROM base b JOIN {keys_table} k
              ON b.nom_commune_norm = k.nom_commune_norm AND b.provincia = k.provincia
            WHERE b.numero IS NOT NULL AND b.nom_voie_norm <> ''"""
    return f"""
        WITH base AS ({base_sql(source)}
        ),
        loc AS ({loc}
        )
        SELECT id, numero, rep, nom_voie, nom_voie_norm,
               coalesce(code_postal, '0') AS code_postal,
               postcode_raw AS code_postal_display,
               code_insee, nom_commune, nom_commune_norm,
               {prov_alias} AS provincia_norm,
               nom_voie_norm || '|' || code_insee AS street_key,
               lon, lat
        FROM loc"""


# Countries extracted in two streamed passes over an S3 view: the source never sits in memory
# (BR 74M rows, US 100M). `canon` is then a view over the written Parquet, not a table.
STREAMED = frozenset({"BR", "US"})
# Streamed countries whose source leaves the city empty on part of the rows (US survey 2026-10-19:
# about 4.7 M green rows, Texas 1.7 M and New York State 0.97 M first). With a GeoNames dump the
# adapter fills those cities in a zero pass (fill_missing_cities) before the shared base_sql filter
# drops rows without a city. The other streamed countries refuse the dump: every row carries its
# region, and the Serbian province alias would need the whole source as a table.
CITY_UMBRELLA = frozenset({"US"})
assert CITY_UMBRELLA <= STREAMED


def stream_canon(con, cc: str, insee_order: str, prov_alias: str,
                 out_parquet: pathlib.Path, source: str = "src") -> int:
    """Copy the streamed second pass and verify its count against the locality-key pass."""
    con.execute("CREATE TABLE keys AS " + keys_sql(insee_order, source))
    expected = con.execute("SELECT sum(n) FROM keys").fetchone()[0] or 0
    con.execute(f"COPY ({canon_sql(insee_order, prov_alias, keys_table='keys', source=source)}) "
                f"TO '{out_parquet}' (FORMAT parquet, COMPRESSION zstd)")
    con.execute(f"CREATE VIEW canon AS SELECT * FROM read_parquet('{out_parquet}')")
    kept = con.execute("SELECT count(*) FROM canon").fetchone()[0]
    if kept != expected:
        raise SystemExit(f"STOP: {cc} canon wrote {kept} rows, key pass counted {expected}")
    return kept


def fill_missing_cities(con, geonames_file: str, source: str = "src",
                        out_view: str = "src_city") -> dict:
    """Zero pass: give the rows of `source` that carry no city the GeoNames umbrella place.

    One more scan of the source (about 20 minutes on S3 for the US), restricted to the rows whose
    city is empty -- the city expression of select_src_sql decides, so this pass and the streamed
    passes agree row by row -- lands in the local table src_nocity (id, lat, lon, state; a few
    million rows, not the sheet). prep/us_umbrella.py assigns each row the nearest populated place
    of its own state (cross-state only when its state has none within reach). The result,
    umbrella_city (id, nom_commune), is joined back by id in `out_view`, which the streamed passes
    read instead of `source`: a hash join whose build side is the small local table and whose probe
    side streams, no sort and no window over the hundred million rows. Rows still without a city
    after the umbrella are dropped by base_sql as before and stay in rows_dropped."""
    con.execute(f"CREATE TABLE src_nocity AS SELECT id, lat, lon, provincia AS state, nom_commune "
                f"FROM {source} WHERE nom_commune = ''")
    rows_nocity = con.execute("SELECT count(*) FROM src_nocity").fetchone()[0]
    filled, cross = attach_umbrella(con, "src_nocity", geonames_file, city_col="nom_commune",
                                    rid_col="rowid", out_table="umbrella")
    con.execute("CREATE TABLE umbrella_city AS SELECT id, nom_commune FROM src_nocity "
                "WHERE nom_commune <> ''")
    con.execute("DROP TABLE src_nocity; DROP TABLE umbrella;")
    con.execute(f"""CREATE VIEW {out_view} AS
        SELECT s.* EXCLUDE (nom_commune),
               coalesce(nullif(s.nom_commune, ''), u.nom_commune) AS nom_commune
        FROM {source} s LEFT JOIN umbrella_city u ON u.id = s.id""")
    return {"rows_nocity_src": int(rows_nocity), "umbrella_filled": int(filled),
            "umbrella_cross_state": int(cross)}


# Countries whose source repeats whole rows verbatim under different feature ids (US survey
# 2026-10-19: one point three times). Only these get the merge: the accepted row counts of the
# other sheets stay as measured. The merge rewrites the Parquet, so `canon` must be the view.
EXACT_DUPLICATE_MERGE = frozenset({"US"})
assert EXACT_DUPLICATE_MERGE <= STREAMED
# Every canon column but the feature id: two rows equal on all of them are one address.
CANON_KEY = ("numero", "rep", "nom_voie", "nom_voie_norm", "code_postal", "code_postal_display",
             "code_insee", "nom_commune", "nom_commune_norm", "provincia_norm", "street_key",
             "lon", "lat")


def merge_exact_duplicates(con, out_parquet: pathlib.Path) -> int:
    """Keep one row per verbatim repeat in the written canon Parquet; return the rows removed.

    Three local passes instead of one wide DISTINCT: the hash pass keeps only (hash, count)
    per row (fits the memory bound at 100M rows), the full-column grouping runs on the repeated
    rows alone, and the copy streams. The first occurrence (lowest file row number) survives."""
    key = "hash(" + ", ".join(CANON_KEY) + ")"
    numbered = f"read_parquet('{out_parquet}', file_row_number=true)"
    con.execute(f"CREATE OR REPLACE TABLE repeated AS SELECT {key} AS k FROM {numbered} "
                "GROUP BY k HAVING count(*) > 1")
    if con.execute("SELECT count(*) FROM repeated").fetchone()[0] == 0:
        return 0
    con.execute(f"""CREATE OR REPLACE TABLE survivors AS
        SELECT min(file_row_number) AS file_row_number FROM {numbered}
        WHERE {key} IN (SELECT k FROM repeated)
        GROUP BY {", ".join(CANON_KEY)}""")
    merged = out_parquet.with_name(out_parquet.stem + ".merged.parquet")
    con.execute(f"""COPY (SELECT * EXCLUDE (file_row_number) FROM {numbered}
                         WHERE {key} NOT IN (SELECT k FROM repeated)
                            OR file_row_number IN (SELECT file_row_number FROM survivors))
                    TO '{merged}' (FORMAT parquet, COMPRESSION zstd)""")
    before = con.execute(f"SELECT count(*) FROM {numbered}").fetchone()[0]
    after = con.execute(f"SELECT count(*) FROM read_parquet('{merged}')").fetchone()[0]
    os.replace(merged, out_parquet)
    return before - after


def main(argv: list[str] | None = None) -> None:
    args = _parse_args(argv)
    cc = args.country.upper()
    geonames_file = args.geonames
    # License/catalog/release validation is deliberately before connect_tuned(), extension
    # loading and construction/execution of the remote S3 query.
    if cc in STREAMED and geonames_file and cc not in CITY_UMBRELLA:
        # Every streamed row already carries its region, so the umbrella would never fire; it
        # would only turn the region into a search alias (the "Roma" risk below), and it needs
        # the whole source as a table.
        raise SystemExit(f"STOP: {cc} is built without a GeoNames umbrella (streamed source)")
    release, manifest, _sources = preflight(cc, geonames_file, args.release)
    out_parquet = CODE / "data" / f"{cc.lower()}_norm.parquet"

    t0 = time.time()
    con = connect_for_country(cc, args.offline_extensions)
    load_extensions(con, args.offline_extensions)
    con.execute("SET s3_region='us-west-2';")
    con.execute("SET http_timeout=120000;")   # the network can be flaky
    con.execute("SET http_retries=8;")

    # network stage: fetch only the target country (pruned via row-group statistics)
    s3 = s3_for_release(release, cc)
    try:
        if cc in STREAMED:
            # A view, not a table: both passes stream from S3 instead of holding the rows.
            con.execute("CREATE VIEW src AS" + select_src_sql(cc, s3))
        else:
            con.execute(create_src_sql(cc, s3))
    except Exception as exc:
        if cc == "DE":
            _write_stats(cc, _de_failed_stats(release, manifest, "create_src", exc))
        raise
    total = con.execute("SELECT count(*) FROM src").fetchone()[0]

    de_witness = None
    if cc == "DE":
        try:
            de_witness = _de_extract_witness(con, release, manifest, total)
            problems = de_stats_witness_problems(de_witness, release)
        except Exception as exc:
            _write_stats(cc, _de_failed_stats(release, manifest, "source_evidence", exc))
            raise
        if problems:
            de_witness["status"] = "failed"
            de_witness["failure_stage"] = "source_evidence_validation"
            de_witness["errors"] = problems
            _write_stats(cc, de_witness)
            raise SystemExit("STOP: DE source/Land witness failed: " + "; ".join(problems))
        # Persist the validated evidence before the expensive normalization. It is deliberately
        # not release-ready until the final status below becomes `ok`.
        de_witness["status"] = "extract_validated"
        _write_stats(cc, de_witness)

    source = "src"  # what the canon passes read; the city umbrella swaps in a joined view
    umbrella_stats = None
    if geonames_file and cc in CITY_UMBRELLA:
        # The city itself, not an alias: rows without a city get the nearest populated place of
        # their own state (prep/us_umbrella.py) in one extra streamed scan, then join back by id.
        umbrella_stats = fill_missing_cities(con, geonames_file, source="src", out_view="src_city")
        source = "src_city"
    elif geonames_file:
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

    # The commune ALIAS comes ONLY from the GeoNames umbrella (city level). The Overture
    # province (address_levels[-2]) is too broad as an alias — the "Roma" province spans
    # hundreds of communes, so province-as-alias misroutes city-name queries. The
    # province stays in the KEY (code_insee) to split same-named communes. The city umbrella
    # (CITY_UMBRELLA) fills nom_commune itself and leaves provincia the bare state code, which
    # must not become an alias either.
    prov_alias = NORM.format(col="provincia") if geonames_file and cc not in CITY_UMBRELLA else "''"
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
    if cc in STREAMED:
        # Streamed: pass 1 builds the small locality-key table, pass 2 copies the canon rows
        # straight to Parquet. Same codes as the window form (tools/test_br_overture.py).
        kept = stream_canon(con, cc, insee_order, prov_alias, out_parquet, source=source)
    else:
        con.execute("CREATE TABLE canon AS " + canon_sql(insee_order, prov_alias))
        kept = con.execute("SELECT count(*) FROM canon").fetchone()[0]
        con.execute(f"COPY canon TO '{out_parquet}' (FORMAT parquet, COMPRESSION zstd)")
    duplicates = merge_exact_duplicates(con, out_parquet) if cc in EXACT_DUPLICATE_MERGE else None

    metrics = {
        "country": cc,
        "release": release,
        "rows_src": total,
        "rows_kept": kept - (duplicates or 0),
        "rows_dropped": total - kept,  # filter drops only; verbatim repeats are counted apart
    }
    if duplicates is not None:
        metrics["rows_duplicate"] = duplicates
    if umbrella_stats is not None:
        metrics.update(umbrella_stats)  # rows_nocity_src, umbrella_filled, umbrella_cross_state
    metrics.update({
        "localities": con.execute("SELECT count(DISTINCT code_insee) FROM canon").fetchone()[0],
        "streets": con.execute("SELECT count(DISTINCT street_key) FROM canon").fetchone()[0],
        "parquet_mb": round(out_parquet.stat().st_size / 1e6, 1),
        "seconds": round(time.time() - t0, 1),
    })
    if cc == "DE":
        assert de_witness is not None
        stats = {**de_witness, **metrics, "status": "ok"}
        problems = de_stats_witness_problems(stats, release)
        if problems:
            stats["status"] = "failed"
            stats["failure_stage"] = "final_stats_validation"
            stats["errors"] = problems
            _write_stats(cc, stats)
            raise SystemExit("STOP: final DE stats witness failed: " + "; ".join(problems))
    else:
        stats = metrics
    _write_stats(cc, stats)
    print(json.dumps(stats, ensure_ascii=False, indent=2))

    # provenance manifest -> SEC_META of the sheet (v6): flat EN strings, the two identity
    # keys (country, layer) are mandatory. Overture ADDRESSES are NOT one blanket CDLA license
    # each national source carries its own, per the Overture attribution
    # page (docs.overturemaps.org/attribution/, verified 2026-07). Distributed via OpenAddresses.
    (CODE / "data" / f"{cc.lower()}_manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
