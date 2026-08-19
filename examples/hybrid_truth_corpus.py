#!/usr/bin/env python3
"""Build a fixed, offline-only Overture + direct-OSM truth corpus.

The builder never contacts a network service.  It consumes the retained BL14
Overture acquisition snapshot and four immutable Geofabrik PBF snapshots,
selecting exactly 50 Overture and 250 direct OpenStreetMap rows per country.
No geocoder response participates in candidate filtering or ranking.

The output schema is intentionally version 4.  The existing public benchmark
runner only accepts its narrower schema-3 Overture contract; accepting this
hybrid corpus requires a separately reviewed runner change.
"""

from __future__ import annotations

import argparse
import collections
import functools
import hashlib
import importlib.util
import json
import math
import os
import pathlib
import platform
import re
import shlex
import shutil
import stat
import subprocess
import sys
import unicodedata
from typing import BinaryIO, Iterable, Mapping, Sequence


EXAMPLES = pathlib.Path(__file__).resolve().parent
OVERTURE_SCRIPT = EXAMPLES / "overture_places_corpus.py"
_OVERTURE_SPEC = importlib.util.spec_from_file_location(
    "hybrid_truth_overture_helpers", OVERTURE_SCRIPT
)
if _OVERTURE_SPEC is None or _OVERTURE_SPEC.loader is None:  # pragma: no cover
    raise RuntimeError(f"cannot load Overture helpers from {OVERTURE_SCRIPT}")
overture = importlib.util.module_from_spec(_OVERTURE_SPEC)
_OVERTURE_SPEC.loader.exec_module(overture)


CorpusError = overture.CorpusError
TRUTH_SCHEMA = 4
DIAGNOSTICS_SCHEMA = 1
MANIFEST_KIND = "hybrid_overture_osm_truth_corpus"
COUNTRIES = tuple(overture.DEFAULT_COUNTRIES)
PER_COUNTRY = 300
MIN_UNKNOWN_PER_COUNTRY = 150
OVERTURE_QUOTA = 50
OSM_QUOTA = 250
OVERTURE_FAMILY = "overture_places"
OSM_FAMILY = "openstreetmap"
OVERTURE_SOURCE_ID = "overture_places_2026_06_17_0"
OVERTURE_ACQUISITION_NAME = (
    "overture-places-2026-06-17.0-instrumented-v3.acquisition.jsonl"
)
OVERTURE_ACQUISITION_SHA256 = (
    "1697a275dfca8e1b5bb3577dd65200d9b6ed3f335b02ec73a748eac2d94707b9"
)
OVERTURE_ACQUISITION_BYTES = 4_233_660
OVERTURE_ACQUISITION_MANIFEST_SHA256 = (
    "ab9b3ea830189ac3d7eaa9d356035503a4ae1d8c698bdc5a74b752d5f8aece16"
)
OVERTURE_ACQUISITION_MANIFEST_BYTES = 30_802
SELECTION_SEED = "gridpin-bl14-hybrid-truth-v1"
LINEAGE_POLICY = overture.LINEAGE_POLICY
LINEAGE_CLASSES = ("outside_chain", "common_upstream", "unknown_lineage")
OSM_LICENSE = "ODbL-1.0"
OSM_ATTRIBUTION = "© OpenStreetMap contributors; ODbL 1.0"
OSM_COPYRIGHT_URL = "https://www.openstreetmap.org/copyright"
PINNED_OSMIUM_VERSION = "1.19.1"
PINNED_LIBOSMIUM_VERSION = "2.23.1"
PINNED_SPATIAL_EXTENSION_VERSION = "b68b309"
CAPS = {
    "municipality": 12,
    "street": 2,
    "cell_0_01_degree": 3,
    "category": 24,
    "network": 9,
}
SELECTION_FORMULA = (
    "SHA-256(seed|truth_source_id|country|record_id|"
    "NFKC-casefold-alnum-space(query)|latitude:.7f|longitude:.7f)"
)


OSM_SOURCES: dict[str, dict[str, object]] = {
    "FR": {
        "region": "alsace",
        "coverage_scope": "regional: Alsace",
        "filename": "alsace-260701.osm.pbf",
        "source_id": "osm_geofabrik_fr_alsace_260701",
        "uri": "https://download.geofabrik.de/europe/france/alsace-260701.osm.pbf",
        "replication_base_url": "https://download.geofabrik.de/europe/france/alsace-updates",
        "sha256": "f8a63f9a31864821a16fa1fd1fd2626a587c4ea2d780a2d863bfa361d19bfaa7",
        "bytes": 129_643_154,
        "snapshot_at": "2026-07-01T20:22:00Z",
        "replication_sequence": 4830,
    },
    "IT": {
        "region": "isole",
        "coverage_scope": "regional: Isole",
        "filename": "isole-260701.osm.pbf",
        "source_id": "osm_geofabrik_it_isole_260701",
        "uri": "https://download.geofabrik.de/europe/italy/isole-260701.osm.pbf",
        "replication_base_url": "https://download.geofabrik.de/europe/italy/isole-updates",
        "sha256": "b820ee216ef76b326bf1306ea946abfbfe58bdf08db9147289cf556d22665e88",
        "bytes": 212_733_976,
        "snapshot_at": "2026-07-01T20:22:00Z",
        "replication_sequence": 3893,
    },
    "NL": {
        "region": "drenthe",
        "coverage_scope": "regional: Drenthe",
        "filename": "drenthe-260701.osm.pbf",
        "source_id": "osm_geofabrik_nl_drenthe_260701",
        "uri": "https://download.geofabrik.de/europe/netherlands/drenthe-260701.osm.pbf",
        "replication_base_url": "https://download.geofabrik.de/europe/netherlands/drenthe-updates",
        "sha256": "2814290012b08420820e2ef47373156707128de68a2b15783302e8a6feafa326",
        "bytes": 62_699_767,
        "snapshot_at": "2026-07-01T20:22:00Z",
        "replication_sequence": 2774,
    },
    "RS": {
        "region": "serbia",
        "coverage_scope": "national: Serbia",
        "filename": "serbia-260701.osm.pbf",
        "source_id": "osm_geofabrik_rs_serbia_260701",
        "uri": "https://download.geofabrik.de/europe/serbia-260701.osm.pbf",
        "replication_base_url": "https://download.geofabrik.de/europe/serbia-updates",
        "sha256": "0d5e526a7411e6a0dd7400bf188392d79de17477bd0612dee216a5a255fb83d0",
        "bytes": 236_966_213,
        "snapshot_at": "2026-07-01T20:22:00Z",
        "replication_sequence": 4835,
    },
}


def _utc_now() -> str:
    return overture._utc_now()


def _norm(value: object) -> str:
    return overture._norm(value)


def _json_bytes(value: Mapping[str, object]) -> bytes:
    return overture._json_bytes(value)


def _jsonl_bytes(rows: Iterable[Mapping[str, object]]) -> bytes:
    return overture._jsonl_bytes(rows)


def _artifact_manifest_path(output: pathlib.Path) -> pathlib.Path:
    return output.with_suffix(output.suffix + ".manifest.json")


def _diagnostics_path(output: pathlib.Path) -> pathlib.Path:
    return output.with_suffix(output.suffix + ".diagnostics.json")


def _identity(info: os.stat_result) -> tuple[int, int, int, int]:
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)


def _capture_streaming(path: pathlib.Path, label: str) -> dict[str, object]:
    """Capture one immutable regular file without loading it into memory."""

    path = path.absolute()
    handle, before = overture._open_regular_nofollow(path, label)
    digest = hashlib.sha256()
    with handle:
        while True:
            chunk = handle.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
        after = os.fstat(handle.fileno())
    if _identity(before) != _identity(after):
        raise CorpusError(f"{label} changed while it was hashed: {path}")
    try:
        current = path.lstat()
    except FileNotFoundError as exc:
        raise CorpusError(f"{label} disappeared while it was hashed: {path}") from exc
    if not stat.S_ISREG(current.st_mode) or _identity(current) != _identity(after):
        raise CorpusError(f"{label} path identity changed while it was hashed: {path}")
    return {
        "_path": path,
        "_identity": _identity(after),
        "logical_name": path.name,
        "sha256": digest.hexdigest(),
        "bytes": after.st_size,
    }


