#!/usr/bin/env python3
"""Build the fixed low-memory, multi-region schema-5 truth corpus.

``extract-region`` is intentionally a one-PBF/one-region operation.  It scans
the complete region stream in a fresh process, compares every valid OSM row
with the small retained Overture candidate maps, and retains only a bounded
deterministic heap.  ``assemble`` consumes the twelve fragments one at a time.
Neither command contacts the network and no geocoder answer influences
selection.
"""

from __future__ import annotations

import argparse
import collections
import contextlib
import dataclasses
import fcntl
import hashlib
import heapq
import importlib.util
import json
import math
import os
import pathlib
import re
import shlex
import stat
import subprocess
import sys
import tempfile
from typing import Iterable, Mapping, Sequence


EXAMPLES = pathlib.Path(__file__).resolve().parent


def _load_helper(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, EXAMPLES / filename)
    if spec is None or spec.loader is None:  # pragma: no cover - import invariant
        raise RuntimeError(f"cannot load {filename}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


hybrid = _load_helper("multiregion_hybrid_helpers", "hybrid_truth_corpus.py")
overture = hybrid.overture
CorpusError = hybrid.CorpusError

TRUTH_SCHEMA = 5
FRAGMENT_SCHEMA = 1
MANIFEST_KIND = "multiregion_overture_osm_truth_corpus"
FRAGMENT_KIND = "multiregion_osm_candidate_fragment"
COUNTRIES = ("FR", "IT", "NL", "RS")
PER_COUNTRY = 300
OVERTURE_QUOTA = 50
ROLE_QUOTAS = {"major_metro": 84, "mid_city": 83, "rural": 83}
OUTSIDE_MINIMA = {"FR": 25, "IT": 20, "NL": 10, "RS": 25}
TOTAL_OUTSIDE_MINIMUM = 80
EXPECTED_OVERTURE_CANDIDATES = 5_115
CANDIDATE_MULTIPLIER = 64
MAX_FETCH_ROWS = 512
DUCKDB_MEMORY_LIMIT = "768MB"
DUCKDB_MAX_TEMP = "1GB"
SELECTION_SEED = "gridpin-bl14-multiregion-truth-v1"
SELECTION_FORMULA = (
    "SHA-256(seed|sampling_region_id|truth_source_id|country|record_id|"
    "NFKC-casefold-alnum-space(query)|latitude:.7f|longitude:.7f)"
)
ROLE_RULES = {
    "major_metro": "fixed metro-area window with population >= 1,000,000",
    "mid_city": "fixed city window with population from 50,000 through 500,000",
    "rural": "fixed rural/small-town window anchored below 20,000 residents",
}
DIRECT_REGION_CAPS = {
    dimension: cap for dimension, cap in hybrid.CAPS.items()
    if dimension != "municipality"
}
# Countrywide Overture keeps the schema-v4 cap of three rows per ~1 km cell.
# A fixed direct stratum is already geographically bounded, and sparse OSM
# node coverage in Novi Sad cannot fill its 83-row quota under that same cap.
# Five still forces at least seventeen occupied cells for an 83-row region.
DIRECT_REGION_CAPS["cell_0_01_degree"] = 5
DIRECT_REGION_POLICY = (
    "municipality cap is disabled inside a fixed city/rural stratum; "
    "street, category, and network caps remain; the fixed-region "
    "0.01-degree cell cap is five instead of the countrywide cap of three"
)
COUNTRYWIDE_REGION_IDS = {country: f"{country.lower()}-countrywide" for country in COUNTRIES}
GLOBAL_EXTRACTION_LOCK_NAME = "gridpin-schema-v5-extract.lock"


def _source(
    source_id: str,
    country: str,
    filename: str,
    uri_path: str,
    sha256: str,
    byte_count: int,
    sequence: int,
    update_path: str,
) -> dict[str, object]:
    snapshot = "2026-07-01T20:22:00Z"
    return {
        "source_id": source_id,
        "country": country,
        "family": hybrid.OSM_FAMILY,
        "dataset": "OpenStreetMap via Geofabrik",
        "theme": "addresses",
        "type": "node",
        "source_release": f"geofabrik-replication-{sequence}@{snapshot}",
        "snapshot_at": snapshot,
        "license": hybrid.OSM_LICENSE,
        "attribution": hybrid.OSM_ATTRIBUTION,
        "copyright_url": hybrid.OSM_COPYRIGHT_URL,
        "public_uri": f"https://download.geofabrik.de/europe/{uri_path}",
        "retained_input": {
            "logical_name": filename,
            "sha256": sha256,
            "bytes": byte_count,
        },
        "pbf": {
            "replication_sequence": sequence,
            "replication_base_url": f"https://download.geofabrik.de/europe/{update_path}",
            "snapshot_at": snapshot,
            "format": "PBF",
            "objects_ordered": True,
            "multiple_versions": False,
        },
    }


OSM_SOURCE_CATALOG: dict[str, dict[str, object]] = {
    "fr-idf": _source(
        "osm_geofabrik_fr_ile_de_france_260701", "FR",
        "ile-de-france-260701.osm.pbf", "france/ile-de-france-260701.osm.pbf",
        "8cc2d3af326222a013eab1141ca4c388944893c918011a1930d7aa053045de1e",
        334_789_593, 4_833, "france/ile-de-france-updates",
    ),
    "fr-alsace": _source(
        "osm_geofabrik_fr_alsace_260701", "FR", "alsace-260701.osm.pbf",
        "france/alsace-260701.osm.pbf",
        "f8a63f9a31864821a16fa1fd1fd2626a587c4ea2d780a2d863bfa361d19bfaa7",
        129_643_154, 4_830, "france/alsace-updates",
    ),
    "it-centro": _source(
        "osm_geofabrik_it_centro_260701", "IT", "centro-260701.osm.pbf",
        "italy/centro-260701.osm.pbf",
        "2c84214c99b21a2d89cf0b6479a248d3bab263751041c7f1c51cd2cffc55ffae",
        379_876_352, 3_893, "italy/centro-updates",
    ),
    "it-isole": _source(
        "osm_geofabrik_it_isole_260701", "IT", "isole-260701.osm.pbf",
        "italy/isole-260701.osm.pbf",
        "b820ee216ef76b326bf1306ea946abfbfe58bdf08db9147289cf556d22665e88",
        212_733_976, 3_893, "italy/isole-updates",
    ),
    "nl-noord-holland": _source(
        "osm_geofabrik_nl_noord_holland_260701", "NL",
        "noord-holland-260701.osm.pbf", "netherlands/noord-holland-260701.osm.pbf",
        "6a757482b385e576d32abe4d4b77f8dfcb69f59d1f1fe0b220da373a9467baf3",
        187_808_082, 2_774, "netherlands/noord-holland-updates",
    ),
    "nl-drenthe": _source(
        "osm_geofabrik_nl_drenthe_260701", "NL", "drenthe-260701.osm.pbf",
        "netherlands/drenthe-260701.osm.pbf",
        "2814290012b08420820e2ef47373156707128de68a2b15783302e8a6feafa326",
        62_699_767, 2_774, "netherlands/drenthe-updates",
    ),
    "rs-serbia": _source(
        "osm_geofabrik_rs_serbia_260701", "RS", "serbia-260701.osm.pbf",
        "serbia-260701.osm.pbf",
        "0d5e526a7411e6a0dd7400bf188392d79de17477bd0612dee216a5a255fb83d0",
        236_966_213, 4_835, "serbia-updates",
    ),
}

OVERTURE_SOURCE_PIN = {
    "source_id": hybrid.OVERTURE_SOURCE_ID,
    "family": hybrid.OVERTURE_FAMILY,
    "dataset": "Overture Maps Foundation",
    "theme": overture.OVERTURE_THEME,
    "type": overture.OVERTURE_TYPE,
    "source_release": overture.OVERTURE_RELEASE,
    "public_uri": overture.OVERTURE_S3,
    "coverage_scope": "country extents: FR, IT, NL, RS",
    "snapshot_at": "2026-08-01T20:49:04Z",
    "retained_input": {
        "logical_name": hybrid.OVERTURE_ACQUISITION_NAME,
        "sha256": hybrid.OVERTURE_ACQUISITION_SHA256,
        "bytes": hybrid.OVERTURE_ACQUISITION_BYTES,
    },
    "acquisition_manifest": {
        "logical_name": f"{hybrid.OVERTURE_ACQUISITION_NAME}.manifest.json",
        "sha256": hybrid.OVERTURE_ACQUISITION_MANIFEST_SHA256,
        "bytes": hybrid.OVERTURE_ACQUISITION_MANIFEST_BYTES,
    },
}


def _region(
    region_id: str,
    country: str,
    role: str,
    source_key: str,
    bbox: tuple[float, float, float, float],
    anchor: str,
) -> dict[str, object]:
    return {
        "region_id": region_id,
        "country": country,
        "role": role,
        "quota": ROLE_QUOTAS[role],
        "selection_order": ("major_metro", "mid_city", "rural").index(role),
        "anchor": anchor,
        "population_rule": ROLE_RULES[role],
        "selection_bbox": list(bbox),
        "bbox_order": "min_lon,min_lat,max_lon,max_lat; inclusive",
        "source_key": source_key,
        "source_id": OSM_SOURCE_CATALOG[source_key]["source_id"],
        "coverage_scope": f"fixed {role} window around {anchor}",
    }


REGION_CATALOG: dict[str, dict[str, object]] = {
    item["region_id"]: item
    for item in (
        _region("fr-paris-metro", "FR", "major_metro", "fr-idf", (2.15, 48.75, 2.55, 49.00), "Paris metro area"),
        _region("fr-strasbourg-mid", "FR", "mid_city", "fr-alsace", (7.60, 48.45, 7.90, 48.70), "Strasbourg"),
        _region("fr-altkirch-rural", "FR", "rural", "fr-alsace", (7.00, 47.40, 7.50, 47.75), "Altkirch rural area"),
        _region("it-rome-metro", "IT", "major_metro", "it-centro", (12.25, 41.70, 12.75, 42.10), "Rome metro area"),
        _region("it-cagliari-mid", "IT", "mid_city", "it-isole", (8.95, 39.15, 9.25, 39.35), "Cagliari"),
        _region("it-ghilarza-rural", "IT", "rural", "it-isole", (8.45, 39.85, 9.05, 40.35), "Ghilarza rural area"),
        _region("nl-amsterdam-metro", "NL", "major_metro", "nl-noord-holland", (4.70, 52.25, 5.05, 52.50), "Amsterdam metro area"),
        _region("nl-assen-mid", "NL", "mid_city", "nl-drenthe", (6.45, 52.93, 6.65, 53.08), "Assen"),
        _region("nl-westerbork-rural", "NL", "rural", "nl-drenthe", (6.45, 52.65, 7.05, 52.90), "Westerbork rural area"),
        _region("rs-belgrade-metro", "RS", "major_metro", "rs-serbia", (20.25, 44.65, 20.65, 44.95), "Belgrade metro area"),
        _region("rs-novi-sad-mid", "RS", "mid_city", "rs-serbia", (19.65, 45.15, 20.05, 45.40), "Novi Sad"),
        _region("rs-backa-topola-rural", "RS", "rural", "rs-serbia", (19.30, 45.55, 20.15, 46.05), "Backa Topola rural/small-town area"),
    )
}

# Primary-statistics evidence is part of the sampling contract rather than a
# prose rationale.  "rural" means a rural/small-town sampling window anchored
# by a place below 20k; it is not a claim about a jurisdiction's legal status.
POPULATION_EVIDENCE: dict[str, dict[str, object]] = {
    "fr-paris-metro": {
        "territory_code": "COM-75056", "territory_unit": "municipality",
        "population": 2_113_705, "population_year": 2022,
        "evidence_url": "https://www.insee.fr/en/statistiques/8588289?geo=COM-75056",
        "publisher": "INSEE",
    },
    "fr-strasbourg-mid": {
        "territory_code": "COM-67482", "territory_unit": "municipality",
        "population": 291_709, "population_year": 2022,
        "evidence_url": "https://www.insee.fr/fr/statistiques/8309996",
        "publisher": "INSEE",
    },
    "fr-altkirch-rural": {
        "territory_code": "COM-68004", "territory_unit": "municipality anchor",
        "population": 5_659, "population_year": 2019,
        "evidence_url": "https://www.insee.fr/fr/statistiques/6455183?geo=COM-68004",
        "publisher": "INSEE",
    },
    "it-rome-metro": {
        "territory_code": "ISTAT-058091", "territory_unit": "municipality",
        "population": 2_747_290, "population_year": 2024,
        "evidence_url": "https://www.istat.it/comunicato-stampa/censimento-e-dinamica-della-popolazione-anno-2024/",
        "publisher": "ISTAT",
    },
    "it-cagliari-mid": {
        "territory_code": "ISTAT-092009", "territory_unit": "municipality",
        "population": 147_411, "population_year": 2023,
        "evidence_url": "https://www.istat.it/wp-content/uploads/2025/04/Censimento-permanente-popolazione_Anno-2023_Sardegna.pdf",
        "publisher": "ISTAT",
    },
    "it-ghilarza-rural": {
        "territory_code": "ISTAT-095021", "territory_unit": "municipality anchor",
        "population": 4_175, "population_year": 2023,
        "evidence_url": "https://www.istat.it/notizia/popolazione-censuaria/",
        "publisher": "ISTAT (single-municipality data linked from the census page)",
    },
    "nl-amsterdam-metro": {
        "territory_code": "CBS-CR23", "territory_unit": "Groot-Amsterdam COROP metro area",
        "population": 1_480_814, "population_year": 2025,
        "evidence_url": "https://www.cbs.nl/en-gb/figures/detail/37259eng",
        "publisher": "CBS",
    },
    "nl-assen-mid": {
        "territory_code": "CBS-GM0106", "territory_unit": "municipality",
        "population": 70_392, "population_year": 2025,
        "evidence_url": "https://www.cbs.nl/nl-nl/cijfers/detail/86059NED",
        "publisher": "CBS",
    },
    "nl-westerbork-rural": {
        "territory_code": "CBS-population-core-Westerbork", "territory_unit": "population core anchor",
        "population": 4_710, "population_year": 2021,
        "evidence_url": "https://www.cbs.nl/nl-nl/longread/statistische-trends/2025/bevolkingsontwikkeling-van-bevolkingskernen-tussen-2011-en-2021?onepage=true",
        "publisher": "CBS",
    },
    "rs-belgrade-metro": {
        "territory_code": "SORS-settlement-Belgrade", "territory_unit": "settlement",
        "population": 1_197_714, "population_year": 2022,
        "evidence_url": "https://popis2022.stat.gov.rs/media/31355/0_ukupan-broj-stanovnika-naselja.xlsx",
        "evidence_sha256": "5b1498923b90ec9930485ac3115cf10ad8f5ccf988840807d87ae70a58f57dc2",
        "evidence_bytes": 231_325,
        "publisher": "Statistical Office of the Republic of Serbia",
    },
    "rs-novi-sad-mid": {
        "territory_code": "SORS-settlement-Novi-Sad", "territory_unit": "settlement",
        "population": 260_438, "population_year": 2022,
        "evidence_url": "https://popis2022.stat.gov.rs/media/31355/0_ukupan-broj-stanovnika-naselja.xlsx",
        "evidence_sha256": "5b1498923b90ec9930485ac3115cf10ad8f5ccf988840807d87ae70a58f57dc2",
        "evidence_bytes": 231_325,
        "publisher": "Statistical Office of the Republic of Serbia",
    },
    "rs-backa-topola-rural": {
        "territory_code": "SORS-settlement-Backa-Topola", "territory_unit": "small-town settlement anchor",
        "population": 11_930, "population_year": 2022,
        "evidence_url": "https://popis2022.stat.gov.rs/media/31355/0_ukupan-broj-stanovnika-naselja.xlsx",
        "evidence_sha256": "5b1498923b90ec9930485ac3115cf10ad8f5ccf988840807d87ae70a58f57dc2",
        "evidence_bytes": 231_325,
        "publisher": "Statistical Office of the Republic of Serbia",
    },
}

URBAN_EXCLUSION_BBOXES: dict[str, list[dict[str, object]]] = {
    "fr-altkirch-rural": [
        {"name": "Mulhouse urban edge", "bbox": [7.20, 47.68, 7.50, 47.75]},
    ],
    "it-ghilarza-rural": [
        {"name": "Oristano urban area", "bbox": [8.45, 39.85, 8.75, 40.02]},
    ],
    "nl-westerbork-rural": [
        {"name": "Hoogeveen urban area", "bbox": [6.38, 52.68, 6.62, 52.80]},
        {"name": "Emmen urban area", "bbox": [6.80, 52.70, 7.05, 52.88]},
    ],
}

for _region_id, _region_spec in REGION_CATALOG.items():
    _evidence = dict(POPULATION_EVIDENCE[_region_id])
    _evidence["checked_at"] = "2026-08-02"
    _region_spec["population_evidence"] = _evidence
    _region_spec["excluded_urban_bboxes"] = URBAN_EXCLUSION_BBOXES.get(_region_id, [])
    if _region_spec["role"] == "rural":
        _region_spec["role_interpretation"] = (
            "rural/small-town window anchored below 20,000 residents; "
            "not a legal-status classification"
        )


def _validate_catalog() -> None:
    if len(REGION_CATALOG) != 12:
        raise CorpusError("region catalog must contain exactly twelve regions")
    for country in COUNTRIES:
        regions = [r for r in REGION_CATALOG.values() if r["country"] == country]
        if collections.Counter(str(r["role"]) for r in regions) != collections.Counter(
            {role: 1 for role in ROLE_QUOTAS}
        ):
            raise CorpusError(f"{country} region roles do not match the fixed contract")
        if sum(int(r["quota"]) for r in regions) != 250:
            raise CorpusError(f"{country} direct OSM quotas do not total 250")
    for region_id, region in REGION_CATALOG.items():
        evidence = region.get("population_evidence")
        if (
            not isinstance(evidence, dict)
            or not isinstance(evidence.get("territory_code"), str)
            or not evidence["territory_code"]
            or not isinstance(evidence.get("population"), int)
            or evidence["population"] <= 0
            or not isinstance(evidence.get("population_year"), int)
            or not str(evidence.get("evidence_url", "")).startswith("https://")
        ):
            raise CorpusError(f"{region_id} has no auditable population evidence")
        population = int(evidence["population"])
        role = region["role"]
        if (
            (role == "major_metro" and population < 1_000_000)
            or (role == "mid_city" and not 50_000 <= population <= 500_000)
            or (role == "rural" and population >= 20_000)
        ):
            raise CorpusError(f"{region_id} population evidence contradicts its role")
    for source_key, source in OSM_SOURCE_CATALOG.items():
        artifact = source["retained_input"]
        if re.fullmatch(r"[0-9a-f]{64}", str(artifact["sha256"])) is None:
            raise CorpusError(f"{source_key} has no pinned SHA-256")
        if int(artifact["bytes"]) <= 0:
            raise CorpusError(f"{source_key} has no pinned byte count")


def _manifest_path(path: pathlib.Path) -> pathlib.Path:
    return path.with_suffix(path.suffix + ".manifest.json")


@contextlib.contextmanager
def _global_extraction_lock():
    """Prevent concurrent region/PBF scans even when outputs differ.

    The inode remains in the system temporary directory.  ``flock`` state is
    released automatically when the descriptor closes, including after a
    crashed process, so no stale-lock deletion protocol is needed.
    """

    lock = pathlib.Path(tempfile.gettempdir()) / GLOBAL_EXTRACTION_LOCK_NAME
    flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(lock, flags, 0o600)
    except OSError as exc:
        raise CorpusError(f"cannot open global extraction lock safely: {lock}: {exc}") from exc
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise CorpusError(f"global extraction lock must be a single-link regular file: {lock}")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise CorpusError(
                "another extract-region process owns the global low-memory lock; "
                "region extraction must be sequential"
            ) from exc
        yield lock
    finally:
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)


