#!/usr/bin/env python3
"""Canonical public provenance for the German address sheet.

This module is deliberately dependency-free: the extractor, the build
orchestrator and the release gate must validate the same bytes before any of
them opens DuckDB or touches the network.  Germany is a per-source licensed
partial product, not an Overture-wide blanket-license product.
"""
from __future__ import annotations

import copy
import hashlib
import json


DE_RELEASE = "2026-08-19.0"
DE_EXPECTED_ROWS = 19_269_891
DE_COVERAGE = "15/16 Länder; Bavaria not covered"
DE_ACCESS_DATE = "2026-08-20"
DE_BAVARIA_CODE = "DE-BY"

DL_DE_BY_20 = "Datenlizenz Deutschland – Namensnennung – Version 2.0"
DL_DE_ZERO_20 = "Datenlizenz Deutschland – Zero – Version 2.0"
CC_BY_40 = "Creative Commons Attribution 4.0 International"

DL_DE_BY_20_URL = "https://www.govdata.de/dl-de/by-2-0"
DL_DE_ZERO_20_URL = "https://www.govdata.de/dl-de/zero-2-0"
CC_BY_40_URL = "https://creativecommons.org/licenses/by/4.0/legalcode.en"

DE_LICENSE_SUMMARY = (
    f"{DL_DE_BY_20}; {DL_DE_ZERO_20}; {CC_BY_40} "
    "(per source; see source_catalog)"
)
DE_SOURCES_SUMMARY = (
    "Overture Maps Addresses via OpenAddresses; "
    "15 German Länder public address sources"
)
DE_ATTRIBUTION_SUMMARY = (
    "Per-Land provider, source URI, license URI and indication of changes: "
    "see source_catalog"
)
DE_TRANSFORMATIONS = (
    f"ingested via OpenAddresses into Overture Maps Addresses release {DE_RELEASE}",
    "filtered to country=DE and the declared 15-Länder public coverage",
    "normalized to the GridPin canonical address schema and indexed by GridPin",
)