def _verify_capture(capture: Mapping[str, object], label: str) -> None:
    current = _capture_streaming(pathlib.Path(str(capture["_path"])), label)
    if (
        current["_identity"] != capture["_identity"]
        or current["sha256"] != capture["sha256"]
        or current["bytes"] != capture["bytes"]
    ):
        raise CorpusError(f"{label} changed after its start-of-run capture")


def _public_capture(capture: Mapping[str, object]) -> dict[str, object]:
    return {
        "logical_name": capture["logical_name"],
        "sha256": capture["sha256"],
        "bytes": capture["bytes"],
    }


def _require_fixed_capture(
    capture: Mapping[str, object],
    *,
    label: str,
    logical_name: str,
    sha256: str,
    byte_count: int,
) -> None:
    if capture.get("logical_name") != logical_name:
        raise CorpusError(
            f"{label} must be the fixed {logical_name}; "
            f"got {capture.get('logical_name')}"
        )
    if capture.get("sha256") != sha256 or capture.get("bytes") != byte_count:
        raise CorpusError(f"{label} does not match the pinned SHA-256 and byte count")


def _capture_fixed_overture(
    acquisition_path: pathlib.Path,
) -> tuple[dict[str, object], dict[str, object]]:
    raw_capture = _capture_streaming(acquisition_path, "Overture acquisition")
    manifest_capture = _capture_streaming(
        overture._raw_manifest_path(acquisition_path),
        "Overture acquisition manifest",
    )
    _require_fixed_capture(
        raw_capture,
        label="Overture acquisition",
        logical_name=OVERTURE_ACQUISITION_NAME,
        sha256=OVERTURE_ACQUISITION_SHA256,
        byte_count=OVERTURE_ACQUISITION_BYTES,
    )
    _require_fixed_capture(
        manifest_capture,
        label="Overture acquisition manifest",
        logical_name=f"{OVERTURE_ACQUISITION_NAME}.manifest.json",
        sha256=OVERTURE_ACQUISITION_MANIFEST_SHA256,
        byte_count=OVERTURE_ACQUISITION_MANIFEST_BYTES,
    )
    return raw_capture, manifest_capture


def _safe_source_url(value: object, label: str) -> str:
    if not isinstance(value, str) or re.fullmatch(r"https://[^\s]+", value) is None:
        raise CorpusError(f"{label} must be an absolute HTTPS URL")
    return value


@functools.lru_cache(maxsize=1)
def _osmium_runtime() -> dict[str, object]:
    discovered = shutil.which("osmium")
    if discovered is None:
        raise CorpusError("osmium-tool is required for local PBF provenance")
    # Homebrew and most package managers expose osmium through a symlink.  The
    # command lookup is allowed to resolve that installation link, but the
    # artifact we subsequently hash is the resolved single-link regular file.
    # This keeps the no-follow capture invariant without rejecting normal,
    # immutable package-manager layouts.
    try:
        executable = str(pathlib.Path(discovered).resolve(strict=True))
    except OSError as exc:
        raise CorpusError(f"cannot resolve osmium-tool executable: {exc}") from exc
    process = subprocess.run(
        [executable, "--version"], capture_output=True, text=True, timeout=30
    )
    if process.returncode != 0:
        raise CorpusError(f"cannot identify osmium-tool: {process.stderr.strip()[:300]}")
    lines = [line.strip() for line in process.stdout.splitlines() if line.strip()]
    if not lines or re.fullmatch(r"osmium version \d+\.\d+\.\d+", lines[0]) is None:
        raise CorpusError("osmium-tool returned an unrecognized version string")
    libosmium_lines = [line for line in lines if line.startswith("libosmium version ")]
    osmium_version = lines[0].removeprefix("osmium version ")
    libosmium_version = (
        libosmium_lines[0].removeprefix("libosmium version ")
        if len(libosmium_lines) == 1
        else ""
    )
    if (
        osmium_version != PINNED_OSMIUM_VERSION
        or libosmium_version != PINNED_LIBOSMIUM_VERSION
    ):
        raise CorpusError(
            "runtime must use osmium-tool "
            f"{PINNED_OSMIUM_VERSION}/libosmium {PINNED_LIBOSMIUM_VERSION}"
        )
    binary = _capture_streaming(pathlib.Path(executable), "osmium executable")
    return {
        "executable": executable,
        "version": osmium_version,
        "libosmium_version": libosmium_version,
        "binary": binary,
    }


def _pbf_metadata(
    country: str, capture: Mapping[str, object]
) -> tuple[dict[str, object], dict[str, object]]:
    cfg = OSM_SOURCES[country]
    _require_fixed_capture(
        capture,
        label=f"{country} PBF",
        logical_name=str(cfg["filename"]),
        sha256=str(cfg["sha256"]),
        byte_count=int(cfg["bytes"]),
    )
    runtime = _osmium_runtime()
    process = subprocess.run(
        [str(runtime["executable"]), "fileinfo", "-e", "-j", str(capture["_path"])],
        capture_output=True,
        text=True,
        timeout=300,
    )
    if process.returncode != 0:
        raise CorpusError(f"osmium fileinfo failed for {country}: {process.stderr.strip()[:300]}")
    try:
        payload = json.loads(process.stdout)
        file_info = payload["file"]
        header = payload["header"]
        data = payload["data"]
        options = header["option"]
    except (KeyError, TypeError, json.JSONDecodeError) as exc:
        raise CorpusError(f"invalid osmium fileinfo JSON for {country}") from exc
    if file_info.get("format") != "PBF" or file_info.get("size") != capture["bytes"]:
        raise CorpusError(f"{country} PBF fileinfo does not match the captured artifact")
    if header.get("with_history") is not False:
        raise CorpusError(f"{country} PBF must be a current, non-history snapshot")
    if data.get("objects_ordered") is not True or data.get("multiple_versions") is not False:
        raise CorpusError(f"{country} PBF must be ordered with one object version")
    if options.get("osmosis_replication_base_url") != cfg["replication_base_url"]:
        raise CorpusError(f"{country} PBF replication origin does not match the fixed source")
    snapshot_at = overture._canonical_utc(
        str(options.get("osmosis_replication_timestamp", ""))
    )
    if options.get("timestamp") != snapshot_at:
        raise CorpusError(f"{country} PBF header timestamps disagree")
    sequence = str(options.get("osmosis_replication_sequence_number", ""))
    if re.fullmatch(r"[1-9][0-9]*", sequence) is None:
        raise CorpusError(f"{country} PBF has no valid replication sequence")
    if snapshot_at != cfg["snapshot_at"] or int(sequence) != cfg["replication_sequence"]:
        raise CorpusError(
            f"{country} PBF replication timestamp/sequence does not match the fixed snapshot"
        )
    _safe_source_url(cfg["uri"], f"{country} source URI")
    metadata = {
        "format": "PBF",
        "snapshot_at": snapshot_at,
        "replication_sequence": int(sequence),
        "replication_base_url": cfg["replication_base_url"],
        "header_boxes": header.get("boxes", []),
        "data_bbox": data.get("bbox"),
        "object_counts": data.get("count"),
        "objects_ordered": True,
        "multiple_versions": False,
        "crc32": data.get("crc32"),
    }
    catalog = {
        "source_id": cfg["source_id"],
        "family": OSM_FAMILY,
        "dataset": "OpenStreetMap via Geofabrik",
        "theme": "addresses",
        "type": "node",
        "source_release": f"geofabrik-replication-{sequence}@{snapshot_at}",
        "public_uri": cfg["uri"],
        "coverage_scope": cfg["coverage_scope"],
        "region": cfg["region"],
        "snapshot_at": snapshot_at,
        "license": OSM_LICENSE,
        "attribution": OSM_ATTRIBUTION,
        "copyright_url": OSM_COPYRIGHT_URL,
        "retained_input": _public_capture(capture),
        "pbf": metadata,
    }
    return metadata, catalog