def _selection_hash(row: Mapping[str, object], region_id: str) -> str:
    material = "|".join((
        SELECTION_SEED,
        region_id,
        str(row["truth_source_id"]),
        str(row["country"]),
        str(row["record_id"]),
        hybrid._norm(row["query"]),
        f"{float(row['lat']):.7f}",
        f"{float(row['lon']):.7f}",
    ))
    return hashlib.sha256(material.encode()).hexdigest()


def _is_sha256(value: object) -> bool:
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def _inside_bbox(row: Mapping[str, object], bbox: Sequence[float]) -> bool:
    lon, lat = float(row["lon"]), float(row["lat"])
    return bbox[0] <= lon <= bbox[2] and bbox[1] <= lat <= bbox[3]


def _inside_region(row: Mapping[str, object], region: Mapping[str, object]) -> bool:
    if not _inside_bbox(row, region["selection_bbox"]):
        return False
    return not any(
        _inside_bbox(row, exclusion["bbox"])
        for exclusion in region.get("excluded_urban_bboxes", [])
    )


def _osm_query(path: pathlib.Path, bbox: Sequence[float]) -> str:
    """Return an unsorted, node-only local stream restricted to the fixed bbox."""

    base = hybrid._osm_candidate_query(path)
    base, substitutions = re.subn(r"\nORDER BY object_id\s*\Z", "", base)
    if substitutions != 1 or re.search(r"\bORDER\s+BY\b", base, re.IGNORECASE):
        raise CorpusError("OSM helper query ORDER BY shape changed; refusing a possible full sort")
    marker = "  AND tags['demolished'] IS NULL"
    bbox_sql = (
        f"\n  AND lon BETWEEN {bbox[0]:.8f} AND {bbox[2]:.8f}"
        f"\n  AND lat BETWEEN {bbox[1]:.8f} AND {bbox[3]:.8f}"
    )
    if marker not in base:
        raise CorpusError("OSM helper query shape changed")
    return base.replace(marker, marker + bbox_sql)