# code -> (Land, source, provider, exact license, exact license URL, exact source URL)
# Source URLs are the primary Land pages accepted in the F0 legal groundwork.
_SOURCE_SPECS = {
    "DE-BW": (
        "Baden-Württemberg",
        "Hauskoordinaten",
        "Landesamt für Geoinformation und Landentwicklung Baden-Württemberg (LGL)",
        DL_DE_BY_20,
        DL_DE_BY_20_URL,
        "https://www.lgl-bw.de/Produkte/Liegenschaftskataster/Hauskoordinaten/",
    ),
    "DE-BE": (
        "Berlin",
        "Georeferenzierte Gebäudeadressen Berlin",
        "Geoportal Berlin / Land Berlin",
        DL_DE_ZERO_20,
        DL_DE_ZERO_20_URL,
        "https://gdi.berlin.de/geonetwork/srv/ger/catalog.search",
    ),
    "DE-BB": (
        "Brandenburg",
        "Georeferenzierte Adresse",
        "Landesvermessung und Geobasisinformation Brandenburg (LGB)",
        DL_DE_BY_20,
        DL_DE_BY_20_URL,
        "https://geobasis-bb.de/lgb/de/geodaten/liegenschaftskataster/georeferenzierte-adresse/",
    ),
    "DE-HB": (
        "Bremen",
        "ALKIS Hauskoordinaten",
        "GeoInformation Bremen",
        CC_BY_40,
        CC_BY_40_URL,
        "https://www.geo.bremen.de/produkte/katasterprodukte/auszuege-aus-dem-liegenschaftskataster-12272",
    ),
    "DE-HH": (
        "Hamburg",
        "ALKIS Adressen Hamburg",
        "Landesbetrieb Geoinformation und Vermessung Hamburg (LGV)",
        DL_DE_BY_20,
        DL_DE_BY_20_URL,
        "https://suche.transparenz.hamburg.de/dataset/alkis-adressen-hamburg6",
    ),
    "DE-HE": (
        "Hessen",
        "Hauskoordinaten ohne postalische Angaben",
        "Hessisches Landesamt für Bodenmanagement und Geoinformation (HLBG)",
        DL_DE_ZERO_20,
        DL_DE_ZERO_20_URL,
        "https://gds.hessen.de/INTERSHOP/web/WFS/HLBG-Geodaten-Site/de_DE/-/EUR/ViewDownloadcenter-Start?path=Liegenschaftskataster%2FHauskoordinaten+ohne+Postalische+Angaben+%28txt%29",
    ),
    "DE-MV": (
        "Mecklenburg-Vorpommern",
        "ALKIS Adressen",
        "Landesamt für innere Verwaltung Mecklenburg-Vorpommern (LAiV)",
        CC_BY_40,
        CC_BY_40_URL,
        "https://laiv.geodaten-mv.de/afgvk/Liegenschaftskataster/Beschreibung?produkt=ALKIS",
    ),
    "DE-NI": (
        "Niedersachsen",
        "Liegenschaftskataster Hauskoordinaten",
        "Landesamt für Geoinformation und Landesvermessung Niedersachsen (LGLN)",
        CC_BY_40,
        CC_BY_40_URL,
        "https://ni-lgln-opengeodata.hub.arcgis.com/search?tags=liegenschaftskataster",
    ),
    "DE-NW": (
        "Nordrhein-Westfalen",
        "Georeferenzierte Gebäudeadressen (gebref_txt)",
        "Bezirksregierung Köln, Geobasis NRW",
        DL_DE_ZERO_20,
        DL_DE_ZERO_20_URL,
        "https://www.opengeodata.nrw.de/produkte/geobasis/lk/akt/gebref_txt/",
    ),
    "DE-RP": (
        "Rheinland-Pfalz",
        "Hauskoordinaten",
        "Landesamt für Vermessung und Geobasisinformation Rheinland-Pfalz (LVermGeo)",
        DL_DE_BY_20,
        DL_DE_BY_20_URL,
        "https://lvermgeo.rlp.de/geodaten-geoshop/open-data",
    ),
    "DE-SL": (
        "Saarland",
        "Hauskoordinaten",
        "Landesamt für Vermessung, Geoinformation und Landentwicklung Saarland (LVGL)",
        DL_DE_BY_20,
        DL_DE_BY_20_URL,
        "https://geoportal.saarland.de/",
    ),
    "DE-SN": (
        "Sachsen",
        "Hauskoordinaten",
        "Staatsbetrieb Geobasisinformation und Vermessung Sachsen (GeoSN)",
        DL_DE_BY_20,
        DL_DE_BY_20_URL,
        "https://www.geodaten.sachsen.de/downloadbereich-hauskoordinaten-4172.html",
    ),
    "DE-ST": (
        "Sachsen-Anhalt",
        "Hauskoordinaten",
        "Landesamt für Vermessung und Geoinformation Sachsen-Anhalt (LVermGeo)",
        DL_DE_BY_20,
        DL_DE_BY_20_URL,
        "https://www.lvermgeo.sachsen-anhalt.de/de/gdp-open-data.html",
    ),
    "DE-SH": (
        "Schleswig-Holstein",
        "Liegenschaftskataster Hauskoordinaten",
        "Landesamt für Vermessung und Geoinformation Schleswig-Holstein (LVermGeo SH)",
        CC_BY_40,
        CC_BY_40_URL,
        "https://geodaten.schleswig-holstein.de/gaialight-sh/_apps/dladownload/lizenz.html",
    ),
    "DE-TH": (
        "Thüringen",
        "Hauskoordinaten",
        "Thüringer Landesamt für Bodenmanagement und Geoinformation (TLBG)",
        DL_DE_BY_20,
        DL_DE_BY_20_URL,
        "https://geoportal.thueringen.de/gdi-th/download-offene-geodaten",
    ),
}

DE_EXPECTED_LAND_CODES = frozenset(_SOURCE_SPECS)