def _sql_string(value: object) -> str:
    return "'" + str(value).replace("'", "''") + "'"


def _osm_candidate_query(path: pathlib.Path) -> str:
    """Return the offline node-only ST_ReadOSM query."""

    return f"""
SELECT cast(id AS BIGINT) AS object_id,
       trim(coalesce(tags['addr:street'], tags['addr:place'])) AS street,
       CASE WHEN nullif(trim(tags['addr:street']), '') IS NOT NULL
            THEN 'addr:street' ELSE 'addr:place' END AS street_tag,
       trim(tags['addr:housenumber']) AS house_number,
       coalesce(trim(tags['addr:postcode']), '') AS postcode,
       trim(coalesce(
           tags['addr:city'], tags['addr:town'], tags['addr:village'],
           tags['addr:municipality'], tags['addr:district'], tags['addr:suburb']
       )) AS municipality,
       CASE
           WHEN nullif(trim(tags['addr:city']), '') IS NOT NULL THEN 'addr:city'
           WHEN nullif(trim(tags['addr:town']), '') IS NOT NULL THEN 'addr:town'
           WHEN nullif(trim(tags['addr:village']), '') IS NOT NULL THEN 'addr:village'
           WHEN nullif(trim(tags['addr:municipality']), '') IS NOT NULL THEN 'addr:municipality'
           WHEN nullif(trim(tags['addr:district']), '') IS NOT NULL THEN 'addr:district'
           ELSE 'addr:suburb'
       END AS municipality_tag,
       coalesce(trim(tags['addr:country']), '') AS tagged_country,
       coalesce(trim(tags['amenity']), '') AS amenity,
       coalesce(trim(tags['shop']), '') AS shop,
       coalesce(trim(tags['office']), '') AS office,
       coalesce(trim(tags['tourism']), '') AS tourism,
       coalesce(trim(tags['craft']), '') AS craft,
       coalesce(trim(tags['leisure']), '') AS leisure,
       coalesce(trim(tags['healthcare']), '') AS healthcare,
       coalesce(trim(tags['brand']), trim(tags['network']), trim(tags['operator']), '') AS network,
       cast(lat AS DOUBLE) AS lat,
       cast(lon AS DOUBLE) AS lon
FROM st_readosm({_sql_string(path)})
WHERE kind = 'node'
  AND id IS NOT NULL
  AND lat IS NOT NULL AND lon IS NOT NULL
  AND nullif(trim(tags['addr:housenumber']), '') IS NOT NULL
  AND nullif(trim(coalesce(tags['addr:street'], tags['addr:place'])), '') IS NOT NULL
  AND nullif(trim(coalesce(
      tags['addr:city'], tags['addr:town'], tags['addr:village'],
      tags['addr:municipality'], tags['addr:district'], tags['addr:suburb']
  )), '') IS NOT NULL
  AND tags['disused'] IS NULL
  AND tags['abandoned'] IS NULL
  AND tags['demolished'] IS NULL
ORDER BY object_id
""".strip()


def _load_offline_spatial(connection) -> dict[str, str]:
    connection.execute("SET autoinstall_known_extensions=false")
    connection.execute("SET autoload_known_extensions=false")
    connection.execute("LOAD spatial")
    row = connection.execute(
        "SELECT extension_version, installed_from, install_mode "
        "FROM duckdb_extensions() WHERE extension_name = 'spatial' AND loaded"
    ).fetchone()
    if row is None or row[0] != PINNED_SPATIAL_EXTENSION_VERSION:
        got = None if row is None else row[0]
        raise CorpusError(
            "runtime must use the pinned local DuckDB spatial extension "
            f"{PINNED_SPATIAL_EXTENSION_VERSION}; got {got}"
        )
    return {
        "extension_version": str(row[0]),
        "installed_from": str(row[1]),
        "install_mode": str(row[2]),
        "autoinstall_known_extensions": "false",
        "autoload_known_extensions": "false",
    }


def _strict_text(value: object, label: str, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str):
        raise CorpusError(f"{label} must be text")
    cleaned = " ".join(value.split())
    if not allow_empty and not cleaned:
        raise CorpusError(f"{label} must be non-empty text")
    if any(ord(character) < 0x20 or ord(character) == 0x7F for character in cleaned):
        raise CorpusError(f"{label} contains control characters")
    return cleaned


def _selection_hash(row: Mapping[str, object], seed: str) -> str:
    material = "|".join(
        (
            seed,
            str(row["truth_source_id"]),
            str(row["country"]),
            str(row["record_id"]),
            _norm(row["query"]),
            f"{float(row['lat']):.7f}",
            f"{float(row['lon']):.7f}",
        )
    )
    return hashlib.sha256(material.encode("utf-8")).hexdigest()


def _category(raw: Mapping[str, object]) -> str:
    for namespace in (
        "amenity",
        "shop",
        "office",
        "tourism",
        "craft",
        "leisure",
        "healthcare",
    ):
        value = _strict_text(raw.get(namespace, ""), namespace, allow_empty=True)
        if value:
            return f"{namespace}:{value}"
    return ""


def _osm_row_from_raw(
    country: str,
    raw: Mapping[str, object],
    catalog: Mapping[str, object],
    assembled_at: str,
    seed: str,
) -> dict[str, object]:
    try:
        object_id = int(raw["object_id"])
        lat = float(raw["lat"])
        lon = float(raw["lon"])
    except (KeyError, TypeError, ValueError, OverflowError) as exc:
        raise CorpusError("invalid OSM object id or coordinate") from exc
    if object_id <= 0:
        raise CorpusError("OSM node id must be positive")
    if not overture._finite_in_bounds(country, lat, lon):
        raise CorpusError("OSM coordinate is non-finite or outside product extent")
    street = _strict_text(raw.get("street"), "OSM street")
    house_number = _strict_text(raw.get("house_number"), "OSM house number")
    postcode = _strict_text(raw.get("postcode", ""), "OSM postcode", allow_empty=True)
    municipality = _strict_text(raw.get("municipality"), "OSM municipality")
    tagged_country = _strict_text(
        raw.get("tagged_country", ""), "OSM addr:country", allow_empty=True
    )
    if tagged_country and tagged_country.upper() != country:
        raise CorpusError("OSM addr:country conflicts with the fixed source country")
    if not any(character.isalpha() for character in street):
        raise CorpusError("OSM street has no alphabetic text")
    if not any(character.isdigit() for character in house_number):
        raise CorpusError("OSM house number has no digit")
    if postcode and not any(character.isdigit() for character in postcode):
        raise CorpusError("OSM postcode has no digit")
    if not any(character.isalpha() for character in municipality):
        raise CorpusError("OSM municipality has no alphabetic text")
    network = _strict_text(raw.get("network", ""), "OSM network", allow_empty=True)
    street_tag = _strict_text(raw.get("street_tag"), "OSM street tag")
    municipality_tag = _strict_text(
        raw.get("municipality_tag"), "OSM municipality tag"
    )
    if street_tag not in {"addr:street", "addr:place"}:
        raise CorpusError("OSM street tag is not an approved address tag")
    if municipality_tag not in {
        "addr:city",
        "addr:town",
        "addr:village",
        "addr:municipality",
        "addr:district",
        "addr:suburb",
    }:
        raise CorpusError("OSM municipality tag is not an approved address tag")
    street_address = f"{street} {house_number}"
    locality = " ".join(part for part in (postcode, municipality) if part)
    query = ", ".join(
        (street_address, locality, str(overture.COUNTRIES[country]["name"]))
    )
    source_id = str(catalog["source_id"])
    object_url = f"https://www.openstreetmap.org/node/{object_id}"
    row: dict[str, object] = {
        "schema": TRUTH_SCHEMA,
        "country": country,
        "query": query,
        "street_address": street_address,
        "street_name": street,
        "house_number": house_number,
        "postcode": postcode,
        "municipality": municipality,
        "address": {
            "freeform": street_address,
            "postcode": postcode,
            "locality": municipality,
            "region": "",
            "country": country,
        },
        "lat": lat,
        "lon": lon,
        "network": network,
        "record_id": f"osm:node:{object_id}",
        "source_record_id": f"node/{object_id}",
        "truth_source_id": source_id,
        "truth_source_family": OSM_FAMILY,
        "source_url": catalog["public_uri"],
        "source_release": catalog["source_release"],
        "source_theme": "addresses",
        "source_type": "node",
        "source_snapshot_at": catalog["snapshot_at"],
        "source_license": OSM_LICENSE,
        "source_sha256": catalog["retained_input"]["sha256"],
        "source_artifact": catalog["retained_input"],
        "retrieved_at": assembled_at,
        "license": OSM_LICENSE,
        "licenses": [OSM_LICENSE],
        "coordinate_provenance": {
            "source_name": "OpenStreetMap via Geofabrik",
            "source_family": OSM_FAMILY,
            "source_url": object_url,
            "record_id": f"node/{object_id}",
            "object_type": "node",
            "object_id": object_id,
            "coordinate_method": "node_location",
            "source_snapshot_at": catalog["snapshot_at"],
            "snapshot_sha256": catalog["retained_input"]["sha256"],
            "snapshot_logical_name": catalog["retained_input"]["logical_name"],
            "replication_sequence": catalog["pbf"]["replication_sequence"],
            "replication_base_url": catalog["pbf"]["replication_base_url"],
            "address_tags": {
                "street": street_tag,
                "house_number": "addr:housenumber",
                "postcode": "addr:postcode" if postcode else None,
                "municipality": municipality_tag,
                "country": "addr:country" if tagged_country else None,
            },
            "retrieved_at": assembled_at,
            "license": OSM_LICENSE,
            "common_ancestor": None,
            "evidence_url": object_url,
            "attribution_url": OSM_COPYRIGHT_URL,
            "same_export_as_indexed_sheet": False,
        },
        "lineage_class": "unknown_lineage",
        "lineage_evidence": [
            "direct OSM node export; factual upstream overlap with the indexed "
            "national registry is not proved"
        ],
        "lineage_policy": LINEAGE_POLICY,
    }
    category = _category(raw)
    if category:
        row["category"] = category
    row["selection_sha256"] = _selection_hash(row, seed)
    return row