def _region_query(path: pathlib.Path, region: Mapping[str, object]) -> str:
    query = _osm_query(path, region["selection_bbox"])
    exclusions = "".join(
        "\n  AND NOT ("
        f"lon BETWEEN {item['bbox'][0]:.8f} AND {item['bbox'][2]:.8f} AND "
        f"lat BETWEEN {item['bbox'][1]:.8f} AND {item['bbox'][3]:.8f})"
        for item in region.get("excluded_urban_bboxes", [])
    )
    return query + exclusions


def _source_catalog_entry(source_key: str, capture: Mapping[str, object]) -> dict[str, object]:
    source = json.loads(json.dumps(OSM_SOURCE_CATALOG[source_key]))
    hybrid._require_fixed_capture(
        capture,
        label=f"{source_key} PBF",
        logical_name=str(source["retained_input"]["logical_name"]),
        sha256=str(source["retained_input"]["sha256"]),
        byte_count=int(source["retained_input"]["bytes"]),
    )
    runtime = hybrid._osmium_runtime()
    completed = subprocess.run(
        [str(runtime["executable"]), "fileinfo", "-e", "-j", str(capture["_path"])],
        capture_output=True,
        text=True,
        timeout=300,
    )
    if completed.returncode != 0:
        raise CorpusError(f"osmium fileinfo failed: {completed.stderr.strip()[:300]}")
    try:
        payload = json.loads(completed.stdout)
        file_info = payload["file"]
        header = payload["header"]
        data = payload["data"]
        options = header["option"]
    except (KeyError, TypeError, json.JSONDecodeError) as exc:
        raise CorpusError("invalid osmium fileinfo JSON") from exc
    expected_pbf = source["pbf"]
    snapshot = overture._canonical_utc(str(options.get("osmosis_replication_timestamp", "")))
    sequence = str(options.get("osmosis_replication_sequence_number", ""))
    if (
        file_info.get("format") != "PBF"
        or file_info.get("size") != capture["bytes"]
        or header.get("with_history") is not False
        or data.get("objects_ordered") is not True
        or data.get("multiple_versions") is not False
        or options.get("osmosis_replication_base_url") != expected_pbf["replication_base_url"]
        or snapshot != expected_pbf["snapshot_at"]
        or sequence != str(expected_pbf["replication_sequence"])
    ):
        raise CorpusError(f"{source_key} PBF metadata disagrees with the exact pin")
    source["pbf"].update({
        "header_boxes": header.get("boxes", []),
        "data_bbox": data.get("bbox"),
        "object_counts": data.get("count"),
        "crc32": data.get("crc32"),
    })
    return source