# Exact root ``SourceItem.dataset`` strings observed in the sole authorized
# 2026-08-19.0 DE scan.  The retained witness saw blank license text and the
# root property (``""``); its old coalesce could not distinguish missing, null,
# and empty license states.  Runtime identity therefore has to bind Land + this
# exact dataset root to the already-preflighted legal catalog.  This is
# intentionally not a prefix/family allow-list: a new spelling or suffix is new
# evidence and must fail closed until independently reviewed.
_OVERTURE_DATASET_ROOTS = {
    "DE-BB": "OpenAddresses/GeoBasis-DE/LGB",
    "DE-BE": "OpenAddresses/Geoportal Berlin",
    "DE-BW": "OpenAddresses/LGL",
    "DE-HB": "OpenAddresses/Landesamt GeoInformation Bremen",
    "DE-HE": "OpenAddresses/HLBG",
    "DE-HH": "OpenAddresses/HVBG",
    "DE-MV": "OpenAddresses/GeoBasis-DE/MV",
    "DE-NI": "OpenAddresses/GeoBasis-DE/LGLN",
    "DE-NW": "OpenAddresses/Geobasis NRW",
    "DE-RP": "OpenAddresses/GeoBasis-DE/LVermGeo RP",
    "DE-SH": "OpenAddresses/GeoBasis-DE/LVermGeo SH",
    "DE-SL": "OpenAddresses/GeoBasis-DE/LVermGeo SL",
    "DE-SN": "OpenAddresses/SGVS",
    "DE-ST": "OpenAddresses/GeoBasis-DE/LVermGeo ST",
    "DE-TH": "OpenAddresses/Freistaat Thüringen",
}
if set(_OVERTURE_DATASET_ROOTS) != DE_EXPECTED_LAND_CODES:
    raise RuntimeError("DE Overture dataset-root catalog must cover exactly 15 Länder")

# Per-Land counts from that same immutable release scan.  Pinning the vector,
# not merely its total, prevents a wrong Land/dataset join from balancing out
# against another Land while retaining the known 19,269,891-row denominator.
DE_EXPECTED_LAND_ROWS = {
    "DE-BB": 867_290,
    "DE-BE": 394_933,
    "DE-BW": 3_377_947,
    "DE-HB": 176_304,
    "DE-HE": 1_628_120,
    "DE-HH": 284_575,
    "DE-MV": 515_149,
    "DE-NI": 2_558_127,
    "DE-NW": 4_502_731,
    "DE-RP": 1_403_783,
    "DE-SH": 948_763,
    "DE-SL": 332_936,
    "DE-SN": 990_090,
    "DE-ST": 664_183,
    "DE-TH": 624_960,
}
if (
    set(DE_EXPECTED_LAND_ROWS) != DE_EXPECTED_LAND_CODES
    or sum(DE_EXPECTED_LAND_ROWS.values()) != DE_EXPECTED_ROWS
):
    raise RuntimeError("DE per-Land row pins must cover the exact F2 COUNT")


def _entry(code: str, spec: tuple[str, str, str, str, str, str]) -> dict:
    land, source, provider, license_name, license_url, source_url = spec
    changes = "; ".join(DE_TRANSFORMATIONS)
    return {
        "land": land,
        "source": source,
        "provider": provider,
        "license": license_name,
        "source_url": source_url,
        "license_url": license_url,
        "overture_dataset_root": _OVERTURE_DATASET_ROOTS[code],
        "accessed_at": DE_ACCESS_DATE,
        "transformations": list(DE_TRANSFORMATIONS),
        "attribution": (
            f"{provider} — {source}; {license_name}; source: {source_url}; "
            f"accessed {DE_ACCESS_DATE}; changes: {changes}"
        ),
    }


_CANONICAL_CATALOG = {
    code: _entry(code, spec) for code, spec in sorted(_SOURCE_SPECS.items())
}
# The legal provider is taken from the independently preflighted catalog, not
# synthesized from optional runtime SourceItem metadata.  When SourceItem's
# ``license`` value is present, the official attribution page historically used
# both SPDX spellings and an arrow spelling.  Keep the accepted aliases explicit
# and tiny; never fuzzy-match a licence at runtime.  An absent observed license
# is not rewritten: it is accepted only by the exact catalog join below.
_OBSERVED_LICENSE_ALIASES = {
    DL_DE_BY_20: frozenset({"DL-DE-BY-2.0", "DL-DE->BY-2.0"}),
    DL_DE_ZERO_20: frozenset({
        "DL-DE-ZERO-2.0",
        "DL-DE->Zero-2.0",
        "DL-DE->ZERO-2.0",
    }),
    CC_BY_40: frozenset({"CC-BY-4.0", "CC BY 4.0", "CC BY-4.0"}),
}