def _load_osm_candidates(
    country: str,
    capture: Mapping[str, object],
    catalog: Mapping[str, object],
    assembled_at: str,
    seed: str,
) -> tuple[list[dict[str, object]], dict[str, int], str]:
    try:
        import duckdb
    except ImportError as exc:  # pragma: no cover - runtime contract
        raise CorpusError("DuckDB is required for local ST_ReadOSM extraction") from exc
    overture._assert_runtime(duckdb_version=str(duckdb.__version__))
    query = _osm_candidate_query(pathlib.Path(str(capture["_path"])))
    connection = duckdb.connect()
    try:
        connection.execute("SET threads=2")
        connection.execute("SET memory_limit='2GB'")
        connection.execute("SET max_temp_directory_size='2GB'")
        # LOAD is offline.  INSTALL is deliberately forbidden here because it
        # may contact an extension repository.
        _load_offline_spatial(connection)
        cursor = connection.execute(query)
        columns = [description[0] for description in cursor.description]
        rows: list[dict[str, object]] = []
        rejected: collections.Counter[str] = collections.Counter()
        while True:
            batch = cursor.fetchmany(2_000)
            if not batch:
                break
            for values in batch:
                raw = dict(zip(columns, values))
                try:
                    rows.append(
                        _osm_row_from_raw(country, raw, catalog, assembled_at, seed)
                    )
                except CorpusError as exc:
                    rejected[str(exc)] += 1
    finally:
        connection.close()
    rows.sort(key=lambda row: (str(row["selection_sha256"]), str(row["record_id"])))
    canonical_query = _osm_candidate_query(
        pathlib.Path(str(OSM_SOURCES[country]["filename"]))
    )
    return (
        rows,
        dict(sorted(rejected.items())),
        hashlib.sha256(canonical_query.encode()).hexdigest(),
    )


def _derive_street_name(street_address: object) -> str:
    text = _strict_text(street_address, "Overture street address")
    without_number = re.sub(r"\b\d+[\w/-]*\b", " ", text, flags=re.UNICODE)
    return " ".join(without_number.split()) or text


def _convert_overture_row(
    row: Mapping[str, object], source_catalog: Mapping[str, object], seed: str
) -> dict[str, object]:
    converted = json.loads(json.dumps(dict(row), ensure_ascii=False, allow_nan=False))
    original_id = _strict_text(converted["record_id"], "Overture record id")
    converted.update(
        {
            "schema": TRUTH_SCHEMA,
            "record_id": f"overture:place:{original_id}",
            "source_record_id": original_id,
            "truth_source_id": OVERTURE_SOURCE_ID,
            "truth_source_family": OVERTURE_FAMILY,
            "source_type": "place",
            "source_snapshot_at": source_catalog["snapshot_at"],
            "source_artifact": source_catalog["retained_input"],
            "street_name": _derive_street_name(converted["street_address"]),
        }
    )
    provenance = dict(converted["coordinate_provenance"])
    provenance["source_family"] = OVERTURE_FAMILY
    provenance["snapshot_sha256"] = source_catalog["retained_input"]["sha256"]
    provenance["snapshot_logical_name"] = source_catalog["retained_input"][
        "logical_name"
    ]
    provenance["acquisition_manifest_sha256"] = source_catalog[
        "acquisition_manifest"
    ]["sha256"]
    provenance["source_snapshot_at"] = source_catalog["snapshot_at"]
    converted["coordinate_provenance"] = provenance
    converted["lineage_evaluation_policy"] = converted["lineage_policy"]
    converted["lineage_policy"] = LINEAGE_POLICY
    converted["source_license"] = converted["license"]
    converted["source_sha256"] = source_catalog["retained_input"]["sha256"]
    converted["selection_sha256"] = _selection_hash(converted, seed)
    return converted


def _load_overture_candidates(
    acquisition_path: pathlib.Path,
    seed: str,
    raw_capture: Mapping[str, object] | None = None,
    manifest_capture: Mapping[str, object] | None = None,
) -> tuple[
    list[dict[str, object]],
    dict[str, object],
    dict[str, object],
    str,
    dict[str, int],
]:
    if raw_capture is None or manifest_capture is None:
        raw_capture, manifest_capture = _capture_fixed_overture(acquisition_path)
    args = argparse.Namespace(
        from_acquisition=acquisition_path,
        countries=list(COUNTRIES),
        retrieved_at="1970-01-01T00:00:00Z",
        seed=overture.DEFAULT_SEED,
        per_country=PER_COUNTRY,
        candidate_multiplier=overture.DEFAULT_CANDIDATE_MULTIPLIER,
    )
    raw_rows, duckdb_version, acquisition_manifest, manifest_sha256 = (
        overture._read_raw_snapshot(args)
    )
    if manifest_sha256 != manifest_capture["sha256"]:
        raise CorpusError("Overture manifest hash disagrees with its pinned capture")
    _verify_capture(raw_capture, "Overture acquisition")
    _verify_capture(manifest_capture, "Overture acquisition manifest")
    source = acquisition_manifest["source_details"]
    catalog: dict[str, object] = {
        "source_id": OVERTURE_SOURCE_ID,
        "family": OVERTURE_FAMILY,
        "dataset": "Overture Maps Foundation",
        "theme": overture.OVERTURE_THEME,
        "type": overture.OVERTURE_TYPE,
        "source_release": overture.OVERTURE_RELEASE,
        "public_uri": overture.OVERTURE_S3,
        "coverage_scope": "country extents: FR, IT, NL, RS",
        "snapshot_at": source["retrieved_at"],
        "license": "MIXED: exact license is retained per SourceItem and row",
        "retained_input": {
            **_public_capture(raw_capture),
        },
        "acquisition_manifest": {
            **_public_capture(manifest_capture),
        },
        "runtime": {"duckdb_version": duckdb_version},
    }
    candidates: list[dict[str, object]] = []
    rejected: collections.Counter[str] = collections.Counter()
    for raw in raw_rows:
        try:
            candidate = overture.candidate_from_raw(
                raw, str(source["retrieved_at"]), overture.DEFAULT_SEED
            )
            candidates.append(_convert_overture_row(candidate, catalog, seed))
        except CorpusError as exc:
            rejected[str(exc)] += 1
    candidates.sort(
        key=lambda row: (
            str(row["country"]),
            str(row["selection_sha256"]),
            str(row["record_id"]),
        )
    )
    return (
        candidates,
        catalog,
        acquisition_manifest,
        manifest_sha256,
        dict(sorted(rejected.items())),
    )