def _overture_maps(rows: Iterable[Mapping[str, object]], country: str):
    by_query: dict[str, set[str]] = collections.defaultdict(set)
    by_coordinate: dict[tuple[float, float], set[str]] = collections.defaultdict(set)
    count = 0
    for row in rows:
        if row["country"] != country:
            continue
        count += 1
        record_id = str(row["record_id"])
        by_query[hybrid._address_key(row)].add(record_id)
        by_coordinate[hybrid._coordinate_key(row)].add(record_id)
    return by_query, by_coordinate, count


@dataclasses.dataclass
class _HeapItem:
    rank: tuple[str, str]
    row: dict[str, object] = dataclasses.field(compare=False)

    def __lt__(self, other: "_HeapItem") -> bool:
        return self.rank > other.rank


def _bounded_add(heap: list[_HeapItem], row: dict[str, object], limit: int) -> None:
    item = _HeapItem((str(row["selection_sha256"]), str(row["record_id"])), row)
    if len(heap) < limit:
        heapq.heappush(heap, item)
    elif item.rank < heap[0].rank:
        heapq.heapreplace(heap, item)


def _stream_region(
    region: Mapping[str, object],
    pbf_path: pathlib.Path,
    source_catalog: Mapping[str, object],
    overture_rows: Sequence[Mapping[str, object]],
    assembled_at: str,
    temp_directory: pathlib.Path,
) -> tuple[list[dict[str, object]], dict[str, object], str]:
    try:
        import duckdb
    except ImportError as exc:  # pragma: no cover - runtime contract
        raise CorpusError("DuckDB is required for ST_ReadOSM") from exc
    overture._assert_runtime(duckdb_version=str(duckdb.__version__))
    if not temp_directory.is_absolute():
        raise CorpusError("DuckDB temp directory must be an absolute external path")
    code_root = EXAMPLES.parent.resolve()
    if temp_directory.resolve() == code_root or code_root in temp_directory.resolve().parents:
        raise CorpusError("DuckDB temp directory must be outside the code tree")
    temp_directory.mkdir(parents=True, exist_ok=True)
    query = _region_query(pbf_path, region)
    query_sha256 = hashlib.sha256(
        _region_query(
            pathlib.Path(str(source_catalog["retained_input"]["logical_name"])), region
        ).encode()
    ).hexdigest()
    by_query, by_coordinate, overture_count = _overture_maps(overture_rows, str(region["country"]))
    heap: list[_HeapItem] = []
    collided_ids: set[str] = set()
    rejected: collections.Counter[str] = collections.Counter()
    collision_reasons: collections.Counter[str] = collections.Counter()
    valid_rows = 0
    source_for_row = dict(source_catalog)
    limit = int(region["quota"]) * CANDIDATE_MULTIPLIER
    connection = duckdb.connect()
    try:
        connection.execute("SET threads=1")
        connection.execute(f"SET memory_limit='{DUCKDB_MEMORY_LIMIT}'")
        connection.execute(f"SET max_temp_directory_size='{DUCKDB_MAX_TEMP}'")
        connection.execute(
            "SET temp_directory=" + hybrid._sql_string(str(temp_directory))
        )
        hybrid._load_offline_spatial(connection)
        cursor = connection.execute(query)
        columns = [description[0] for description in cursor.description]
        while True:
            batch = cursor.fetchmany(MAX_FETCH_ROWS)
            if not batch:
                break
            for values in batch:
                raw = dict(zip(columns, values))
                try:
                    row = hybrid._osm_row_from_raw(
                        str(region["country"]), raw, source_for_row, assembled_at,
                        f"{SELECTION_SEED}|{region['region_id']}",
                    )
                except CorpusError as exc:
                    rejected[str(exc)] += 1
                    continue
                valid_rows += 1
                if not _inside_region(row, region):
                    raise CorpusError("DuckDB returned an OSM row outside the fixed region mask")
                query_ids = by_query.get(hybrid._address_key(row), set())
                coordinate_ids = by_coordinate.get(hybrid._coordinate_key(row), set())
                if query_ids or coordinate_ids:
                    if query_ids:
                        collision_reasons["normalized_query"] += 1
                    if coordinate_ids:
                        collision_reasons["rounded_coordinate"] += 1
                    collided_ids.update(query_ids)
                    collided_ids.update(coordinate_ids)
                    continue
                row["schema"] = TRUTH_SCHEMA
                row["sampling_region_id"] = region["region_id"]
                row["sampling_region_role"] = region["role"]
                row["selection_sha256"] = _selection_hash(row, str(region["region_id"]))
                _bounded_add(heap, row, limit)
    finally:
        connection.close()
    rows = [item.row for item in sorted(heap, key=lambda item: item.rank)]
    diagnostics = {
        "overture_candidates_compared": overture_count,
        "valid_osm_rows_streamed": valid_rows,
        "rejected_by_reason": dict(sorted(rejected.items())),
        "collision_row_occurrences": dict(sorted(collision_reasons.items())),
        "collided_overture_record_ids": sorted(collided_ids),
        "bounded_heap_limit": limit,
        "bounded_candidates_retained": len(rows),
        "full_stream_collision_detection": True,
    }
    return rows, diagnostics, query_sha256


def _fragment_manifest(
    args: argparse.Namespace,
    rows: Sequence[Mapping[str, object]],
    region: Mapping[str, object],
    source: Mapping[str, object],
    diagnostics: Mapping[str, object],
    query_sha256: str,
    script_capture: Mapping[str, object],
    helper_capture: Mapping[str, object],
) -> dict[str, object]:
    return {
        "schema": FRAGMENT_SCHEMA,
        "kind": FRAGMENT_KIND,
        "fragment": args.candidate_output.name,
        "fragment_sha256": overture._sha256_file(args.candidate_output),
        "rows": len(rows),
        "assembled_at": args.assembled_at,
        "region": dict(region),
        "source": dict(source),
        "overture_input": {
            "logical_name": hybrid.OVERTURE_ACQUISITION_NAME,
            "sha256": hybrid.OVERTURE_ACQUISITION_SHA256,
            "bytes": hybrid.OVERTURE_ACQUISITION_BYTES,
            "manifest_sha256": hybrid.OVERTURE_ACQUISITION_MANIFEST_SHA256,
        },
        "extraction": {
            "fresh_process_contract": "one invocation scans exactly one region and one PBF",
            "global_advisory_lock": {
                "logical_name": GLOBAL_EXTRACTION_LOCK_NAME,
                "mode": "fcntl.flock LOCK_EX|LOCK_NB",
                "scope": "all extract-region invocations regardless of output path",
                "stale_release": "descriptor close; lock inode is retained",
            },
            "threads": 1,
            "memory_limit": DUCKDB_MEMORY_LIMIT,
            "max_temp_directory_size": DUCKDB_MAX_TEMP,
            "temp_directory": args.duckdb_temp_directory.name,
            "fetch_rows": MAX_FETCH_ROWS,
            "candidate_multiplier": CANDIDATE_MULTIPLIER,
            "order_by_full_stream": False,
            "selection_formula": SELECTION_FORMULA,
            "osm_query_sha256": query_sha256,
        },
        "collision_diagnostics": dict(diagnostics),
        "collision_universe": {
            "overture_candidate_rows": EXPECTED_OVERTURE_CANDIDATES,
            "comparison_key_includes_country": True,
        },
        "recipe": {
            "script": "examples/multiregion_truth_corpus.py",
            "script_sha256": script_capture["sha256"],
            "helper_script": "examples/hybrid_truth_corpus.py",
            "helper_script_sha256": helper_capture["sha256"],
            "command": _logical_command(args.argv),
        },
    }


