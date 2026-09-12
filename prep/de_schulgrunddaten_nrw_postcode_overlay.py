#!/usr/bin/env python3
"""Fill blank DE builder postcodes from frozen official NRW school metadata.

The overlay is deliberately narrow: it reads only ``PLZ``, ``Ort`` and
``Strasse`` values from the byte-pinned Schulgrunddaten CSV, treats the state
as the fixed product constant ``DE-NW``, and never admits a source point.  A
postcode can be copied only onto an exact existing builder
street/locality/single-house/suffix identity.  All source multiplicity and all
base ambiguity are fail-closed.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
from collections.abc import Mapping, Sequence
import csv
from dataclasses import dataclass
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
from typing import Any

import de_kibiz_postcode_overlay as invariant
import de_photon_osm_supplement as builder


SCHEMA = "gridpin-de-schulgrunddaten-nrw-postcode-overlay-v1"
STATUS = "PUBLIC_PERMISSIVE_DEVELOPMENT"
SOURCE_CARD_SCHEMA = "gridpin-de-schulgrunddaten-nrw-source-card-v1"
LICENSE_FREEZE_SCHEMA = "gridpin-de-schulgrunddaten-nrw-license-freeze-v1"
ACQUISITION_SCHEMA = "gridpin-de-source-acquisition-v1"
AUDIT_SCHEMA = "gridpin-de-schulen-nrw-source-audit-v1"
LICENSE = "DL-DE-BY-2.0"
LICENSE_NAME = "Datenlizenz Deutschland – Namensnennung – Version 2.0"
LICENSE_URI = "http://dcat-ap.de/def/licenses/dl-by-de/2.0"
LICENSE_URL = "https://www.govdata.de/dl-de/by-2-0"
PUBLISHER = "Ministerium für Schule und Bildung des Landes NRW"
ACQUISITION_PUBLISHER = (
    "Ministerium fuer Schule und Bildung des Landes Nordrhein-Westfalen"
)
DATASET_TITLE = "Schulgrunddaten NRW"
DATASET_ID = "001ec65e-9b58-4809-878a-34b182d5e0e3"
DATASET_IDENTIFIER = "d1918da1-1f2d-52ea-8295-6069860fb0c9"
DISTRIBUTION_ID = "c88140e7-f9ac-4307-b092-632be123d532"
DATASET_URL = f"https://ckan.open.nrw.de/dataset/{DATASET_ID}"
DISTRIBUTION_URL = (
    "https://www.schulministerium.nrw.de/BiPo/OpenData/Schuldaten/schuldaten.csv"
)
RESOLVED_DISTRIBUTION_URL = (
    "https://apps.schulministerium.nrw.de/BiPo/OpenData/Schuldaten/schuldaten.csv"
)
ATTRIBUTION = (
    "Ministerium für Schule und Bildung des Landes NRW; Schulgrunddaten NRW; "
    "Datenlizenz Deutschland – Namensnennung – Version 2.0 "
    "(https://www.govdata.de/dl-de/by-2-0); "
    f"{DATASET_URL}; Daten wurden geändert."
)
FIXED_STATE_CODE = "DE-NW"
SAFE_SOURCE_COLUMNS = ("PLZ", "Ort", "Strasse")
DEFAULT_MIN_FREE_BYTES = 10 * 2**30
MAX_SOURCE_LINE_BYTES = 1 * 2**20
_POSTCODE = re.compile(r"[0-9]{5}")
_STREET_HOUSE = re.compile(r"\s*(.+?)\s+([0-9]+)\s*([A-Za-z]?)\s*")
_SHA256 = re.compile(r"[0-9a-f]{64}")

RAW_HEADER = (
    "Schulnummer",
    "Schulform",
    "Schulbezeichnung_1",
    "Schulbezeichnung_2",
    "Schulbezeichnung_3",
    "Kurzbezeichnung",
    "Bezirksregierung",
    "PLZ",
    "Ort",
    "Strasse",
    "Telefonvorwahl",
    "Telefon",
    "Faxvorwahl",
    "Fax",
    "E-Mail",
    "Homepage",
    "Rechtsform",
    "Traegernummer",
    "Gemeindeschluessel",
    "Schulbetriebsschluessel",
    "Schulbetriebsdatum",
    "EPSG",
    "UTMRechtswert",
    "UTMHochwert",
    "",
)
SAFE_SOURCE_INDEXES = tuple(RAW_HEADER.index(name) for name in SAFE_SOURCE_COLUMNS)

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
FROZEN_SOURCE_CARD = REPOSITORY_ROOT / "code/prep/de_schulgrunddaten_nrw_source_v1.json"
FROZEN_LICENSE_FREEZE = (
    REPOSITORY_ROOT / "code/eval/de_schulgrunddaten_nrw_license_freeze_v1.json"
)
FROZEN_SOURCE = (
    REPOSITORY_ROOT
    / "code/eval/work/de_f5_wave_e_schulen_nrw_20260821/schuldaten.csv"
)
FROZEN_ACQUISITION = (
    REPOSITORY_ROOT
    / "code/eval/work/de_f5_wave_e_schulen_nrw_20260821/source_acquisition_v1.json"
)
FROZEN_AUDIT = (
    REPOSITORY_ROOT
    / "code/eval/work/de_f5_wave_e_schulen_nrw_20260821/source_audit_v1.json"
)
FROZEN_HEADERS = (
    REPOSITORY_ROOT
    / "code/eval/work/de_f5_wave_e_schulen_nrw_20260821/schuldaten.http_headers.txt"
)
FROZEN_BNETZA_BUILDER = (
    REPOSITORY_ROOT
    / "code/eval/work/de_continuous_product_optimization_v1/"
    "bnetza_postcode_fill_only_v1/"
    "build_de_permissive_bnetza_postcode_fill_only_v1.csv.gz"
)


class SchoolOverlayError(RuntimeError):
    """The metadata overlay could not be produced without weakening a guard."""


@dataclass(frozen=True)
class Pin:
    sha256: str
    bytes: int


FROZEN_SOURCE_CARD_PIN = Pin(
    "8cabf90492581eeedb12bde3c822c4750d099eb0b7fb0802005b1df1b426079b",
    3_031,
)
FROZEN_LICENSE_FREEZE_PIN = Pin(
    "4db5b49a9179a17fdec0bedb7d8e3549d65874b830665ffc24495a18efa0d0bc",
    5_286,
)
FROZEN_SOURCE_PIN = Pin(
    "7138a5796ca63c07bb519b4b068d49adb65885f996c50859e9916f12ee60c1de",
    1_788_962,
)
FROZEN_ACQUISITION_PIN = Pin(
    "e03ce24e7bed3998e5d3d44911931e9c1e4ba0e71fa6fe1234e44903c8157278",
    1_060,
)
FROZEN_AUDIT_PIN = Pin(
    "a534feebb72a20d82369783ebf98717400daf54a9d718cf07703efd8fec80977",
    2_526,
)
FROZEN_HEADERS_PIN = Pin(
    "390efe5c628bb6fd242ba6bde057f75b7e3ba496b499ec15dec3a4f9a539d1c3",
    813,
)
FROZEN_BNETZA_BUILDER_PIN = Pin(
    "a40543295cfd261a2e76012b45d7d3712b2987abc09fa9ef6cbb1e4fb599f116",
    248_839_581,
)


@dataclass(frozen=True)
class SourceContract:
    source_card: Path
    source_card_pin: Pin
    license_freeze: Path
    license_freeze_pin: Pin
    raw_csv: Path
    raw_csv_pin: Pin
    acquisition: Path
    acquisition_pin: Pin
    audit: Path
    audit_pin: Pin
    response_headers: Path
    response_headers_pin: Pin
    snapshot_date: str


@dataclass(frozen=True)
class Config:
    source: SourceContract
    expected_source_rows: int
    builder_csv: Path
    builder_pin: Pin
    expected_builder_rows: int
    output_csv: Path
    receipt: Path
    status: str = STATUS
    minimum_free_bytes: int = DEFAULT_MIN_FREE_BYTES


Projection = invariant.Projection


def canonical_json_bytes(value: Mapping[str, Any]) -> bytes:
    return invariant.canonical_json_bytes(value)


def _validate_pin(pin: Pin, label: str) -> None:
    if not isinstance(pin.sha256, str) or _SHA256.fullmatch(pin.sha256) is None:
        raise SchoolOverlayError(f"{label} SHA-256 pin is invalid")
    if isinstance(pin.bytes, bool) or pin.bytes <= 0:
        raise SchoolOverlayError(f"{label} byte pin must be positive")


def _builder_pin(pin: Pin) -> builder.Pin:
    return builder.Pin(sha256=pin.sha256, bytes=pin.bytes)


def _open_pinned(path: Path, pin: Pin, label: str) -> builder.PinnedFile:
    _validate_pin(pin, label)
    try:
        return builder._open_pinned(path, _builder_pin(pin), label)
    except builder.SupplementError as exc:
        raise SchoolOverlayError(str(exc)) from exc


def _recheck_pinned(pinned: builder.PinnedFile, label: str) -> None:
    try:
        builder._recheck_pinned(pinned, label)
    except builder.SupplementError as exc:
        raise SchoolOverlayError(str(exc)) from exc


def _strict_json(raw: str, label: str) -> Any:
    try:
        return builder.strict_json_loads(raw)
    except (ValueError, json.JSONDecodeError) as exc:
        raise SchoolOverlayError(f"{label}: invalid strict JSON: {exc}") from exc


def _read_pinned_json(pinned: builder.PinnedFile, label: str) -> Mapping[str, Any]:
    pinned.stream.seek(0)
    try:
        text = pinned.stream.read().decode("utf-8", errors="strict")
    except UnicodeDecodeError as exc:
        raise SchoolOverlayError(f"{label} is not strict UTF-8") from exc
    value = _strict_json(text, label)
    if not isinstance(value, dict):
        raise SchoolOverlayError(f"{label} must be a JSON object")
    return value


def _path_text(path: Path) -> str:
    resolved = path.resolve(strict=True)
    try:
        return str(resolved.relative_to(REPOSITORY_ROOT))
    except ValueError:
        return str(resolved)


def _receipt_path(value: Any, label: str) -> Path:
    if not isinstance(value, str) or not value:
        raise SchoolOverlayError(f"{label} path is missing")
    candidate = Path(value)
    if not candidate.is_absolute():
        candidate = REPOSITORY_ROOT / candidate
    try:
        resolved = candidate.resolve(strict=True)
    except OSError as exc:
        raise SchoolOverlayError(f"{label} path is unavailable") from exc
    return resolved


def _pin_from_document(value: Any, label: str) -> Pin:
    if not isinstance(value, dict):
        raise SchoolOverlayError(f"{label} evidence is missing")
    pin = Pin(sha256=value.get("sha256"), bytes=value.get("bytes"))
    _validate_pin(pin, label)
    return pin


def _exact_evidence(path: Path, pin: Pin) -> dict[str, Any]:
    return {"path": _path_text(path), "bytes": pin.bytes, "sha256": pin.sha256}


def _validate_source_card(
    card: Mapping[str, Any], contract: SourceContract, *, expected_source_rows: int
) -> None:
    if card.get("schema") != SOURCE_CARD_SCHEMA:
        raise SchoolOverlayError("school source-card schema drift")
    if card.get("attribution") != ATTRIBUTION or card.get("publisher") != PUBLISHER:
        raise SchoolOverlayError("school source-card attribution drift")
    if card.get("dataset") != {
        "ckan_dataset_id": DATASET_ID,
        "dataset_identifier": DATASET_IDENTIFIER,
        "distribution_id": DISTRIBUTION_ID,
        "title": DATASET_TITLE,
    }:
        raise SchoolOverlayError("school source-card dataset binding drift")
    if card.get("license") != {
        "changes_marked": True,
        "identifier": LICENSE,
        "name": LICENSE_NAME,
        "url": LICENSE_URL,
    }:
        raise SchoolOverlayError("school source-card license drift")
    if card.get("license_freeze") != _exact_evidence(
        contract.license_freeze, contract.license_freeze_pin
    ):
        raise SchoolOverlayError("school source-card license-freeze binding drift")
    frozen = card.get("frozen_source")
    if not isinstance(frozen, dict):
        raise SchoolOverlayError("school source-card frozen-source binding is missing")
    expected_frozen = {
        "snapshot": {
            **_exact_evidence(contract.raw_csv, contract.raw_csv_pin),
        },
        "acquisition_manifest": _exact_evidence(
            contract.acquisition, contract.acquisition_pin
        ),
        "outcome_blind_audit": _exact_evidence(contract.audit, contract.audit_pin),
        "response_headers": _exact_evidence(
            contract.response_headers, contract.response_headers_pin
        ),
        "rows": expected_source_rows,
        "snapshot_date": contract.snapshot_date,
    }
    if frozen != expected_frozen:
        raise SchoolOverlayError("school source-card frozen-source binding drift")
    policy = card.get("safe_field_policy")
    if not isinstance(policy, dict):
        raise SchoolOverlayError("school safe-field policy is missing")
    if policy.get("fixed_state_code") != FIXED_STATE_CODE:
        raise SchoolOverlayError("school fixed-state policy drift")
    if policy.get("source_columns_accessed") != list(SAFE_SOURCE_COLUMNS):
        raise SchoolOverlayError("school safe source columns drift")
    if policy.get("source_values_not_accessed") != (
        "school identifiers, school status/type/name, provider identifiers, "
        "contact details, web fields and coordinates"
    ):
        raise SchoolOverlayError("school forbidden-value policy drift")


def _validate_acquisition(
    acquisition: Mapping[str, Any],
    audit: Mapping[str, Any],
    headers: str,
    contract: SourceContract,
    *,
    expected_source_rows: int,
) -> None:
    if acquisition.get("schema") != ACQUISITION_SCHEMA:
        raise SchoolOverlayError("school acquisition schema drift")
    source = acquisition.get("source")
    if not isinstance(source, dict) or source != {
        "publisher": ACQUISITION_PUBLISHER,
        "url": RESOLVED_DISTRIBUTION_URL,
        "snapshot_path": _path_text(contract.raw_csv),
        "response_headers_path": _path_text(contract.response_headers),
    }:
        raise SchoolOverlayError("school acquisition source binding drift")
    acquired = acquisition.get("acquisition")
    if not isinstance(acquired, dict):
        raise SchoolOverlayError("school acquisition facts are missing")
    required_acquisition = {
        "request_count": 1,
        "redirect_count": 0,
        "http_status": 200,
        "content_type": "text/csv",
        "no_clobber": True,
    }
    for key, expected in required_acquisition.items():
        if acquired.get(key) != expected:
            raise SchoolOverlayError(f"school acquisition drift at {key}")
    integrity = acquisition.get("integrity")
    if not isinstance(integrity, dict) or integrity != {
        "bytes": contract.raw_csv_pin.bytes,
        "sha256": contract.raw_csv_pin.sha256,
        "response_headers_bytes": contract.response_headers_pin.bytes,
        "response_headers_sha256": contract.response_headers_pin.sha256,
    }:
        raise SchoolOverlayError("school acquisition integrity drift")
    if audit.get("schema") != AUDIT_SCHEMA or audit.get("outcome_blind") is not True:
        raise SchoolOverlayError("school source-audit contract drift")
    if audit.get("engine_or_competitor_calls_performed") is not False:
        raise SchoolOverlayError("school source audit is not engine/competitor blind")
    source_audit = audit.get("source")
    if not isinstance(source_audit, dict):
        raise SchoolOverlayError("school source-audit source binding is missing")
    expected_audit = {
        "rows": expected_source_rows,
        "bytes": contract.raw_csv_pin.bytes,
        "sha256": contract.raw_csv_pin.sha256,
        "schema_columns": 24,
        "all_row_widths": 24,
        "separator_declaration": "sep=;",
        "header_has_terminal_empty_dialect_column": True,
    }
    if source_audit != expected_audit:
        raise SchoolOverlayError("school source-audit source binding drift")
    lower_headers = headers.lower()
    for required in (
        "200",
        f"content-length: {contract.raw_csv_pin.bytes}",
        "content-type: text/csv",
    ):
        if required not in lower_headers:
            raise SchoolOverlayError("school frozen response-header witness drift")


EXPECTED_METADATA_ROLES = {
    "open_nrw_ckan_package_show",
    "open_nrw_dcat_jsonld",
    "open_nrw_distribution_page",
    "govdata_license_terms",
    "official_distribution_redirect_chain",
}
EXPECTED_METADATA_URLS = {
    "open_nrw_ckan_package_show": (
        f"https://ckan.open.nrw.de/api/3/action/package_show?id={DATASET_ID}"
    ),
    "open_nrw_dcat_jsonld": f"{DATASET_URL}.jsonld",
    "open_nrw_distribution_page": f"{DATASET_URL}/resource/{DISTRIBUTION_ID}",
    "govdata_license_terms": LICENSE_URL,
    "official_distribution_redirect_chain": DISTRIBUTION_URL,
}


def _validate_license_freeze_document(
    freeze: Mapping[str, Any], contract: SourceContract, *, expected_source_rows: int
) -> list[tuple[str, Path, Pin]]:
    if freeze.get("schema") != LICENSE_FREEZE_SCHEMA:
        raise SchoolOverlayError("school license-freeze schema drift")
    if freeze.get("dataset") != {
        "ckan_dataset_id": DATASET_ID,
        "dataset_identifier": DATASET_IDENTIFIER,
        "dataset_title": DATASET_TITLE,
        "dataset_url": DATASET_URL,
        "distribution_id": DISTRIBUTION_ID,
        "distribution_url": DISTRIBUTION_URL,
        "publisher": PUBLISHER,
        "resolved_distribution_url": RESOLVED_DISTRIBUTION_URL,
    }:
        raise SchoolOverlayError("school license-freeze dataset drift")
    if freeze.get("license") != {
        "changes_must_be_marked": True,
        "identifier": LICENSE,
        "name": LICENSE_NAME,
        "terms_url": LICENSE_URL,
        "distribution_license_uri": LICENSE_URI,
    }:
        raise SchoolOverlayError("school license-freeze license drift")
    expected_frozen = {
        "snapshot": _exact_evidence(contract.raw_csv, contract.raw_csv_pin),
        "acquisition_manifest": _exact_evidence(
            contract.acquisition, contract.acquisition_pin
        ),
        "outcome_blind_audit": _exact_evidence(contract.audit, contract.audit_pin),
        "response_headers": _exact_evidence(
            contract.response_headers, contract.response_headers_pin
        ),
        "rows": expected_source_rows,
        "snapshot_date": contract.snapshot_date,
    }
    if freeze.get("frozen_data") != expected_frozen:
        raise SchoolOverlayError("school license-freeze frozen-data binding drift")
    verification = freeze.get("verification")
    if not isinstance(verification, dict):
        raise SchoolOverlayError("school license verification is missing")
    for key in (
        "distribution_access_url_bound",
        "distribution_csv_format_bound",
        "distribution_license_bound",
        "frozen_apps_url_is_redirect_target",
        "live_distribution_changed_after_frozen_snapshot",
    ):
        if verification.get(key) is not True:
            raise SchoolOverlayError(f"school license verification drift at {key}")
    metadata = freeze.get("metadata_freeze")
    if not isinstance(metadata, dict):
        raise SchoolOverlayError("school metadata freeze is missing")
    if metadata.get("network_calls") != 5 or metadata.get("raw_data_downloaded") is not False:
        raise SchoolOverlayError("school metadata-freeze scope drift")
    resources = metadata.get("resources")
    if not isinstance(resources, list) or len(resources) != 5:
        raise SchoolOverlayError("school metadata resource count drift")
    opened: list[tuple[str, Path, Pin]] = []
    seen_roles: set[str] = set()
    for resource in resources:
        if not isinstance(resource, dict):
            raise SchoolOverlayError("school metadata resource is malformed")
        role = resource.get("role")
        if role not in EXPECTED_METADATA_ROLES or role in seen_roles:
            raise SchoolOverlayError("school metadata resource role drift")
        seen_roles.add(role)
        if resource.get("url") != EXPECTED_METADATA_URLS[role]:
            raise SchoolOverlayError(f"school metadata URL drift at {role}")
        if role == "official_distribution_redirect_chain":
            if resource.get("method") != "HEAD" or resource.get("url") != DISTRIBUTION_URL:
                raise SchoolOverlayError("school redirect witness contract drift")
        paths = ("headers",) if role == "official_distribution_redirect_chain" else (
            "body",
            "headers",
        )
        for kind in paths:
            evidence = resource.get(kind)
            pin = _pin_from_document(evidence, f"{role} {kind}")
            path = _receipt_path(evidence.get("path"), f"{role} {kind}")
            if _path_text(path) != evidence.get("path"):
                raise SchoolOverlayError(f"{role} {kind} path binding drift")
            opened.append((f"{role} {kind}", path, pin))
    if seen_roles != EXPECTED_METADATA_ROLES:
        raise SchoolOverlayError("school metadata resource roles are incomplete")
    return opened


def _contains_all(raw: str, required: Sequence[str], label: str) -> None:
    if any(value not in raw for value in required):
        raise SchoolOverlayError(f"{label} does not reproduce the pinned license chain")


def _validate_metadata_contents(contents: Mapping[str, bytes]) -> None:
    package = _strict_json(
        contents["open_nrw_ckan_package_show body"].decode("utf-8", errors="strict"),
        "school CKAN package",
    )
    if not isinstance(package, dict) or package.get("success") is not True:
        raise SchoolOverlayError("school CKAN package response drift")
    result = package.get("result")
    if not isinstance(result, dict):
        raise SchoolOverlayError("school CKAN package result is missing")
    if (
        result.get("id"),
        result.get("title"),
        result.get("license_id"),
        result.get("license_url"),
    ) != (DATASET_ID, DATASET_TITLE, LICENSE_URI, LICENSE_URL):
        raise SchoolOverlayError("school CKAN dataset/license binding drift")
    identifiers = {
        item.get("value")
        for item in result.get("extras", [])
        if isinstance(item, dict) and item.get("key") == "identifier"
    }
    if identifiers != {DATASET_IDENTIFIER}:
        raise SchoolOverlayError("school CKAN dataset identifier drift")
    distributions = [
        item
        for item in result.get("resources", [])
        if isinstance(item, dict) and item.get("id") == DISTRIBUTION_ID
    ]
    if len(distributions) != 1:
        raise SchoolOverlayError("school CKAN CSV distribution is ambiguous")
    distribution = distributions[0]
    if (
        distribution.get("url"),
        distribution.get("license"),
        distribution.get("mimetype"),
    ) != (
        DISTRIBUTION_URL,
        LICENSE_URI,
        "https://www.iana.org/assignments/media-types/text/csv",
    ):
        raise SchoolOverlayError("school CKAN CSV distribution drift")

    jsonld = _strict_json(
        contents["open_nrw_dcat_jsonld body"].decode("utf-8", errors="strict"),
        "school DCAT JSON-LD",
    )
    graph = jsonld.get("@graph") if isinstance(jsonld, dict) else None
    if not isinstance(graph, list):
        raise SchoolOverlayError("school DCAT graph is missing")
    dataset_nodes = [item for item in graph if isinstance(item, dict) and item.get("@id") == DATASET_URL]
    distribution_node_id = f"{DATASET_URL}/resource/{DISTRIBUTION_ID}"
    distribution_nodes = [
        item for item in graph if isinstance(item, dict) and item.get("@id") == distribution_node_id
    ]
    if len(dataset_nodes) != 1 or len(distribution_nodes) != 1:
        raise SchoolOverlayError("school DCAT dataset/distribution identity drift")
    if (
        dataset_nodes[0].get("dct:identifier"),
        dataset_nodes[0].get("dct:title"),
    ) != (DATASET_IDENTIFIER, DATASET_TITLE):
        raise SchoolOverlayError("school DCAT dataset metadata drift")
    dcat_distribution = distribution_nodes[0]
    if (
        dcat_distribution.get("dcat:accessURL"),
        dcat_distribution.get("dct:license"),
        dcat_distribution.get("dct:format"),
    ) != (
        {"@id": DISTRIBUTION_URL},
        {"@id": LICENSE_URI},
        {"@id": "http://publications.europa.eu/resource/authority/file-type/CSV"},
    ):
        raise SchoolOverlayError("school DCAT CSV/license/accessURL drift")

    resource_html = contents["open_nrw_distribution_page body"].decode(
        "utf-8", errors="strict"
    )
    _contains_all(
        resource_html,
        (DATASET_ID, DISTRIBUTION_ID, DISTRIBUTION_URL, "Schulgrunddaten NRW"),
        "school distribution page",
    )
    license_html = contents["govdata_license_terms body"].decode("utf-8", errors="strict")
    _contains_all(
        license_html,
        (
            "Datenlizenz Deutschland – Namensnennung – Version 2.0",
            "dl-de/by-2-0",
            "Daten geändert wurden",
        ),
        "school license terms",
    )
    redirect = contents["official_distribution_redirect_chain headers"].decode(
        "iso-8859-1", errors="strict"
    )
    _contains_all(
        redirect,
        ("302 Found", f"Location: {RESOLVED_DISTRIBUTION_URL}", "content-type: text/csv"),
        "school distribution redirect",
    )
    header_expectations = {
        "open_nrw_ckan_package_show headers": "application/json",
        "open_nrw_dcat_jsonld headers": "application/ld+json",
        "open_nrw_distribution_page headers": "text/html",
        "govdata_license_terms headers": "text/html",
    }
    for label, content_type in header_expectations.items():
        text = contents[label].decode("iso-8859-1", errors="strict").lower()
        if "200 ok" not in text or content_type not in text:
            raise SchoolOverlayError(f"{label} HTTP witness drift")


def _safe_projection(row: Sequence[str], counts: defaultdict[str, int]) -> Projection | None:
    postcode, locality, street_line = (row[index].strip() for index in SAFE_SOURCE_INDEXES)
    if any("\x00" in value for value in (postcode, locality, street_line)):
        raise SchoolOverlayError("school safe source value contains NUL")
    if _POSTCODE.fullmatch(postcode) is None or postcode == "00000":
        counts["source_rows_rejected_postcode"] += 1
        return None
    locality_norm = builder.normalize_text(locality)
    if not locality_norm:
        counts["source_rows_rejected_locality"] += 1
        return None
    if re.search(r"\bpostfach\b", builder.normalize_text(street_line)) is not None:
        counts["source_rows_rejected_postfach"] += 1
        return None
    matched = _STREET_HOUSE.fullmatch(street_line)
    if matched is None:
        counts["source_rows_rejected_street_house"] += 1
        return None
    street_norm = builder.normalize_text(matched.group(1))
    number = int(matched.group(2))
    if not street_norm or number <= 0 or number > 0xFFFF_FFFF:
        counts["source_rows_rejected_street_house"] += 1
        return None
    return Projection(
        street_norm=street_norm,
        locality_norm=locality_norm,
        number=number,
        suffix=matched.group(3).lower(),
        postcode=postcode,
    )


def load_projections(
    pinned: builder.PinnedFile, *, expected_rows: int
) -> tuple[dict[tuple[str, str, int, str], Projection], dict[str, int]]:
    """Project only the three safe address columns from the pinned source."""

    pinned.stream.seek(0)
    raw_bytes = pinned.stream.read()
    if raw_bytes.startswith(b"\xef\xbb\xbf"):
        raise SchoolOverlayError("school CSV must not contain a UTF-8 BOM")
    if b"\x00" in raw_bytes:
        raise SchoolOverlayError("school CSV must not contain NUL bytes")
    if b"\r" in raw_bytes or not raw_bytes.endswith(b"\n"):
        raise SchoolOverlayError("school CSV must use LF and have a final LF")
    if any(len(line) > MAX_SOURCE_LINE_BYTES for line in raw_bytes.splitlines()):
        raise SchoolOverlayError("school CSV row exceeds bounded limit")
    pinned.stream.seek(0)
    text = io.TextIOWrapper(pinned.stream, encoding="utf-8", errors="strict", newline="")
    counts: defaultdict[str, int] = defaultdict(int)
    grouped: dict[tuple[str, str, int, str], list[str]] = defaultdict(list)
    try:
        if text.readline() != "sep=;\n":
            raise SchoolOverlayError("school CSV separator declaration drift")
        reader = csv.reader(text, delimiter=";", quotechar='"', strict=True)
        try:
            header = next(reader)
        except StopIteration as exc:
            raise SchoolOverlayError("school CSV header is missing") from exc
        if tuple(header) != RAW_HEADER:
            raise SchoolOverlayError("school CSV header drift")
        for row in reader:
            counts["source_rows_seen"] += 1
            if len(row) != len(RAW_HEADER) - 1:
                raise SchoolOverlayError("school CSV row shape drift")
            projection = _safe_projection(row, counts)
            if projection is None:
                continue
            counts["source_rows_projected"] += 1
            grouped[projection.identity].append(projection.postcode)
    except (csv.Error, UnicodeDecodeError) as exc:
        raise SchoolOverlayError(f"school CSV parsing failed: {exc}") from exc
    finally:
        text.detach()
    if counts["source_rows_seen"] != expected_rows:
        raise SchoolOverlayError(
            f"school row-count pin mismatch: got {counts['source_rows_seen']}, "
            f"expected {expected_rows}"
        )
    accepted: dict[tuple[str, str, int, str], Projection] = {}
    for identity in sorted(grouped):
        postcodes = grouped[identity]
        distinct_postcodes = set(postcodes)
        if len(postcodes) != 1:
            counts["source_ambiguous_semantic_identities"] += 1
            counts["source_ambiguous_semantic_rows"] += len(postcodes)
            if len(distinct_postcodes) != 1:
                counts["source_conflicting_postcode_identities"] += 1
                counts["source_conflicting_postcode_rows"] += len(postcodes)
            continue
        accepted[identity] = Projection(
            street_norm=identity[0],
            locality_norm=identity[1],
            number=identity[2],
            suffix=identity[3],
            postcode=next(iter(distinct_postcodes)),
        )
    counts["accepted_source_identities"] = len(accepted)
    if not accepted:
        raise SchoolOverlayError("school source produced no safe source identities")
    if pinned.evidence["sha256"] == FROZEN_SOURCE_PIN.sha256:
        frozen_checks = {
            "source_rows_seen": 5_643,
            "source_rows_projected": 5_155,
            "source_rows_rejected_street_house": 488,
            "source_rows_rejected_postfach": 0,
            "source_ambiguous_semantic_identities": 233,
            "source_ambiguous_semantic_rows": 551,
            "accepted_source_identities": 4_604,
        }
        for key, expected in frozen_checks.items():
            if counts[key] != expected:
                raise SchoolOverlayError(f"frozen school source accounting drift at {key}")
    return accepted, dict(sorted(counts.items()))


def scan_builder(
    pinned: builder.PinnedFile,
    projections: Mapping[tuple[str, str, int, str], Projection],
    *,
    expected_rows: int,
) -> tuple[dict[tuple[str, str, int, str], str], dict[str, int]]:
    try:
        selected, counts = invariant.scan_builder(
            pinned, projections, expected_rows=expected_rows
        )
    except invariant.KiBizOverlayError as exc:
        raise SchoolOverlayError(str(exc).replace("KiBiz", "school")) from exc
    if pinned.evidence["sha256"] == FROZEN_BNETZA_BUILDER_PIN.sha256:
        expected_counts = {
            "builder_rows_scanned": 19_267_049,
            "fillable_builder_identities": 2_006,
            "fillable_builder_rows": 2_041,
            "source_identities_already_complete": 43,
            "source_identities_missing_from_builder": 2_555,
        }
        for key, expected in expected_counts.items():
            if counts.get(key, 0) != expected:
                raise SchoolOverlayError(f"frozen school builder accounting drift at {key}")
        if counts.get("source_identities_vetoed_locality_code", 0) != 0:
            raise SchoolOverlayError("frozen school builder locality-code veto drift")
        if counts.get("source_identities_vetoed_base_postcode", 0) != 0:
            raise SchoolOverlayError("frozen school builder postcode veto drift")
    return selected, counts


def write_output(
    pinned: builder.PinnedFile,
    selected: Mapping[tuple[str, str, int, str], str],
    output: Path,
) -> dict[str, int]:
    try:
        return invariant.write_output(pinned, selected, output)
    except invariant.KiBizOverlayError as exc:
        raise SchoolOverlayError(str(exc).replace("KiBiz", "school")) from exc


def _file_evidence(path: Path) -> dict[str, Any]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        while block := handle.read(8 * 2**20):
            digest.update(block)
            size += len(block)
    return {"path": str(path), "bytes": size, "sha256": digest.hexdigest()}


def _validate_config(config: Config) -> None:
    pinned_inputs = (
        ("school source card", config.source.source_card_pin),
        ("school license freeze", config.source.license_freeze_pin),
        ("school raw CSV", config.source.raw_csv_pin),
        ("school acquisition", config.source.acquisition_pin),
        ("school source audit", config.source.audit_pin),
        ("school response headers", config.source.response_headers_pin),
        ("builder CSV", config.builder_pin),
    )
    for label, pin in pinned_inputs:
        _validate_pin(pin, label)
    if config.expected_source_rows <= 0 or config.expected_builder_rows <= 0:
        raise SchoolOverlayError("expected row counts must be positive")
    if not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", config.source.snapshot_date):
        raise SchoolOverlayError("school source snapshot date must be YYYY-MM-DD")
    if config.status != STATUS:
        raise SchoolOverlayError("unknown output status")
    if config.minimum_free_bytes < 0:
        raise SchoolOverlayError("minimum free bytes must be non-negative")
    inputs = (
        config.source.source_card,
        config.source.license_freeze,
        config.source.raw_csv,
        config.source.acquisition,
        config.source.audit,
        config.source.response_headers,
        config.builder_csv,
    )
    outputs = (config.output_csv, config.receipt)
    for path in outputs:
        if path.exists() or path.is_symlink():
            raise SchoolOverlayError(f"refusing to overwrite output: {path}")
        if not path.parent.is_dir() or path.parent.is_symlink():
            raise SchoolOverlayError(
                f"output parent must be an existing real directory: {path.parent}"
            )
    resolved = [path.resolve(strict=True) for path in inputs]
    resolved.extend(path.resolve(strict=False) for path in outputs)
    if len(set(resolved)) != len(resolved):
        raise SchoolOverlayError("input and output paths must all be distinct")


def build(config: Config) -> dict[str, Any]:
    """Build the deterministic create-only postcode overlay and receipt."""

    _validate_config(config)
    if shutil.disk_usage(config.output_csv.parent).free < config.minimum_free_bytes:
        raise SchoolOverlayError("disk floor crossed before school overlay build")
    primary_specs = (
        ("school source card", config.source.source_card, config.source.source_card_pin),
        ("school license freeze", config.source.license_freeze, config.source.license_freeze_pin),
        ("school raw CSV", config.source.raw_csv, config.source.raw_csv_pin),
        ("school acquisition", config.source.acquisition, config.source.acquisition_pin),
        ("school source audit", config.source.audit, config.source.audit_pin),
        (
            "school response headers",
            config.source.response_headers,
            config.source.response_headers_pin,
        ),
        ("builder CSV", config.builder_csv, config.builder_pin),
    )
    opened: list[tuple[str, builder.PinnedFile]] = []
    metadata_opened: list[tuple[str, builder.PinnedFile]] = []
    try:
        for label, path, pin in primary_specs:
            opened.append((label, _open_pinned(path, pin, label)))
        files = {label: pinned for label, pinned in opened}
        card = _read_pinned_json(files["school source card"], "school source card")
        freeze = _read_pinned_json(files["school license freeze"], "school license freeze")
        acquisition = _read_pinned_json(files["school acquisition"], "school acquisition")
        audit = _read_pinned_json(files["school source audit"], "school source audit")
        files["school response headers"].stream.seek(0)
        headers = files["school response headers"].stream.read().decode(
            "iso-8859-1", errors="strict"
        )
        _validate_source_card(
            card, config.source, expected_source_rows=config.expected_source_rows
        )
        _validate_acquisition(
            acquisition,
            audit,
            headers,
            config.source,
            expected_source_rows=config.expected_source_rows,
        )
        metadata_specs = _validate_license_freeze_document(
            freeze, config.source, expected_source_rows=config.expected_source_rows
        )
        primary_paths = {pinned.path.resolve(strict=True) for _, pinned in opened}
        metadata_paths: set[Path] = set()
        for label, path, pin in metadata_specs:
            resolved = path.resolve(strict=True)
            if resolved in primary_paths or resolved in metadata_paths:
                raise SchoolOverlayError("school metadata evidence path is reused")
            metadata_paths.add(resolved)
            metadata_opened.append((label, _open_pinned(path, pin, label)))
        metadata_contents: dict[str, bytes] = {}
        for label, pinned in metadata_opened:
            pinned.stream.seek(0)
            metadata_contents[label] = pinned.stream.read()
        _validate_metadata_contents(metadata_contents)
        projections, source_counts = load_projections(
            files["school raw CSV"], expected_rows=config.expected_source_rows
        )
        selected, scan_counts = scan_builder(
            files["builder CSV"], projections, expected_rows=config.expected_builder_rows
        )
        write_counts = write_output(files["builder CSV"], selected, config.output_csv)
        for label, pinned in (*opened, *metadata_opened):
            _recheck_pinned(pinned, label)
        counts = dict(sorted({**source_counts, **scan_counts, **write_counts}.items()))
        counts["source_rows_added"] = 0
        if counts.get("output_rows") != config.expected_builder_rows:
            raise SchoolOverlayError("school overlay changed the builder row count")
        if counts.get("base_rows_retained") != config.expected_builder_rows:
            raise SchoolOverlayError("school overlay dropped a base row")
        if counts.get("blank_postcode_rows_filled") != counts.get("fillable_builder_rows"):
            raise SchoolOverlayError("school fill count changed between passes")
        if counts.get("blank_postcode_rows_filled", 0) <= 0:
            raise SchoolOverlayError("school postcode overlay is vacuous")
        receipt: dict[str, Any] = {
            "schema": SCHEMA,
            "status": config.status,
            "license": {
                "data": LICENSE,
                "name": LICENSE_NAME,
                "url": LICENSE_URL,
                "attribution": ATTRIBUTION,
                "changes_marked": True,
            },
            "policy": {
                "outcome_blind_full_source": True,
                "safe_source_value_columns": list(SAFE_SOURCE_COLUMNS),
                "fixed_state_code": FIXED_STATE_CODE,
                "source_identifier_values_read": False,
                "source_status_values_read": False,
                "source_school_name_type_values_read": False,
                "source_provider_contact_web_values_read": False,
                "source_coordinates_read": False,
                "source_coordinates_written": False,
                "source_rows_added": False,
                "strict_terminal_positive_single_house_only": True,
                "source_postfach_veto": True,
                "source_semantic_multiplicity_fail_closed": True,
                "exact_street_locality_house_suffix": True,
                "exactly_one_builder_locality_code": True,
                "base_nonblank_postcode_precedence": True,
                "base_coordinates_preserved": True,
                "base_row_order_preserved": True,
                "first_candidate_selection": False,
                "network_calls_during_build": 0,
                "gridpin_engine_calls_during_build": 0,
                "photon_engine_calls_during_build": 0,
            },
            "configuration": {
                "expected_source_rows": config.expected_source_rows,
                "expected_builder_rows": config.expected_builder_rows,
                "minimum_free_bytes": config.minimum_free_bytes,
                "maximum_source_line_bytes": MAX_SOURCE_LINE_BYTES,
                "single_house_grammar": _STREET_HOUSE.pattern,
                "raw_header": list(RAW_HEADER),
                "builder_header": list(builder.BUILDER_HEADER),
            },
            "inputs": {
                "source_card": dict(files["school source card"].evidence),
                "license_freeze": dict(files["school license freeze"].evidence),
                "raw_csv": dict(files["school raw CSV"].evidence),
                "acquisition": dict(files["school acquisition"].evidence),
                "source_audit": dict(files["school source audit"].evidence),
                "response_headers": dict(files["school response headers"].evidence),
                "license_chain": {
                    label: dict(pinned.evidence) for label, pinned in metadata_opened
                },
                "builder_csv": dict(files["builder CSV"].evidence),
            },
            "counts": counts,
            "output": _file_evidence(config.output_csv),
        }
        with config.receipt.open("xb") as handle:
            handle.write(canonical_json_bytes(receipt))
            handle.flush()
            os.fsync(handle.fileno())
        return receipt
    finally:
        for _, pinned in (*opened, *metadata_opened):
            pinned.stream.close()


def frozen_source_contract() -> SourceContract:
    return SourceContract(
        source_card=FROZEN_SOURCE_CARD,
        source_card_pin=FROZEN_SOURCE_CARD_PIN,
        license_freeze=FROZEN_LICENSE_FREEZE,
        license_freeze_pin=FROZEN_LICENSE_FREEZE_PIN,
        raw_csv=FROZEN_SOURCE,
        raw_csv_pin=FROZEN_SOURCE_PIN,
        acquisition=FROZEN_ACQUISITION,
        acquisition_pin=FROZEN_ACQUISITION_PIN,
        audit=FROZEN_AUDIT,
        audit_pin=FROZEN_AUDIT_PIN,
        response_headers=FROZEN_HEADERS,
        response_headers_pin=FROZEN_HEADERS_PIN,
        snapshot_date="2026-08-21",
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--builder-csv", type=Path, required=True)
    parser.add_argument("--builder-sha256", required=True)
    parser.add_argument("--builder-bytes", type=int, required=True)
    parser.add_argument("--expected-builder-rows", type=int, required=True)
    parser.add_argument("--output-csv", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--minimum-free-bytes", type=int, default=DEFAULT_MIN_FREE_BYTES)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    config = Config(
        source=frozen_source_contract(),
        expected_source_rows=5_643,
        builder_csv=args.builder_csv,
        builder_pin=Pin(args.builder_sha256, args.builder_bytes),
        expected_builder_rows=args.expected_builder_rows,
        output_csv=args.output_csv,
        receipt=args.receipt,
        minimum_free_bytes=args.minimum_free_bytes,
    )
    try:
        receipt = build(config)
    except SchoolOverlayError as exc:
        raise SystemExit(f"DE Schulgrunddaten NRW postcode overlay refused: {exc}") from exc
    print(json.dumps(receipt, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