def _address_key(row: Mapping[str, object]) -> str:
    return "".join(
        character.casefold()
        for character in str(row["query"])
        if character.isalnum()
    )


def _coordinate_key(row: Mapping[str, object]) -> tuple[float, float]:
    return round(float(row["lat"]), 5), round(float(row["lon"]), 5)


def _dedupe_key(row: Mapping[str, object]) -> tuple[str, str, tuple[float, float]]:
    return str(row["record_id"]), _address_key(row), _coordinate_key(row)


def _cross_source_dedupe(
    rows: Sequence[dict[str, object]],
) -> tuple[list[dict[str, object]], dict[str, object]]:
    query_sources: dict[tuple[str, str], set[str]] = collections.defaultdict(set)
    coordinate_sources: dict[tuple[str, tuple[float, float]], set[str]] = (
        collections.defaultdict(set)
    )
    for row in rows:
        country = str(row["country"])
        family = str(row["truth_source_family"])
        query_sources[(country, _address_key(row))].add(family)
        coordinate_sources[(country, _coordinate_key(row))].add(family)
    colliding_queries = {key for key, sources in query_sources.items() if len(sources) > 1}
    colliding_coordinates = {
        key for key, sources in coordinate_sources.items() if len(sources) > 1
    }
    kept: list[dict[str, object]] = []
    excluded = collections.Counter()
    by_country_source = collections.Counter()
    for row in rows:
        country = str(row["country"])
        source_id = str(row["truth_source_id"])
        reasons: list[str] = []
        if (country, _address_key(row)) in colliding_queries:
            reasons.append("normalized_query_cross_source_collision")
        if (country, _coordinate_key(row)) in colliding_coordinates:
            reasons.append("rounded_coordinate_cross_source_collision")
        if reasons:
            for reason in reasons:
                excluded[reason] += 1
            by_country_source[f"{country}:{source_id}"] += 1
        else:
            kept.append(row)
    return kept, {
        "policy": "exclude every row in a cross-source query or rounded-coordinate collision group",
        "query_collision_groups": len(colliding_queries),
        "coordinate_collision_groups": len(colliding_coordinates),
        "excluded_rows": len(rows) - len(kept),
        "excluded_reason_occurrences": dict(sorted(excluded.items())),
        "excluded_rows_by_country_and_source": dict(sorted(by_country_source.items())),
    }


def _cell_key(row: Mapping[str, object]) -> str:
    lat_cell = math.floor(float(row["lat"]) * 100)
    lon_cell = math.floor(float(row["lon"]) * 100)
    return f"{lat_cell}:{lon_cell}"


def _diversity_keys(row: Mapping[str, object]) -> dict[str, str]:
    municipality = _norm(row["municipality"])
    street = _norm(row.get("street_name", row["street_address"]))
    return {
        "municipality": municipality,
        "street": f"{municipality}|{street}" if street else "",
        "cell_0_01_degree": _cell_key(row),
        "category": _norm(row.get("category", "")),
        "network": _norm(row.get("network", "")),
    }


def _add_counts(
    row: Mapping[str, object], counts: Mapping[str, collections.Counter[str]]
) -> None:
    for dimension, key in _diversity_keys(row).items():
        if key:
            counts[dimension][key] += 1


def _constraint_order(
    rows: Iterable[dict[str, object]], caps: Mapping[str, int]
) -> list[dict[str, object]]:
    materialized = list(rows)
    frequencies = {dimension: collections.Counter() for dimension in caps}
    for row in materialized:
        for dimension, key in _diversity_keys(row).items():
            if key:
                frequencies[dimension][key] += 1

    def key(row: Mapping[str, object]) -> tuple[float, float, str, str]:
        pressure = [
            frequencies[dimension][value] / caps[dimension]
            for dimension, value in _diversity_keys(row).items()
            if value
        ]
        return (
            max(pressure, default=0.0),
            sum(pressure),
            str(row["selection_sha256"]),
            str(row["record_id"]),
        )

    return sorted(materialized, key=key)


def _select_country(
    rows: Sequence[dict[str, object]],
    requested: Mapping[str, int],
    minimum_unknown: int,
    caps: Mapping[str, int],
) -> tuple[list[dict[str, object]], list[str], dict[str, object]]:
    selected: list[dict[str, object]] = []
    counts = {dimension: collections.Counter() for dimension in caps}
    seen_ids: set[str] = set()
    seen_queries: set[str] = set()
    seen_coordinates: set[tuple[float, float]] = set()
    failures: list[str] = []
    family_diagnostics: dict[str, object] = {}
    for family in (OVERTURE_FAMILY, OSM_FAMILY):
        quota = requested[family]
        family_selected = 0
        ordered = _constraint_order(
            (row for row in rows if row["truth_source_family"] == family), caps
        )
        skipped: collections.Counter[str] = collections.Counter()
        examined = 0
        for row in ordered:
            if family_selected >= quota:
                break
            examined += 1
            record_id, query, coordinate = _dedupe_key(row)
            if record_id in seen_ids:
                skipped["duplicate_record_id"] += 1
                continue
            if query in seen_queries:
                skipped["duplicate_normalized_query"] += 1
                continue
            if coordinate in seen_coordinates:
                skipped["duplicate_rounded_coordinate"] += 1
                continue
            violated = [
                dimension
                for dimension, key in _diversity_keys(row).items()
                if key and counts[dimension][key] >= caps[dimension]
            ]
            if violated:
                for dimension in violated:
                    skipped[f"cap:{dimension}"] += 1
                continue
            selected.append(row)
            family_selected += 1
            seen_ids.add(record_id)
            seen_queries.add(query)
            seen_coordinates.add(coordinate)
            _add_counts(row, counts)
        if family_selected != quota:
            failures.append(f"{family}: selected {family_selected}; exactly {quota} required")
        family_diagnostics[family] = {
            "candidate_rows": len(ordered),
            "examined_rows": examined,
            "requested_rows": quota,
            "selected_rows": family_selected,
            "skipped_reason_occurrences": dict(sorted(skipped.items())),
        }
    unknown = sum(row["lineage_class"] == "unknown_lineage" for row in selected)
    if unknown < minimum_unknown:
        failures.append(
            f"unknown_lineage: selected {unknown}; at least {minimum_unknown} required"
        )
    if len(selected) != sum(requested.values()):
        failures.append(
            f"total: selected {len(selected)}; exactly {sum(requested.values())} required"
        )
    selected.sort(key=lambda row: (str(row["selection_sha256"]), str(row["record_id"])))
    return selected, failures, {"family_selection": family_diagnostics}