def _extract_region_locked(args: argparse.Namespace) -> dict[str, object]:
    _validate_catalog()
    overture._assert_runtime()
    region = REGION_CATALOG[args.region_id]
    source_key = str(region["source_key"])
    script_capture = hybrid._capture_streaming(pathlib.Path(__file__), "multiregion builder")
    helper_capture = hybrid._capture_streaming(
        pathlib.Path(hybrid.__file__), "hybrid corpus helper"
    )
    pbf_capture = hybrid._capture_streaming(args.osm, f"{args.region_id} PBF")
    overture_captures = hybrid._capture_fixed_overture(args.overture_acquisition)
    source = _source_catalog_entry(source_key, pbf_capture)
    overture_rows, _, _, _, _ = hybrid._load_overture_candidates(
        args.overture_acquisition, SELECTION_SEED, *overture_captures
    )
    if len(overture_rows) != EXPECTED_OVERTURE_CANDIDATES:
        raise CorpusError(
            "retained Overture collision universe must contain exactly "
            f"{EXPECTED_OVERTURE_CANDIDATES} rows"
        )
    with overture._output_claim(args.candidate_output):
        rows, diagnostics, query_sha256 = _stream_region(
            region, args.osm, source, overture_rows, args.assembled_at,
            args.duckdb_temp_directory,
        )
        if len(rows) < int(region["quota"]):
            raise CorpusError(
                f"{args.region_id} retained {len(rows)} candidates; at least {region['quota']} required"
            )
        hybrid._verify_capture(pbf_capture, f"{args.region_id} PBF")
        hybrid._verify_capture(overture_captures[0], "Overture acquisition")
        hybrid._verify_capture(overture_captures[1], "Overture acquisition manifest")
        hybrid._verify_capture(helper_capture, "hybrid corpus helper")
        overture._write_jsonl(args.candidate_output, rows)
        manifest = _fragment_manifest(
            args, rows, region, source, diagnostics, query_sha256,
            script_capture, helper_capture,
        )
        overture._write_json(
            _manifest_path(args.candidate_output), manifest, "region fragment manifest"
        )
    return manifest


def extract_region(args: argparse.Namespace) -> dict[str, object]:
    with _global_extraction_lock():
        return _extract_region_locked(args)


def _read_json(path: pathlib.Path, label: str) -> dict[str, object]:
    payload, _ = overture._read_regular_bytes(path, label)
    try:
        value = json.loads(
            payload,
            parse_constant=lambda token: (_ for _ in ()).throw(
                ValueError(f"non-finite JSON constant {token}")
            ),
        )
    except (json.JSONDecodeError, ValueError) as exc:
        raise CorpusError(f"invalid {label} JSON: {path}") from exc
    if not isinstance(value, dict):
        raise CorpusError(f"{label} must be a JSON object")
    return value


def _read_jsonl(path: pathlib.Path, label: str) -> list[dict[str, object]]:
    payload, _ = overture._read_regular_bytes(path, label)
    rows: list[dict[str, object]] = []
    for line_no, line in enumerate(payload.splitlines(), 1):
        try:
            row = json.loads(
                line,
                parse_constant=lambda token: (_ for _ in ()).throw(
                    ValueError(f"non-finite JSON constant {token}")
                ),
            )
        except (json.JSONDecodeError, ValueError) as exc:
            raise CorpusError(f"invalid {label} JSON at line {line_no}") from exc
        if not isinstance(row, dict):
            raise CorpusError(f"{label} line {line_no} is not an object")
        rows.append(row)
    return rows


def _validate_osm_row_source(
    row: Mapping[str, object],
    region: Mapping[str, object],
    source: Mapping[str, object],
    label: str,
) -> None:
    retained = source["retained_input"]
    try:
        lat, lon = float(row["lat"]), float(row["lon"])
    except (KeyError, TypeError, ValueError, OverflowError) as exc:
        raise CorpusError(f"{label} has invalid coordinates") from exc
    if (
        not math.isfinite(lat)
        or not math.isfinite(lon)
        or not overture._finite_in_bounds(str(region["country"]), lat, lon)
        or not _inside_region(row, region)
    ):
        raise CorpusError(f"{label} has non-finite or out-of-region coordinates")
    if (
        row.get("schema") != TRUTH_SCHEMA
        or row.get("country") != region["country"]
        or row.get("sampling_region_id") != region["region_id"]
        or row.get("sampling_region_role") != region["role"]
        or row.get("truth_source_id") != source["source_id"]
        or row.get("truth_source_family") != hybrid.OSM_FAMILY
        or row.get("source_url") != source["public_uri"]
        or row.get("source_release") != source["source_release"]
        or row.get("source_theme") != source["theme"]
        or row.get("source_type") != source["type"]
        or row.get("source_snapshot_at") != source["snapshot_at"]
        or row.get("source_sha256") != retained["sha256"]
        or row.get("source_license") != hybrid.OSM_LICENSE
        or row.get("license") != hybrid.OSM_LICENSE
        or row.get("licenses") != [hybrid.OSM_LICENSE]
        or row.get("lineage_class") != "unknown_lineage"
    ):
        raise CorpusError(f"{label} violates its region/source coupling")
    record_id = row.get("record_id")
    source_record_id = row.get("source_record_id")
    match = re.fullmatch(r"osm:node:([1-9][0-9]*)", str(record_id))
    if match is None or source_record_id != f"node/{match.group(1)}":
        raise CorpusError(f"{label} has inconsistent OSM record identity")
    if not _is_sha256(row.get("source_sha256")) or not _is_sha256(row.get("selection_sha256")):
        raise CorpusError(f"{label} has a non-canonical SHA-256")
    if row.get("selection_sha256") != _selection_hash(row, str(region["region_id"])):
        raise CorpusError(f"{label} selection SHA-256 was not recomputed from the row")
    artifact = row.get("source_artifact")
    provenance = row.get("coordinate_provenance")
    if artifact != retained or not isinstance(provenance, Mapping):
        raise CorpusError(f"{label} has incomplete retained-artifact provenance")
    if (
        provenance.get("source_family") != hybrid.OSM_FAMILY
        or provenance.get("snapshot_sha256") != retained["sha256"]
        or provenance.get("snapshot_logical_name") != retained["logical_name"]
        or provenance.get("source_snapshot_at") != source["snapshot_at"]
        or provenance.get("same_export_as_indexed_sheet") is not False
        or provenance.get("object_type") != "node"
        or provenance.get("object_id") != int(match.group(1))
        or provenance.get("record_id") != f"node/{match.group(1)}"
        or provenance.get("source_url") != f"https://www.openstreetmap.org/node/{match.group(1)}"
        or provenance.get("evidence_url") != f"https://www.openstreetmap.org/node/{match.group(1)}"
        or provenance.get("coordinate_method") != "node_location"
        or provenance.get("replication_sequence") != source["pbf"]["replication_sequence"]
        or provenance.get("replication_base_url") != source["pbf"]["replication_base_url"]
        or provenance.get("license") != hybrid.OSM_LICENSE
        or provenance.get("attribution_url") != hybrid.OSM_COPYRIGHT_URL
    ):
        raise CorpusError(f"{label} coordinate provenance disagrees with its source pin")