def canonical_json(value: object) -> str:
    """Stable UTF-8 JSON representation used by all provenance hashes."""
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    )


def canonical_sha256(value: object) -> str:
    return hashlib.sha256(canonical_json(value).encode("utf-8")).hexdigest()


def source_catalog() -> dict:
    """Return a defensive copy; callers must never mutate the source of truth."""
    return copy.deepcopy(_CANONICAL_CATALOG)


def canonical_catalog_sha256() -> str:
    return canonical_sha256(_CANONICAL_CATALOG)


DE_SOURCE_CATALOG_SHA256 = canonical_catalog_sha256()


def catalog_problems(catalog: object) -> list[str]:
    if not isinstance(catalog, dict):
        return ["source_catalog must be an object keyed by ISO 3166-2 Land code"]
    problems = []
    got_codes = set(catalog)
    missing = sorted(DE_EXPECTED_LAND_CODES - got_codes)
    extra = sorted(got_codes - DE_EXPECTED_LAND_CODES)
    if missing:
        problems.append(f"source_catalog missing Länder: {missing}")
    if extra:
        problems.append(f"source_catalog has forbidden/unknown Länder: {extra}")
    if DE_BAVARIA_CODE in got_codes:
        problems.append("source_catalog must not contain DE-BY (Bavaria)")
    for code in sorted(DE_EXPECTED_LAND_CODES & got_codes):
        got = catalog.get(code)
        want = _CANONICAL_CATALOG[code]
        if not isinstance(got, dict):
            problems.append(f"source_catalog[{code}] must be an object")
            continue
        missing_fields = sorted(set(want) - set(got))
        extra_fields = sorted(set(got) - set(want))
        if missing_fields:
            problems.append(f"source_catalog[{code}] missing fields: {missing_fields}")
        if extra_fields:
            problems.append(f"source_catalog[{code}] has unknown fields: {extra_fields}")
        for field, expected in want.items():
            if got.get(field) != expected:
                problems.append(f"source_catalog[{code}].{field} differs from canonical value")
    if isinstance(catalog, dict) and canonical_sha256(catalog) != DE_SOURCE_CATALOG_SHA256:
        problems.append("source_catalog canonical SHA-256 mismatch")
    return problems


def validate_catalog(catalog: object) -> None:
    problems = catalog_problems(catalog)
    if problems:
        raise ValueError("; ".join(problems))


def _require_release(release: object) -> str:
    value = str(release or "").strip()
    if value != DE_RELEASE:
        raise ValueError(f"DE release must be exactly {DE_RELEASE}, got {value!r}")
    return value


def build_manifest(release: str) -> dict:
    """Build the only accepted public DE manifest from this module's catalog."""
    release = _require_release(release)
    catalog = source_catalog()
    validate_catalog(catalog)
    return {
        "country": "de",
        "layer": "addresses",
        "license": DE_LICENSE_SUMMARY,
        "sources": DE_SOURCES_SUMMARY,
        "source_release": release,
        "coverage": DE_COVERAGE,
        "attribution": DE_ATTRIBUTION_SUMMARY,
        "source_catalog_sha256": DE_SOURCE_CATALOG_SHA256,
        "source_catalog": catalog,
    }


def _decoded_catalog(value: object) -> object:
    if not isinstance(value, str):
        return value
    try:
        return json.loads(value)
    except json.JSONDecodeError as exc:
        raise ValueError(f"source_catalog is not valid JSON: {exc}") from exc