def _count_matrices(rows: Sequence[Mapping[str, object]]) -> dict[str, object]:
    by_country = {country: 0 for country in COUNTRIES}
    source_ids = (
        OVERTURE_SOURCE_ID,
        *(str(OSM_SOURCES[country]["source_id"]) for country in COUNTRIES),
    )
    by_source = {source_id: 0 for source_id in source_ids}
    by_country_source: dict[str, dict[str, int]] = {
        country: {source_id: 0 for source_id in source_ids} for country in COUNTRIES
    }
    by_country_source_lineage: dict[str, dict[str, dict[str, int]]] = {
        country: {
            source_id: {name: 0 for name in LINEAGE_CLASSES}
            for source_id in source_ids
        }
        for country in COUNTRIES
    }
    by_lineage = {lineage: 0 for lineage in LINEAGE_CLASSES}
    by_country_lineage = {
        country: {lineage: 0 for lineage in LINEAGE_CLASSES} for country in COUNTRIES
    }
    for row in rows:
        country = str(row["country"])
        source_id = str(row["truth_source_id"])
        lineage = str(row["lineage_class"])
        by_country[country] += 1
        if source_id not in by_source:
            raise CorpusError(f"unsupported selected source id: {source_id}")
        by_source[source_id] += 1
        by_country_source[country][source_id] += 1
        source_lineage = by_country_source_lineage[country][source_id]
        source_lineage[lineage] += 1
        by_lineage[lineage] += 1
        by_country_lineage[country][lineage] += 1
    return {
        "rows_by_country": by_country,
        "rows_by_lineage": by_lineage,
        "rows_by_source": dict(sorted(by_source.items())),
        "rows_by_country_and_source": {
            country: dict(sorted(values.items()))
            for country, values in by_country_source.items()
        },
        "rows_by_country_source_and_lineage": {
            country: {
                source: dict(sorted(lineages.items()))
                for source, lineages in sorted(values.items())
            }
            for country, values in by_country_source_lineage.items()
        },
        "rows_by_country_and_lineage": by_country_lineage,
    }


def _validate_selected_rows(
    rows: Sequence[Mapping[str, object]],
    source_catalog: Mapping[str, Mapping[str, object]],
) -> None:
    """Fail closed if the selected rows do not satisfy the schema-4 contract."""

    expected_source_ids = {
        OVERTURE_SOURCE_ID,
        *(str(OSM_SOURCES[country]["source_id"]) for country in COUNTRIES),
    }
    if set(source_catalog) != expected_source_ids:
        raise CorpusError("source catalog does not contain the exact fixed source set")
    if len(rows) != PER_COUNTRY * len(COUNTRIES):
        raise CorpusError("selected corpus does not contain exactly 300 rows per country")
    seen_ids: set[str] = set()
    seen_queries: set[str] = set()
    seen_coordinates: set[tuple[float, float]] = set()
    forbidden_engine_fields = {
        "distance_m",
        "engine_response",
        "geocoder_result",
        "matched_lat",
        "matched_lon",
        "result_status",
    }
    required_text = {
        "query",
        "street_address",
        "municipality",
        "record_id",
        "source_record_id",
        "truth_source_id",
        "truth_source_family",
        "source_url",
        "source_release",
        "source_theme",
        "source_type",
        "source_snapshot_at",
        "source_license",
        "source_sha256",
        "license",
        "retrieved_at",
        "lineage_class",
        "lineage_policy",
        "selection_sha256",
    }
    by_country_family: dict[str, collections.Counter[str]] = {
        country: collections.Counter() for country in COUNTRIES
    }
    by_country_unknown = {country: 0 for country in COUNTRIES}
    cap_counts = {
        country: {dimension: collections.Counter() for dimension in CAPS}
        for country in COUNTRIES
    }
    for number, row in enumerate(rows, 1):
        if type(row.get("schema")) is not int or row["schema"] != TRUTH_SCHEMA:
            raise CorpusError(f"selected row {number} is not explicit schema {TRUTH_SCHEMA}")
        country = row.get("country")
        if country not in COUNTRIES:
            raise CorpusError(f"selected row {number} has unsupported country")
        assert isinstance(country, str)
        missing = sorted(required_text - row.keys())
        if missing:
            raise CorpusError(
                f"selected row {number} is missing schema-4 fields: {', '.join(missing)}"
            )
        for field in required_text:
            _strict_text(row[field], f"selected row {number} {field}")
        unexpected = sorted(forbidden_engine_fields & row.keys())
        if unexpected:
            raise CorpusError(
                f"selected row {number} contains engine-result fields: {', '.join(unexpected)}"
            )
        try:
            lat = float(row["lat"])
            lon = float(row["lon"])
        except (KeyError, TypeError, ValueError, OverflowError) as exc:
            raise CorpusError(f"selected row {number} has invalid coordinates") from exc
        if not overture._finite_in_bounds(country, lat, lon):
            raise CorpusError(f"selected row {number} is outside the product extent")
        source_id = str(row["truth_source_id"])
        if source_id not in source_catalog:
            raise CorpusError(f"selected row {number} names an unsupported source")
        catalog = source_catalog[source_id]
        retained_input = catalog.get("retained_input")
        if not isinstance(retained_input, Mapping):
            raise CorpusError(f"source catalog {source_id} has no retained input")
        expected = {
            "truth_source_family": catalog.get("family"),
            "source_release": catalog.get("source_release"),
            "source_theme": catalog.get("theme"),
            "source_type": catalog.get("type"),
            "source_snapshot_at": catalog.get("snapshot_at"),
            "source_sha256": retained_input.get("sha256"),
        }
        mismatches = [
            field for field, value in expected.items() if row.get(field) != value
        ]
        if mismatches:
            raise CorpusError(
                f"selected row {number} disagrees with source catalog: {', '.join(mismatches)}"
            )
        if row["source_license"] != row["license"]:
            raise CorpusError(f"selected row {number} source license disagrees with row")
        source_artifact = row.get("source_artifact")
        provenance = row.get("coordinate_provenance")
        if not isinstance(source_artifact, Mapping) or not isinstance(provenance, Mapping):
            raise CorpusError(f"selected row {number} has incomplete source provenance")
        if (
            source_artifact.get("sha256") != row["source_sha256"]
            or provenance.get("snapshot_sha256") != row["source_sha256"]
        ):
            raise CorpusError(f"selected row {number} has inconsistent artifact hashes")
        if re.fullmatch(r"[0-9a-f]{64}", str(row["source_sha256"])) is None:
            raise CorpusError(f"selected row {number} has invalid source SHA-256")
        if re.fullmatch(r"[0-9a-f]{64}", str(row["selection_sha256"])) is None:
            raise CorpusError(f"selected row {number} has invalid selection SHA-256")
        if row["lineage_class"] not in LINEAGE_CLASSES:
            raise CorpusError(f"selected row {number} has invalid lineage class")
        if row["lineage_policy"] != LINEAGE_POLICY:
            raise CorpusError(f"selected row {number} has invalid lineage policy")
        family = str(row["truth_source_family"])
        if family == OSM_FAMILY:
            if (
                source_id != OSM_SOURCES[country]["source_id"]
                or row["lineage_class"] != "unknown_lineage"
                or row["license"] != OSM_LICENSE
            ):
                raise CorpusError(f"selected OSM row {number} violates country/lineage/license")
        elif family != OVERTURE_FAMILY or source_id != OVERTURE_SOURCE_ID:
            raise CorpusError(f"selected row {number} has invalid source family coupling")
        record_id, query, coordinate = _dedupe_key(row)
        if record_id in seen_ids or query in seen_queries or coordinate in seen_coordinates:
            raise CorpusError(f"selected row {number} violates final deduplication")
        seen_ids.add(record_id)
        seen_queries.add(query)
        seen_coordinates.add(coordinate)
        by_country_family[country][family] += 1
        by_country_unknown[country] += row["lineage_class"] == "unknown_lineage"
        for dimension, key in _diversity_keys(row).items():
            if key:
                cap_counts[country][dimension][key] += 1
    for country in COUNTRIES:
        expected_family = {
            OVERTURE_FAMILY: OVERTURE_QUOTA,
            OSM_FAMILY: OSM_QUOTA,
        }
        if dict(by_country_family[country]) != expected_family:
            raise CorpusError(f"{country} selected source quotas do not match the fixed contract")
        if by_country_unknown[country] < MIN_UNKNOWN_PER_COUNTRY:
            raise CorpusError(f"{country} selected unknown-lineage quota is too small")
        for dimension, cap in CAPS.items():
            maximum = max(cap_counts[country][dimension].values(), default=0)
            if maximum > cap:
                raise CorpusError(f"{country} selected {dimension} cap is exceeded")