def _validate_fragment(
    path: pathlib.Path,
    region: Mapping[str, object],
    *,
    script_capture: Mapping[str, object] | None = None,
    helper_capture: Mapping[str, object] | None = None,
    manifest_capture: Mapping[str, object] | None = None,
) -> tuple[list[dict[str, object]], dict[str, object], dict[str, object]]:
    manifest_path = _manifest_path(path)
    fragment_capture = hybrid._capture_streaming(path, "region fragment")
    manifest = _read_json(manifest_path, "region fragment manifest")
    if manifest_capture is not None:
        hybrid._verify_capture(manifest_capture, "region fragment manifest")
    if manifest.get("schema") != FRAGMENT_SCHEMA or manifest.get("kind") != FRAGMENT_KIND:
        raise CorpusError(f"{path.name} has the wrong fragment contract")
    if manifest.get("region") != dict(region):
        raise CorpusError(f"{path.name} region manifest disagrees with the exact catalog")
    expected_source = OSM_SOURCE_CATALOG[str(region["source_key"])]
    actual_source = manifest.get("source")
    if not isinstance(actual_source, dict):
        raise CorpusError(f"{path.name} has no source catalog entry")
    for field in (
        "source_id", "country", "family", "dataset", "theme", "type",
        "public_uri", "snapshot_at", "source_release", "license",
        "attribution", "copyright_url",
    ):
        if actual_source.get(field) != expected_source[field]:
            raise CorpusError(f"{path.name} source field {field} disagrees with its pin")
    if actual_source.get("retained_input") != expected_source["retained_input"]:
        raise CorpusError(f"{path.name} retained artifact pin changed")
    actual_pbf = actual_source.get("pbf")
    if not isinstance(actual_pbf, dict) or any(
        actual_pbf.get(field) != value
        for field, value in expected_source["pbf"].items()
    ):
        raise CorpusError(f"{path.name} PBF provenance pin changed")
    if manifest.get("fragment_sha256") != fragment_capture["sha256"]:
        raise CorpusError(f"{path.name} hash disagrees with its manifest")
    extraction = manifest.get("extraction")
    if not isinstance(extraction, dict) or extraction.get("threads") != 1:
        raise CorpusError(f"{path.name} was not extracted with one DuckDB thread")
    if (
        extraction.get("memory_limit") != DUCKDB_MEMORY_LIMIT
        or extraction.get("max_temp_directory_size") != DUCKDB_MAX_TEMP
        or extraction.get("fetch_rows") != MAX_FETCH_ROWS
        or extraction.get("candidate_multiplier") != CANDIDATE_MULTIPLIER
        or extraction.get("order_by_full_stream") is not False
        or extraction.get("selection_formula") != SELECTION_FORMULA
        or extraction.get("osm_query_sha256") != hashlib.sha256(
            _region_query(
                pathlib.Path(str(expected_source["retained_input"]["logical_name"])),
                region,
            ).encode()
        ).hexdigest()
    ):
        raise CorpusError(f"{path.name} does not prove the low-memory extraction contract")
    lock = extraction.get("global_advisory_lock")
    if (
        not isinstance(lock, dict)
        or lock.get("logical_name") != GLOBAL_EXTRACTION_LOCK_NAME
        or lock.get("mode") != "fcntl.flock LOCK_EX|LOCK_NB"
    ):
        raise CorpusError(f"{path.name} does not prove global sequential extraction")
    diagnostics = manifest.get("collision_diagnostics")
    if not isinstance(diagnostics, dict) or diagnostics.get("full_stream_collision_detection") is not True:
        raise CorpusError(f"{path.name} does not prove full-stream collision detection")
    collided = diagnostics.get("collided_overture_record_ids")
    if not isinstance(collided, list) or any(not isinstance(value, str) for value in collided):
        raise CorpusError(f"{path.name} has invalid Overture collision ids")
    rows = _read_jsonl(path, "region fragment")
    hybrid._verify_capture(fragment_capture, "region fragment")
    if len(rows) != manifest.get("rows") or len(rows) > int(region["quota"]) * CANDIDATE_MULTIPLIER:
        raise CorpusError(f"{path.name} row count violates its bounded manifest")
    ranks: list[tuple[str, str]] = []
    for line_no, row in enumerate(rows, 1):
        _validate_osm_row_source(row, region, actual_source, f"{path.name} row {line_no}")
        ranks.append((str(row.get("selection_sha256")), str(row.get("record_id"))))
    if ranks != sorted(ranks) or len(ranks) != len(set(ranks)):
        raise CorpusError(f"{path.name} candidate ranking is not canonical")
    recipe = manifest.get("recipe")
    script_capture = script_capture or hybrid._capture_streaming(
        pathlib.Path(__file__), "multiregion builder"
    )
    helper_capture = helper_capture or hybrid._capture_streaming(
        pathlib.Path(hybrid.__file__), "hybrid corpus helper"
    )
    if (
        not isinstance(recipe, dict)
        or recipe.get("script") != "examples/multiregion_truth_corpus.py"
        or recipe.get("helper_script") != "examples/hybrid_truth_corpus.py"
        or recipe.get("script_sha256") != script_capture["sha256"]
        or recipe.get("helper_script_sha256") != helper_capture["sha256"]
    ):
        raise CorpusError(f"{path.name} has incomplete recipe hashes")
    if manifest_capture is not None:
        hybrid._verify_capture(manifest_capture, "region fragment manifest")
    hybrid._verify_capture(fragment_capture, "region fragment")
    return rows, manifest, {
        "logical_name": manifest_path.name,
        "sha256": overture._sha256_file(manifest_path),
        "fragment_sha256": fragment_capture["sha256"],
    }


def _candidate_allowed(
    row: Mapping[str, object],
    seen_ids: set[str],
    seen_queries: set[str],
    seen_coordinates: set[tuple[float, float]],
    cap_counts: Mapping[str, collections.Counter[str]],
    caps: Mapping[str, int],
) -> bool:
    record_id, query, coordinate = hybrid._dedupe_key(row)
    if record_id in seen_ids or query in seen_queries or coordinate in seen_coordinates:
        return False
    return not any(
        key and cap_counts[dimension][key] >= caps[dimension]
        for dimension, key in hybrid._diversity_keys(row).items()
        if dimension in caps
    )


def _constraint_order_v5(
    rows: Iterable[dict[str, object]], caps: Mapping[str, int]
) -> list[dict[str, object]]:
    materialized = list(rows)
    frequencies = {dimension: collections.Counter() for dimension in caps}
    for row in materialized:
        for dimension, key in hybrid._diversity_keys(row).items():
            if dimension in caps and key:
                frequencies[dimension][key] += 1

    def rank(row: Mapping[str, object]) -> tuple[float, float, str, str]:
        pressure = [
            frequencies[dimension][key] / caps[dimension]
            for dimension, key in hybrid._diversity_keys(row).items()
            if dimension in caps and key
        ]
        return (
            max(pressure, default=0.0),
            sum(pressure),
            str(row["selection_sha256"]),
            str(row["record_id"]),
        )

    return sorted(materialized, key=rank)


def _select_from_pool(
    pool: Iterable[dict[str, object]],
    quota: int,
    selected: list[dict[str, object]],
    seen_ids: set[str],
    seen_queries: set[str],
    seen_coordinates: set[tuple[float, float]],
    cap_counts: Mapping[str, collections.Counter[str]],
    caps: Mapping[str, int] | None = None,
) -> int:
    caps = hybrid.CAPS if caps is None else caps
    taken = 0
    for row in _constraint_order_v5(pool, caps):
        if taken >= quota:
            break
        if not _candidate_allowed(
            row, seen_ids, seen_queries, seen_coordinates, cap_counts, caps
        ):
            continue
        selected.append(row)
        taken += 1
        record_id, query, coordinate = hybrid._dedupe_key(row)
        seen_ids.add(record_id)
        seen_queries.add(query)
        seen_coordinates.add(coordinate)
        hybrid._add_counts(row, cap_counts)
    return taken