def manifest_problems(obj: object, release: str) -> list[str]:
    try:
        release = _require_release(release)
    except ValueError as exc:
        return [str(exc)]
    if not isinstance(obj, dict):
        return ["DE manifest must be a JSON object"]
    expected = build_manifest(release)
    problems = []
    for key in (
        "country",
        "layer",
        "license",
        "sources",
        "source_release",
        "coverage",
        "attribution",
        "source_catalog_sha256",
    ):
        if obj.get(key) != expected[key]:
            problems.append(f"DE manifest {key} differs from canonical value")
    try:
        catalog = _decoded_catalog(obj.get("source_catalog"))
    except ValueError as exc:
        problems.append(str(exc))
    else:
        problems.extend(catalog_problems(catalog))
    return problems


def validate_manifest(obj: object, release: str) -> None:
    """Validate raw JSON manifests and flat-string SEC_META dictionaries alike."""
    problems = manifest_problems(obj, release)
    if problems:
        raise ValueError("; ".join(problems))


# The authorized live release proved address_levels[1].value is the exact
# two-letter subdivision suffix (BB, BE, ...), not the full Land name.  Bind
# only the 15 catalogued suffixes; BY is recognized solely so its count cannot
# hide in the unknown bucket and must trip the explicit Bavaria=0 gate.
_LAND_RAW_CODE_TO_CODE = {
    code.removeprefix("DE-"): code
    for code in DE_EXPECTED_LAND_CODES
}
_LAND_RAW_CODE_TO_CODE["BY"] = DE_BAVARIA_CODE


def land_code_for_name(value: object) -> str | None:
    """Map only the exact observed two-letter subdivision suffix; never guess."""
    return _LAND_RAW_CODE_TO_CODE.get(value) if isinstance(value, str) else None


def observed_license_for_code(code: str) -> str:
    """Return the preferred observed SourceItem licence spelling for fixtures."""
    record = _CANONICAL_CATALOG[code]
    preferred = {
        DL_DE_BY_20: "DL-DE-BY-2.0",
        DL_DE_ZERO_20: "DL-DE-ZERO-2.0",
        CC_BY_40: "CC-BY-4.0",
    }
    return preferred[record["license"]]


def build_land_witness(raw_counts: list[tuple[object, object]]) -> dict:
    """Bind exact address_levels[1] codes without guessing unknown aliases."""
    counts = {code: 0 for code in sorted(DE_EXPECTED_LAND_CODES | {DE_BAVARIA_CODE})}
    raw = []
    unknown = {}
    for raw_name, raw_count in raw_counts:
        name = str(raw_name or "").strip()
        count = int(raw_count)
        raw.append({"land_raw": name, "rows": count})
        code = _LAND_RAW_CODE_TO_CODE.get(name)
        if code is None:
            unknown[name] = unknown.get(name, 0) + count
        else:
            counts[code] += count
    return {
        "land_level_expression": "address_levels[1].value",
        "raw_land_counts": sorted(raw, key=lambda item: item["land_raw"]),
        "observed_land_counts": counts,
        "observed_land_codes": sorted(
            code for code in DE_EXPECTED_LAND_CODES if counts.get(code, 0) > 0
        ),
        "unknown_land_raw_counts": dict(sorted(unknown.items())),
        "bavaria_rows": counts[DE_BAVARIA_CODE],
    }