def _selection_diagnostics(
    candidates: Sequence[dict[str, object]],
    requested: Mapping[str, int],
    minimum_unknown: int,
    caps: Mapping[str, int],
) -> tuple[list[dict[str, object]], dict[str, object], list[str]]:
    selected: list[dict[str, object]] = []
    failures: list[str] = []
    country_details: dict[str, object] = {}
    for country in COUNTRIES:
        pool = [row for row in candidates if row["country"] == country]
        country_selected, country_failures, trace = _select_country(
            pool, requested, minimum_unknown, caps
        )
        selected.extend(country_selected)
        failures.extend(f"{country}: {failure}" for failure in country_failures)
        pool_by_family = collections.Counter(
            str(row["truth_source_family"]) for row in pool
        )
        selected_by_family = collections.Counter(
            str(row["truth_source_family"]) for row in country_selected
        )
        selected_lineage = collections.Counter(
            str(row["lineage_class"]) for row in country_selected
        )
        maximum_group_use = {dimension: 0 for dimension in caps}
        group_counts = {dimension: collections.Counter() for dimension in caps}
        for row in country_selected:
            for dimension, key in _diversity_keys(row).items():
                if key:
                    group_counts[dimension][key] += 1
        for dimension, counter in group_counts.items():
            maximum_group_use[dimension] = max(counter.values(), default=0)
        country_details[country] = {
            "status": "selected" if not country_failures else "failed_closed",
            "candidate_rows_by_family": dict(sorted(pool_by_family.items())),
            "requested_rows_by_family": dict(requested),
            "selected_rows_by_family": dict(sorted(selected_by_family.items())),
            "selected_lineage": dict(sorted(selected_lineage.items())),
            "maximum_group_use": maximum_group_use,
            **trace,
            "failures": country_failures,
        }
    selected.sort(key=lambda row: (str(row["country"]), str(row["selection_sha256"])))
    return selected, {"countries": country_details}, failures


def _logical_command(argv: Sequence[str]) -> str:
    logical = ["python3", "examples/hybrid_truth_corpus.py"]
    for value in argv:
        if "=" in value and value.split("=", 1)[0].upper() in COUNTRIES:
            country, raw_path = value.split("=", 1)
            logical.append(f"{country.upper()}={pathlib.Path(raw_path).name}")
        elif "/" in value or "\\" in value:
            logical.append(pathlib.Path(value).name)
        else:
            logical.append(value)
    return shlex.join(logical)


def _parse_osm_paths(values: Sequence[str], parser: argparse.ArgumentParser) -> dict[str, pathlib.Path]:
    result: dict[str, pathlib.Path] = {}
    for value in values:
        if "=" not in value:
            parser.error(f"invalid --osm {value!r}; expected CC=PATH")
        country, raw_path = value.split("=", 1)
        country = country.upper()
        if country not in COUNTRIES or country in result:
            parser.error(f"invalid or duplicate --osm country {country!r}")
        result[country] = pathlib.Path(raw_path).resolve()
    missing = sorted(set(COUNTRIES) - result.keys())
    if missing:
        parser.error("missing fixed PBFs for: " + ", ".join(missing))
    return result


def _parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--overture-acquisition", required=True, type=pathlib.Path)
    parser.add_argument("--osm", required=True, action="append", default=[])
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--assembled-at", required=True)
    parser.set_defaults(seed=SELECTION_SEED)
    args = parser.parse_args(raw_argv)
    try:
        args.assembled_at = overture._canonical_utc(args.assembled_at)
    except CorpusError as exc:
        parser.error(str(exc))
    args.overture_acquisition = args.overture_acquisition.resolve()
    args.output = args.output.resolve()
    args.osm = _parse_osm_paths(args.osm, parser)
    args.logical_command = _logical_command(raw_argv)
    return args


def _preflight(args: argparse.Namespace) -> tuple[pathlib.Path, pathlib.Path]:
    manifest_path = _artifact_manifest_path(args.output)
    diagnostics_path = _diagnostics_path(args.output)
    paths = [
        args.output,
        manifest_path,
        diagnostics_path,
        args.overture_acquisition,
        overture._raw_manifest_path(args.overture_acquisition),
        *args.osm.values(),
    ]
    normalized = [os.path.normcase(os.path.abspath(path)) for path in paths]
    if len(normalized) != len(set(normalized)):
        raise CorpusError("output, sidecars, Overture inputs, and PBF inputs must be distinct")
    overture._preflight_absent(
        (
            (args.output, "hybrid corpus"),
            (manifest_path, "hybrid corpus manifest"),
            (diagnostics_path, "hybrid selection diagnostics"),
        )
    )
    return manifest_path, diagnostics_path


def _input_verifier(
    script_capture: Mapping[str, object],
    helper_capture: Mapping[str, object],
    pbf_captures: Mapping[str, Mapping[str, object]],
    overture_captures: Sequence[Mapping[str, object]],
    acquisition_path: pathlib.Path,
    acquisition_manifest: Mapping[str, object],
    acquisition_manifest_sha256: str,
) -> None:
    _verify_capture(script_capture, "hybrid builder script")
    _verify_capture(helper_capture, "Overture helper script")
    for country, capture in pbf_captures.items():
        _verify_capture(capture, f"{country} PBF")
    _verify_capture(overture_captures[0], "Overture acquisition")
    _verify_capture(overture_captures[1], "Overture acquisition manifest")
    overture._verify_acquisition_pair(
        acquisition_path,
        overture._raw_manifest_path(acquisition_path),
        acquisition_manifest,
        acquisition_manifest_sha256,
    )


def _runtime_identity() -> dict[str, object]:
    try:
        import duckdb
    except ImportError as exc:  # pragma: no cover
        raise CorpusError("DuckDB is required") from exc
    osmium_runtime = _osmium_runtime()
    connection = duckdb.connect()
    try:
        spatial_runtime = _load_offline_spatial(connection)
    finally:
        connection.close()
    return {
        "python_implementation": platform.python_implementation(),
        "python_version": platform.python_version(),
        "unicode_version": unicodedata.unidata_version,
        "duckdb_version": str(duckdb.__version__),
        "osmium_version": osmium_runtime["version"],
        "libosmium_version": osmium_runtime["libosmium_version"],
        "osmium_binary": _public_capture(osmium_runtime["binary"]),
        "spatial_extension": spatial_runtime,
    }