def _select_countrywide_overture(
    country: str,
    rows: Sequence[dict[str, object]],
    collided_ids: set[str],
    selected: list[dict[str, object]],
    seen_ids: set[str],
    seen_queries: set[str],
    seen_coordinates: set[tuple[float, float]],
    cap_counts: Mapping[str, collections.Counter[str]],
) -> None:
    pool = [
        row for row in rows
        if row["country"] == country and row["record_id"] not in collided_ids
    ]
    for row in pool:
        row["schema"] = TRUTH_SCHEMA
        row["sampling_region_id"] = COUNTRYWIDE_REGION_IDS[country]
        row["sampling_region_role"] = "countrywide"
        row["selection_sha256"] = _selection_hash(row, COUNTRYWIDE_REGION_IDS[country])
    outside = [row for row in pool if row["lineage_class"] == "outside_chain"]
    minimum = OUTSIDE_MINIMA[country]
    got_outside = _select_from_pool(
        outside, minimum, selected, seen_ids, seen_queries, seen_coordinates, cap_counts
    )
    if got_outside != minimum:
        raise CorpusError(f"{country} outside_chain selected {got_outside}; exactly {minimum} minimum phase required")
    already = {str(row["record_id"]) for row in selected if row["country"] == country}
    remainder = [row for row in pool if row["record_id"] not in already]
    got_remainder = _select_from_pool(
        remainder, OVERTURE_QUOTA - minimum, selected,
        seen_ids, seen_queries, seen_coordinates, cap_counts,
    )
    if got_remainder != OVERTURE_QUOTA - minimum:
        raise CorpusError(f"{country} Overture countrywide quota cannot be filled")


def _rows_by_region(rows: Sequence[Mapping[str, object]]) -> dict[str, int]:
    counts = collections.Counter(str(row["sampling_region_id"]) for row in rows)
    return dict(sorted(counts.items()))


def _count_matrices(
    rows: Sequence[Mapping[str, object]], source_ids: Sequence[str]
) -> dict[str, object]:
    by_country = {country: 0 for country in COUNTRIES}
    by_source = {source_id: 0 for source_id in source_ids}
    by_country_source = {
        country: {source_id: 0 for source_id in source_ids} for country in COUNTRIES
    }
    by_lineage = {lineage: 0 for lineage in hybrid.LINEAGE_CLASSES}
    by_country_lineage = {
        country: {lineage: 0 for lineage in hybrid.LINEAGE_CLASSES}
        for country in COUNTRIES
    }
    by_country_source_lineage = {
        country: {
            source_id: {lineage: 0 for lineage in hybrid.LINEAGE_CLASSES}
            for source_id in source_ids
        }
        for country in COUNTRIES
    }
    for row in rows:
        country = str(row["country"])
        source_id = str(row["truth_source_id"])
        lineage = str(row["lineage_class"])
        if source_id not in by_source or lineage not in by_lineage:
            raise CorpusError("selected row names an unsupported source or lineage")
        by_country[country] += 1
        by_source[source_id] += 1
        by_country_source[country][source_id] += 1
        by_lineage[lineage] += 1
        by_country_lineage[country][lineage] += 1
        by_country_source_lineage[country][source_id][lineage] += 1
    return {
        "rows_by_country": by_country,
        "rows_by_source": dict(sorted(by_source.items())),
        "rows_by_country_and_source": {
            country: dict(sorted(values.items()))
            for country, values in by_country_source.items()
        },
        "rows_by_lineage": by_lineage,
        "rows_by_country_and_lineage": by_country_lineage,
        "rows_by_country_source_and_lineage": {
            country: {
                source_id: dict(sorted(lineages.items()))
                for source_id, lineages in sorted(values.items())
            }
            for country, values in by_country_source_lineage.items()
        },
    }


def _validate_final(rows: Sequence[Mapping[str, object]]) -> None:
    if len(rows) != 1_200:
        raise CorpusError("schema-5 corpus must contain exactly 1,200 rows")
    if sum(row["lineage_class"] == "outside_chain" for row in rows) < TOTAL_OUTSIDE_MINIMUM:
        raise CorpusError("schema-5 corpus does not meet the fixed outside_chain minimum of 80")
    seen_ids: set[str] = set()
    seen_queries: set[str] = set()
    seen_coordinates: set[tuple[float, float]] = set()
    for country in COUNTRIES:
        country_rows = [row for row in rows if row["country"] == country]
        if len(country_rows) != PER_COUNTRY:
            raise CorpusError(f"{country} does not contain exactly 300 rows")
        expected = {COUNTRYWIDE_REGION_IDS[country]: OVERTURE_QUOTA}
        expected.update({str(r["region_id"]): int(r["quota"]) for r in REGION_CATALOG.values() if r["country"] == country})
        if collections.Counter(str(row["sampling_region_id"]) for row in country_rows) != collections.Counter(expected):
            raise CorpusError(f"{country} sampling-region quotas changed")
        outside = sum(row["lineage_class"] == "outside_chain" for row in country_rows)
        if outside < OUTSIDE_MINIMA[country]:
            raise CorpusError(f"{country} outside_chain minimum changed")
    for row in rows:
        if row.get("schema") != TRUTH_SCHEMA:
            raise CorpusError("final row has the wrong schema")
        record_id, query, coordinate = hybrid._dedupe_key(row)
        if record_id in seen_ids or query in seen_queries or coordinate in seen_coordinates:
            raise CorpusError("final schema-5 corpus violates global deduplication")
        seen_ids.add(record_id)
        seen_queries.add(query)
        seen_coordinates.add(coordinate)