def stats_witness_problems(stats: object, release: str) -> list[str]:
    """Validate the DE extract witness consumed by the strict sheet gate."""
    try:
        release = _require_release(release)
    except ValueError as exc:
        return [str(exc)]
    if not isinstance(stats, dict):
        return ["DE stats witness must be a JSON object"]
    problems = []
    if stats.get("country") != "DE":
        problems.append("DE stats country must be 'DE'")
    if stats.get("release") != release:
        problems.append("DE stats release differs from pinned release")
    if stats.get("coverage") != DE_COVERAGE:
        problems.append("DE stats coverage statement differs from canonical value")
    if stats.get("source_catalog_sha256") != DE_SOURCE_CATALOG_SHA256:
        problems.append("DE stats source_catalog_sha256 differs from canonical value")
    catalog = stats.get("source_catalog")
    catalog_issues = catalog_problems(catalog)
    problems.extend(catalog_issues)
    # Source evidence may join only to a catalog which independently passed the
    # exact canonical preflight above.  A malformed catalog never becomes a
    # permissive fallback for observed rows.
    joined_catalog = catalog if isinstance(catalog, dict) and not catalog_issues else {}
    if stats.get("status") != "ok":
        problems.append("DE stats status is not 'ok'")

    rows_src = stats.get("rows_src")
    if not isinstance(rows_src, int) or isinstance(rows_src, bool) or rows_src <= 0:
        problems.append("DE stats rows_src must be a positive integer")
    elif rows_src != DE_EXPECTED_ROWS:
        problems.append(
            f"DE stats rows_src must equal pinned F2 COUNT {DE_EXPECTED_ROWS}"
        )

    counts = stats.get("observed_land_counts")
    expected_count_keys = DE_EXPECTED_LAND_CODES | {DE_BAVARIA_CODE}
    if not isinstance(counts, dict) or set(counts) != expected_count_keys:
        problems.append("DE observed_land_counts must contain exactly 15 Länder plus DE-BY")
    else:
        for code in sorted(DE_EXPECTED_LAND_CODES):
            if not isinstance(counts.get(code), int) or counts[code] <= 0:
                problems.append(f"DE observed_land_counts[{code}] must be positive")
            elif counts[code] != DE_EXPECTED_LAND_ROWS[code]:
                problems.append(
                    f"DE observed_land_counts[{code}] differs from pinned release count")
        if counts.get(DE_BAVARIA_CODE) != 0:
            problems.append("DE-BY/Bavaria observed row count must be zero")
        if isinstance(rows_src, int) and sum(counts.values()) != rows_src:
            problems.append("DE observed Land counts do not sum to rows_src")
    if stats.get("observed_land_codes") != sorted(DE_EXPECTED_LAND_CODES):
        problems.append("DE observed_land_codes is not the exact canonical 15-Länder set")
    if stats.get("unknown_land_raw_counts") != {}:
        problems.append("DE stats contain unknown address_levels[1] Land names")
    if stats.get("bavaria_rows") != 0:
        problems.append("DE stats do not prove Bavaria count zero")
    raw_counts = stats.get("raw_land_counts")
    if not isinstance(raw_counts, list):
        problems.append("DE stats raw_land_counts must be a list")
    else:
        try:
            rebuilt = build_land_witness([
                (item["land_raw"], item["rows"])
                for item in raw_counts
                if isinstance(item, dict)
            ])
        except (KeyError, TypeError, ValueError):
            problems.append("DE stats raw_land_counts is malformed")
        else:
            if len(rebuilt["raw_land_counts"]) != len(raw_counts):
                problems.append("DE stats raw_land_counts contains non-object entries")
            for field in (
                "land_level_expression",
                "raw_land_counts",
                "observed_land_counts",
                "observed_land_codes",
                "unknown_land_raw_counts",
                "bavaria_rows",
            ):
                if stats.get(field) != rebuilt[field]:
                    problems.append(f"DE stats {field} does not match raw_land_counts")

    evidence = stats.get("observed_source_evidence")
    if not isinstance(evidence, dict):
        problems.append("DE stats have no observed_source_evidence object")
        return problems
    if evidence.get("dimensions") != [
        "land_code", "land_raw", "dataset", "license", "license_state", "property"
    ]:
        problems.append("DE observed source dimensions differ from the extraction contract")
    items = evidence.get("items")
    if not isinstance(items, list) or not items:
        problems.append("DE observed_source_evidence.items must be a non-empty list")
    else:
        observed_root_codes = set()
        observed_root_rows = {code: 0 for code in DE_EXPECTED_LAND_CODES}
        counted_items = 0
        counted_root_items = 0
        for number, item in enumerate(items, 1):
            if not isinstance(item, dict):
                problems.append(f"DE observed source item #{number} is not an object")
                continue
            for field in (
                "land_code", "land_raw", "dataset", "license", "license_state", "property"
            ):
                if not isinstance(item.get(field), str):
                    problems.append(
                        f"DE observed source item #{number} {field} is not a string")
            for field in ("land_code", "land_raw", "dataset"):
                if isinstance(item.get(field), str) and not item[field].strip():
                    problems.append(f"DE observed source item #{number} has no {field}")
            code = item.get("land_code")
            raw_code = land_code_for_name(item.get("land_raw"))
            land_matches = (
                isinstance(code, str)
                and code in DE_EXPECTED_LAND_CODES
                and raw_code == code
            )
            if not land_matches:
                problems.append(
                    f"DE observed source item #{number} has inconsistent/unknown Land")
            catalog_record = joined_catalog.get(code) if land_matches else None
            expected_dataset = (
                catalog_record.get("overture_dataset_root")
                if isinstance(catalog_record, dict)
                else None
            )
            dataset = item.get("dataset")
            dataset_matches = (
                isinstance(dataset, str)
                and isinstance(expected_dataset, str)
                and dataset == expected_dataset
            )
            if not dataset_matches:
                problems.append(
                    f"DE observed source item #{number} dataset root does not match {code}"
                )
            license_name = item.get("license")
            license_state = item.get("license_state")
            prop = item.get("property")
            if isinstance(prop, str) and prop != "":
                problems.append(
                    f"DE observed source item #{number} is not a dataset-root SourceItem")
            license_matches = False
            absence_states = {"missing", "null", "empty"}
            if license_state in absence_states and license_name == "":
                # Preserve the observed absence.  It is sufficient only for the
                # exact root identity already joined to the legal catalog; no
                # observed licence string is synthesized or attributed to Overture.
                license_matches = prop == "" and land_matches and dataset_matches
                if not license_matches:
                    problems.append(
                        f"DE observed source item #{number} empty license lacks exact root identity")
            elif (
                license_state == "value"
                and isinstance(license_name, str)
                and bool(license_name)
                and isinstance(catalog_record, dict)
            ):
                canonical_license = catalog_record.get("license")
                license_matches = license_name in _OBSERVED_LICENSE_ALIASES.get(
                    canonical_license, frozenset())
                if not license_matches:
                    problems.append(
                        f"DE observed source item #{number} license does not match {code}")
            elif isinstance(license_name, str):
                problems.append(
                    f"DE observed source item #{number} has inconsistent license_state/value")
            rows = item.get("rows")
            if not isinstance(rows, int) or isinstance(rows, bool) or rows <= 0:
                problems.append(f"DE observed source item #{number} has invalid rows")
            else:
                counted_items += rows
            if prop == "":
                if land_matches and dataset_matches and license_matches:
                    observed_root_codes.add(code)
                    if isinstance(rows, int) and not isinstance(rows, bool) and rows > 0:
                        observed_root_rows[code] += rows
                if isinstance(rows, int) and not isinstance(rows, bool) and rows > 0:
                    counted_root_items += rows
        missing_codes = sorted(DE_EXPECTED_LAND_CODES - observed_root_codes)
        if missing_codes:
            problems.append(f"DE observed root source evidence is missing Länder: {missing_codes}")
        if isinstance(counts, dict) and set(counts) == expected_count_keys:
            for code in sorted(DE_EXPECTED_LAND_CODES):
                if observed_root_rows[code] != counts[code]:
                    problems.append(
                        f"DE observed root source rows for {code} do not match Land rows")
        if evidence.get("source_items_total") != counted_items:
            problems.append("DE source_items_total does not equal the sum of observed item counts")
        if evidence.get("root_source_items_total") != counted_root_items:
            problems.append(
                "DE root_source_items_total does not equal the sum of root item counts")
    if isinstance(rows_src, int):
        with_sources = evidence.get("rows_with_sources")
        without_sources = evidence.get("rows_without_sources")
        if with_sources != rows_src or without_sources != 0:
            problems.append("DE source evidence does not cover every source row")
        with_root = evidence.get("rows_with_root_sources")
        without_root = evidence.get("rows_without_root_sources")
        if with_root != rows_src or without_root != 0:
            problems.append("DE root source evidence does not cover every source row")
    return problems


def validate_stats_witness(stats: object, release: str) -> None:
    problems = stats_witness_problems(stats, release)
    if problems:
        raise ValueError("; ".join(problems))