def _manifest(
    args: argparse.Namespace,
    rows: Sequence[Mapping[str, object]],
    source_catalog: Mapping[str, Mapping[str, object]],
    diagnostics_path: pathlib.Path,
    diagnostics_sha256: str,
    script_capture: Mapping[str, object],
    helper_capture: Mapping[str, object],
    osm_query_hashes: Mapping[str, str],
) -> dict[str, object]:
    matrices = _count_matrices(rows)
    requested = {
        country: {OVERTURE_FAMILY: OVERTURE_QUOTA, OSM_FAMILY: OSM_QUOTA}
        for country in COUNTRIES
    }
    realized = {
        country: {
            family: sum(
                row["country"] == country and row["truth_source_family"] == family
                for row in rows
            )
            for family in (OVERTURE_FAMILY, OSM_FAMILY)
        }
        for country in COUNTRIES
    }
    return {
        "schema": TRUTH_SCHEMA,
        "kind": MANIFEST_KIND,
        "corpus": args.output.name,
        "sha256": overture._sha256_file(args.output),
        "rows": len(rows),
        "assembled_at": args.assembled_at,
        "lineage_policy": LINEAGE_POLICY,
        "licenses": sorted({str(row["license"]) for row in rows}),
        **matrices,
        "source_catalog": dict(sorted(source_catalog.items())),
        "source_mix": {
            "policy": "fixed country quotas selected before any engine result",
            "requested_rows_by_country_and_family": requested,
            "realized_rows_by_country_and_family": realized,
        },
        "selection": {
            "seed": args.seed,
            "formula": SELECTION_FORMULA,
            "per_country": PER_COUNTRY,
            "minimum_unknown_lineage_per_country": MIN_UNKNOWN_PER_COUNTRY,
            "fixed_family_quotas": {
                OVERTURE_FAMILY: OVERTURE_QUOTA,
                OSM_FAMILY: OSM_QUOTA,
            },
            "diversity_caps": dict(CAPS),
            "street_key": "normalized municipality|normalized street name",
            "cell_key": "floor(latitude*100):floor(longitude*100)",
            "optional_caps": "category and network caps apply only to non-empty values",
            "deduplication": (
                "cross-source collision groups excluded first; then record id, "
                "normalized full query, and coordinate rounded to 5 decimals"
            ),
            "engine_blind": True,
        },
        "diagnostics": {
            "logical_name": diagnostics_path.name,
            "sha256": diagnostics_sha256,
        },
        "recipe": {
            "script": "examples/hybrid_truth_corpus.py",
            "script_sha256": script_capture["sha256"],
            "helper_script": "examples/overture_places_corpus.py",
            "helper_script_sha256": helper_capture["sha256"],
            "command": args.logical_command,
            "osm_query_sha256_by_country": dict(sorted(osm_query_hashes.items())),
        },
        "runtime": _runtime_identity(),
    }


def _write_failure_diagnostics(
    path: pathlib.Path, diagnostics: dict[str, object], error: Exception
) -> None:
    diagnostics["status"] = "failed_closed"
    diagnostics.setdefault("failures", []).append(str(error))
    overture._write_json(path, diagnostics, "hybrid selection diagnostics")


def _run(args: argparse.Namespace) -> dict[str, object]:
    overture._assert_runtime()
    manifest_path, diagnostics_path = _preflight(args)
    script_capture = _capture_streaming(pathlib.Path(__file__), "hybrid builder script")
    helper_capture = _capture_streaming(OVERTURE_SCRIPT, "Overture helper script")
    diagnostics: dict[str, object] = {
        "schema": DIAGNOSTICS_SCHEMA,
        "kind": "hybrid_truth_selection_diagnostics",
        "assembled_at": args.assembled_at,
        "status": "running",
        "failures": [],
        "network_access": "forbidden; all inputs are retained local artifacts",
    }
    pbf_captures: dict[str, dict[str, object]] = {}
    overture_captures: tuple[dict[str, object], dict[str, object]] | None = None
    acquisition_manifest: dict[str, object] | None = None
    acquisition_manifest_sha256: str | None = None
    with overture._output_claim(args.output):
        try:
            overture_captures = _capture_fixed_overture(args.overture_acquisition)
            pbf_captures = {
                country: _capture_streaming(args.osm[country], f"{country} PBF")
                for country in COUNTRIES
            }
            pbf_catalog: dict[str, dict[str, object]] = {}
            pbf_metadata: dict[str, dict[str, object]] = {}
            for country in COUNTRIES:
                metadata, catalog = _pbf_metadata(country, pbf_captures[country])
                pbf_metadata[country] = metadata
                pbf_catalog[str(catalog["source_id"])] = catalog
            (
                overture_rows,
                overture_catalog,
                acquisition_manifest,
                acquisition_manifest_sha256,
                overture_rejected,
            ) = _load_overture_candidates(
                args.overture_acquisition,
                args.seed,
                *overture_captures,
            )
            osm_rows: list[dict[str, object]] = []
            osm_rejected: dict[str, dict[str, int]] = {}
            osm_query_hashes: dict[str, str] = {}
            for country in COUNTRIES:
                source_id = OSM_SOURCES[country]["source_id"]
                rows, rejected, query_sha256 = _load_osm_candidates(
                    country,
                    pbf_captures[country],
                    pbf_catalog[source_id],
                    args.assembled_at,
                    args.seed,
                )
                osm_rows.extend(rows)
                osm_rejected[country] = rejected
                osm_query_hashes[country] = query_sha256
            all_candidates = [*overture_rows, *osm_rows]
            deduped, collision_diagnostics = _cross_source_dedupe(all_candidates)
            selected, selection_details, failures = _selection_diagnostics(
                deduped,
                {OVERTURE_FAMILY: OVERTURE_QUOTA, OSM_FAMILY: OSM_QUOTA},
                MIN_UNKNOWN_PER_COUNTRY,
                CAPS,
            )
            source_catalog = {OVERTURE_SOURCE_ID: overture_catalog, **pbf_catalog}
            if not failures:
                try:
                    _validate_selected_rows(selected, source_catalog)
                except CorpusError as exc:
                    failures.append(str(exc))
            diagnostics.update(
                {
                    "input_artifacts": {
                        "overture_acquisition": {
                            **_public_capture(overture_captures[0]),
                            "manifest": _public_capture(overture_captures[1]),
                        },
                        "pbf_by_country": {
                            country: _public_capture(capture)
                            for country, capture in pbf_captures.items()
                        },
                    },
                    "pbf_metadata_by_country": pbf_metadata,
                    "candidates": {
                        "overture_valid": len(overture_rows),
                        "overture_rejected_by_reason": overture_rejected,
                        "osm_valid_by_country": {
                            country: sum(row["country"] == country for row in osm_rows)
                            for country in COUNTRIES
                        },
                        "osm_rejected_by_country_and_reason": osm_rejected,
                    },
                    "cross_source_deduplication": collision_diagnostics,
                    "selection": {
                        "requested_per_country": PER_COUNTRY,
                        "minimum_unknown_per_country": MIN_UNKNOWN_PER_COUNTRY,
                        "fixed_family_quotas": {
                            OVERTURE_FAMILY: OVERTURE_QUOTA,
                            OSM_FAMILY: OSM_QUOTA,
                        },
                        "caps": dict(CAPS),
                        "engine_blind": True,
                    },
                    **selection_details,
                    "selected_counts": _count_matrices(selected),
                    "failures": failures,
                    "status": "selected" if not failures else "failed_closed",
                }
            )
            assert acquisition_manifest_sha256 is not None
            _input_verifier(
                script_capture,
                helper_capture,
                pbf_captures,
                overture_captures,
                args.overture_acquisition,
                acquisition_manifest,
                acquisition_manifest_sha256,
            )
            overture._write_json(
                diagnostics_path, diagnostics, "hybrid selection diagnostics"
            )
            diagnostics_sha256 = overture._sha256_file(diagnostics_path)
            if failures:
                raise CorpusError("; ".join(failures))
            overture._write_jsonl(args.output, selected)
            manifest = _manifest(
                args,
                selected,
                source_catalog,
                diagnostics_path,
                diagnostics_sha256,
                script_capture,
                helper_capture,
                osm_query_hashes,
            )
            _input_verifier(
                script_capture,
                helper_capture,
                pbf_captures,
                overture_captures,
                args.overture_acquisition,
                acquisition_manifest,
                acquisition_manifest_sha256,
            )
            overture._write_json(manifest_path, manifest, "hybrid corpus manifest")
            return manifest
        except Exception as exc:
            if not overture._path_exists_nofollow(diagnostics_path):
                _write_failure_diagnostics(diagnostics_path, diagnostics, exc)
            raise


def main(argv: Sequence[str] | None = None) -> None:
    args = _parse_args(argv)
    manifest = _run(args)
    print(
        json.dumps(
            {
                "corpus": args.output.name,
                "manifest": _artifact_manifest_path(args.output).name,
                "sha256": manifest["sha256"],
                "rows": manifest["rows"],
            },
            indent=2,
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    try:
        main()
    except CorpusError as error:
        raise SystemExit(f"STOP: {error}") from error