def assemble(args: argparse.Namespace) -> dict[str, object]:
    _validate_catalog()
    overture._assert_runtime()
    script_capture = hybrid._capture_streaming(pathlib.Path(__file__), "multiregion builder")
    helper_capture = hybrid._capture_streaming(
        pathlib.Path(hybrid.__file__), "hybrid corpus helper"
    )
    overture_captures = hybrid._capture_fixed_overture(args.overture_acquisition)
    overture_rows, overture_catalog, _, acquisition_manifest_sha, _ = hybrid._load_overture_candidates(
        args.overture_acquisition, SELECTION_SEED, *overture_captures
    )
    if len(overture_rows) != EXPECTED_OVERTURE_CANDIDATES:
        raise CorpusError(
            "retained Overture collision universe must contain exactly "
            f"{EXPECTED_OVERTURE_CANDIDATES} rows"
        )
    fragment_paths = {
        region_id: args.region_fragments / f"{region_id}.candidates.jsonl"
        for region_id in REGION_CATALOG
    }
    collided_ids: set[str] = set()
    fragment_manifests: dict[str, dict[str, object]] = {}
    fragment_manifest_captures: dict[str, dict[str, object]] = {}
    overture_ids_by_country = {
        country: {
            str(row["record_id"]) for row in overture_rows if row["country"] == country
        }
        for country in COUNTRIES
    }
    overture_counts_by_country = {
        country: len(overture_ids_by_country[country]) for country in COUNTRIES
    }
    # First pass reads only the small manifests so every collision is known
    # before any Overture row can be selected.
    for region_id, region in REGION_CATALOG.items():
        region_manifest_path = _manifest_path(fragment_paths[region_id])
        capture = hybrid._capture_streaming(
            region_manifest_path, "region fragment manifest"
        )
        fragment_manifest_captures[region_id] = capture
        manifest = _read_json(region_manifest_path, "region fragment manifest")
        hybrid._verify_capture(capture, "region fragment manifest")
        if manifest.get("region") != dict(region):
            raise CorpusError(f"{region_id} fragment manifest changed")
        diagnostics = manifest.get("collision_diagnostics")
        if not isinstance(diagnostics, dict) or diagnostics.get("full_stream_collision_detection") is not True:
            raise CorpusError(f"{region_id} lacks full-stream collision evidence")
        ids = diagnostics.get("collided_overture_record_ids")
        country = str(region["country"])
        if (
            not isinstance(ids, list)
            or any(not isinstance(value, str) for value in ids)
            or ids != sorted(set(ids))
            or not set(ids).issubset(overture_ids_by_country[country])
            or diagnostics.get("overture_candidates_compared")
            != overture_counts_by_country[country]
        ):
            raise CorpusError(f"{region_id} collision ids are invalid")
        if manifest.get("overture_input") != {
            "logical_name": hybrid.OVERTURE_ACQUISITION_NAME,
            "sha256": hybrid.OVERTURE_ACQUISITION_SHA256,
            "bytes": hybrid.OVERTURE_ACQUISITION_BYTES,
            "manifest_sha256": hybrid.OVERTURE_ACQUISITION_MANIFEST_SHA256,
        } or manifest.get("collision_universe") != {
            "overture_candidate_rows": EXPECTED_OVERTURE_CANDIDATES,
            "comparison_key_includes_country": True,
        }:
            raise CorpusError(f"{region_id} collision universe changed")
        collided_ids.update(ids)
    selected: list[dict[str, object]] = []
    seen_ids: set[str] = set()
    seen_queries: set[str] = set()
    seen_coordinates: set[tuple[float, float]] = set()
    for country in COUNTRIES:
        cap_counts = {dimension: collections.Counter() for dimension in hybrid.CAPS}
        _select_countrywide_overture(
            country, overture_rows, collided_ids, selected,
            seen_ids, seen_queries, seen_coordinates, cap_counts,
        )
        regions = sorted(
            (r for r in REGION_CATALOG.values() if r["country"] == country),
            key=lambda r: int(r["selection_order"]),
        )
        for region in regions:
            region_id = str(region["region_id"])
            # Exactly one bounded fragment is materialized at a time.
            pool, _, public_manifest_capture = _validate_fragment(
                fragment_paths[region_id],
                region,
                script_capture=script_capture,
                helper_capture=helper_capture,
                manifest_capture=fragment_manifest_captures[region_id],
            )
            fragment_manifests[region_id] = public_manifest_capture
            got = _select_from_pool(
                pool, int(region["quota"]), selected,
                seen_ids, seen_queries, seen_coordinates, cap_counts,
                DIRECT_REGION_CAPS,
            )
            if got != int(region["quota"]):
                raise CorpusError(f"{region_id} selected {got}; exactly {region['quota']} required")
            del pool
    selected.sort(key=lambda row: (
        COUNTRIES.index(str(row["country"])),
        0 if row["sampling_region_role"] == "countrywide" else 1 + int(REGION_CATALOG[str(row["sampling_region_id"])]["selection_order"]),
        str(row["selection_sha256"]),
        str(row["record_id"]),
    ))
    _validate_final(selected)
    source_catalog = {hybrid.OVERTURE_SOURCE_ID: overture_catalog}
    source_catalog.update({str(value["source_id"]): value for value in OSM_SOURCE_CATALOG.values()})
    with overture._output_claim(args.output):
        hybrid._verify_capture(overture_captures[0], "Overture acquisition")
        hybrid._verify_capture(overture_captures[1], "Overture acquisition manifest")
        hybrid._verify_capture(helper_capture, "hybrid corpus helper")
        for capture in fragment_manifest_captures.values():
            hybrid._verify_capture(capture, "region fragment manifest")
        overture._write_jsonl(args.output, selected)
        matrices = _count_matrices(selected, tuple(source_catalog))
        rows_by_country_region = {
            country: _rows_by_region([row for row in selected if row["country"] == country])
            for country in COUNTRIES
        }
        manifest = {
            "schema": TRUTH_SCHEMA,
            "kind": MANIFEST_KIND,
            "corpus": args.output.name,
            "sha256": overture._sha256_file(args.output),
            "rows": len(selected),
            "assembled_at": args.assembled_at,
            "lineage_policy": hybrid.LINEAGE_POLICY,
            "licenses": sorted({str(row["license"]) for row in selected}),
            **matrices,
            "rows_by_sampling_region": _rows_by_region(selected),
            "rows_by_country_and_sampling_region": rows_by_country_region,
            "source_catalog": dict(sorted(source_catalog.items())),
            "region_catalog": dict(sorted(REGION_CATALOG.items())),
            "countrywide_sampling": {
                country: {
                    "sampling_region_id": COUNTRYWIDE_REGION_IDS[country],
                    "role": "countrywide",
                    "quota": OVERTURE_QUOTA,
                    "coverage_scope": "country extent",
                }
                for country in COUNTRIES
            },
            "selection": {
                "seed": SELECTION_SEED,
                "formula": SELECTION_FORMULA,
                "per_country": PER_COUNTRY,
                "countrywide_overture_quota": OVERTURE_QUOTA,
                "direct_osm_role_quotas": dict(ROLE_QUOTAS),
                "outside_chain_minimum_by_country": dict(OUTSIDE_MINIMA),
                "outside_chain_minimum_total": TOTAL_OUTSIDE_MINIMUM,
                "candidate_multiplier": CANDIDATE_MULTIPLIER,
                "diversity_caps": dict(hybrid.CAPS),
                "direct_region_diversity_caps": dict(DIRECT_REGION_CAPS),
                "direct_region_municipality_policy": DIRECT_REGION_POLICY,
                "fixed_order": "country -> Overture countrywide -> major_metro -> mid_city -> rural",
                "deduplication": "full-stream cross-source collision exclusion, then global record/query/coordinate dedupe",
                "engine_blind": True,
            },
            "low_memory_contract": {
                "one_process_per_region_pbf": True,
                "global_advisory_lock": {
                    "logical_name": GLOBAL_EXTRACTION_LOCK_NAME,
                    "mode": "fcntl.flock LOCK_EX|LOCK_NB",
                    "scope": "all extract-region invocations regardless of output path",
                    "stale_release": "descriptor close; lock inode is retained",
                },
                "threads": 1,
                "memory_limit": DUCKDB_MEMORY_LIMIT,
                "max_temp_directory_size": DUCKDB_MAX_TEMP,
                "fetch_rows": MAX_FETCH_ROWS,
                "bounded_heap_rows": "region quota * 64",
                "full_stream_collision_detection": True,
            },
            "collision_diagnostics": {
                "collided_overture_record_ids": sorted(collided_ids),
                "collided_overture_record_count": len(collided_ids),
                "overture_candidate_universe_rows": EXPECTED_OVERTURE_CANDIDATES,
                "policy": "exclude both OSM row and every matching Overture id before bounded selection",
            },
            "fragment_manifests": dict(sorted(fragment_manifests.items())),
            "recipe": {
                "script": "examples/multiregion_truth_corpus.py",
                "script_sha256": script_capture["sha256"],
                "helper_script": "examples/hybrid_truth_corpus.py",
                "helper_script_sha256": helper_capture["sha256"],
                "overture_acquisition_manifest_sha256": acquisition_manifest_sha,
                "command": _logical_command(args.argv),
            },
        }
        overture._write_json(_manifest_path(args.output), manifest, "schema-5 corpus manifest")
    return manifest


def _logical_command(argv: Sequence[str]) -> str:
    logical = ["python3", "examples/multiregion_truth_corpus.py"]
    for value in argv:
        logical.append(pathlib.Path(value).name if "/" in value or "\\" in value else value)
    return shlex.join(logical)


def _canonical_assembled_at(value: str) -> str:
    return overture._canonical_utc(value)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    extract = commands.add_parser("extract-region", help="scan exactly one pinned region/PBF")
    extract.add_argument("--region-id", required=True, choices=sorted(REGION_CATALOG))
    extract.add_argument("--osm", required=True, type=pathlib.Path)
    extract.add_argument("--overture-acquisition", required=True, type=pathlib.Path)
    extract.add_argument("--candidate-output", required=True, type=pathlib.Path)
    extract.add_argument("--duckdb-temp-directory", required=True, type=pathlib.Path)
    extract.add_argument("--assembled-at", required=True, type=_canonical_assembled_at)
    extract.set_defaults(handler=extract_region)
    assemble_parser = commands.add_parser("assemble", help="assemble all twelve bounded fragments")
    assemble_parser.add_argument("--overture-acquisition", required=True, type=pathlib.Path)
    assemble_parser.add_argument("--region-fragments", required=True, type=pathlib.Path)
    assemble_parser.add_argument("--assembled-at", required=True, type=_canonical_assembled_at)
    assemble_parser.add_argument("--output", required=True, type=pathlib.Path)
    assemble_parser.set_defaults(handler=assemble)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    raw = list(sys.argv[1:] if argv is None else argv)
    args = build_parser().parse_args(raw)
    args.argv = tuple(raw)
    manifest = args.handler(args)
    print(json.dumps({
        "schema": manifest["schema"],
        "kind": manifest["kind"],
        "rows": manifest["rows"],
    }, sort_keys=True))
    return 0


_validate_catalog()


if __name__ == "__main__":
    raise SystemExit(main())
